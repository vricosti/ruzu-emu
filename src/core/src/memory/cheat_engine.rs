// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden's core/memory/cheat_engine.h and cheat_engine.cpp.
//! Cheat engine for applying game cheats via the dmnt cheat VM.

use super::dmnt_cheat_types::{CheatEntry, CheatProcessMetadata, MemoryRegionExtents};
use super::dmnt_cheat_vm::{DmntCheatVm, VmCallbacks};
use crate::core::SystemRef;
use crate::core_timing::{create_event, CoreTiming, EventType, UnscheduleEventType};
use crate::hle::kernel::svc_types::ProcessActivity;
use crate::hle::service::hid::hid_server::IHidServer;
use hid_core::hid_types::NpadButton;
use parking_lot::Mutex as ParkingMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// Cheat engine callback frequency: ~12 Hz (1000000000 / 12 ns).
const CHEAT_ENGINE_NS: Duration = Duration::from_nanos(1_000_000_000 / 12);

// ---- Helper: Extract name from cheat text ----

fn extract_name(data: &str, start_index: usize, match_char: char) -> Option<(usize, &str)> {
    let remaining = &data[start_index..];
    if let Some(end_offset) = remaining.find(match_char) {
        let name_size = end_offset;
        // Clamp to CheatDefinition::readable_name size (0x40)
        let clamped = name_size.min(0x40);
        Some((name_size, &remaining[..clamped]))
    } else {
        None
    }
}

// ---- StandardVmCallbacks ----

/// Standard VM callbacks that interact with the emulator's memory system.
/// Port of StandardVmCallbacks from cheat_engine.h/cpp.
pub struct StandardVmCallbacks {
    metadata: Arc<Mutex<CheatProcessMetadata>>,
    /// Non-owning system reference matching upstream `Core::System& m_system`.
    system: SystemRef,
}

impl StandardVmCallbacks {
    pub fn new(system: SystemRef, metadata: Arc<Mutex<CheatProcessMetadata>>) -> Self {
        Self { metadata, system }
    }

    fn is_address_in_range(&self, address: u64) -> bool {
        let metadata = self.metadata.lock().unwrap();
        let in_main = address >= metadata.main_nso_extents.base
            && address
                < metadata
                    .main_nso_extents
                    .base
                    .wrapping_add(metadata.main_nso_extents.size);
        let in_heap = address >= metadata.heap_extents.base
            && address
                < metadata
                    .heap_extents
                    .base
                    .wrapping_add(metadata.heap_extents.size);
        let in_alias = address >= metadata.alias_extents.base
            && address
                < metadata
                    .alias_extents
                    .base
                    .wrapping_add(metadata.alias_extents.size);
        let in_aslr = address >= metadata.aslr_extents.base
            && address
                < metadata
                    .aslr_extents
                    .base
                    .wrapping_add(metadata.aslr_extents.size);

        if !in_main && !in_heap && !in_alias && !in_aslr {
            log::debug!(
                "Cheat attempting to access memory at invalid address={:016X}, if this persists, \
                 the cheat may be incorrect. However, this may be normal early in execution if \
                 the game has not properly set up yet.",
                address
            );
            return false;
        }

        true
    }
}

impl VmCallbacks for StandardVmCallbacks {
    fn memory_read_unsafe(&self, address: u64, data: &mut [u8]) {
        if !self.is_address_in_range(address) {
            data.fill(0);
            return;
        }
        let Some(memory) = self
            .system
            .get()
            .current_process_arc_opt()
            .and_then(|process| process.lock().unwrap().get_memory())
        else {
            data.fill(0);
            return;
        };
        let memory = memory.access().unwrap();
        if !memory.is_valid_virtual_address(address) {
            data.fill(0);
            return;
        }
        memory.read_block(address, data);
    }

    fn memory_write_unsafe(&self, address: u64, data: &[u8]) {
        if !self.is_address_in_range(address) {
            return;
        }
        let Some(process) = self.system.get().current_process_arc_opt() else {
            return;
        };
        let Some(memory) = process.lock().unwrap().get_memory() else {
            return;
        };
        let memory = memory.access().unwrap();
        if !memory.is_valid_virtual_address(address) {
            return;
        }
        if memory.write_block(address, data) {
            drop(memory);
            let mut process = process.lock().unwrap();
            crate::arm::debug::invalidate_instruction_cache_range(
                &mut process,
                address,
                data.len() as u64,
            );
        }
    }

    fn hid_keys_down(&self) -> u64 {
        let Some(service_manager) = self.system.get().service_manager() else {
            log::warn!("Attempted to read input state, but hid is not initialized!");
            return 0;
        };
        let Some(hid) = service_manager.lock().unwrap().get_service("hid") else {
            log::warn!("Attempted to read input state, but hid is not initialized!");
            return 0;
        };
        let Some(hid) = hid.as_any().downcast_ref::<IHidServer>() else {
            log::warn!("Attempted to read input state, but hid has an unexpected type!");
            return 0;
        };
        let resource_manager = hid.get_resource_manager();
        let npad = resource_manager.lock().get_npad();
        let Some(npad) = npad else {
            log::warn!("Attempted to read input state, but applet resource is not initialized!");
            return 0;
        };
        let press_state = (npad.lock().get_and_reset_press_state() & NpadButton::ALL).bits();
        press_state
    }

