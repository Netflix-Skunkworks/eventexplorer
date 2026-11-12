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

#[allow(unused_imports)]
use super::*;

/// Emit cpu-provider stack samples in **folded-stack** format on
/// stdout. The output is the input format expected by Brendan Gregg's
/// `flamegraph.pl`:
///
/// ```text
/// stack_root;stack_mid;stack_leaf <count>
/// ```
///
/// Filters compose with AND semantics. Frame labels follow the
/// standard cascade implemented by [`CpuProfile::format_pc`]:
///   1. resolved SYMBOL_RANGE -> `name+0xINNER` (or `file:name+0xINNER`
///      when `--show-module` is passed).
///   2. otherwise (default), when `KERNEL_INFO` is present and the
///      frame is in the kernel text segment -> `kernel+0xOFFSET`. Pass
///      `--raw-addrs` to disable this fallback and print raw hex.
///   3. otherwise -> raw `0xADDR`.
///
/// `--rel-text` is accepted as a no-op alias for backward compat
/// (kernel-relative formatting is the default now).
pub fn cmd_flame(profile: &CpuProfile, capture_start_us: u64, args: &[String]) {
    if profile.is_empty() {
        eprintln!("This capture has no cpu-provider events.");
        return;
    }

    let mut filter = FlameFilter::new();
    let mut rel_text = true;
    let mut show_module = false;
    // Tri-state: `None` = use the default ("by-symbol when symbols
    // are present, by-PC otherwise"). `Some(true)` / `Some(false)`
    // honours an explicit user flag and lets us print a more
    // targeted warning when `--by-symbol` was requested but the
    // capture has no symbols.
    let mut by_symbol_explicit: Option<bool> = None;
    // User-mode bar scope. Default is `false` (merge across pids) to
    // match the GUI and to keep N-forks-of-the-same-daemon captures
    // readable. `--split-user-by-pid` restores the legacy behaviour
    // (one tower per pid). Has no effect under `--by-pc` — see the
    // `build_flame_tree` docstring for why.
    let mut split_user_by_pid = false;
    let mut group_by_process = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            // Kept for backward compat: this is now the default.
            "--rel-text" => rel_text = true,
            "--raw-addrs" => rel_text = false,
            "--show-module" => show_module = true,
            "--by-symbol" => by_symbol_explicit = Some(true),
            "--by-pc" => by_symbol_explicit = Some(false),
            "--split-user-by-pid" => split_user_by_pid = true,
            "--group-by-process" => group_by_process = true,
            "--cpu" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    filter.cpus = parse_csv_u(v);
                }
            }
            "--pid" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    filter.pids = parse_csv_u(v);
                }
            }
            "--tid" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    filter.tids = parse_csv_u(v);
                }
            }
            "--start-s" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                    filter.ts_start_us = capture_start_us.saturating_add((v * 1e6) as u64);
                }
            }
            "--end-s" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                    filter.ts_end_us = capture_start_us.saturating_add((v * 1e6) as u64);
                }
            }
            "--comm" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    filter.comms = v
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
            other => {
                eprintln!("flame: unknown option {other}");
                process::exit(1);
            }
        }
        i += 1;
    }

    // Resolve the by-symbol flag: explicit user choice wins; otherwise
    // default to "yes when we have something to collapse on, no
    // otherwise". Without symbols both modes produce byte-identical
    // output, so the default is purely a presentation choice for the
    // stderr summary.
    let nsym = profile.symbol_count();
    let nlf = profile.linker_files.len();
    let nusym = profile.user_symbol_count();
    let nuf = profile.user_files.len();
    let npm = profile.proc_map_count();
    // "By-symbol" is the default whenever the capture carries
    // *any* symbol metadata — kernel SYMBOL_RANGE or user
    // USER_SYMBOL_RANGE, since `elog -U` typically delivers user
    // symbols even when `--embed-symbols` was not used.
    let any_symbols = nsym + nusym > 0;
    let by_symbol = by_symbol_explicit.unwrap_or(any_symbols);
    if by_symbol_explicit == Some(true) && !any_symbols {
        eprintln!(
            "# note: --by-symbol requested but capture has no SYMBOL_RANGE / \
             USER_SYMBOL_RANGE events; output will be identical to per-PC \
             aggregation. Re-capture with `elog --embed-symbols -U` to \
             actually collapse by symbol."
        );
    }
    let aggregation = if by_symbol {
        // CLI doesn't paint per-PC overlay rectangles — folded output
        // would gain a redundant extra level for each function. Use
        // the overlay-free flavour so leaf-self samples just bump the
        // symbol's self_count and the output reads like
        // stackcollapse-perf.pl.
        FlameAggregation::DEFAULT_CLI
    } else {
        FlameAggregation::ByPc
    };
    let tree = build_flame_tree(
        profile,
        &filter,
        aggregation,
        split_user_by_pid,
        group_by_process,
    );
    // Only call out the user-scope flag when it's actually doing
    // something, i.e. in by-symbol mode. ByPc can't honour it (see
    // `build_flame_tree`), so flagging "split user by pid" there
    // would lie about the output.
    let user_scope_note = if by_symbol && any_symbols {
        if split_user_by_pid {
            " (user bars split per-pid)"
        } else {
            " (user bars merged across pids)"
        }
    } else {
        ""
    };
    let group_note = if group_by_process {
        " (grouped by process)"
    } else {
        ""
    };
    // `tree.self_count` is the synthetic root's self-time: samples
    // that had no frames at all (empty-stack samples). On fresh
    // captures this is always 0 — the kernel drops idle ticks before
    // emitting — but older captures may still carry empty-stack idle
    // samples, in which case they're surfaced as a single `IDLE`
    // line in the folded output below.
    eprintln!(
        "# {} samples after filter — {} kernel syms / {} linker files \
         — {} user syms / {} user files / {} proc maps — aggregated {}{}{}",
        tree.total_count,
        nsym,
        nlf,
        nusym,
        nuf,
        npm,
        if by_symbol && any_symbols {
            "by symbol"
        } else {
            "by PC"
        },
        user_scope_note,
        group_note,
    );
    if nsym == 0 && nlf > 0 {
        eprintln!(
            "# note: LINKER_FILE events present but no SYMBOL_RANGE events; \
             re-capture with `elog --embed-symbols` for resolved kernel names."
        );
    }
    if nusym == 0 && nuf > 0 {
        eprintln!(
            "# note: USER_FILE events present but no USER_SYMBOL_RANGE events; \
             re-capture with `elog -U` for resolved user-space names."
        );
    }

    let folded = fold_flame_tree(&tree, |addr, pid| {
        let pid_opt = if pid == 0 { None } else { Some(pid) };
        if let Some(hit) = profile.symbolize(addr, pid_opt) {
            return if show_module {
                hit.label_with_file()
            } else {
                hit.label()
            };
        }
        if rel_text {
            if let Some(k) = &profile.kernel {
                if addr >= k.text_base && addr < k.text_base.saturating_add(k.text_size) {
                    return format!("kernel+{:#x}", addr - k.text_base);
                }
            }
        }
        format!("{addr:#x}")
    });
    print!("{folded}");
}
