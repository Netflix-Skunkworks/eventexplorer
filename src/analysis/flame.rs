// Copyright 2026 Netflix, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::elog_parser::*;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Flame-graph aggregation (cpu provider)
// ---------------------------------------------------------------------------

/// Filter applied to [`CpuStackSample`] when building a flame-graph tree.
///
/// Multiple filters compose with AND semantics; an unset (empty) field
/// means "no constraint on that axis". Convention: `cpus` of an empty
/// slice means "all CPUs"; same for `pids`. `comms` matches the
/// sample-time process name ([`CpuProfile::process_comm_at`]) and
/// `ThreadInfo.tdname` (case-sensitive prefix match).
///
/// `CPU_SAMPLE_FLAG_IDLE` / `CPU_SAMPLE_FLAG_TRUNC` are *not* filter
/// knobs here. Current kernels (post May 2026) no longer emit idle
/// samples at all — the statclock tail hook returns early for
/// `TDF_IDLETD` threads — so nothing to filter out for fresh captures.
/// Older captures with empty-stack idle samples show up as samples on
/// the synthetic root (rendered as a single `IDLE` line in folded
/// output). `CPU_SAMPLE_FLAG_TRUNC` samples are rare in practice (≤ 14
/// out of 400 k on a 4 kHz live-traffic capture — 0.003 %) and not worth
/// a dedicated filter — they're included unconditionally and their
/// captured fragment is grafted onto the flame-graph root at the
/// deepest frame we did manage to unwind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlameFilter {
    /// Restrict to samples taken on these cpu ids. Empty = no constraint.
    pub cpus: Vec<u16>,
    /// Restrict to samples whose `pid` is in this list. Empty = no constraint.
    pub pids: Vec<u32>,
    /// Restrict to samples whose `tid` is in this list. Empty = no constraint.
    pub tids: Vec<u32>,
    /// Restrict to samples whose sample-time process name *or*
    /// `tdname` starts with one of these strings. Empty = no
    /// constraint. Useful for "all nginx threads" (`comms =
    /// ["nginx"]`) or "the rack TCP pacer" (`comms = ["rack_pace"]`).
    /// Samples with no resolvable process name and no `tdname` hit are
    /// dropped when `comms` is non-empty.
    pub comms: Vec<String>,
    /// Restrict to samples whose timestamp falls in `[ts_start_us,
    /// ts_end_us]`. `u64::MAX` for `ts_end_us` (the default) means
    /// "no upper bound".
    pub ts_start_us: u64,
    pub ts_end_us: u64,
    /// Keep only samples whose stack hits at least one of these
    /// selectors. Empty = no constraint. GUI: CTRL+click flame bars.
    pub include_stacks: Vec<StackSelector>,
    /// Drop samples whose stack hits any of these selectors (wins
    /// over `include_stacks`). GUI: ALT+click a flame bar.
    pub exclude_stacks: Vec<StackSelector>,
}

/// One stack-filter term for [`FlameFilter::include_stacks`] /
/// [`FlameFilter::exclude_stacks`]. A sample "hits" a selector when
/// its stack passes through the matching frame (or group bar).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackSelector {
    /// `(addr, pid)` in the tree's node-key space: the canonical
    /// symbol start in `BySymbol` mode, the raw PC in `ByPc` mode;
    /// pid 0 for kernel frames. A sample hits when any of its frames
    /// keys to the pair (overlay PCs count too, so a `+0xINNER`
    /// sliver clicked in the GUI stays clickable).
    Frame(u64, u32),
    /// Process / kernel-thread group bar label (`group_by_process`).
    /// A sample hits when its process-group label — or, for kernel
    /// samples, its thread-group label — equals the string.
    Group(String),
    /// Frame display name (`tcp_input`, a DSO basename,
    /// `kernel+0xOFFSET`). A sample hits when any frame's label
    /// equals the string, or when its process / thread group label
    /// does. Compare mode uses this so ALT/CTRL-click match by name
    /// on both captures.
    Name(String),
}

// Hand-written so the all-defaults value matches `new()`: `ts_end_us`
// must be `u64::MAX` ("no upper bound"), not the `0` a derived `Default`
// would produce. A `0` upper bound silently filters out every sample
// (see `matches`), which manifests as an empty flame graph.
impl Default for FlameFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl FlameFilter {
    pub fn new() -> Self {
        Self {
            cpus: Vec::new(),
            pids: Vec::new(),
            tids: Vec::new(),
            comms: Vec::new(),
            ts_start_us: 0,
            ts_end_us: u64::MAX,
            include_stacks: Vec::new(),
            exclude_stacks: Vec::new(),
        }
    }

    pub(crate) fn matches(&self, s: &CpuStackSample, profile: &CpuProfile) -> bool {
        if s.ts < self.ts_start_us || s.ts > self.ts_end_us {
            return false;
        }
        if !self.cpus.is_empty() && !self.cpus.contains(&s.cpu) {
            return false;
        }
        let owner = profile.sample_owner_pid(s.tid, s.pid, s.ts, &s.frames);
        if !self.pids.is_empty() && !self.pids.contains(&owner) {
            return false;
        }
        if !self.tids.is_empty() && !self.tids.contains(&s.tid) {
            return false;
        }
        if !self.comms.is_empty() {
            let proc_comm = profile.process_comm_at(owner, Some(s.tid), s.ts);
            let tdname = profile.threads.get(&s.tid).map(|i| i.tdname.as_str());
            let hit = self.comms.iter().any(|prefix| {
                proc_comm.is_some_and(|c| c.starts_with(prefix))
                    || tdname.is_some_and(|t| t.starts_with(prefix))
            });
            if !hit {
                return false;
            }
        }
        true
    }
}

/// One node of a built flame-graph tree.
///
/// The synthetic root has `addr == 0` (no real kernel pc lands at 0).
/// Each child represents a frame *one level closer to the leaf* than
/// its parent — i.e. children are the next-deeper-into-the-call-stack
/// frame relative to their parent. `total_count` is inclusive (this
/// frame plus everything below it); `self_count` is exclusive — the
/// number of samples whose *leaf* frame is this node. The classic
/// flame-graph rectangle for this node is sized by `total_count`; the
/// "self time" stripe at the top edge is sized by `self_count`.
#[derive(Debug, Clone)]
pub struct FlameNode {
    /// In [`FlameAggregation::ByPc`] mode this is the raw PC for the
    /// frame (same value as the on-wire `frames[]` entry — kernel or
    /// user-space). In [`FlameAggregation::BySymbol`] mode this is
    /// the *canonical* address for the resolved symbol
    /// (`LinkerFile.base + SymbolRange.offset` for kernel hits;
    /// `ProcMap.base + (UserSymbolRange.offset - ProcMap.offset)`
    /// — i.e. the symbol's runtime VA in the owning pid — for user
    /// hits) so all PCs that landed in the same function collapse
    /// onto one node. Unresolved frames keep their raw PC. Synthetic
    /// per-PC "self-offset" overlay children
    /// (`is_self_pc == true`) carry the original raw PC. `0` is
    /// reserved for the synthetic root.
    pub addr: u64,
    /// Pid the frame was sampled in, or `0` for the synthetic root
    /// and for kernel-resolved frames (whose runtime VA is global).
    /// Tree-key matching uses `(addr, pid)` so user-space PCs from
    /// different pids never conflate even if their runtime VAs
    /// happen to collide (they almost never do, thanks to ASLR, but
    /// making the key pid-scoped avoids a silent correctness gap
    /// and lets the renderer thread the pid through to
    /// `CpuProfile::symbolize` without a side-table lookup).
    pub pid: u32,
    /// Number of samples whose call stack passes through this frame.
    pub total_count: u64,
    /// Number of samples whose *leaf* frame equals this frame.
    /// In [`FlameAggregation::BySymbol`] with `with_offset_overlay`
    /// enabled, leaf-self samples for *resolved* symbols are recorded
    /// as `self_count` on a synthetic child instead, so the parent
    /// function node typically reads `self_count == 0`.
    pub self_count: u64,
    pub children: Vec<FlameNode>,
    /// Synthetic per-PC overlay child injected by
    /// [`FlameAggregation::BySymbol`] with `with_offset_overlay`. The
    /// renderer labels these with just `+0xINNER` (the function name
    /// is already on the parent) and styles them so they don't read
    /// as a real callee. Always `false` in `ByPc` mode and on the
    /// synthetic root. `addr` is the original raw PC.
    pub is_self_pc: bool,
    /// Process / kernel grouping bar injected by
    /// [`build_flame_tree`] when `group_by_process` is set. The
    /// renderer and [`fold_flame_tree`] use this string as the
    /// frame label (`"kernel"` or the process `comm`) instead of
    /// symbolising `addr`.
    pub group_label: Option<String>,
}

/// How [`build_flame_tree`] keys its tree nodes.
///
/// `BySymbol { with_offset_overlay: true }` is the **default**: it
/// gives the classic Brendan Gregg flame-graph look (one bar per
/// function) while still preserving per-instruction hot-spot detail
/// as small `+0xINNER` overlay rectangles above each function bar.
/// Falls back transparently to per-PC keying for any frame whose
/// symbol can't be resolved (no `SYMBOL_RANGE` match), so a capture
/// with no symbol metadata behaves identically to `ByPc`.
///
/// `ByPc` is the original behaviour — every unique program counter
/// becomes its own node. Maximum detail; same-function hot
/// instructions show up as separate adjacent rectangles ("the
/// function is shattered"), which is great for spotting tight loops
/// vs. straight-line work but visually noisy on big call graphs.
/// Useful as an opt-out when you want the raw per-PC view of a
/// capture that *does* have symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlameAggregation {
    /// One tree node per unique program counter.
    ByPc,
    /// One tree node per resolved symbol (collapsed); unresolved PCs
    /// keep their raw addresses as keys.
    BySymbol {
        /// When true (the GUI default), leaf-self samples for
        /// resolved symbols are recorded on synthetic per-PC child
        /// nodes (`is_self_pc == true`) instead of bumping the
        /// parent symbol's `self_count`, so the renderer can paint
        /// hot-instruction overlay rectangles above each function
        /// bar. The CLI sets `false` so folded output reads like
        /// `stackcollapse-perf.pl` (no extra nested level).
        with_offset_overlay: bool,
    },
}

impl FlameAggregation {
    /// GUI default: collapse-by-symbol with per-PC overlay rectangles
    /// painted above each function bar. Falls through to per-PC
    /// behaviour for any frame whose symbol can't be resolved
    /// (no `SYMBOL_RANGE` match), so this is safe to use
    /// unconditionally — captures without `--embed-symbols` end up
    /// rendering identically to `ByPc`.
    pub const DEFAULT_GUI: Self = Self::BySymbol {
        with_offset_overlay: true,
    };
    /// CLI flavour of the default: collapse-by-symbol *without* the
    /// overlay (a nested `+0xINNER` level renders awkwardly through
    /// `flamegraph.pl`).
    pub const DEFAULT_CLI: Self = Self::BySymbol {
        with_offset_overlay: false,
    };
}

impl Default for FlameAggregation {
    fn default() -> Self {
        // Match the GUI default so any code path that constructs a
        // `FlameAggregation::default()` (rather than picking
        // explicitly) behaves the same as a freshly-opened flame view.
        Self::DEFAULT_GUI
    }
}

impl FlameNode {
    fn new(addr: u64, pid: u32) -> Self {
        Self {
            addr,
            pid,
            total_count: 0,
            self_count: 0,
            children: Vec::new(),
            is_self_pc: false,
            group_label: None,
        }
    }

    fn child_mut(&mut self, addr: u64, pid: u32) -> &mut FlameNode {
        // Linear search rather than a map: child fanout is typically
        // very low (a handful of unique callees per kernel function),
        // and the linear scan keeps the tree compact and cache-friendly.
        // Real callee children are matched by `(addr, pid)` only —
        // overlay children are kept separate (see `self_pc_child_mut`)
        // so a genuine callee that happens to share an address with
        // a self-pc entry can't accidentally absorb it.
        if let Some(idx) = self
            .children
            .iter()
            .position(|c| c.addr == addr && c.pid == pid && !c.is_self_pc)
        {
            &mut self.children[idx]
        } else {
            self.children.push(FlameNode::new(addr, pid));
            self.children.last_mut().unwrap()
        }
    }