    fn pause_process(&self) {
        let Some(process) = self.system.get().current_process_arc_opt() else {
            return;
        };
        let mut process = process.lock().unwrap();
        if !process.is_suspended() {
            process.set_activity(ProcessActivity::Paused);
        }
    }

    fn resume_process(&self) {
        let Some(process) = self.system.get().current_process_arc_opt() else {
            return;
        };
        let mut process = process.lock().unwrap();
        if process.is_suspended() {
            process.set_activity(ProcessActivity::Runnable);
        }
    }

    fn debug_log(&self, id: u8, value: u64) {
        log::info!(
            "Cheat triggered DebugLog: ID '{:01X}' Value '{:016X}'",
            id,
            value
        );
    }

    fn command_log(&self, data: &str) {
        let trimmed = data.strip_suffix('\n').unwrap_or(data);
        log::debug!("[DmntCheatVm]: {}", trimmed);
    }
}

// ---- CheatParser ----

/// Trait for parsing cheat text into CheatEntry lists.
/// Port of CheatParser from cheat_engine.h.
pub trait CheatParser {
    fn parse(&self, data: &str) -> Vec<CheatEntry>;
}

/// Text-based cheat parser. Port of TextCheatParser.
pub struct TextCheatParser;

impl CheatParser for TextCheatParser {
    fn parse(&self, data: &str) -> Vec<CheatEntry> {
        let mut out = vec![CheatEntry::default()];
        let mut current_entry: Option<usize> = None;

        let chars: Vec<char> = data.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            if chars[i].is_whitespace() {
                i += 1;
                continue;
            }

            if chars[i] == '{' {
                current_entry = Some(0);

                if out[0].definition.num_opcodes > 0 {
                    return vec![];
                }

                let start = i + 1;
                if let Some((name_size, name)) = extract_name(data, start, '}') {
                    let name_bytes = name.as_bytes();
                    let copy_len = name_bytes
                        .len()
                        .min(out[0].definition.readable_name.len() - 1);
                    out[0].definition.readable_name[..copy_len]
                        .copy_from_slice(&name_bytes[..copy_len]);
                    let last = out[0].definition.readable_name.len() - 1;
                    out[0].definition.readable_name[last] = 0;
                    i += name_size + 1;
                } else {
                    return vec![];
                }

                i += 1;
            } else if chars[i] == '[' {
                let idx = out.len();
                current_entry = Some(idx);
                out.push(CheatEntry::default());

                let start = i + 1;
                if let Some((name_size, name)) = extract_name(data, start, ']') {
                    let name_bytes = name.as_bytes();
                    let copy_len = name_bytes
                        .len()
                        .min(out[idx].definition.readable_name.len() - 1);
                    out[idx].definition.readable_name[..copy_len]
                        .copy_from_slice(&name_bytes[..copy_len]);
                    let last = out[idx].definition.readable_name.len() - 1;
                    out[idx].definition.readable_name[last] = 0;
                    i += name_size + 1;
                } else {
                    return vec![];
                }

                i += 1;
            } else if chars[i].is_ascii_hexdigit() {
                let entry_idx = match current_entry {
                    Some(idx) => idx,
                    None => return vec![],
                };

                if out[entry_idx].definition.num_opcodes as usize
                    >= out[entry_idx].definition.opcodes.len()
                {
                    return vec![];
                }

                // Read 8 hex characters
                if i + 8 > chars.len() {
                    return vec![];
                }
                let hex_str: String = chars[i..i + 8].iter().collect();
                if !hex_str.chars().all(|c| c.is_ascii_hexdigit()) {
                    return vec![];
                }

                let value = u32::from_str_radix(&hex_str, 16).unwrap_or(0);
                let num = out[entry_idx].definition.num_opcodes as usize;
                out[entry_idx].definition.opcodes[num] = value;
                out[entry_idx].definition.num_opcodes += 1;

                i += 8;
            } else {
                return vec![];
            }
        }

        out[0].enabled = out[0].definition.num_opcodes > 0;
        out[0].cheat_id = 0;

        for idx in 1..out.len() {
            out[idx].enabled = out[idx].definition.num_opcodes > 0;
            out[idx].cheat_id = idx as u32;
        }

        out
    }
}

// ---- CheatEngine ----

/// Encapsulates a CheatList and manages its interaction with the cheat VM.
/// Port of CheatEngine from cheat_engine.h/cpp.
struct CheatEngineState {
    vm: DmntCheatVm,
    metadata: Arc<Mutex<CheatProcessMetadata>>,
    cheats: Vec<CheatEntry>,
    is_pending_reload: AtomicBool,
}

impl CheatEngineState {
    fn frame_callback(&mut self) {
        if self.is_pending_reload.swap(false, Ordering::AcqRel) {
            self.vm.load_program(&self.cheats);
        }

        if self.vm.get_program_size() == 0 {
            return;
        }

        let metadata = self.metadata.lock().unwrap().clone();
        self.vm.execute(&metadata);
    }
}

