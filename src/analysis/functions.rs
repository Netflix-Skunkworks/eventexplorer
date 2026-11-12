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

//! Per-binary function rankings and targeted stack drill-downs.

use super::{filtered_sample_indices, FlameFilter};
use crate::elog_parser::{CpuProfile, SymbolHit};
use rustc_hash::{FxHashMap, FxHashSet};

/// Capture-local identity of an executable image. The numeric id is stable
/// across every process mapping the same user ELF; kernel linker files have
/// an equivalent capture-local id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProfileBinaryId {
    Kernel(u32),
    User(u32),
}

/// A function's stable identity within a capture. `offset` is relative to a
/// kernel linker file's runtime base or is the user ELF's on-disk file offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProfileFunctionId {
    pub binary: ProfileBinaryId,
    pub offset: u64,
}

#[derive(Debug, Clone)]
pub struct FunctionRow {
    pub id: ProfileFunctionId,
    pub name: String,
    pub size: u32,
    pub self_samples: u64,
    pub inclusive_samples: u64,
    /// PCs of this function on any stack frame, in the same coordinate
    /// space as `id.offset`. Source annotation looks these up in
    /// `SOURCE_LINE`.
    pub sampled_offsets: Vec<(u64, u64)>,
}

#[derive(Debug, Clone)]
pub struct BinaryFunctionProfile {
    pub id: ProfileBinaryId,
    pub name: String,
    pub pathname: String,
    pub build_id: String,
    pub functions: Vec<FunctionRow>,
}

#[derive(Debug, Clone, Default)]
pub struct FunctionProfile {
    pub binaries: Vec<BinaryFunctionProfile>,
    pub selected_samples: u64,
    pub unresolved_leaf_samples: u64,
}

#[derive(Debug, Clone)]
pub struct FunctionStack {
    /// Root-most to leaf-most display labels.
    pub frames: Vec<String>,
    pub count: u64,
}

#[derive(Debug, Clone)]
pub struct ResolvedProfileFunction {
    pub id: ProfileFunctionId,
    pub name: String,
    pub size: u32,
    /// Sampled instruction in the same binary-relative/file-offset
    /// coordinate space as `id.offset`.
    pub sampled_offset: u64,
}

#[derive(Default)]
struct MutableFunction {
    name: String,
    size: u32,
    self_samples: u64,
    inclusive_samples: u64,
    sampled_offsets: FxHashMap<u64, u64>,
}

pub fn resolve_profile_function(
    profile: &CpuProfile,
    pc: u64,
    pid: u32,
) -> Option<ResolvedProfileFunction> {
    match profile.symbolize(pc, Some(pid))? {
        SymbolHit::Kernel {
            file,
            symbol,
            inner_offset,
        } => Some(ResolvedProfileFunction {
            id: ProfileFunctionId {
                binary: ProfileBinaryId::Kernel(file.file_id),
                offset: symbol.offset,
            },
            name: symbol.name.clone(),
            size: symbol.size,
            sampled_offset: symbol.offset + inner_offset,
        }),
        SymbolHit::User {
            file,
            symbol,
            inner_offset,
            ..
        } => Some(ResolvedProfileFunction {
            id: ProfileFunctionId {
                binary: ProfileBinaryId::User(file.user_file_id),
                offset: symbol.offset,
            },
            name: symbol.name.clone(),
            size: symbol.size,
            sampled_offset: symbol.offset + inner_offset,
        }),
        SymbolHit::UserModuleOnly { .. } => None,
    }
}

