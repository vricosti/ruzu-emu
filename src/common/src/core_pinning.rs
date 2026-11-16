// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Lichii `common/core_pinning.h` / `common/core_pinning.cpp`.
//!
//! Lichii extension (absent from Eden), behind the `pin_gpu_threads` setting:
//! the planner behind `Common::PinCurrentThread` (see [`crate::thread`]).

/// Lichii `Common::LogicalProcessor`: one logical processor as the host
/// reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalProcessor {
    /// What the host's affinity call takes (a CPU set id on Windows).
    pub id: u32,
    /// The physical core: equal for SMT siblings.
    pub core: u32,
    /// Higher is faster. Equal on every processor of a CPU that is not hybrid.
    pub efficiency_class: u8,
}

/// Lichii `Common::MinCoresForGpuThreadPinning`: physical cores of the fastest
/// class needed before the GPU threads are pinned.
pub const MIN_CORES_FOR_GPU_THREAD_PINNING: usize = 6;

/// Lichii `Common::GpuThreadPinning`: logical processor ids for each pinned
/// role. All three are empty when the host has fewer than
/// [`MIN_CORES_FOR_GPU_THREAD_PINNING`] performance cores.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuThreadPinning {
    pub performance_cores: usize,
    pub front: Vec<u32>,
    pub back: Vec<u32>,
    pub guest: Vec<u32>,
}

/// Lichii `Common::PlanGpuThreadPinning`: the split GPU thread's front and
/// back each get a whole physical performance core, so no SMT sibling shares
/// it, and the guest cores get the remaining performance cores. Core 0 takes
/// most interrupts, so the GPU threads get the next two.
pub fn plan_gpu_thread_pinning(processors: &[LogicalProcessor]) -> GpuThreadPinning {
    let mut plan = GpuThreadPinning::default();
    let Some(fastest) = processors.iter().map(|processor| processor.efficiency_class).max() else {
        return plan;
    };
    let mut cores: Vec<u32> = processors
        .iter()
        .filter(|processor| processor.efficiency_class == fastest)
        .map(|processor| processor.core)
        .collect();
    cores.sort_unstable();
    cores.dedup();
    plan.performance_cores = cores.len();
    if cores.len() < MIN_CORES_FOR_GPU_THREAD_PINNING {
        return plan;
    }
    let front_core = cores[1];
    let back_core = cores[2];
    for processor in processors {
        if processor.efficiency_class != fastest {
            continue;
        }
        if processor.core == front_core {
            plan.front.push(processor.id);
        } else if processor.core == back_core {
            plan.back.push(processor.id);
        } else {
            plan.guest.push(processor.id);
        }
    }
    plan
}