    /// Used by [`build_flame_tree`] in `BySymbol { with_offset_overlay: true }`
    /// mode: find-or-insert a synthetic overlay child for the given
    /// raw PC, marked `is_self_pc`. Distinct from `child_mut` so an
    /// overlay child for `pc` and a real callee whose entry happens
    /// to be at the same address don't collide on lookup.
    fn self_pc_child_mut(&mut self, pc: u64, pid: u32) -> &mut FlameNode {
        if let Some(idx) = self
            .children
            .iter()
            .position(|c| c.addr == pc && c.pid == pid && c.is_self_pc)
        {
            &mut self.children[idx]
        } else {
            self.children.push(FlameNode {
                addr: pc,
                pid,
                total_count: 0,
                self_count: 0,
                children: Vec::new(),
                is_self_pc: true,
                group_label: None,
            });
            self.children.last_mut().unwrap()
        }
    }

    /// Find-or-insert a process / kernel grouping child, keyed on
    /// `label` so every pid that shares a `comm` (and every kernel
    /// thread) collapses onto one bar. `pid` is the first pid we
    /// saw for this label — kept so the renderer can colour the
    /// bar with the same hash the CPU-usage plot uses.
    fn group_child_mut(&mut self, label: &str, pid: u32) -> &mut FlameNode {
        if let Some(idx) = self
            .children
            .iter()
            .position(|c| c.group_label.as_deref() == Some(label))
        {
            &mut self.children[idx]
        } else {
            self.children.push(FlameNode {
                addr: FLAME_GROUP_ADDR,
                pid,
                total_count: 0,
                self_count: 0,
                children: Vec::new(),
                is_self_pc: false,
                group_label: Some(label.to_string()),
            });
            self.children.last_mut().unwrap()
        }
    }
}

/// Sentinel address for [`FlameNode::group_label`] bars. Real
/// program counters sit well below `u64::MAX`, so zoom-path
/// `(addr, pid)` keys for these nodes stay distinct from
/// symbolised frames.
pub const FLAME_GROUP_ADDR: u64 = u64::MAX;
pub const KERNEL_GROUP_LABEL: &str = "kernel";
pub const USER_GROUP_LABEL: &str = "user";

/// amd64 user / kernel VA split. Used to decide whether a sample's
/// pid is a user process (any user-space frame, or `USER_MODE`) so
/// its stacks sit under `comm` rather than the synthetic `kernel`
/// bar. Same constant [`build_flame_tree`] uses for unresolved-PC
/// pid scoping.
const USER_KERNEL_BOUNDARY: u64 = 0x0000_8000_0000_0000;

/// Lowest address treated as a real user PC. The first page is
/// unmapped on amd64; a stack walk that runs off a kernel thread
/// often leaves a value there.
const USER_PC_FLOOR: u64 = 0x1000;

/// Whether a program counter lies in the kernel half of the virtual
/// address space. The flame renderer uses the same classification as
/// the tree builder so unresolved kernel PCs still receive kernel styling.
pub fn flame_pc_is_kernel(addr: u64) -> bool {
    addr >= USER_KERNEL_BOUNDARY
}

/// Build a flame-graph tree from a [`CpuProfile`] under the given
/// [`FlameFilter`] and [`FlameAggregation`] strategy.
///
/// The wire format stores frames leaf-first (`frames[0]` is the
/// instruction the CPU was executing); the classic flame-graph
/// orientation puts the root call at the bottom and the leaf at the
/// top, so we walk each sample's frames in *reverse* — `frames.last()`
/// (the deepest reachable caller) becomes the first child of the
/// synthetic root. The synthetic root's `total_count` equals the number
/// of samples that survived the filter and is also the denominator for
/// any "% of CPU time" calculation in the renderer.
///
/// `split_user_by_pid` controls whether resolved user-mode frames stay
/// scoped to their owning pid (`true`, the historical behaviour —
/// useful for attributing cost per-process) or fold across pids that
/// share the same code (`false`, the default — N forks of the same
/// daemon produce one `_start;main;…` tower instead of N towers). In
/// the combined mode, user-mode frames that the symbolizer resolved
/// are keyed on the stable `(UserFile::user_file_id,
/// UserSymbolRange::offset)` identity (and `(user_file_id, 0)` for
/// [`SymbolHit::UserModuleOnly`]). Unresolved PCs always fall back to
/// being keyed by their raw PC (pid-scoped for user-space frames) — we
/// have no way to prove two raw PCs in two address spaces point at the
/// same code, so each distinct PC stays its own node.
///
/// `group_by_process` inserts a synthetic first frame under the root:
/// the process `comm` for user pids, or `"kernel"` for kernel threads
/// (pid 0, no user mapping, no user-mode sample, no user-space frame).
/// Same-`comm` pids share one bar. Under `kernel`, a second bar
/// splits samples by thread `comm` (`idle`, `intr`, `clock`, …).
/// The GUI default is on; the CLI default is off
/// so folded-stack output stays `stackcollapse-perf.pl` shaped
/// unless `--group-by-process` is passed. `kernel_group` false
/// keeps the user-process bars and hangs kernel stacks on the
/// synthetic root.
///
/// `kernel_user_base` (see [`build_flame_tree_with_base`]) inserts
/// `kernel` and `user` under the synthetic root before that grouping.
/// User-process samples hang off `user`; kernel-thread samples hang
/// off `kernel`. The GUI draws those two bars as the bottom row.
/// Tree key for one frame: `(key, key_pid, overlay_pc)`.
///
/// In ByPc the key is always the raw PC and the pid scopes the node
/// (so user PCs from different pids stay separate even if their
/// runtime VAs happen to coincide). In BySymbol the key is the
/// symbol's *runtime* start address (kernel: file.base + sym.offset;
/// user: map.base + sym.offset - map.offset), so all PCs that landed
/// in the same function fold to one node. Kernel hits use pid 0
/// because their runtime VA is global. User hits honour
/// `split_user_by_pid`: when true (legacy) they carry the sample's
/// pid so forks of the same binary stay in separate trees; when
/// false (default) they get canonicalised onto the first pid we ever
/// saw for the `(user_file_id, symbol.offset)` identity so
/// ASLR-independent cross-pid merging works. The canonical pair is a
/// real runtime-VA / pid combination (the first caller's), so the
/// downstream `profile.symbolize(addr, Some(pid))` in the formatter
/// still resolves correctly. Unresolved PCs are keyed by raw addr;
/// in the default cross-pid mode they merge on pid 0 (identical raw
/// VAs across forked workers are the same code), and only stay
/// pid-scoped under `split_user_by_pid`.
///
/// `overlay_pc` is the raw-PC key used for the BySymbol-with-overlay
/// synthetic child. In split mode it's the raw PC; in combined mode
/// we re-express it as `canonical_parent_addr + inner_offset` so
/// identical leaf instructions from different pids merge onto one
/// overlay rectangle (otherwise each pid's ASLR slide would produce
/// its own raw-PC sliver).
///
/// The canon tables are the same lazily-built ones
/// [`build_flame_tree`] uses on its insert walk; shared so the
/// stack-filter pre-pass and the insert agree on canonical identity.
#[allow(clippy::too_many_arguments)]
fn frame_key(
    profile: &CpuProfile,
    addr: u64,
    sample_pid: u32,
    by_symbol: bool,
    split_user_by_pid: bool,
    user_sym_canon: &mut HashMap<(u32, u64), (u64, u32)>,
    user_mod_canon: &mut HashMap<u32, (u64, u32)>,
) -> (u64, u32, u64) {
    // Resolve once and reuse: the symbolizer trial-runs the kernel
    // path, then (when pid is known) the user path, so we get the
    // right `(key, key_pid)` for either origin in a single call.
    let hit = profile.symbolize(addr, Some(sample_pid));
    if by_symbol {
        match &hit {
            Some(SymbolHit::Kernel { file, symbol, .. }) => (file.base + symbol.offset, 0u32, addr),
            Some(SymbolHit::User {
                file,
                map,
                symbol,
                inner_offset,
            }) => {
                let raw_runtime_addr = map
                    .base
                    .wrapping_add(symbol.offset)
                    .wrapping_sub(map.offset);
                if split_user_by_pid {
                    (raw_runtime_addr, sample_pid, addr)
                } else {
                    let canon = *user_sym_canon
                        .entry((file.user_file_id, symbol.offset))
                        .or_insert((raw_runtime_addr, sample_pid));
                    // Rebuild the overlay's raw PC against the
                    // canonical parent so pid 1's `+0x12` and pid 2's
                    // `+0x12` map to the same overlay child.
                    (canon.0, canon.1, canon.0.wrapping_add(*inner_offset))
                }
            }
            Some(SymbolHit::UserModuleOnly {
                file,
                map,
                runtime_offset,
            }) => {
                if split_user_by_pid {
                    (map.base, sample_pid, addr)
                } else {
                    let canon = *user_mod_canon
                        .entry(file.user_file_id)
                        .or_insert((map.base, sample_pid));
                    (canon.0, canon.1, canon.0.wrapping_add(*runtime_offset))
                }
            }
            None => {
                // Unresolved frame. Kernel PCs are global, so they
                // always merge on pid 0. Unresolved user PCs merge on
                // pid 0 too in the default cross-pid mode: forked
                // workers (nginx &c.) share an address-space layout,
                // so an unresolved frame at the same raw VA across
                // pids is the same code and should fold into one bar
                // rather than one tower per worker. Only
                // `split_user_by_pid` keeps them scoped.
                let is_kernel = flame_pc_is_kernel(addr);
                let kp = if is_kernel || !split_user_by_pid {
                    0
                } else {
                    sample_pid
                };
                (addr, kp, addr)
            }
        }
    } else {
        // ByPc: kernel PCs are global so use pid 0 to avoid
        // pointlessly splitting kernel-frame nodes by which pid
        // happened to cause the sample. User PCs use the sample's
        // pid so they stay scoped — in ByPc there's no stable
        // per-symbol identity to fold on, and two pids' raw PCs for
        // the same instruction only coincide when their ASLR slides
        // happen to match (which is *not* a reliable merging signal;
        // if you want cross-pid merging for a ByPc view, user-mode
        // bars will always have to split). `split_user_by_pid`
        // therefore has no effect in ByPc mode.
        let pid_for_key = match &hit {
            Some(SymbolHit::Kernel { .. }) => 0,
            _ => sample_pid,
        };
        (addr, pid_for_key, addr)
    }
}

/// Stack-selector gate for one sample (the GUI's CTRL/ALT-click
/// flame filters). An empty include list passes everything;
/// otherwise the sample must hit at least one include selector. A
/// sample hitting any exclude selector is dropped — exclude wins
/// over include.
#[allow(clippy::too_many_arguments)]
fn stack_filter_passes(
    profile: &CpuProfile,
    filter: &FlameFilter,
    s: &CpuStackSample,
    frame_keys: &[(u64, u32, u64)],
    has_group_selector: bool,
    user_pids: &FxHashSet<u32>,
    threads: &FxHashMap<u32, ThreadInfo>,
) -> bool {
    // Group labels are computed only when a Group selector exists.
    let (proc_label, thread_label) = if has_group_selector {
        let p = process_group_label(s, user_pids, profile);
        let t = (p == KERNEL_GROUP_LABEL).then(|| thread_name_for_sample(s, threads));
        (Some(p), t)
    } else {
        (None, None)
    };
    let hit = |sel: &StackSelector| match sel {
        StackSelector::Frame(addr, pid) => frame_keys.iter().any(|&(key, key_pid, overlay_pc)| {
            key_pid == *pid && (key == *addr || overlay_pc == *addr)
        }),
        StackSelector::Group(label) => {
            if label == USER_GROUP_LABEL {
                proc_label
                    .as_deref()
                    .is_some_and(|p| p != KERNEL_GROUP_LABEL)
            } else {
                proc_label.as_deref() == Some(label.as_str())
                    || thread_label.as_deref() == Some(label.as_str())
            }
        }
        StackSelector::Name(name) => {
            proc_label.as_deref() == Some(name.as_str())
                || thread_label.as_deref() == Some(name.as_str())
                || frame_keys.iter().any(|&(key, key_pid, overlay_pc)| {
                    frame_display_name(profile, key, key_pid) == name.as_str()
                        || (overlay_pc != key
                            && frame_display_name(profile, overlay_pc, key_pid) == name.as_str())
                })
        }
    };
    if !filter.include_stacks.is_empty() && !filter.include_stacks.iter().any(&hit) {
        return false;
    }
    !filter.exclude_stacks.iter().any(hit)
}