pub struct CheatEngine {
    state: Arc<Mutex<CheatEngineState>>,
    event: Option<Arc<ParkingMutex<EventType>>>,
    core_timing: Arc<CoreTiming>,
    system: SystemRef,
}

impl CheatEngine {
    pub fn new(system: SystemRef, cheats: Vec<CheatEntry>, build_id: &[u8; 0x20]) -> Self {
        let mut metadata = CheatProcessMetadata::default();
        metadata.main_nso_build_id = *build_id;
        let metadata = Arc::new(Mutex::new(metadata));

        let callbacks = Box::new(StandardVmCallbacks::new(system, metadata.clone()));
        let vm = DmntCheatVm::new(callbacks);

        Self {
            state: Arc::new(Mutex::new(CheatEngineState {
                vm,
                metadata,
                cheats,
                is_pending_reload: AtomicBool::new(false),
            })),
            event: None,
            core_timing: system.get().core_timing_shared(),
            system,
        }
    }

    pub fn initialize(&mut self) {
        let build_id = self
            .state
            .lock()
            .unwrap()
            .metadata
            .lock()
            .unwrap()
            .main_nso_build_id;
        let event_name = format!(
            "CheatEngine::FrameCallback::{}",
            build_id
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<String>()
        );
        let weak_state: Weak<Mutex<CheatEngineState>> = Arc::downgrade(&self.state);
        let event = create_event(
            event_name,
            Box::new(move |_time, _ns_late| {
                if let Some(state) = weak_state.upgrade() {
                    state.lock().unwrap().frame_callback();
                }
                None
            }),
        );
        self.core_timing
            .schedule_looping_event(CHEAT_ENGINE_NS, CHEAT_ENGINE_NS, &event, false);
        self.event = Some(event);

        let process = self.system.get().current_process_arc();
        let process = process.lock().unwrap();
        let page_table = &process.page_table;
        let state = self.state.lock().unwrap();
        {
            let mut metadata = state.metadata.lock().unwrap();
            metadata.process_id = process.get_process_id();
            metadata.title_id = process.get_program_id();
            metadata.heap_extents = MemoryRegionExtents {
                base: page_table.get_heap_region_start().get(),
                size: page_table.get_heap_region_size() as u64,
            };
            metadata.aslr_extents = MemoryRegionExtents {
                base: page_table.get_alias_code_region_start().get(),
                size: page_table.get_alias_code_region_size() as u64,
            };
            metadata.alias_extents = MemoryRegionExtents {
                base: page_table.get_alias_region_start().get(),
                size: page_table.get_alias_region_size() as u64,
            };
        }
        state.is_pending_reload.store(true, Ordering::Release);
    }

    pub fn set_main_memory_parameters(&mut self, main_region_begin: u64, main_region_size: u64) {
        self.state
            .lock()
            .unwrap()
            .metadata
            .lock()
            .unwrap()
            .main_nso_extents = MemoryRegionExtents {
            base: main_region_begin,
            size: main_region_size,
        };
    }

    pub fn reload(&mut self, reload_cheats: Vec<CheatEntry>) {
        let mut state = self.state.lock().unwrap();
        state.cheats = reload_cheats;
        state.is_pending_reload.store(true, Ordering::Release);
    }

    /// Called each frame (at CHEAT_ENGINE_NS intervals).
    pub fn frame_callback(&self) {
        self.state.lock().unwrap().frame_callback();
    }
}

impl Drop for CheatEngine {
    fn drop(&mut self) {
        if let Some(event) = self.event.take() {
            self.core_timing
                .unschedule_event(&event, UnscheduleEventType::Wait);
        } else {
            log::error!("~CheatEngine before event was registered");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_schedules_periodic_frame_callback() {
        let mut system = crate::core::System::new();
        let mut process = crate::hle::kernel::k_process::KProcess::new();
        process.process_id = 0x51;
        process.program_id = 0x0100_0000_0000_1000;
        system.set_current_process_arc(Arc::new(
            crate::hle::kernel::k_process::ProcessLock::from_value(process),
        ));
        let core_timing = system.core_timing_shared();
        let build_id = [0xAB; 0x20];
        let mut engine = CheatEngine::new(SystemRef::from_ref(&system), Vec::new(), &build_id);

        engine.initialize();

        assert_eq!(
            engine.event.as_ref().unwrap().lock().name,
            format!("CheatEngine::FrameCallback::{}", "AB".repeat(0x20))
        );
        assert!(engine
            .state
            .lock()
            .unwrap()
            .is_pending_reload
            .load(Ordering::Acquire));
        let metadata = engine
            .state
            .lock()
            .unwrap()
            .metadata
            .lock()
            .unwrap()
            .clone();
        assert_eq!(metadata.process_id, 0x51);
        assert_eq!(metadata.title_id, 0x0100_0000_0000_1000);

        core_timing.add_ticks(common::wall_clock::CPU_TICK_FREQ / 12 + 1);
        core_timing.advance();

        assert!(!engine
            .state
            .lock()
            .unwrap()
            .is_pending_reload
            .load(Ordering::Acquire));
    }
}