/// Lichii `Common::ParseCpuList`: the CPUs of a Linux sysfs CPU list such as
/// "0-3,8,10-11", in order. Empty for an empty or malformed list.
pub fn parse_cpu_list(list: &str) -> Vec<u32> {
    let mut cpus = Vec::new();
    let mut list = list.trim_end_matches(['\n', ' ']);
    // `std::from_chars` into a u32: digits only, no sign, no overflow.
    let number = |text: &str| -> Option<u32> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        text.parse::<u32>().ok()
    };
    while !list.is_empty() {
        // As Lichii's loop: a trailing comma ends the list without an empty range.
        let (range, rest) = match list.find(',') {
            Some(comma) => (&list[..comma], &list[comma + 1..]),
            None => (list, ""),
        };
        list = rest;
        let (first_text, last_text) = match range.find('-') {
            Some(dash) => (&range[..dash], &range[dash + 1..]),
            None => (range, range),
        };
        let (Some(first), Some(last)) = (number(first_text), number(last_text)) else {
            return Vec::new();
        };
        if last < first || last - first >= 4096 {
            return Vec::new();
        }
        // 64-bit, so a range ending at the largest u32 doesn't wrap to 0 and loop forever.
        for cpu in u64::from(first)..=u64::from(last) {
            cpus.push(cpu as u32);
        }
    }
    cpus
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cores` physical cores of class `class`, `threads` logical processors
    /// each, numbered like Linux on x86 (the SMT siblings of core `c` are
    /// `c`, `c + cores`, ...).
    fn smt_cpu(cores: u32, threads: u32, class: u8) -> Vec<LogicalProcessor> {
        (0..threads)
            .flat_map(|thread| {
                (0..cores).map(move |core| LogicalProcessor {
                    id: thread * cores + core,
                    core,
                    efficiency_class: class,
                })
            })
            .collect()
    }

    #[test]
    fn eight_core_smt_cpu_pins_cores_one_and_two_and_keeps_guests_on_the_other_six() {
        // The measurement host: 8 cores, 16 logical processors.
        let plan = plan_gpu_thread_pinning(&smt_cpu(8, 2, 128));
        assert_eq!(plan.performance_cores, 8);
        assert_eq!(plan.front, vec![1, 9]);
        assert_eq!(plan.back, vec![2, 10]);
        assert_eq!(plan.guest, vec![0, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 15]);
    }

    #[test]
    fn six_cores_is_the_minimum() {
        let plan = plan_gpu_thread_pinning(&smt_cpu(6, 1, 128));
        assert_eq!(plan.front, vec![1]);
        assert_eq!(plan.back, vec![2]);
        assert_eq!(plan.guest, vec![0, 3, 4, 5]);
        let plan = plan_gpu_thread_pinning(&smt_cpu(5, 2, 128));
        assert_eq!(plan.performance_cores, 5);
        assert!(plan.front.is_empty() && plan.back.is_empty() && plan.guest.is_empty());
    }

    #[test]
    fn no_processors_plans_nothing() {
        assert_eq!(plan_gpu_thread_pinning(&[]), GpuThreadPinning::default());
    }

    #[test]
    fn hybrid_cpu_counts_and_uses_only_its_fastest_class() {
        // 6 performance cores with SMT (ids 0-11) and 8 efficiency cores (12-19).
        let mut processors: Vec<LogicalProcessor> = (0..12)
            .map(|id| LogicalProcessor { id, core: id / 2, efficiency_class: 128 })
            .collect();
        processors.extend((12..20).map(|id| LogicalProcessor { id, core: 100 + id, efficiency_class: 64 }));
        let plan = plan_gpu_thread_pinning(&processors);
        assert_eq!(plan.performance_cores, 6);
        assert_eq!(plan.front, vec![2, 3]);
        assert_eq!(plan.back, vec![4, 5]);
        assert_eq!(plan.guest, vec![0, 1, 6, 7, 8, 9, 10, 11]);
        // 4 performance cores and many efficiency cores: skipped.
        let mut small: Vec<LogicalProcessor> = (0..8)
            .map(|id| LogicalProcessor { id, core: id / 2, efficiency_class: 128 })
            .collect();
        small.extend((8..24).map(|id| LogicalProcessor { id, core: 100 + id, efficiency_class: 64 }));
        let plan = plan_gpu_thread_pinning(&small);
        assert_eq!(plan.performance_cores, 4);
        assert!(plan.front.is_empty());
    }

    #[test]
    fn cores_are_ranked_by_id_not_by_listing_order() {
        // Two packages: Lichii keys a core as (package << 16) | core_id.
        let processors: Vec<LogicalProcessor> = [(0u32, 1u32 << 16), (1, 0), (2, (1 << 16) | 1), (3, 1), (4, 2), (5, 3)]
            .iter()
            .map(|&(id, core)| LogicalProcessor { id, core, efficiency_class: 128 })
            .collect();
        let plan = plan_gpu_thread_pinning(&processors);
        assert_eq!(plan.front, vec![3]);
        assert_eq!(plan.back, vec![4]);
        assert_eq!(plan.guest, vec![0, 1, 2, 5]);
    }

    #[test]
    fn parse_cpu_list_reads_sysfs_lists() {
        assert_eq!(parse_cpu_list("0-3,8,10-11\n"), vec![0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(parse_cpu_list("5"), vec![5]);
        assert_eq!(parse_cpu_list(""), Vec::<u32>::new());
        assert_eq!(parse_cpu_list("\n"), Vec::<u32>::new());
        assert_eq!(parse_cpu_list("3-1"), Vec::<u32>::new());
        assert_eq!(parse_cpu_list("0-4096"), Vec::<u32>::new());
        assert_eq!(parse_cpu_list("0-4095").len(), 4096);
        assert_eq!(parse_cpu_list("a"), Vec::<u32>::new());
        assert_eq!(parse_cpu_list("1,,2"), Vec::<u32>::new());
        assert_eq!(parse_cpu_list("1,"), vec![1]);
        assert_eq!(parse_cpu_list("1-"), Vec::<u32>::new());
        assert_eq!(parse_cpu_list("4294967295"), vec![u32::MAX]);
    }
}