/// Return the sample indices selected by the same filter semantics used by
/// [`build_flame_tree`]. Function rankings and stack drill-downs use this
/// helper so their totals agree with the Flame tab, including CTRL/ALT stack
/// selectors and process-group selectors.
pub(crate) fn filtered_sample_indices(profile: &CpuProfile, filter: &FlameFilter) -> Vec<usize> {
    let mut user_sym_canon: HashMap<(u32, u64), (u64, u32)> = HashMap::new();
    let mut user_mod_canon: HashMap<u32, (u64, u32)> = HashMap::new();
    let has_group_selector = filter
        .include_stacks
        .iter()
        .chain(filter.exclude_stacks.iter())
        .any(|s| matches!(s, StackSelector::Group(_) | StackSelector::Name(_)));
    let stack_filters_on = !filter.include_stacks.is_empty() || !filter.exclude_stacks.is_empty();
    let user_pids = if has_group_selector {
        collect_user_pids(profile)
    } else {
        FxHashSet::default()
    };
    let mut frame_keys = Vec::new();
    let mut selected = Vec::new();

    for (idx, sample) in profile.samples.iter().enumerate() {
        if !filter.matches(sample, profile) {
            continue;
        }
        if stack_filters_on {
            frame_keys.clear();
            for &addr in sample.frames.iter().rev() {
                frame_keys.push(frame_key(
                    profile,
                    addr,
                    sample.pid,
                    true,
                    false,
                    &mut user_sym_canon,
                    &mut user_mod_canon,
                ));
            }
            if !stack_filter_passes(
                profile,
                filter,
                sample,
                &frame_keys,
                has_group_selector,
                &user_pids,
                &profile.threads,
            ) {
                continue;
            }
        }
        selected.push(idx);
    }
    selected
}

/// One filtered sample whose symbol-key stack contains the clicked
/// flame path, plus the symbol key of the frame directly under that
/// path (a direct callee). `callee` is absent when the path is the leaf.
pub struct StackPathSample {
    pub index: usize,
    pub callee: Option<(u64, u32)>,
}

/// Samples that pass `filter` and contain `path` as a contiguous run of
/// BySymbol frame keys. `FLAME_GROUP_ADDR` entries in `path` are ignored
/// so a process-grouped click still matches the symbol stack under it.
///
/// An empty `path` keeps samples that contain `anchor` and reports the
/// symbol key of that function plus its direct callee. Canon tables are
/// filled in sample order, matching [`build_flame_tree`].
pub fn samples_on_symbol_path(
    profile: &CpuProfile,
    filter: &FlameFilter,
    path: &[(u64, u32)],
    split_user_by_pid: bool,
    anchor: Option<super::ProfileFunctionId>,
) -> (Vec<StackPathSample>, Option<(u64, u32)>) {
    let path: Vec<(u64, u32)> = path
        .iter()
        .copied()
        .filter(|(addr, _)| *addr != FLAME_GROUP_ADDR)
        .collect();
    let mut user_sym_canon: HashMap<(u32, u64), (u64, u32)> = HashMap::new();
    let mut user_mod_canon: HashMap<u32, (u64, u32)> = HashMap::new();
    let mut frame_keys: Vec<(u64, u32, u64)> = Vec::new();
    let mut out = Vec::new();
    let mut anchor_key = None;
    for (index, sample) in profile.samples.iter().enumerate() {
        if !filter.matches(sample, profile) {
            continue;
        }
        frame_keys.clear();
        for &addr in sample.frames.iter().rev() {
            frame_keys.push(frame_key(
                profile,
                addr,
                sample.pid,
                true,
                split_user_by_pid,
                &mut user_sym_canon,
                &mut user_mod_canon,
            ));
        }
        let keys: Vec<(u64, u32)> = frame_keys.iter().map(|&(key, pid, _)| (key, pid)).collect();
        if path.is_empty() {
            let Some(anchor) = anchor else {
                continue;
            };
            let Some(pos) = keys.iter().position(|&(addr, pid)| {
                super::resolve_profile_function(profile, addr, pid)
                    .is_some_and(|hit| hit.id == anchor)
            }) else {
                continue;
            };
            if anchor_key.is_none() {
                anchor_key = Some(keys[pos]);
            }
            out.push(StackPathSample {
                index,
                callee: keys.get(pos + 1).copied(),
            });
        } else if let Some(pos) = contiguous_path(&keys, &path) {
            out.push(StackPathSample {
                index,
                callee: keys.get(pos + path.len()).copied(),
            });
        }
    }
    (out, anchor_key)
}

fn contiguous_path(keys: &[(u64, u32)], path: &[(u64, u32)]) -> Option<usize> {
    if path.is_empty() || keys.len() < path.len() {
        return None;
    }
    keys.windows(path.len()).position(|window| window == path)
}

pub fn build_flame_tree(
    profile: &CpuProfile,
    filter: &FlameFilter,
    aggregation: FlameAggregation,
    split_user_by_pid: bool,
    group_by_process: bool,
) -> FlameNode {
    build_flame_tree_with_base(
        profile,
        filter,
        aggregation,
        split_user_by_pid,
        group_by_process,
        false,
        true,
    )
}

/// [`build_flame_tree`] with a `kernel` / `user` bar under the synthetic
/// root. `kernel_user_base` false is [`build_flame_tree`].
pub fn build_flame_tree_with_base(
    profile: &CpuProfile,
    filter: &FlameFilter,
    aggregation: FlameAggregation,
    split_user_by_pid: bool,
    group_by_process: bool,
    kernel_user_base: bool,
    kernel_group: bool,
) -> FlameNode {
    build_flame_tree_filtered(
        profile,
        filter,
        aggregation,
        split_user_by_pid,
        group_by_process,
        kernel_user_base,
        kernel_group,
        None,
    )
}

#[allow(clippy::too_many_arguments)] // kernel_group joins the existing layout flags
pub fn build_flame_tree_filtered(
    profile: &CpuProfile,
    filter: &FlameFilter,
    aggregation: FlameAggregation,
    split_user_by_pid: bool,
    group_by_process: bool,
    kernel_user_base: bool,
    kernel_group: bool,
    sample_indices: Option<&[usize]>,
) -> FlameNode {
    // Pre-compute aggregation-mode booleans to avoid pattern-matching
    // on every frame iteration.
    let by_symbol = matches!(aggregation, FlameAggregation::BySymbol { .. });
    let with_overlay = matches!(
        aggregation,
        FlameAggregation::BySymbol {
            with_offset_overlay: true
        }
    );

    // Canonicalisation tables for cross-pid merging. Populated lazily
    // as we observe each `SymbolHit::User` / `SymbolHit::UserModuleOnly`
    // for the first time. The canonical `(addr, pid)` is the first
    // pid's runtime-VA pair for that symbol/module, so the renderer's
    // `profile.symbolize(canonical_addr, Some(canonical_pid))` call
    // continues to resolve correctly after merging (it walks the
    // canonical pid's `ProcMap` list, which still lives in the
    // profile). Kept as `HashMap` (not `BTreeMap`) because the lookup
    // is on the hot per-frame path; empty and unused when
    // `split_user_by_pid == true`.
    //
    // - `user_sym_canon`: `(user_file_id, symbol.offset) ->
    //   (canonical_runtime_addr, canonical_pid)`. Maps every future
    //   `SymbolHit::User` for the same `(file, symbol)` onto the
    //   first pid we ever saw for that symbol, so all samples fold
    //   into one node.
    // - `user_mod_canon`: `user_file_id -> (canonical_map_base,
    //   canonical_pid)`. Same idea for `SymbolHit::UserModuleOnly`,
    //   where "the DSO" is the finest-grained identity we have.
    let mut user_sym_canon: HashMap<(u32, u64), (u64, u32)> = HashMap::new();
    let mut user_mod_canon: HashMap<u32, (u64, u32)> = HashMap::new();

    let has_group_selector = filter
        .include_stacks
        .iter()
        .chain(filter.exclude_stacks.iter())
        .any(|s| matches!(s, StackSelector::Group(_) | StackSelector::Name(_)));
    let stack_filters_on = !filter.include_stacks.is_empty() || !filter.exclude_stacks.is_empty();

    let user_pids = if group_by_process || kernel_user_base || has_group_selector {
        collect_user_pids(profile)
    } else {
        FxHashSet::default()
    };

    let mut root = FlameNode::new(0, 0);
    // Scratch buffer of one frame's `(key, key_pid, overlay_pc)` per
    // frame, reused across samples. Computed once per sample so the
    // stack filter and the insert walk agree on every key.
    let mut frame_keys: Vec<(u64, u32, u64)> = Vec::new();
    let sample_iter: Box<dyn Iterator<Item = &CpuStackSample>> = match sample_indices {
        Some(indices) => Box::new(indices.iter().filter_map(|&i| profile.samples.get(i))),
        None => Box::new(profile.samples.iter()),
    };
    for s in sample_iter {
        if !filter.matches(s, profile) {
            continue;
        }
        // Walk root-most → leaf-most. `frames` is leaf-first on the
        // wire, so iterate in reverse.
        frame_keys.clear();
        let owner = profile.sample_owner_pid(s.tid, s.pid, s.ts, &s.frames);
        for &addr in s.frames.iter().rev() {
            frame_keys.push(frame_key(
                profile,
                addr,
                s.pid,
                by_symbol,
                split_user_by_pid,
                &mut user_sym_canon,
                &mut user_mod_canon,
            ));
        }
        if stack_filters_on
            && !stack_filter_passes(
                profile,
                filter,
                s,
                &frame_keys,
                has_group_selector,
                &user_pids,
                &profile.threads,
            )
        {
            continue;
        }
        root.total_count += 1;
        // Optional process / kernel bar sits between the synthetic
        // root and the first real frame. Empty-stack samples become
        // self-time on that bar (or on the root when grouping is off).
        // `kernel_user_base` puts `user` or `kernel` on first; process
        // and thread bars then hang off that bar.
        let mut node = if kernel_user_base {
            let label = process_group_label(s, &user_pids, profile);
            let is_kernel = label == KERNEL_GROUP_LABEL;
            let (mode_label, mode_pid) = if is_kernel {
                (KERNEL_GROUP_LABEL, 0)
            } else {
                (USER_GROUP_LABEL, super::USER_PSEUDO_PID)
            };
            let mode = root.group_child_mut(mode_label, mode_pid);
            mode.total_count += 1;
            if group_by_process {
                if is_kernel {
                    kernel_thread_child(mode, s, &profile.threads)
                } else {
                    let group = mode.group_child_mut(&label, owner);
                    group.total_count += 1;
                    group
                }
            } else {
                mode
            }
        } else if group_by_process {
            let label = process_group_label(s, &user_pids, profile);
            if !kernel_group && label == KERNEL_GROUP_LABEL {
                &mut root
            } else {
                let group_pid = if label == KERNEL_GROUP_LABEL {
                    0
                } else {
                    profile.sample_owner_pid(s.tid, s.pid, s.ts, &s.frames)
                };
                let group = root.group_child_mut(&label, group_pid);
                group.total_count += 1;
                if label == KERNEL_GROUP_LABEL {
                    kernel_thread_child(group, s, &profile.threads)
                } else {
                    group
                }
            }
        } else {
            &mut root
        };
        if s.frames.is_empty() {
            node.self_count += 1;
            continue;
        }
        let nframes = s.frames.len();
        for (i, &(key, key_pid, overlay_pc)) in frame_keys.iter().enumerate() {
            node = node.child_mut(key, key_pid);
            node.total_count += 1;
            if i == nframes - 1 {
                // Leaf frame. In ByPc / BySymbol-without-overlay we
                // bump self_count directly. In BySymbol-with-overlay
                // we route resolved leaves into a synthetic per-PC
                // child so the renderer can paint an offset overlay;
                // unresolved leaves still bump self_count directly
                // (no offset detail to show). Overlay children
                // inherit the parent node's (possibly canonicalised)
                // pid so the formatter can re-symbolise them through
                // the same pid-scoped path as the parent.
                if with_overlay && key != overlay_pc {
                    let overlay = node.self_pc_child_mut(overlay_pc, key_pid);
                    overlay.total_count += 1;
                    overlay.self_count += 1;
                } else {
                    node.self_count += 1;
                }
            }
        }
    }
    root
}