/// Build per-binary function totals. Ranking defaults to self samples in the
/// GUI, while `inclusive_samples` counts each function at most once per stack
/// so recursion does not inflate its total.
pub fn build_function_profile(profile: &CpuProfile, filter: &FlameFilter) -> FunctionProfile {
    let selected = filtered_sample_indices(profile, filter);
    let mut functions: FxHashMap<ProfileFunctionId, MutableFunction> = FxHashMap::default();
    let mut unresolved_leaf_samples = 0;

    for idx in &selected {
        let sample = &profile.samples[*idx];
        let mut seen = FxHashSet::default();
        for (frame_idx, &pc) in sample.frames.iter().enumerate() {
            let Some(resolved) = resolve_profile_function(profile, pc, sample.pid) else {
                if frame_idx == 0 {
                    unresolved_leaf_samples += 1;
                }
                continue;
            };
            let entry = functions.entry(resolved.id).or_default();
            if entry.name.is_empty() {
                entry.name = resolved.name;
                entry.size = resolved.size;
            }
            if seen.insert(resolved.id) {
                entry.inclusive_samples += 1;
            }
            *entry
                .sampled_offsets
                .entry(resolved.sampled_offset)
                .or_default() += 1;
            if frame_idx == 0 {
                entry.self_samples += 1;
            }
        }
    }

    let mut by_binary: FxHashMap<ProfileBinaryId, Vec<FunctionRow>> = FxHashMap::default();
    for (id, f) in functions {
        let mut sampled_offsets: Vec<_> = f.sampled_offsets.into_iter().collect();
        sampled_offsets.sort_unstable_by_key(|&(offset, _)| offset);
        by_binary.entry(id.binary).or_default().push(FunctionRow {
            id,
            name: f.name,
            size: f.size,
            self_samples: f.self_samples,
            inclusive_samples: f.inclusive_samples,
            sampled_offsets,
        });
    }

    let mut binaries = Vec::new();
    for (id, mut functions) in by_binary {
        functions.sort_unstable_by(|a, b| {
            b.self_samples
                .cmp(&a.self_samples)
                .then_with(|| b.inclusive_samples.cmp(&a.inclusive_samples))
                .then_with(|| a.name.cmp(&b.name))
        });
        let (name, pathname, build_id) = match id {
            ProfileBinaryId::Kernel(file_id) => {
                let file = &profile.linker_files[&file_id];
                (
                    file.filename.clone(),
                    file.pathname.clone(),
                    file.build_id.clone(),
                )
            }
            ProfileBinaryId::User(file_id) => {
                let file = &profile.user_files[&file_id];
                (
                    file.short_name().to_string(),
                    file.pathname.clone(),
                    file.build_id.clone(),
                )
            }
        };
        binaries.push(BinaryFunctionProfile {
            id,
            name,
            pathname,
            build_id,
            functions,
        });
    }
    binaries.sort_unstable_by(|a, b| {
        let a_samples: u64 = a.functions.iter().map(|f| f.self_samples).sum();
        let b_samples: u64 = b.functions.iter().map(|f| f.self_samples).sum();
        b_samples.cmp(&a_samples).then_with(|| a.name.cmp(&b.name))
    });

    FunctionProfile {
        binaries,
        selected_samples: selected.len() as u64,
        unresolved_leaf_samples,
    }
}

/// Aggregate only the stacks containing `target`. This intentionally performs
/// a targeted second pass instead of retaining every full stack under every
/// function, whose memory cost would be proportional to stack-depth squared.
pub fn function_stacks(
    profile: &CpuProfile,
    filter: &FlameFilter,
    target: ProfileFunctionId,
) -> Vec<FunctionStack> {
    let selected = filtered_sample_indices(profile, filter);
    function_stacks_for_indices(profile, &selected, Some(target))
}

pub fn function_stacks_for_indices(
    profile: &CpuProfile,
    indices: &[usize],
    target: Option<ProfileFunctionId>,
) -> Vec<FunctionStack> {
    let mut stacks: FxHashMap<Vec<String>, u64> = FxHashMap::default();
    for &idx in indices {
        let sample = match profile.samples.get(idx) {
            Some(s) => s,
            None => continue,
        };
        if let Some(target_id) = target {
            if !sample.frames.iter().any(|&pc| {
                resolve_profile_function(profile, pc, sample.pid).is_some_and(|r| r.id == target_id)
            }) {
                continue;
            }
        }
        let labels = sample
            .frames
            .iter()
            .rev()
            .map(|&pc| profile.format_pc(pc, Some(sample.pid), true))
            .collect();
        *stacks.entry(labels).or_default() += 1;
    }
    let mut rows: Vec<_> = stacks
        .into_iter()
        .map(|(frames, count)| FunctionStack { frames, count })
        .collect();
    rows.sort_unstable_by(|a, b| b.count.cmp(&a.count).then_with(|| a.frames.cmp(&b.frames)));
    rows
}

