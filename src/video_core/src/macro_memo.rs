// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `video_core/macro_memo.h` (header-only upstream).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

pub type MacroMemoPairs = Vec<(u32, u32)>;

/// Identity hash for the already mixed u64 macro-call keys.
#[derive(Default)]
pub struct MacroMemoHash(u64);

impl Hasher for MacroMemoHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
    fn write(&mut self, bytes: &[u8]) {
        self.0 = u64::from_ne_bytes(bytes.try_into().expect("macro memo keys are u64"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_preserves_upstream_wrapping_and_identity_map_hash() {
        assert_eq!(BasicMacroMemo::hash(0, &[]), 0x9e3779b97f4a7c15);
        let mixed = BasicMacroMemo::hash(u32::MAX, &[0, u32::MAX, 0x12345678]);
        assert_eq!(mixed, 0xa3c8a203c7db9fb6);
        let mut hasher = MacroMemoHash::default();
        hasher.write_u64(mixed);
        assert_eq!(hasher.finish(), mixed);
    }

    #[test]
    fn guards_and_ordered_effects_follow_upstream_record_layout() {
        let mut memo = BasicMacroMemo::new();
        assert!(memo.find(3, &[7], &[11]).is_none());
        memo.store(3, &[7], &[(0, 11)], &[(0, 12), (0, 13)], &[(8, 1), (8, 2)]);
        assert_eq!(
            memo.arena,
            [3, 1, 1, 2, 2, 7, 0, 11, 0, 12, 0, 13, 8, 1, 8, 2]
        );
        let effects = memo.find(3, &[7], &[11]).unwrap();
        assert_eq!(effects.data_ram_writes, [0, 12, 0, 13]);
        assert_eq!(effects.writes, [8, 1, 8, 2]);
        assert!(memo.find(3, &[7], &[12]).is_none());
        assert_eq!(memo.get_statistics().slot_hits, 1);
        assert_eq!(memo.get_statistics().guard_failures, 1);
        assert_eq!(memo.get_statistics().misses, 1);
    }

    #[test]
    fn direct_slot_collision_falls_back_to_map_and_overwrite_keeps_arena() {
        let mut memo = BasicMacroMemo::new();
        memo.store(1, &[], &[(0, 2)], &[], &[(10, 20)]);
        memo.store(257, &[], &[], &[], &[(30, 40)]);
        let words = memo.arena_words();
        memo.store(1, &[], &[(0, 3)], &[], &[(10, 21)]);
        assert_eq!(memo.arena_words(), words);
        assert_eq!(memo.get_statistics().overwrites, 1);
        assert_eq!(memo.find(1, &[], &[3]).unwrap().writes, [10, 21]);
        assert_eq!(memo.get_statistics().slot_hits, 0);
        assert_eq!(memo.find(1, &[], &[3]).unwrap().writes, [10, 21]);
        assert_eq!(memo.get_statistics().slot_hits, 1);
    }

    #[test]
    fn compaction_discards_stale_shapes_and_resets_direct_slots() {
        let mut memo = BasicMacroMemo::new();
        memo.store(4, &[1], &[], &[], &[(1, 2)]);
        memo.store(4, &[1], &[], &[], &[(1, 3), (2, 4)]);
        assert_eq!(memo.size(), 1);
        assert_eq!(memo.arena_words(), 18);
        // Reach the real upstream threshold with stale backing; the next new
        // record must compact first, while overwriting in place must not.
        memo.arena.resize(BasicMacroMemo::MAX_ARENA_WORDS, 0);
        memo.store(4, &[1], &[], &[], &[(1, 5), (2, 6)]);
        assert_eq!(memo.get_statistics().compactions, 0);
        memo.store(5, &[], &[], &[], &[]);
        assert_eq!(memo.arena_words(), 15);
        assert_eq!(memo.get_statistics().compactions, 1);
        assert_eq!(memo.find(4, &[1], &[]).unwrap().writes, [1, 5, 2, 6]);
        assert_eq!(memo.get_statistics().slot_hits, 0);
        assert_eq!(memo.get_statistics().clears, 0);
    }

    #[test]
    fn live_threshold_clears_but_generation_clear_does_not_increment_statistics() {
        let mut memo = BasicMacroMemo::new();
        let parameters = vec![0; BasicMacroMemo::MAX_ARENA_WORDS / 4 * 3 - 5];
        memo.store(9, &parameters, &[], &[], &[]);
        memo.compact();
        assert_eq!(memo.size(), 0);
        assert_eq!(memo.arena_words(), 0);
        assert_eq!(memo.get_statistics().clears, 1);
        memo.store(1, &[], &[], &[], &[]);
        memo.synchronize(0);
        assert!(memo.find(1, &[], &[]).is_some());
        let statistics = *memo.get_statistics();
        memo.synchronize(1);
        assert_eq!(*memo.get_statistics(), statistics);
        assert_eq!(memo.size(), 0);
        assert!(memo.last_by_start.iter().all(|&x| x == (0, u32::MAX)));
    }

    #[test]
    fn hash_collision_is_guarded_by_start_and_parameters() {
        let mut memo = BasicMacroMemo::new();
        memo.store(0, &[1], &[], &[], &[(2, 3)]);
        let offset = memo.entries[&BasicMacroMemo::hash(0, &[1])];
        // Force the same record to be probed by an unrelated key.
        memo.entries.insert(BasicMacroMemo::hash(256, &[1]), offset);
        assert!(memo.find(256, &[1], &[]).is_none());
        memo.entries.insert(BasicMacroMemo::hash(0, &[2]), offset);
        assert!(memo.find(0, &[2], &[]).is_none());
        assert_eq!(memo.get_statistics().guard_failures, 2);
    }
}

pub struct Effects<'a> {
    pub data_ram_writes: &'a [u32],
    pub writes: &'a [u32],
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Statistics {
    pub hits: u64,
    pub slot_hits: u64,
    pub misses: u64,
    pub guard_failures: u64,
    pub stores: u64,
    pub overwrites: u64,
    pub compactions: u64,
    pub clears: u64,
}

/// Upstream's map template is instantiated with a u64 -> u32 hash map.
/// Borrowed effects cannot outlive a subsequent arena mutation in Rust.
pub struct BasicMacroMemo {
    entries: HashMap<u64, u32, BuildHasherDefault<MacroMemoHash>>,
    arena: Vec<u32>,
    compacted: Vec<u32>,
    last_by_start: [(u32, u32); 256],
    generation: u64,
    statistics: Statistics,
}

impl Default for BasicMacroMemo {
    fn default() -> Self {
        Self::new()
    }
}

impl BasicMacroMemo {
    const NO_RECORD: u32 = u32::MAX;
    const HEADER_WORDS: usize = 5;
    const MAX_ARENA_WORDS: usize = 1 << 22;

    pub fn new() -> Self {
        Self {
            entries: HashMap::default(),
            arena: Vec::new(),
            compacted: Vec::new(),
            last_by_start: [(0, Self::NO_RECORD); 256],
            generation: 0,
            statistics: Statistics::default(),
        }
    }

    pub fn find(
        &mut self,
        start: u32,
        parameters: &[u32],
        data_ram: &[u32],
    ) -> Option<Effects<'_>> {
        let slot = start as usize % self.last_by_start.len();
        let last = self.last_by_start[slot];
        // Match returns ranges so statistics can be updated before borrowing effects.
        if last.0 == start && last.1 != Self::NO_RECORD {
            if let Some((data, writes)) = self.r#match(last.1, start, parameters, data_ram) {
                self.statistics.hits = self.statistics.hits.wrapping_add(1);
                self.statistics.slot_hits = self.statistics.slot_hits.wrapping_add(1);
                return Some(Effects {
                    data_ram_writes: &self.arena[data],
                    writes: &self.arena[writes],
                });
            }
        }
        let Some(&offset) = self.entries.get(&Self::hash(start, parameters)) else {
            self.statistics.misses = self.statistics.misses.wrapping_add(1);
            return None;
        };
        let Some((data, writes)) = self.r#match(offset, start, parameters, data_ram) else {
            self.statistics.guard_failures = self.statistics.guard_failures.wrapping_add(1);
            return None;
        };
        self.last_by_start[slot] = (start, offset);
        self.statistics.hits = self.statistics.hits.wrapping_add(1);
        Some(Effects {
            data_ram_writes: &self.arena[data],
            writes: &self.arena[writes],
        })
    }

    /// Port of Match; ranges replace output spans solely to satisfy borrowing.
    fn r#match(
        &self,
        offset: u32,
        start: u32,
        parameters: &[u32],
        data_ram: &[u32],
    ) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
        let offset = offset as usize;
        let record = &self.arena[offset..];
        let num_parameters = record[1] as usize;
        let num_reads = record[2] as usize;
        if record[0] != start
            || num_parameters != parameters.len()
            || &record[Self::HEADER_WORDS..Self::HEADER_WORDS + num_parameters] != parameters
        {
            return None;
        }
        let reads = Self::HEADER_WORDS + num_parameters;
        for i in 0..num_reads {
            if data_ram[record[reads + i * 2] as usize] != record[reads + i * 2 + 1] {
                return None;
            }
        }
        let data_begin = offset + reads + num_reads * 2;
        let writes_begin = data_begin + record[3] as usize * 2;
        Some((
            data_begin..writes_begin,
            writes_begin..writes_begin + record[4] as usize * 2,
        ))
    }

    pub fn store(
        &mut self,
        start: u32,
        parameters: &[u32],
        data_ram_reads: &[(u32, u32)],
        data_ram_writes: &[(u32, u32)],
        writes: &[(u32, u32)],
    ) {
        self.statistics.stores = self.statistics.stores.wrapping_add(1);
        let hash = Self::hash(start, parameters);
        if let Some(&offset) = self.entries.get(&hash) {
            let record = &mut self.arena[offset as usize..];
            if record[0] == start
                && record[1] as usize == parameters.len()
                && record[2] as usize == data_ram_reads.len()
                && record[3] as usize == data_ram_writes.len()
                && record[4] as usize == writes.len()
                && &record[Self::HEADER_WORDS..Self::HEADER_WORDS + parameters.len()] == parameters
            {
                let mut out = Self::HEADER_WORDS + parameters.len();
                for pairs in [data_ram_reads, data_ram_writes, writes] {
                    for &(address, value) in pairs {
                        record[out] = address;
                        record[out + 1] = value;
                        out += 2;
                    }
                }
                self.statistics.overwrites = self.statistics.overwrites.wrapping_add(1);
                return;
            }
        }
        if self.arena.len() >= Self::MAX_ARENA_WORDS {
            self.compact();
        }
        let offset = self.arena.len() as u32;
        self.arena.extend_from_slice(&[
            start,
            parameters.len() as u32,
            data_ram_reads.len() as u32,
            data_ram_writes.len() as u32,
            writes.len() as u32,
        ]);
        self.arena.extend_from_slice(parameters);
        for pairs in [data_ram_reads, data_ram_writes, writes] {
            for &(address, value) in pairs {
                self.arena.extend_from_slice(&[address, value]);
            }
        }
        self.entries.insert(hash, offset);
        self.last_by_start[start as usize % 256] = (start, offset);
    }

    pub fn get_statistics(&self) -> &Statistics {
        &self.statistics
    }
    pub fn size(&self) -> usize {
        self.entries.len()
    }
    pub fn arena_words(&self) -> usize {
        self.arena.len()
    }

    pub fn synchronize(&mut self, code_generation: u64) {
        if code_generation != self.generation {
            self.generation = code_generation;
            self.clear();
        }
    }

    fn hash(start: u32, parameters: &[u32]) -> u64 {
        let mut hash = 0x9e3779b97f4a7c15u64 ^ ((start as u64) << 32) ^ parameters.len() as u64;
        for &word in parameters {
            hash = (hash ^ word as u64).wrapping_mul(0xff51afd7ed558ccd);
            hash ^= hash >> 32;
        }
        hash
    }

    fn clear(&mut self) {
        self.arena.clear();
        self.entries.clear();
        self.last_by_start.fill((0, Self::NO_RECORD));
    }

    fn compact(&mut self) {
        self.compacted.clear();
        for offset in self.entries.values_mut() {
            let record = &self.arena[*offset as usize..];
            let words = Self::HEADER_WORDS
                + record[1] as usize
                + 2 * record[2].wrapping_add(record[3]).wrapping_add(record[4]) as usize;
            *offset = self.compacted.len() as u32;
            self.compacted.extend_from_slice(&record[..words]);
        }
        std::mem::swap(&mut self.arena, &mut self.compacted);
        self.last_by_start.fill((0, Self::NO_RECORD));
        self.statistics.compactions = self.statistics.compactions.wrapping_add(1);
        if self.arena.len() >= Self::MAX_ARENA_WORDS / 4 * 3 {
            self.clear();
            self.statistics.clears = self.statistics.clears.wrapping_add(1);
        }
    }
}