/// Emit a flame-graph tree in **folded-stack** format, the input
/// expected by Brendan Gregg's `flamegraph.pl`:
///
/// ```text
/// frame_root;frame_mid;frame_leaf <count>
/// ```
///
/// One line per leaf frame in the tree; lines are sorted lexically by
/// the stack string for stable output. `frame_fmt` is invoked once per
/// frame with `(addr, pid)` — `pid` is `0` for kernel-resolved frames
/// (whose runtime VA is global) and the sample's owning pid for user
/// frames (so the closure can route the lookup through
/// [`CpuProfile::symbolize`]'s pid-scoped user resolver). Pass a
/// closure that formats hex addresses, optionally subtracts
/// `KernelInfo.text_base`, and/or looks up symbols.
///
/// The synthetic root's `self_count` (idle / no-stack samples) is
/// emitted as a single line `IDLE <count>` to keep the data lossless;
/// callers that want to drop it can grep `^IDLE` out of the output.
pub fn fold_flame_tree<F>(root: &FlameNode, mut frame_fmt: F) -> String
where
    F: FnMut(u64, u32) -> String,
{
    let mut lines: Vec<(String, u64)> = Vec::new();
    if root.self_count > 0 {
        lines.push(("IDLE".to_string(), root.self_count));
    }

    fn walk(
        node: &FlameNode,
        path: &mut Vec<String>,
        frame_fmt: &mut dyn FnMut(u64, u32) -> String,
        out: &mut Vec<(String, u64)>,
    ) {
        let name = match &node.group_label {
            Some(label) => label.clone(),
            None => frame_fmt(node.addr, node.pid),
        };
        path.push(name);
        if node.self_count > 0 {
            out.push((path.join(";"), node.self_count));
        }
        for child in &node.children {
            walk(child, path, frame_fmt, out);
        }
        path.pop();
    }

    let mut path: Vec<String> = Vec::new();
    for child in &root.children {
        walk(child, &mut path, &mut frame_fmt, &mut lines);
    }

    lines.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (stack, count) in lines {
        out.push_str(&stack);
        out.push(' ');
        out.push_str(&count.to_string());
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Differential flame graphs (compare two captures)
// ---------------------------------------------------------------------------

/// Per-node raw sample counts for one frame of a [`FlameDiff`] tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffCounts {
    /// Inclusive samples (this frame plus everything below it) in the
    /// baseline capture.
    pub base: u64,
    /// Inclusive samples in the current capture.
    pub target: u64,
    /// Samples whose *leaf* frame is this node, per capture.
    pub base_self: u64,
    pub target_self: u64,
}

/// A merged, label-keyed flame tree comparing two captures.
///
/// `tree` is a regular [`FlameNode`] so the GUI renderer works on it
/// unchanged, but its fields carry diff-specific meaning:
///
/// - `addr` is a hash of the node's *identity key* (see
///   [`diff_node_identity`]) — stable across captures, never a real PC.
/// - `pid` is always 0.
/// - `group_label` holds the display label for *every* node (not just
///   process bars), so rendering never needs either `CpuProfile`.
/// - `total_count` / `self_count` are normalized union counts: each
///   capture's raw counts scaled by `DIFF_SCALE / capture_total` and
///   summed, so a busier capture doesn't dominate bar widths and frames
///   that vanished in the current capture keep their width instead of
///   disappearing.
///
/// `counts` maps a node's full root-to-node path hash (see
/// [`flame_diff_path_hash`]) to its raw per-capture counts, for delta
/// colouring and tooltips.
pub struct FlameDiff {
    pub tree: FlameNode,
    pub counts: FxHashMap<u64, DiffCounts>,
    /// Post-filter sample totals of each capture (the raw roots).
    pub base_total: u64,
    pub target_total: u64,
}

impl FlameDiff {
    /// Empty comparison, used as a placeholder while the real merged
    /// tree is built on a worker thread.
    pub fn empty() -> Self {
        Self {
            tree: FlameNode::new(0, 0),
            counts: FxHashMap::default(),
            base_total: 0,
            target_total: 0,
        }
    }
}

/// FNV-1a offset basis: the empty path's hash, used as the synthetic
/// root's entry in [`FlameDiff::counts`] and the seed for every
/// [`flame_diff_path_hash`] chain.
pub const FLAME_DIFF_PATH_SEED: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Fold one `(addr, pid)` node key into a running path hash. Both the
/// merge and the renderer walk root→leaf, so a node's full-path hash is
/// built incrementally: seed at the root, fold each descended key.
pub fn flame_diff_path_hash(prev: u64, addr: u64, pid: u32) -> u64 {
    let mut h = prev;
    for b in addr.to_le_bytes().into_iter().chain(pid.to_le_bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Fixed-point scale for normalized union counts: each capture's raw
/// counts are multiplied by `DIFF_SCALE / capture_total` before
/// summing, so both captures contribute equally to bar widths.
const DIFF_SCALE: f64 = 4_398_046_511_104.0; // 2^42

/// FNV-1a hash of an identity key, used as the merged node's `addr`.
/// Forced nonzero so no frame key collides with the synthetic root's
/// `addr == 0`.
fn diff_key_hash(key: &str) -> u64 {
    let mut h = FLAME_DIFF_PATH_SEED;
    for &b in key.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h | 1
}

/// `(identity_key, display_label)` for one node of a per-capture flame
/// tree, for [`build_flame_diff`]'s cross-capture merge. Runtime
/// addresses are useless as a cross-capture key (KASLR / ASLR / build
/// drift), so identity derives from *symbol* identity:
///
/// - group bars: `g:<label>`
/// - resolved kernel frame: `k:<linker filename>:<symbol>`
/// - resolved user frame: `u:<DSO path>:<symbol>`
/// - module-only user frame: `m:<DSO path>` (the per-side tree already
///   collapses each DSO's unresolved PCs into one node)
/// - unresolved kernel PC: `krel:<offset into kernel text>` (stable
///   across captures of the same kernel build, KASLR notwithstanding)
/// - anything else: raw hex (only matches across captures when the
///   address space layout happens to be identical)
///
/// Name-based (not build-id) keys are deliberate: comparing across a
/// kernel / binary upgrade should still match `tcp_input` to
/// `tcp_input`. The display label is the short form the renderer
/// paints on the bar.
/// Display label for one frame address: symbol name, DSO basename,
/// `kernel+0xOFFSET`, or raw hex. Same string the differential flame
/// paints, so a [`StackSelector::Name`] clicked on a diff bar matches
/// the frame on both captures.
fn frame_display_name(profile: &CpuProfile, addr: u64, pid: u32) -> String {
    let pid_opt = (pid != 0).then_some(pid);
    match profile.symbolize(addr, pid_opt) {
        Some(SymbolHit::Kernel { symbol, .. }) => symbol.name.clone(),
        Some(SymbolHit::User { symbol, .. }) => symbol.name.clone(),
        Some(SymbolHit::UserModuleOnly { file, .. }) => file.short_name().to_string(),
        None => {
            if flame_pc_is_kernel(addr) {
                if let Some(k) = &profile.kernel {
                    if addr >= k.text_base && addr < k.text_base.saturating_add(k.text_size) {
                        return format!("kernel+{:#x}", addr - k.text_base);
                    }
                }
            }
            format!("{addr:#x}")
        }
    }
}

fn diff_node_identity(profile: &CpuProfile, node: &FlameNode) -> (String, String) {
    if let Some(label) = &node.group_label {
        return (format!("g:{label}"), label.clone());
    }
    let addr = node.addr;
    let pid_opt = (node.pid != 0).then_some(node.pid);
    let display = frame_display_name(profile, addr, node.pid);
    match profile.symbolize(addr, pid_opt) {
        Some(SymbolHit::Kernel { file, symbol, .. }) => {
            (format!("k:{}:{}", file.filename, symbol.name), display)
        }
        Some(SymbolHit::User { file, symbol, .. }) => {
            (format!("u:{}:{}", file.pathname, symbol.name), display)
        }
        Some(SymbolHit::UserModuleOnly { file, .. }) => (format!("m:{}", file.pathname), display),
        None => {
            if flame_pc_is_kernel(addr) {
                if let Some(k) = &profile.kernel {
                    if addr >= k.text_base && addr < k.text_base.saturating_add(k.text_size) {
                        let rel = addr - k.text_base;
                        return (format!("krel:{rel:#x}"), display);
                    }
                }
            }
            (format!("raw:{display}"), display)
        }
    }
}

/// `filter` for one side of a diff build, with that side's own
/// absolute time window. Address-space-scoped `Frame` selectors are
/// dropped — raw addresses can't match across captures (an include
/// would empty the other side); `Group` selectors (process / thread
/// names) are honoured on both sides.
fn diff_side_filter(filter: &FlameFilter, window: (u64, u64)) -> FlameFilter {
    let mut f = filter.clone();
    f.ts_start_us = window.0;
    f.ts_end_us = window.1;
    f.include_stacks
        .retain(|s| matches!(s, StackSelector::Group(_) | StackSelector::Name(_)));
    f.exclude_stacks
        .retain(|s| matches!(s, StackSelector::Group(_) | StackSelector::Name(_)));
    f
}

/// Build a differential flame tree comparing `base` (the reference
/// capture) against `target` (the current capture). `base_window` and
/// `target_window` are absolute µs on each capture's own clock
/// (`0`..=`u64::MAX` is the full capture). Other `filter` fields
/// (cpu / pid / comm / group selectors) apply to both sides. Both
/// sides are built by-symbol without offset overlays (the merge is
/// label-keyed, so per-PC detail can't match across captures) and
/// with user frames merged across pids.
#[allow(clippy::too_many_arguments)] // kernel_group joins the existing layout flags
pub fn build_flame_diff(
    base: &CpuProfile,
    target: &CpuProfile,
    filter: &FlameFilter,
    base_window: (u64, u64),
    target_window: (u64, u64),
    group_by_process: bool,
    kernel_user_base: bool,
    kernel_group: bool,
) -> FlameDiff {
    let base_filter = diff_side_filter(filter, base_window);
    let target_filter = diff_side_filter(filter, target_window);
    let base_tree = build_flame_tree_with_base(
        base,
        &base_filter,
        FlameAggregation::DEFAULT_CLI,
        false,
        group_by_process,
        kernel_user_base,
        kernel_group,
    );
    let target_tree = build_flame_tree_with_base(
        target,
        &target_filter,
        FlameAggregation::DEFAULT_CLI,
        false,
        group_by_process,
        kernel_user_base,
        kernel_group,
    );

    let scale = |total: u64| {
        if total > 0 {
            DIFF_SCALE / total as f64
        } else {
            0.0
        }
    };
    let mut merge = DiffMerge {
        base_profile: base,
        target_profile: target,
        base_scale: scale(base_tree.total_count),
        target_scale: scale(target_tree.total_count),
        counts: FxHashMap::default(),
    };
    let mut tree = FlameNode {
        addr: 0,
        pid: 0,
        total_count: merge.scaled(base_tree.total_count, target_tree.total_count),
        self_count: merge.scaled(base_tree.self_count, target_tree.self_count),
        children: Vec::new(),
        is_self_pc: false,
        group_label: None,
    };
    merge.counts.insert(
        FLAME_DIFF_PATH_SEED,
        DiffCounts {
            base: base_tree.total_count,
            target: target_tree.total_count,
            base_self: base_tree.self_count,
            target_self: target_tree.self_count,
        },
    );
    tree.children =
        merge.merge_children(Some(&base_tree), Some(&target_tree), FLAME_DIFF_PATH_SEED);
    FlameDiff {
        tree,
        counts: merge.counts,
        base_total: base_tree.total_count,
        target_total: target_tree.total_count,
    }
}

struct DiffMerge<'a> {
    base_profile: &'a CpuProfile,
    target_profile: &'a CpuProfile,
    base_scale: f64,
    target_scale: f64,
    counts: FxHashMap<u64, DiffCounts>,
}

impl<'a> DiffMerge<'a> {
    fn scaled(&self, base_raw: u64, target_raw: u64) -> u64 {
        (base_raw as f64 * self.base_scale + target_raw as f64 * self.target_scale) as u64
    }

    /// Merge the child lists of a matched pair of nodes (either side
    /// may be absent), pairing children across sides by identity key.
    /// `path_hash` is the parent node's full-path hash.
    fn merge_children(
        &mut self,
        base_node: Option<&'a FlameNode>,
        target_node: Option<&'a FlameNode>,
        path_hash: u64,
    ) -> Vec<FlameNode> {
        let mut slot_of: HashMap<String, usize> = HashMap::new();
        let mut pairs: Vec<(Option<&FlameNode>, Option<&FlameNode>)> = Vec::new();
        if let Some(b) = base_node {
            for child in &b.children {
                let (key, _) = diff_node_identity(self.base_profile, child);
                match slot_of.get(&key) {
                    // Same-side key duplicates shouldn't occur (per-side
                    // trees key siblings uniquely); fold defensively.
                    Some(&i) => pairs[i].0 = Some(child),
                    None => {
                        slot_of.insert(key, pairs.len());
                        pairs.push((Some(child), None));
                    }
                }
            }
        }
        if let Some(t) = target_node {
            for child in &t.children {
                let (key, _) = diff_node_identity(self.target_profile, child);
                match slot_of.get(&key) {
                    Some(&i) => pairs[i].1 = Some(child),
                    None => {
                        slot_of.insert(key, pairs.len());
                        pairs.push((None, Some(child)));
                    }
                }
            }
        }
        pairs
            .into_iter()
            .map(|(b, t)| self.merge_node(b, t, path_hash))
            .collect()
    }

    /// Merge one matched pair of nodes (either side may be absent) and
    /// their subtrees. `parent_path_hash` is the parent's full-path
    /// hash; this node folds its own key in.
    fn merge_node(
        &mut self,
        base_node: Option<&'a FlameNode>,
        target_node: Option<&'a FlameNode>,
        parent_path_hash: u64,
    ) -> FlameNode {
        // A matched pair has the same key by construction; take the
        // key from either side and the display label from the current
        // capture when both exist.
        let (key, display) = match (base_node, target_node) {
            (Some(b), Some(t)) => {
                let (key, _) = diff_node_identity(self.base_profile, b);
                let (_, display) = diff_node_identity(self.target_profile, t);
                (key, display)
            }
            (Some(b), None) => diff_node_identity(self.base_profile, b),
            (None, Some(t)) => diff_node_identity(self.target_profile, t),
            (None, None) => unreachable!("merge_node called with two absent sides"),
        };
        let addr = diff_key_hash(&key);
        let path_hash = flame_diff_path_hash(parent_path_hash, addr, 0);

        let base_total = base_node.map_or(0, |n| n.total_count);
        let target_total = target_node.map_or(0, |n| n.total_count);
        let base_self = base_node.map_or(0, |n| n.self_count);
        let target_self = target_node.map_or(0, |n| n.self_count);
        self.counts.insert(
            path_hash,
            DiffCounts {
                base: base_total,
                target: target_total,
                base_self,
                target_self,
            },
        );
        let children = self.merge_children(base_node, target_node, path_hash);
        FlameNode {
            addr,
            pid: 0,
            total_count: self.scaled(base_total, target_total),
            self_count: self.scaled(base_self, target_self),
            children,
            is_self_pc: false,
            group_label: Some(display),
        }
    }
}

/// Pids that belong to a user process: they have a `PROC_MAP`, a
/// `USER_MODE` sample, or at least one user-space frame. A non-kernel
/// PC below [`USER_PC_FLOOR`], or one with kernel frames on both
/// sides, is a stack-walk glitch and does not count. Everything else
/// (idle / ithread / swi / pid 0) folds into the `kernel` bar.
pub(crate) fn collect_user_pids(profile: &CpuProfile) -> FxHashSet<u32> {
    let mut user_pids = FxHashSet::default();
    for &pid in profile.proc_maps.keys() {
        if pid != 0 {
            user_pids.insert(pid);
        }
    }
    for s in &profile.samples {
        let pid = profile.sample_owner_pid(s.tid, s.pid, s.ts, &s.frames);
        if pid == 0 || user_pids.contains(&pid) {
            continue;
        }
        if (s.flags & CPU_SAMPLE_FLAG_USER_MODE) != 0 || sample_has_user_frame(&s.frames) {
            user_pids.insert(pid);
        }
    }
    user_pids
}

/// A non-kernel PC at or above [`USER_PC_FLOOR`] counts when kernel
/// frames sit on at most one side of it.
fn sample_has_user_frame(frames: &[u64]) -> bool {
    frames.iter().enumerate().any(|(i, &pc)| {
        if pc < USER_PC_FLOOR || flame_pc_is_kernel(pc) {
            return false;
        }
        let kernel_before = frames[..i].iter().any(|&p| flame_pc_is_kernel(p));
        let kernel_after = frames[i + 1..].iter().any(|&p| flame_pc_is_kernel(p));
        !(kernel_before && kernel_after)
    })
}

pub(crate) fn collect_comm_by_pid(profile: &CpuProfile) -> FxHashMap<u32, String> {
    let mut comm_by_pid = FxHashMap::default();
    for ti in profile.threads.values() {
        if ti.pid == 0 || comm_by_pid.contains_key(&ti.pid) {
            continue;
        }
        if let Some(c) = profile.process_comm_latest(ti.pid) {
            comm_by_pid.insert(ti.pid, c.to_string());
        }
    }
    for &pid in profile.proc_execs.keys().chain(profile.proc_titles.keys()) {
        if pid == 0 || comm_by_pid.contains_key(&pid) {
            continue;
        }
        if let Some(c) = profile.process_comm_latest(pid) {
            comm_by_pid.insert(pid, c.to_string());
        }
    }
    comm_by_pid
}

fn process_group_label(
    s: &CpuStackSample,
    user_pids: &FxHashSet<u32>,
    profile: &CpuProfile,
) -> String {
    let pid = profile.sample_owner_pid(s.tid, s.pid, s.ts, &s.frames);
    if pid == 0 || !user_pids.contains(&pid) {
        return KERNEL_GROUP_LABEL.to_string();
    }
    if let Some(c) = profile.process_comm_at(pid, Some(s.tid), s.ts) {
        return c.to_string();
    }
    format!("pid {}", pid)
}

/// Thread-name bar under the `kernel` group (`idle`, `intr`, `clock`).
fn kernel_thread_child<'a>(
    group: &'a mut FlameNode,
    sample: &CpuStackSample,
    threads: &FxHashMap<u32, ThreadInfo>,
) -> &'a mut FlameNode {
    let tlabel = thread_name_for_sample(sample, threads);
    if tlabel == KERNEL_GROUP_LABEL {
        return group;
    }
    let tpid = flame_thread_group_pid(&tlabel);
    let thread = group.group_child_mut(&tlabel, tpid);
    thread.total_count += 1;
    thread
}