/// Instruction offsets for one function under the active filter, counting
/// every frame it occupies. Used by annotated source without rebuilding
/// every binary's ranking table.
pub fn function_sampled_offsets(
    profile: &CpuProfile,
    filter: &FlameFilter,
    target: ProfileFunctionId,
) -> Vec<(u64, u64)> {
    let mut offsets: FxHashMap<u64, u64> = FxHashMap::default();
    for idx in filtered_sample_indices(profile, filter) {
        let sample = &profile.samples[idx];
        for &pc in &sample.frames {
            let Some(resolved) = resolve_profile_function(profile, pc, sample.pid) else {
                continue;
            };
            if resolved.id == target {
                *offsets.entry(resolved.sampled_offset).or_default() += 1;
            }
        }
    }
    let mut offsets: Vec<_> = offsets.into_iter().collect();
    offsets.sort_unstable_by_key(|&(offset, _)| offset);
    offsets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elog_parser::{CpuStackSample, LinkerFile, SymbolRange, LINKER_FILE_FLAG_KERNEL};

    fn profile() -> CpuProfile {
        let mut profile = CpuProfile::default();
        profile.linker_files.insert(
            1,
            LinkerFile {
                file_id: 1,
                base: 0x1000,
                size: 0x1000,
                flags: LINKER_FILE_FLAG_KERNEL,
                build_id: "abc".into(),
                filename: "kernel".into(),
                pathname: "/boot/kernel/kernel".into(),
            },
        );
        profile.symbols.insert(
            1,
            vec![
                SymbolRange {
                    offset: 0x10,
                    size: 0x20,
                    name: "leaf".into(),
                },
                SymbolRange {
                    offset: 0x40,
                    size: 0x20,
                    name: "caller".into(),
                },
            ],
        );
        profile.samples = vec![
            CpuStackSample {
                ts: 10,
                cpu: 0,
                tid: 1,
                pid: 2,
                sample_type: 0,
                flags: 0,
                freq_mhz: None,
                frames: vec![0x1012, 0x1042, 0x1044],
            },
            CpuStackSample {
                ts: 20,
                cpu: 0,
                tid: 1,
                pid: 2,
                sample_type: 0,
                flags: 0,
                freq_mhz: None,
                frames: vec![0x1013, 0x1042],
            },
        ];
        profile
    }

    #[test]
    fn ranks_self_and_deduplicates_recursive_inclusive_hits() {
        let profile = profile();
        let built = build_function_profile(&profile, &FlameFilter::default());
        let functions = &built.binaries[0].functions;
        assert_eq!(functions[0].name, "leaf");
        assert_eq!(functions[0].self_samples, 2);
        let caller = functions.iter().find(|f| f.name == "caller").unwrap();
        assert_eq!(caller.inclusive_samples, 2);
        assert_eq!(caller.self_samples, 0);
        assert_eq!(functions[0].sampled_offsets, vec![(0x12, 1), (0x13, 1)]);
        assert_eq!(caller.sampled_offsets, vec![(0x42, 2), (0x44, 1)]);
        let caller_id = ProfileFunctionId {
            binary: ProfileBinaryId::Kernel(1),
            offset: 0x40,
        };
        assert_eq!(
            function_sampled_offsets(&profile, &FlameFilter::default(), caller_id),
            vec![(0x42, 2), (0x44, 1)]
        );
    }

    #[test]
    fn stack_drilldown_aggregates_identical_stacks() {
        let profile = profile();
        let target = ProfileFunctionId {
            binary: ProfileBinaryId::Kernel(1),
            offset: 0x10,
        };
        let stacks = function_stacks(&profile, &FlameFilter::default(), target);
        assert_eq!(stacks.len(), 2);
        assert_eq!(stacks.iter().map(|s| s.count).sum::<u64>(), 2);
        assert!(stacks[0].frames.iter().any(|f| f.contains("leaf")));
    }

    #[test]
    fn cpu_selection_scopes_rankings_stacks_offsets_and_flames() {
        use super::super::{build_flame_diff, build_flame_tree, FlameAggregation};

        let mut profile = profile();
        profile.samples[1].cpu = 8;
        let mut other_cpu = profile.samples[0].clone();
        other_cpu.cpu = 16;
        other_cpu.ts = 30;
        other_cpu.frames = vec![0x1044];
        profile.samples.push(other_cpu);
        let leaf = ProfileFunctionId {
            binary: ProfileBinaryId::Kernel(1),
            offset: 0x10,
        };
        for (cpus, total, leaves) in [
            (vec![], 3, 2),
            (vec![0], 1, 1),
            (vec![0, 8], 2, 2),
            (vec![16], 1, 0),
            (vec![7], 0, 0),
        ] {
            let filter = FlameFilter {
                cpus,
                pids: vec![2],
                ..Default::default()
            };
            let ranked = build_function_profile(&profile, &filter);
            assert_eq!(ranked.selected_samples, total);
            let leaf_row = ranked
                .binaries
                .iter()
                .flat_map(|b| &b.functions)
                .find(|f| f.id == leaf);
            assert_eq!(leaf_row.map_or(0, |f| f.self_samples), leaves);
            assert_eq!(
                function_stacks(&profile, &filter, leaf)
                    .iter()
                    .map(|s| s.count)
                    .sum::<u64>(),
                leaves
            );
            assert_eq!(
                function_sampled_offsets(&profile, &filter, leaf)
                    .iter()
                    .map(|(_, count)| count)
                    .sum::<u64>(),
                leaves
            );
            let tree = build_flame_tree(
                &profile,
                &filter,
                FlameAggregation::DEFAULT_GUI,
                false,
                true,
            );
            assert_eq!(tree.total_count, total);
            let diff = build_flame_diff(
                &profile,
                &profile,
                &filter,
                (0, u64::MAX),
                (0, u64::MAX),
                true,
                false,
                true,
            );
            assert_eq!((diff.base_total, diff.target_total), (total, total));
        }
        let filter = FlameFilter {
            cpus: vec![0, 8],
            ts_start_us: 15,
            ts_end_us: 25,
            ..Default::default()
        };
        assert_eq!(
            build_function_profile(&profile, &filter).selected_samples,
            1
        );
        assert_eq!(
            function_sampled_offsets(&profile, &filter, leaf),
            [(0x13, 1)]
        );
    }
}