fn thread_name_for_sample(s: &CpuStackSample, threads: &FxHashMap<u32, ThreadInfo>) -> String {
    if let Some(ti) = threads.get(&s.tid) {
        if !ti.comm.is_empty() {
            return ti.comm.clone();
        }
    }
    if s.pid != 0 {
        format!("pid {}", s.pid)
    } else {
        KERNEL_GROUP_LABEL.to_string()
    }
}

/// Synthetic pid for a thread-`comm` bar nested under `kernel`.
/// Kernel threads share pid 0, so zoom-path `(addr, pid)` keys use
/// this to keep `idle` / `intr` / `clock` distinct as siblings.
pub fn flame_thread_group_pid(label: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in label.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    0x4000_0000 | (h & 0x3fff_ffff)
}

/// Group-bar label [`build_flame_tree`] assigns `pid` when
/// `group_by_process` is set: process `comm`, [`KERNEL_GROUP_LABEL`],
/// or `"pid N"`. Same-`comm` pids share one label.
pub fn flame_process_group_label(profile: &CpuProfile, pid: u32) -> String {
    let user_pids = collect_user_pids(profile);
    process_group_label_for_pid(pid, None, &user_pids, profile)
}

/// Label for a group bar carried in a zoom path. Process bars use
/// [`flame_process_group_label`]. The thread bar under `kernel`
/// (`idle`, `intr`, `clock`) is keyed by [`flame_thread_group_pid`].
pub fn flame_group_bar_label(profile: &CpuProfile, pid: u32) -> String {
    if pid == super::USER_PSEUDO_PID {
        return USER_GROUP_LABEL.to_string();
    }
    if pid & 0xC000_0000 == 0x4000_0000 {
        let mut seen = FxHashSet::default();
        for ti in profile.threads.values() {
            let label = if !ti.comm.is_empty() {
                ti.comm.clone()
            } else if ti.pid != 0 {
                format!("pid {}", ti.pid)
            } else {
                continue;
            };
            if !seen.insert(label.clone()) {
                continue;
            }
            if flame_thread_group_pid(&label) == pid {
                return label;
            }
        }
        if flame_thread_group_pid(KERNEL_GROUP_LABEL) == pid {
            return KERNEL_GROUP_LABEL.to_string();
        }
    }
    flame_process_group_label(profile, pid)
}

/// Thread `comm` recorded for `pid`, if any. Used to zoom Open-flame
/// from a per-pid row into the nested kernel-thread bar.
pub fn flame_thread_comm_for_pid(profile: &CpuProfile, pid: u32) -> Option<String> {
    collect_comm_by_pid(profile)
        .get(&pid)
        .filter(|c| !c.is_empty())
        .cloned()
}

pub(crate) fn process_group_label_for_pid(
    pid: u32,
    tid: Option<u32>,
    user_pids: &FxHashSet<u32>,
    profile: &CpuProfile,
) -> String {
    if pid == 0 || !user_pids.contains(&pid) {
        return KERNEL_GROUP_LABEL.to_string();
    }
    if let Some(c) = profile.process_comm_latest(pid) {
        return c.to_string();
    }
    if let Some(tid) = tid {
        if let Some(ti) = profile.threads.get(&tid) {
            if !ti.comm.is_empty() {
                return ti.comm.clone();
            }
        }
    }
    format!("pid {pid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contiguous_symbol_path_requires_an_unbroken_run() {
        let keys = [(1, 0), (2, 0), (3, 0)];
        assert_eq!(contiguous_path(&keys, &[(2, 0), (3, 0)]), Some(1));
        assert_eq!(contiguous_path(&keys, &[(1, 0), (3, 0)]), None);
        assert_eq!(contiguous_path(&keys, &[]), None);
    }

    fn diff_full(
        base: &CpuProfile,
        target: &CpuProfile,
        filter: &FlameFilter,
        group_by_process: bool,
    ) -> FlameDiff {
        build_flame_diff(
            base,
            target,
            filter,
            (0, u64::MAX),
            (0, u64::MAX),
            group_by_process,
            false,
            true,
        )
    }

    #[test]
    fn kernel_pc_classification_matches_the_tree_builder_boundary() {
        assert!(!flame_pc_is_kernel(0));
        assert!(!flame_pc_is_kernel(USER_KERNEL_BOUNDARY - 1));
        assert!(flame_pc_is_kernel(USER_KERNEL_BOUNDARY));
        assert!(flame_pc_is_kernel(0xffff_ffff_8000_0000));
    }

    fn sample(pid: u32, tid: u32, flags: u8, frames: Vec<u64>) -> CpuStackSample {
        sample_at(0, pid, tid, flags, frames)
    }

    fn sample_at(ts: u64, pid: u32, tid: u32, flags: u8, frames: Vec<u64>) -> CpuStackSample {
        CpuStackSample {
            ts,
            cpu: 0,
            tid,
            pid,
            sample_type: 0,
            flags,
            freq_mhz: None,
            frames,
        }
    }

    fn thread(tid: u32, pid: u32, comm: &str) -> (u32, ThreadInfo) {
        (
            tid,
            ThreadInfo {
                tid,
                pid,
                comm: comm.to_string(),
                tdname: String::new(),
            },
        )
    }

    fn profile(samples: Vec<CpuStackSample>, threads: Vec<(u32, ThreadInfo)>) -> CpuProfile {
        CpuProfile {
            samples,
            threads: threads.into_iter().collect(),
            ..Default::default()
        }
    }

    #[test]
    fn include_frame_keeps_only_stacks_through_it() {
        // frames are leaf-first on the wire: [leaf, root].
        let root_pc = 0xffff_8000_0000_1000u64;
        let a_pc = 0xffff_8000_0000_2000u64;
        let b_pc = 0xffff_8000_0000_3000u64;
        let p = profile(
            vec![
                sample(0, 1, 0, vec![a_pc, root_pc]),
                sample(0, 1, 0, vec![a_pc, root_pc]),
                sample(0, 1, 0, vec![b_pc, root_pc]),
            ],
            vec![thread(1, 0, "intr")],
        );
        let mut f = FlameFilter::new();
        f.include_stacks = vec![StackSelector::Frame(a_pc, 0)];
        let tree = build_flame_tree(&p, &f, FlameAggregation::ByPc, false, false);
        assert_eq!(tree.total_count, 2);
        // The surviving tree has the a_pc branch only.
        let root_node = &tree.children[0];
        assert_eq!(root_node.total_count, 2);
        assert_eq!(root_node.children.len(), 1);
        assert_eq!(root_node.children[0].addr, a_pc);
    }

    #[test]
    fn exclude_frame_drops_stacks_through_it() {
        let root_pc = 0xffff_8000_0000_1000u64;
        let a_pc = 0xffff_8000_0000_2000u64;
        let b_pc = 0xffff_8000_0000_3000u64;
        let p = profile(
            vec![
                sample(0, 1, 0, vec![a_pc, root_pc]),
                sample(0, 1, 0, vec![a_pc, root_pc]),
                sample(0, 1, 0, vec![b_pc, root_pc]),
            ],
            vec![thread(1, 0, "intr")],
        );
        let mut f = FlameFilter::new();
        f.exclude_stacks = vec![StackSelector::Frame(a_pc, 0)];
        let tree = build_flame_tree(&p, &f, FlameAggregation::ByPc, false, false);
        assert_eq!(tree.total_count, 1);
        assert_eq!(tree.children[0].children[0].addr, b_pc);
    }

    #[test]
    fn exclude_wins_over_include() {
        let root_pc = 0xffff_8000_0000_1000u64;
        let a_pc = 0xffff_8000_0000_2000u64;
        let b_pc = 0xffff_8000_0000_3000u64;
        let p = profile(
            vec![
                sample(0, 1, 0, vec![a_pc, root_pc]),
                sample(0, 1, 0, vec![b_pc, root_pc]),
            ],
            vec![thread(1, 0, "intr")],
        );
        let mut f = FlameFilter::new();
        f.include_stacks = vec![StackSelector::Frame(root_pc, 0)];
        f.exclude_stacks = vec![StackSelector::Frame(a_pc, 0)];
        let tree = build_flame_tree(&p, &f, FlameAggregation::ByPc, false, false);
        assert_eq!(tree.total_count, 1);
        assert_eq!(tree.children[0].children[0].addr, b_pc);
    }

    #[test]
    fn group_selector_matches_process_and_thread_labels() {
        let user_pc = 0x4000_0000u64;
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let p = profile(
            vec![
                sample(9104, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample(0, 2, 0, vec![kernel_pc]),
                sample(0, 2, 0, vec![kernel_pc]),
            ],
            vec![thread(1, 9104, "ella_server"), thread(2, 0, "idle")],
        );
        // Process group bar: only the user sample survives.
        let mut f = FlameFilter::new();
        f.include_stacks = vec![StackSelector::Group("ella_server".to_string())];
        let tree = build_flame_tree(&p, &f, FlameAggregation::ByPc, false, true);
        assert_eq!(tree.total_count, 1);
        // Kernel-thread group bar: only the kernel samples survive.
        let mut f = FlameFilter::new();
        f.include_stacks = vec![StackSelector::Group("idle".to_string())];
        let tree = build_flame_tree(&p, &f, FlameAggregation::ByPc, false, true);
        assert_eq!(tree.total_count, 2);
        // Excluding the top-level "kernel" bar drops both kernel samples.
        let mut f = FlameFilter::new();
        f.exclude_stacks = vec![StackSelector::Group(KERNEL_GROUP_LABEL.to_string())];
        let tree = build_flame_tree(&p, &f, FlameAggregation::ByPc, false, true);
        assert_eq!(tree.total_count, 1);
    }

    #[test]
    fn group_by_process_splits_user_comm_from_kernel() {
        // User-mode sample in ella_server + a kernel-thread sample
        // with only kernel PCs. Grouping should produce two root
        // children: "ella_server" and "kernel".
        let user_pc = 0x4000_0000u64;
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let p = profile(
            vec![
                sample(9104, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample(0, 2, 0, vec![kernel_pc]),
                sample(0, 2, 0, vec![kernel_pc]),
            ],
            vec![thread(1, 9104, "ella_server"), thread(2, 0, "idle")],
        );
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        assert_eq!(tree.total_count, 3);
        assert_eq!(tree.children.len(), 2);
        let labels: Vec<&str> = tree
            .children
            .iter()
            .filter_map(|c| c.group_label.as_deref())
            .collect();
        assert!(labels.contains(&"ella_server"));
        assert!(labels.contains(&"kernel"));
        let ella = tree
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some("ella_server"))
            .unwrap();
        assert_eq!(ella.total_count, 1);
        let kern = tree
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some("kernel"))
            .unwrap();
        assert_eq!(kern.total_count, 2);
        assert_eq!(kern.children.len(), 1);
        assert_eq!(kern.children[0].group_label.as_deref(), Some("idle"));
        assert_eq!(kern.children[0].total_count, 2);
    }

    #[test]
    fn kernel_thread_bar_uses_comm_not_tdname() {
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let mut cpu3 = thread(3, 0, "idle");
        cpu3.1.tdname = "idle: cpu3".to_string();
        let mut cpu4 = thread(4, 0, "idle");
        cpu4.1.tdname = "idle: cpu4".to_string();
        let p = profile(
            vec![
                sample(0, 3, 0, vec![kernel_pc]),
                sample(0, 4, 0, vec![kernel_pc]),
            ],
            vec![cpu3, cpu4],
        );
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        let kern = tree
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some("kernel"))
            .unwrap();
        assert_eq!(kern.children.len(), 1);
        assert_eq!(kern.children[0].group_label.as_deref(), Some("idle"));
        assert_eq!(kern.children[0].total_count, 2);

        let named_kernel = thread(5, 0, "kernel");
        let p = profile(vec![sample(0, 5, 0, vec![kernel_pc])], vec![named_kernel]);
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        let kern = &tree.children[0];
        assert_eq!(kern.group_label.as_deref(), Some("kernel"));
        assert!(kern.children[0].group_label.is_none());
        assert_eq!(kern.children[0].addr, kernel_pc);
    }

    #[test]
    fn no_kernel_group_hangs_kernel_stacks_on_the_root() {
        let user_pc = 0x4000_0000u64;
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let p = profile(
            vec![
                sample(9104, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample(0, 2, 0, vec![kernel_pc]),
            ],
            vec![thread(1, 9104, "ella_server"), thread(2, 0, "idle")],
        );
        let tree = build_flame_tree_with_base(
            &p,
            &FlameFilter::new(),
            FlameAggregation::ByPc,
            false,
            true,
            false,
            false,
        );
        let labels: Vec<&str> = tree
            .children
            .iter()
            .filter_map(|c| c.group_label.as_deref())
            .collect();
        assert_eq!(labels, vec!["ella_server"]);
        let kernel = tree.children.iter().find(|c| c.addr == kernel_pc).unwrap();
        assert!(kernel.group_label.is_none());
        assert_eq!(kernel.total_count, 1);
    }

    #[test]
    fn kernel_user_base_puts_processes_under_user() {
        let user_pc = 0x4000_0000u64;
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let p = profile(
            vec![
                sample(9104, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample(0, 2, 0, vec![kernel_pc]),
                sample(0, 2, 0, vec![kernel_pc]),
            ],
            vec![thread(1, 9104, "ella_server"), thread(2, 0, "idle")],
        );
        let tree = build_flame_tree_with_base(
            &p,
            &FlameFilter::new(),
            FlameAggregation::ByPc,
            false,
            true,
            true,
            true,
        );
        assert_eq!(tree.total_count, 3);
        assert_eq!(tree.children.len(), 2);
        let labels: Vec<&str> = tree
            .children
            .iter()
            .filter_map(|c| c.group_label.as_deref())
            .collect();
        assert!(labels.contains(&USER_GROUP_LABEL));
        assert!(labels.contains(&KERNEL_GROUP_LABEL));
        let user = tree
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some(USER_GROUP_LABEL))
            .unwrap();
        assert_eq!(user.pid, crate::analysis::USER_PSEUDO_PID);
        assert_eq!(user.total_count, 1);
        assert_eq!(user.children.len(), 1);
        assert_eq!(user.children[0].group_label.as_deref(), Some("ella_server"));
        let kern = tree
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some(KERNEL_GROUP_LABEL))
            .unwrap();
        assert_eq!(kern.total_count, 2);
        assert_eq!(kern.children[0].group_label.as_deref(), Some("idle"));
        assert_eq!(kern.children[0].total_count, 2);
        assert_eq!(
            flame_group_bar_label(&p, crate::analysis::USER_PSEUDO_PID),
            USER_GROUP_LABEL
        );

        let mut only_user = FlameFilter::new();
        only_user.include_stacks = vec![StackSelector::Group(USER_GROUP_LABEL.to_string())];
        let filtered = build_flame_tree_with_base(
            &p,
            &only_user,
            FlameAggregation::ByPc,
            false,
            true,
            true,
            true,
        );
        assert_eq!(filtered.total_count, 1);
        assert_eq!(
            filtered.children[0].group_label.as_deref(),
            Some(USER_GROUP_LABEL)
        );

        let direct = build_flame_tree_with_base(
            &p,
            &FlameFilter::new(),
            FlameAggregation::ByPc,
            false,
            false,
            true,
            true,
        );
        let user_direct = direct
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some(USER_GROUP_LABEL))
            .unwrap();
        assert!(user_direct.children.iter().all(|c| c.group_label.is_none()));
        assert_eq!(user_direct.children[0].addr, user_pc);
    }

    #[test]
    fn group_by_process_merges_same_comm_across_pids() {
        let user_pc = 0x4000_0000u64;
        let p = profile(
            vec![
                sample(100, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample(200, 2, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
            ],
            vec![thread(1, 100, "nginx"), thread(2, 200, "nginx")],
        );
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].group_label.as_deref(), Some("nginx"));
        assert_eq!(tree.children[0].total_count, 2);
    }

    #[test]
    fn group_by_process_renames_after_proc_exec() {
        // Shell forks a child (THREAD_CREATE still says "sh"), samples
        // during parse keep the fork-time name, then PROC_EXEC renames
        // the image to "perl" for later samples.
        let user_pc = 0x4000_0000u64;
        let mut p = profile(
            vec![
                sample_at(50, 100, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample_at(150, 100, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample_at(150, 100, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
            ],
            vec![thread(1, 100, "sh")],
        );
        p.apply_proc_exec(100, 100, "perl".to_string());
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        assert_eq!(tree.total_count, 3);
        let labels: Vec<&str> = tree
            .children
            .iter()
            .filter_map(|c| c.group_label.as_deref())
            .collect();
        assert!(labels.contains(&"sh"));
        assert!(labels.contains(&"perl"));
        let sh = tree
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some("sh"))
            .unwrap();
        let perl = tree
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some("perl"))
            .unwrap();
        assert_eq!(sh.total_count, 1);
        assert_eq!(perl.total_count, 2);
        assert_eq!(flame_process_group_label(&p, 100), "perl");
    }

    #[test]
    fn group_by_process_uses_proc_title() {
        let user_pc = 0x4000_0000u64;
        let mut p = profile(
            vec![
                sample_at(50, 100, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample_at(150, 100, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample_at(250, 100, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
            ],
            vec![thread(1, 100, "sh")],
        );
        p.apply_proc_exec(10, 100, "perl".to_string());
        p.apply_proc_title(100, 100, "pop_data".to_string());
        p.apply_proc_exec(200, 100, "elog".to_string());
        p.proc_execs
            .get_mut(&100)
            .unwrap()
            .sort_unstable_by_key(|e| e.ts);
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        let count = |name: &str| {
            tree.children
                .iter()
                .find(|c| c.group_label.as_deref() == Some(name))
                .map(|c| c.total_count)
                .unwrap_or(0)
        };
        assert_eq!(count("perl"), 1);
        assert_eq!(count("pop_data"), 1);
        assert_eq!(count("elog"), 1);
    }

    #[test]
    fn fork_sample_stamped_with_child_pid_stays_on_the_parent() {
        // elog_capture.pl's thread is sampled inside fork/exec with the
        // child's pid, and the child's maps still contain the parent's
        // perl hardlink. That stack belongs on the parent. A sample
        // whose PC is in the child's own ld-elf mapping stays on elog.
        let perl_pc = 0x4000_0000u64;
        let ld_pc = 0x2000_0000u64;
        let libperl_pc = 0x3000_0000u64;
        let mut p = profile(
            vec![
                sample_at(50, 2042, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![perl_pc]),
                sample_at(50, 2042, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![ld_pc]),
                sample_at(60, 2042, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![libperl_pc]),
                sample_at(70, 2042, 2, CPU_SAMPLE_FLAG_USER_MODE, vec![perl_pc]),
            ],
            vec![thread(1, 11637, "elog_capture"), thread(2, 2042, "elog")],
        );
        p.user_files.insert(
            1,
            UserFile {
                user_file_id: 1,
                build_id: String::new(),
                pathname: "/usr/local/ccsc/libexec/perl/rebalance_content2".into(),
            },
        );
        p.user_files.insert(
            2,
            UserFile {
                user_file_id: 2,
                build_id: String::new(),
                pathname: "/libexec/ld-elf.so.1".into(),
            },
        );
        p.user_files.insert(
            4,
            UserFile {
                user_file_id: 4,
                build_id: String::new(),
                pathname: "/usr/bin/elog".into(),
            },
        );
        p.user_files.insert(
            3,
            UserFile {
                user_file_id: 3,
                build_id: String::new(),
                pathname: "/usr/local/lib/perl5/5.42/mach/CORE/libperl.so.5.42.3".into(),
            },
        );
        p.proc_maps.insert(
            2042,
            vec![
                ProcMap {
                    pid: 2042,
                    user_file_id: 1,
                    base: perl_pc,
                    size: 0x1000,
                    offset: 0,
                },
                ProcMap {
                    pid: 2042,
                    user_file_id: 2,
                    base: ld_pc,
                    size: 0x1000,
                    offset: 0,
                },
                ProcMap {
                    pid: 2042,
                    user_file_id: 4,
                    base: 0x1000_0000,
                    size: 0x1000,
                    offset: 0,
                },
                ProcMap {
                    pid: 2042,
                    user_file_id: 3,
                    base: libperl_pc,
                    size: 0x1000,
                    offset: 0,
                },
            ],
        );
        p.apply_proc_exec(10, 2042, "elog".to_string());
        p.apply_proc_exec(10, 11637, "elog_capture".to_string());
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        assert_eq!(tree.total_count, 4);
        let count = |name: &str| {
            tree.children
                .iter()
                .find(|c| c.group_label.as_deref() == Some(name))
                .map(|c| c.total_count)
                .unwrap_or(0)
        };
        assert_eq!(count("elog_capture"), 2);
        assert_eq!(count("elog"), 2);
    }

    #[test]
    fn comm_filter_matches_sample_time_proc_exec_name() {
        let user_pc = 0x4000_0000u64;
        let mut p = profile(
            vec![
                sample_at(50, 100, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample_at(150, 100, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
            ],
            vec![thread(1, 100, "sh")],
        );
        p.apply_proc_exec(100, 100, "perl".to_string());
        let mut f = FlameFilter::new();
        f.comms = vec!["perl".to_string()];
        let tree = build_flame_tree(&p, &f, FlameAggregation::ByPc, false, true);
        assert_eq!(tree.total_count, 1);
        assert_eq!(tree.children[0].group_label.as_deref(), Some("perl"));
    }

    #[test]
    fn group_by_process_nests_kernel_thread_comms() {
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let p = profile(
            vec![
                sample(0, 2, 0, vec![kernel_pc]),
                sample(0, 3, 0, vec![kernel_pc]),
                sample(0, 3, 0, vec![kernel_pc]),
            ],
            vec![thread(2, 0, "idle"), thread(3, 0, "intr")],
        );
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        let kern = tree
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some("kernel"))
            .unwrap();
        assert_eq!(kern.total_count, 3);
        let labels: Vec<&str> = kern
            .children
            .iter()
            .filter_map(|c| c.group_label.as_deref())
            .collect();
        assert!(labels.contains(&"idle"));
        assert!(labels.contains(&"intr"));
        let idle = kern
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some("idle"))
            .unwrap();
        let intr = kern
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some("intr"))
            .unwrap();
        assert_eq!(idle.total_count, 1);
        assert_eq!(intr.total_count, 2);
    }

    #[test]
    fn sandwiched_junk_pc_keeps_intr_under_kernel() {
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let p = profile(
            vec![
                sample(12, 3, 0, vec![kernel_pc, 0x246, kernel_pc]),
                sample(12, 3, 0, vec![kernel_pc]),
            ],
            vec![thread(3, 12, "intr")],
        );
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        let labels: Vec<&str> = tree
            .children
            .iter()
            .filter_map(|c| c.group_label.as_deref())
            .collect();
        assert_eq!(labels, vec!["kernel"]);
        let kern = &tree.children[0];
        assert_eq!(kern.total_count, 2);
        assert_eq!(kern.children[0].group_label.as_deref(), Some("intr"));
        assert_eq!(kern.children[0].total_count, 2);
    }

    #[test]
    fn rootward_junk_pc_keeps_intr_under_kernel() {
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let p = profile(
            vec![
                sample(12, 3, 0, vec![kernel_pc, 0x28]),
                sample(12, 3, 0, vec![kernel_pc]),
            ],
            vec![thread(3, 12, "intr")],
        );
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        assert_eq!(tree.children.len(), 1);
        assert_eq!(
            tree.children[0].group_label.as_deref(),
            Some(KERNEL_GROUP_LABEL)
        );
        assert_eq!(tree.children[0].total_count, 2);
        assert_eq!(
            tree.children[0].children[0].group_label.as_deref(),
            Some("intr")
        );
    }

    #[test]
    fn user_frame_beside_kernel_frames_is_still_a_user_process() {
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let user_pc = 0x4000_0000u64;
        let p = profile(
            vec![sample(50, 1, 0, vec![kernel_pc, user_pc])],
            vec![thread(1, 50, "nginx")],
        );
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        assert_eq!(tree.children[0].group_label.as_deref(), Some("nginx"));
    }

    #[test]
    fn group_by_process_off_has_no_group_bars() {
        let user_pc = 0x4000_0000u64;
        let p = profile(
            vec![sample(9104, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc])],
            vec![thread(1, 9104, "ella_server")],
        );
        let tree = build_flame_tree(
            &p,
            &FlameFilter::new(),
            FlameAggregation::ByPc,
            false,
            false,
        );
        assert!(tree.children.iter().all(|c| c.group_label.is_none()));
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].addr, user_pc);
    }

    #[test]
    fn flame_process_group_label_matches_tree_bars() {
        let user_pc = 0x4000_0000u64;
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let p = profile(
            vec![
                sample(9104, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample(0, 2, 0, vec![kernel_pc]),
            ],
            vec![thread(1, 9104, "ella_server"), thread(2, 0, "idle")],
        );
        assert_eq!(flame_process_group_label(&p, 9104), "ella_server");
        assert_eq!(flame_process_group_label(&p, 0), KERNEL_GROUP_LABEL);
        assert_eq!(flame_group_bar_label(&p, 9104), "ella_server");
        assert_eq!(flame_group_bar_label(&p, 0), KERNEL_GROUP_LABEL);
        assert_eq!(
            flame_group_bar_label(&p, flame_thread_group_pid("idle")),
            "idle"
        );
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        let ella = tree
            .children
            .iter()
            .find(|c| c.group_label.as_deref() == Some("ella_server"))
            .unwrap();
        assert_eq!(ella.pid, 9104);
    }

    #[test]
    fn fold_emits_group_label_as_first_frame() {
        let user_pc = 0x4000_0000u64;
        let p = profile(
            vec![sample(9104, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc])],
            vec![thread(1, 9104, "ella_server")],
        );
        let tree = build_flame_tree(&p, &FlameFilter::new(), FlameAggregation::ByPc, false, true);
        let folded = fold_flame_tree(&tree, |addr, _| format!("{addr:#x}"));
        assert_eq!(folded, "ella_server;0x40000000 1\n");
    }

    /// Walk the merged diff tree by display labels, returning the final
    /// node and its raw per-capture counts from the side map.
    fn diff_lookup<'a>(
        diff: &'a FlameDiff,
        labels: &[&str],
    ) -> (Option<&'a FlameNode>, DiffCounts) {
        let mut node = &diff.tree;
        let mut hash = FLAME_DIFF_PATH_SEED;
        let mut counts = diff.counts.get(&hash).copied().unwrap_or_default();
        for label in labels {
            let Some(child) = node
                .children
                .iter()
                .find(|c| c.group_label.as_deref() == Some(*label))
            else {
                return (None, DiffCounts::default());
            };
            hash = flame_diff_path_hash(hash, child.addr, child.pid);
            counts = diff.counts.get(&hash).copied().unwrap_or_default();
            node = child;
        }
        (Some(node), counts)
    }

    #[test]
    fn diff_merges_shared_stacks() {
        let root_pc = 0xffff_8000_0000_1000u64;
        let a_pc = 0xffff_8000_0000_2000u64;
        let b_pc = 0xffff_8000_0000_3000u64;
        let c_pc = 0xffff_8000_0000_4000u64;
        // frames are leaf-first on the wire: [leaf, root].
        let base = profile(
            vec![
                sample(0, 1, 0, vec![a_pc, root_pc]),
                sample(0, 1, 0, vec![a_pc, root_pc]),
                sample(0, 1, 0, vec![b_pc, root_pc]),
            ],
            vec![thread(1, 0, "intr")],
        );
        let target = profile(
            vec![
                sample(0, 1, 0, vec![a_pc, root_pc]),
                sample(0, 1, 0, vec![c_pc, root_pc]),
                sample(0, 1, 0, vec![c_pc, root_pc]),
                sample(0, 1, 0, vec![c_pc, root_pc]),
            ],
            vec![thread(1, 0, "intr")],
        );
        let diff = diff_full(&base, &target, &FlameFilter::new(), false);
        assert_eq!(diff.base_total, 3);
        assert_eq!(diff.target_total, 4);
        // Root width is the normalized union: full scale per side.
        assert_eq!(diff.tree.total_count, 2 * DIFF_SCALE as u64);
        let root_label = format!("{root_pc:#x}");
        let (_, root_counts) = diff_lookup(&diff, &[&root_label]);
        assert_eq!((root_counts.base, root_counts.target), (3, 4));
        // Shared / vanished / new frames.
        let a_label = format!("{a_pc:#x}");
        let b_label = format!("{b_pc:#x}");
        let c_label = format!("{c_pc:#x}");
        let (_, a) = diff_lookup(&diff, &[&root_label, &a_label]);
        assert_eq!((a.base, a.target), (2, 1));
        assert_eq!((a.base_self, a.target_self), (2, 1));
        let (_, b) = diff_lookup(&diff, &[&root_label, &b_label]);
        assert_eq!((b.base, b.target), (1, 0));
        let (_, c) = diff_lookup(&diff, &[&root_label, &c_label]);
        assert_eq!((c.base, c.target), (0, 3));
        // Union width: each side's raw count scaled by its own total.
        let (a_node, _) = diff_lookup(&diff, &[&root_label, &a_label]);
        let expect = (2.0 * DIFF_SCALE / 3.0 + DIFF_SCALE / 4.0) as u64;
        assert_eq!(a_node.unwrap().total_count, expect);
        // Every merged node carries a nonzero hash addr and pid 0.
        assert_ne!(a_node.unwrap().addr, 0);
        assert_eq!(a_node.unwrap().pid, 0);
    }

    #[test]
    fn diff_normalizes_unequal_capture_totals() {
        let root_pc = 0xffff_8000_0000_1000u64;
        let a_pc = 0xffff_8000_0000_2000u64;
        let base = profile(
            vec![sample(0, 1, 0, vec![a_pc, root_pc]); 100],
            vec![thread(1, 0, "intr")],
        );
        let target = profile(
            vec![sample(0, 1, 0, vec![a_pc, root_pc]); 400],
            vec![thread(1, 0, "intr")],
        );
        let diff = diff_full(&base, &target, &FlameFilter::new(), false);
        let root_label = format!("{root_pc:#x}");
        let a_label = format!("{a_pc:#x}");
        let (node, counts) = diff_lookup(&diff, &[&root_label, &a_label]);
        assert_eq!((counts.base, counts.target), (100, 400));
        // Same share of each capture -> each side contributes full scale.
        assert_eq!(node.unwrap().total_count, 2 * DIFF_SCALE as u64);
    }

    #[test]
    fn diff_group_bars_match_by_label_across_pids() {
        let user_pc = 0x4000_0000u64;
        let base = profile(
            vec![sample(100, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc])],
            vec![thread(1, 100, "nginx")],
        );
        let target = profile(
            vec![
                sample(200, 2, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                sample(200, 2, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
            ],
            vec![thread(2, 200, "nginx")],
        );
        let diff = diff_full(&base, &target, &FlameFilter::new(), true);
        assert_eq!(diff.tree.children.len(), 1);
        let (_, counts) = diff_lookup(&diff, &["nginx"]);
        assert_eq!((counts.base, counts.target), (1, 2));
    }

    #[test]
    fn diff_time_windows_are_independent() {
        let pc = 0xffff_8000_0000_1000u64;
        let mut b1 = sample(0, 1, 0, vec![pc]);
        b1.ts = 1500;
        let mut b2 = sample(0, 1, 0, vec![pc]);
        b2.ts = 2500;
        let base = profile(vec![b1, b2], vec![thread(1, 0, "intr")]);
        let mut t1 = sample(0, 1, 0, vec![pc]);
        t1.ts = 5500;
        let mut t2 = sample(0, 1, 0, vec![pc]);
        t2.ts = 6500;
        let target = profile(vec![t1, t2], vec![thread(1, 0, "intr")]);
        let diff = build_flame_diff(
            &base,
            &target,
            &FlameFilter::new(),
            (1400, 1600),
            (6400, 6600),
            false,
            false,
            true,
        );
        assert_eq!(diff.base_total, 1);
        assert_eq!(diff.target_total, 1);
        let diff = build_flame_diff(
            &base,
            &target,
            &FlameFilter::new(),
            (0, u64::MAX),
            (6400, 6600),
            false,
            false,
            true,
        );
        assert_eq!(diff.base_total, 2);
        assert_eq!(diff.target_total, 1);
    }

    #[test]
    fn same_profile_regions_merge_shared_frames() {
        // frames are leaf-first on the wire: [leaf, root].
        let root_pc = 0xffff_8000_0000_1000u64;
        let a_pc = 0xffff_8000_0000_2000u64;
        let b_pc = 0xffff_8000_0000_3000u64;
        let s_pc = 0xffff_8000_0000_4000u64;
        let at = |ts: u64, frames: Vec<u64>| {
            let mut s = sample(0, 1, 0, frames);
            s.ts = ts;
            s
        };
        let p = profile(
            vec![
                at(1000, vec![a_pc, root_pc]),
                at(1100, vec![a_pc, root_pc]),
                at(1200, vec![s_pc, root_pc]),
                at(5000, vec![s_pc, root_pc]),
                at(5100, vec![b_pc, root_pc]),
            ],
            vec![thread(1, 0, "intr")],
        );
        // Region A (900..1300) vs region B (4900..5200) of one capture.
        let diff = build_flame_diff(
            &p,
            &p,
            &FlameFilter::new(),
            (900, 1300),
            (4900, 5200),
            false,
            false,
            true,
        );
        assert_eq!(diff.base_total, 3);
        assert_eq!(diff.target_total, 2);
        let root_label = format!("{root_pc:#x}");
        let (_, root_counts) = diff_lookup(&diff, &[&root_label]);
        assert_eq!((root_counts.base, root_counts.target), (3, 2));
        // Same profile on both sides: the shared frame folds into one
        // node; the region-unique frames sit beside it.
        let (root_node, _) = diff_lookup(&diff, &[&root_label]);
        assert_eq!(root_node.unwrap().children.len(), 3);
        let a_label = format!("{a_pc:#x}");
        let s_label = format!("{s_pc:#x}");
        let b_label = format!("{b_pc:#x}");
        let (_, a) = diff_lookup(&diff, &[&root_label, &a_label]);
        assert_eq!((a.base, a.target), (2, 0));
        let (_, s) = diff_lookup(&diff, &[&root_label, &s_label]);
        assert_eq!((s.base, s.target), (1, 1));
        let (_, b) = diff_lookup(&diff, &[&root_label, &b_label]);
        assert_eq!((b.base, b.target), (0, 1));
    }

    #[test]
    fn diff_drops_frame_selectors_but_keeps_group_selectors() {
        let user_pc = 0x4000_0000u64;
        let kernel_pc = 0xffff_8000_0010_0000u64;
        let mk = || {
            profile(
                vec![
                    sample(9104, 1, CPU_SAMPLE_FLAG_USER_MODE, vec![user_pc]),
                    sample(0, 2, 0, vec![kernel_pc]),
                ],
                vec![thread(1, 9104, "ella_server"), thread(2, 0, "idle")],
            )
        };
        let (base, target) = (mk(), mk());
        // A Frame include selector is address-space-scoped: applied
        // verbatim it would empty the baseline side. Dropped instead.
        let mut f = FlameFilter::new();
        f.include_stacks = vec![StackSelector::Frame(kernel_pc, 0)];
        let diff = diff_full(&base, &target, &f, false);
        assert_eq!(diff.base_total, 2);
        assert_eq!(diff.target_total, 2);
        // Group selectors are capture-independent and still apply.
        let mut f = FlameFilter::new();
        f.exclude_stacks = vec![StackSelector::Group("ella_server".to_string())];
        let diff = diff_full(&base, &target, &f, true);
        assert_eq!(diff.base_total, 1);
        assert_eq!(diff.target_total, 1);
    }

    /// Kernel-profile fixture with an embedded symbol table, so the
    /// diff merge can be exercised on *resolved* frames whose runtime
    /// addresses differ across captures (KASLR slide).
    fn kernel_symbol_profile(text_base: u64, samples: Vec<CpuStackSample>) -> CpuProfile {
        let mut p = profile(samples, vec![thread(1, 0, "intr")]);
        p.linker_files.insert(
            1,
            LinkerFile {
                file_id: 1,
                base: text_base,
                size: 0x100_0000,
                flags: LINKER_FILE_FLAG_KERNEL,
                build_id: String::new(),
                filename: "kernel".to_string(),
                pathname: "/boot/kernel/kernel".to_string(),
            },
        );
        p.symbols.insert(
            1,
            vec![
                SymbolRange {
                    offset: 0x1000,
                    size: 0x200,
                    name: "tcp_input".to_string(),
                },
                SymbolRange {
                    offset: 0x2000,
                    size: 0x100,
                    name: "ip_input".to_string(),
                },
            ],
        );
        p
    }

    #[test]
    fn diff_matches_resolved_symbols_across_kaslr_slides() {
        // Same kernel, different boot: text slides by 64 KiB between
        // captures. Raw addresses never match; symbol identity must.
        let base = kernel_symbol_profile(
            0xffff_8000_0000_0000,
            vec![
                sample(0, 1, 0, vec![0xffff_8000_0000_1050]), // tcp_input+0x50
                sample(0, 1, 0, vec![0xffff_8000_0000_1050]),
                sample(0, 1, 0, vec![0xffff_8000_0000_2010]), // ip_input+0x10
            ],
        );
        let target = kernel_symbol_profile(
            0xffff_8000_0001_0000,
            vec![sample(0, 1, 0, vec![0xffff_8000_0001_1050])], // tcp_input+0x50
        );
        let diff = diff_full(&base, &target, &FlameFilter::new(), false);
        assert_eq!(diff.tree.children.len(), 2);
        let (_, tcp) = diff_lookup(&diff, &["tcp_input"]);
        assert_eq!((tcp.base, tcp.target), (2, 1));
        let (_, ip) = diff_lookup(&diff, &["ip_input"]);
        assert_eq!((ip.base, ip.target), (1, 0));
    }

    #[test]
    fn name_selector_drops_stacks_through_that_symbol() {
        let base = kernel_symbol_profile(
            0xffff_8000_0000_0000,
            vec![
                sample(0, 1, 0, vec![0xffff_8000_0000_1050]),
                sample(0, 1, 0, vec![0xffff_8000_0000_2010]),
            ],
        );
        let mut f = FlameFilter::new();
        f.exclude_stacks = vec![StackSelector::Name("tcp_input".to_string())];
        let tree = build_flame_tree(&base, &f, FlameAggregation::DEFAULT_CLI, false, false);
        assert_eq!(tree.total_count, 1);
        let target = kernel_symbol_profile(
            0xffff_8000_0001_0000,
            vec![
                sample(0, 1, 0, vec![0xffff_8000_0001_1050]),
                sample(0, 1, 0, vec![0xffff_8000_0001_2010]),
            ],
        );
        let diff = build_flame_diff(
            &base,
            &target,
            &f,
            (0, u64::MAX),
            (0, u64::MAX),
            false,
            false,
            true,
        );
        assert_eq!(diff.base_total, 1);
        assert_eq!(diff.target_total, 1);
        let (_, ip) = diff_lookup(&diff, &["ip_input"]);
        assert_eq!((ip.base, ip.target), (1, 1));
    }
}
