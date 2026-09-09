// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of hid_core/hidbus/ringcon.h and ringcon.cpp

use std::sync::Arc;

use common::input::PollingMode;
use parking_lot::Mutex;

use super::hidbus_base::{HidbusBase, JoyPollingMode};
use crate::frontend::emulated_controller::{EmulatedController, EmulatedDeviceIndex};

// These values are obtained from a real ring controller
const IDLE_VALUE: i16 = 2280;
const IDLE_DEADZONE: i16 = 120;
const RANGE: i16 = 2500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
enum RingConCommands {
    GetFirmwareVersion = 0x00020000,
    ReadId = 0x00020100,
    JoyPolling = 0x00020101,
    Unknown1 = 0x00020104,
    C020105 = 0x00020105,
    Unknown2 = 0x00020204,
    Unknown3 = 0x00020304,
    Unknown4 = 0x00020404,
    ReadUnkCal = 0x00020504,
    ReadFactoryCal = 0x00020A04,
    Unknown5 = 0x00021104,
    Unknown6 = 0x00021204,
    Unknown7 = 0x00021304,
    ReadUserCal = 0x00021A04,
    ReadRepCount = 0x00023104,
    ReadTotalPushCount = 0x00023204,
    ResetRepCount = 0x04013104,
    Unknown8 = 0x04011104,
    Unknown9 = 0x04011204,
    Unknown10 = 0x04011304,
    SaveCalData = 0x10011A04,
    Error = 0xFFFFFFFF,
}

impl RingConCommands {
    fn from_u32(v: u32) -> Self {
        match v {
            0x00020000 => Self::GetFirmwareVersion,
            0x00020100 => Self::ReadId,
            0x00020101 => Self::JoyPolling,
            0x00020104 => Self::Unknown1,
            0x00020105 => Self::C020105,
            0x00020204 => Self::Unknown2,
            0x00020304 => Self::Unknown3,
            0x00020404 => Self::Unknown4,
            0x00020504 => Self::ReadUnkCal,
            0x00020A04 => Self::ReadFactoryCal,
            0x00021104 => Self::Unknown5,
            0x00021204 => Self::Unknown6,
            0x00021304 => Self::Unknown7,
            0x00021A04 => Self::ReadUserCal,
            0x00023104 => Self::ReadRepCount,
            0x00023204 => Self::ReadTotalPushCount,
            0x04013104 => Self::ResetRepCount,
            0x04011104 => Self::Unknown8,
            0x04011204 => Self::Unknown9,
            0x04011304 => Self::Unknown10,
            0x10011A04 => Self::SaveCalData,
            _ => Self::Error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum DataValid {
    Valid = 0,
    BadCRC = 1,
    #[allow(dead_code)] // Protocol value retained for parity; Eden does not construct it either.
    Cal = 2,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct RingConFirmwareVersion {
    sub: u8,
    main: u8,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct FactoryCalibration {
    os_max: i32,
    hk_max: i32,
    zero_min: i32,
    zero_max: i32,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct CalibrationValue {
    value: i16,
    crc: u16,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct UserCalibration {
    os_max: CalibrationValue,
    hk_max: CalibrationValue,
    zero: CalibrationValue,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct RingConData {
    status: u32, // DataValid
    data: i16,
    _padding: [u8; 2],
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct FirmwareVersionReply {
    status: u32,
    firmware: RingConFirmwareVersion,
    _padding: [u8; 2],
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ReadIdReply {
    status: u32,
    id_l_x0: u16,
    id_l_x0_2: u16,
    id_l_x4: u16,
    id_h_x0: u16,
    id_h_x0_2: u16,
    id_h_x4: u16,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct Cmd020105Reply {
    status: u32,
    data: u8,
    _padding: [u8; 3],
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ReadUnkCalReply {
    status: u32,
    data: u16,
    _padding: [u8; 2],
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ReadFactoryCalReply {
    status: u32,
    calibration: FactoryCalibration,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ReadUserCalReply {
    status: u32,
    calibration: UserCalibration,
    _padding: [u8; 4],
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct GetThreeByteReply {
    status: u32,
    data: [u8; 3],
    crc: u8,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct StatusReply {
    status: u32,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct ErrorReply {
    status: u32,
    _padding: [u8; 3],
}

pub struct RingController {
    base: HidbusBase,
    input: Option<Arc<Mutex<EmulatedController>>>,
    command: RingConCommands,
    total_rep_count: u8,
    total_push_count: u8,
    device_id: u8,
    version: RingConFirmwareVersion,
    factory_calibration: FactoryCalibration,
    user_calibration: UserCalibration,
}

impl RingController {
    pub fn new(event: Box<dyn super::hidbus_base::HidbusCommandEvent>, memory: Box<dyn super::hidbus_base::HidbusMemory>) -> Self {
        Self {
            base: HidbusBase::new(event, memory),
            input: None,
            command: RingConCommands::Error,
            total_rep_count: 0,
            total_push_count: 0,
            device_id: 0x20,
            version: RingConFirmwareVersion {
                sub: 0x0,
                main: 0x2c,
            },
            factory_calibration: FactoryCalibration {
                os_max: (IDLE_VALUE + RANGE + IDLE_DEADZONE) as i32,
                hk_max: (IDLE_VALUE - RANGE - IDLE_DEADZONE) as i32,
                zero_min: (IDLE_VALUE - IDLE_DEADZONE) as i32,
                zero_max: (IDLE_VALUE + IDLE_DEADZONE) as i32,
            },
            user_calibration: UserCalibration {
                os_max: CalibrationValue {
                    value: RANGE,
                    crc: 228,
                },
                hk_max: CalibrationValue {
                    value: -RANGE,
                    crc: 239,
                },
                zero: CalibrationValue {
                    value: IDLE_VALUE,
                    crc: 225,
                },
            },
        }
    }

    pub fn new_with_input(input: Arc<Mutex<EmulatedController>>, event: Box<dyn super::hidbus_base::HidbusCommandEvent>, memory: Box<dyn super::hidbus_base::HidbusMemory>) -> Self {
        Self {
            input: Some(input),
            ..Self::new(event, memory)
        }
    }

    pub fn on_init(&mut self) {
        if let Some(input) = &self.input {
            input
                .lock()
                .set_polling_mode(EmulatedDeviceIndex::RightIndex, PollingMode::Ring);
        }
    }

    pub fn on_release(&mut self) {
        if let Some(input) = &self.input {
            input
                .lock()
                .set_polling_mode(EmulatedDeviceIndex::RightIndex, PollingMode::Active);
        }
    }

    pub fn on_update(&mut self) {
        if !self.base.is_activated {
            return;
        }
        if !self.base.device_enabled {
            return;
        }
        if !self.base.polling_mode_enabled || self.base.transfer_memory == 0 {
            return;
        }

        // Upstream TODO: increment multitasking counters from motion and sensor data.
        match self.base.polling_mode {
            JoyPollingMode::SixAxisSensorEnable => {
                let accessor = &mut self.base.enable_sixaxis_data;
                accessor.header.total_entries = 10;
                accessor.header.result = common::ResultCode::SUCCESS;

                let last_index = accessor.header.latest_entry as usize;
                let last_sampling_number = accessor.entries[last_index].sampling_number;
                accessor.header.latest_entry = (accessor.header.latest_entry + 1) % 10;

                let current_index = accessor.header.latest_entry as usize;
                let current_entry = &mut accessor.entries[current_index];
                current_entry.sampling_number = last_sampling_number + 1;
                current_entry.polling_data.sampling_number = current_entry.sampling_number;
                // End the mutable accessor borrow before reading the controller.
                let ringcon_value = self.get_sensor_value();
                let accessor = &mut self.base.enable_sixaxis_data;
                let current_entry = &mut accessor.entries[current_index];
                current_entry.polling_data.out_size = std::mem::size_of::<RingConData>() as u8;

                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        &ringcon_value as *const RingConData as *const u8,
                        std::mem::size_of::<RingConData>(),
                    )
                };
                current_entry.polling_data.data[..bytes.len()].copy_from_slice(bytes);
                // All padding is explicit and initialized by the accessor's Default.
                // Layout is checked below before publishing the upstream 0x190-byte block.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        accessor as *const _ as *const u8,
                        std::mem::size_of_val(accessor),
                    )
                };
                self.base.memory.write_block(self.base.transfer_memory, bytes);
            }
            _ => log::error!("Polling mode not supported {:?}", self.base.polling_mode),
        }
    }

    fn get_sensor_value(&self) -> RingConData {
        let force = self
            .input
            .as_ref()
            .map_or(0.0, |input| input.lock().get_ring_sensor_force().force);
        RingConData {
            status: DataValid::Valid as u32,
            data: (force * RANGE as f32) as i16 + IDLE_VALUE,
            _padding: [0; 2],
        }
    }

    pub fn get_device_id(&self) -> u8 {
        self.device_id
    }

    pub fn set_command(&mut self, data: &[u8]) -> bool {
        if data.len() < 4 {
            log::error!("Command size not supported {}", data.len());
            self.command = RingConCommands::Error;
            return false;
        }

        let cmd_raw = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        self.command = RingConCommands::from_u32(cmd_raw);

        match self.command {
            RingConCommands::GetFirmwareVersion
            | RingConCommands::ReadId
            | RingConCommands::C020105
            | RingConCommands::ReadUnkCal
            | RingConCommands::ReadFactoryCal
            | RingConCommands::ReadUserCal
            | RingConCommands::ReadRepCount
            | RingConCommands::ReadTotalPushCount => {
                assert!(data.len() == 0x4, "data.size is not 0x4 bytes");
                self.base.send_command_async_event.signal();
                true
            }
            RingConCommands::ResetRepCount => {
                assert!(data.len() == 0x4, "data.size is not 0x4 bytes");
                self.total_rep_count = 0;
                self.base.send_command_async_event.signal();
                true
            }
            RingConCommands::SaveCalData => {
                assert!(data.len() == 0x14, "data.size is not 0x14 bytes");
                // Parse SaveCalData: skip 4 bytes of command, read UserCalibration
                if data.len() >= 0x14 {
                    self.user_calibration.os_max.value = i16::from_le_bytes([data[4], data[5]]);
                    self.user_calibration.os_max.crc = u16::from_le_bytes([data[6], data[7]]);
                    self.user_calibration.hk_max.value = i16::from_le_bytes([data[8], data[9]]);
                    self.user_calibration.hk_max.crc = u16::from_le_bytes([data[10], data[11]]);
                    self.user_calibration.zero.value = i16::from_le_bytes([data[12], data[13]]);
                    self.user_calibration.zero.crc = u16::from_le_bytes([data[14], data[15]]);
                }
                self.base.send_command_async_event.signal();
                true
            }
            _ => {
                log::error!("Command not implemented {:?}", self.command);
                self.command = RingConCommands::Error;
                // Signal a reply to avoid softlocking the game
                self.base.send_command_async_event.signal();
                false
            }
        }
    }

    pub fn get_reply(&self, out_data: &mut [u8]) -> u64 {
        match self.command {
            RingConCommands::GetFirmwareVersion => self.get_firmware_version_reply(out_data),
            RingConCommands::ReadId => self.get_read_id_reply(out_data),
            RingConCommands::C020105 => self.get_c020105_reply(out_data),
            RingConCommands::ReadUnkCal => self.get_read_unk_cal_reply(out_data),
            RingConCommands::ReadFactoryCal => self.get_read_factory_cal_reply(out_data),
            RingConCommands::ReadUserCal => self.get_read_user_cal_reply(out_data),
            RingConCommands::ReadRepCount => self.get_read_rep_count_reply(out_data),
            RingConCommands::ReadTotalPushCount => self.get_read_total_push_count_reply(out_data),
            RingConCommands::ResetRepCount => self.get_reset_rep_count_reply(out_data),
            RingConCommands::SaveCalData => self.get_save_data_reply(out_data),
            _ => self.get_error_reply(out_data),
        }
    }

    fn get_firmware_version_reply(&self, out_data: &mut [u8]) -> u64 {
        let reply = FirmwareVersionReply {
            status: DataValid::Valid as u32,
            firmware: self.version,
            _padding: [0; 2],
        };
        Self::get_data(&reply, out_data)
    }

    fn get_read_id_reply(&self, out_data: &mut [u8]) -> u64 {
        // The values are hardcoded from a real joycon
        let reply = ReadIdReply {
            status: DataValid::Valid as u32,
            id_l_x0: 8,
            id_l_x0_2: 41,
            id_l_x4: 22294,
            id_h_x0: 19777,
            id_h_x0_2: 13621,
            id_h_x4: 8245,
        };
        Self::get_data(&reply, out_data)
    }

    fn get_c020105_reply(&self, out_data: &mut [u8]) -> u64 {
        let reply = Cmd020105Reply {
            status: DataValid::Valid as u32,
            data: 1,
            _padding: [0; 3],
        };
        Self::get_data(&reply, out_data)
    }

    fn get_read_unk_cal_reply(&self, out_data: &mut [u8]) -> u64 {
        let reply = ReadUnkCalReply {
            status: DataValid::Valid as u32,
            data: 0,
            _padding: [0; 2],
        };
        Self::get_data(&reply, out_data)
    }

    fn get_read_factory_cal_reply(&self, out_data: &mut [u8]) -> u64 {
        let reply = ReadFactoryCalReply {
            status: DataValid::Valid as u32,
            calibration: self.factory_calibration,
        };
        Self::get_data(&reply, out_data)
    }

    fn get_read_user_cal_reply(&self, out_data: &mut [u8]) -> u64 {
        let reply = ReadUserCalReply {
            status: DataValid::Valid as u32,
            calibration: self.user_calibration,
            _padding: [0; 4],
        };
        Self::get_data(&reply, out_data)
    }

    fn get_read_rep_count_reply(&self, out_data: &mut [u8]) -> u64 {
        let crc = Self::get_crc_value(&[self.total_rep_count, 0, 0, 0]);
        let reply = GetThreeByteReply {
            status: DataValid::Valid as u32,
            data: [self.total_rep_count, 0, 0],
            crc,
        };
        Self::get_data(&reply, out_data)
    }

    fn get_read_total_push_count_reply(&self, out_data: &mut [u8]) -> u64 {
        let crc = Self::get_crc_value(&[self.total_push_count, 0, 0, 0]);
        let reply = GetThreeByteReply {
            status: DataValid::Valid as u32,
            data: [self.total_push_count, 0, 0],
            crc,
        };
        Self::get_data(&reply, out_data)
    }

    fn get_reset_rep_count_reply(&self, out_data: &mut [u8]) -> u64 {
        self.get_read_rep_count_reply(out_data)
    }

    fn get_save_data_reply(&self, out_data: &mut [u8]) -> u64 {
        let reply = StatusReply {
            status: DataValid::Valid as u32,
        };
        Self::get_data(&reply, out_data)
    }

    fn get_error_reply(&self, out_data: &mut [u8]) -> u64 {
        let reply = ErrorReply {
            status: DataValid::BadCRC as u32,
            _padding: [0; 3],
        };
        Self::get_data(&reply, out_data)
    }

    /// Returns 8 bit redundancy check from provided data
    fn get_crc_value(data: &[u8]) -> u8 {
        let mut crc: u8 = 0;
        for &byte in data {
            let mut i: u8 = 0x80;
            while i > 0 {
                let mut bit = (crc & 0x80) != 0;
                if (byte & i) != 0 {
                    bit = !bit;
                }
                crc <<= 1;
                if bit {
                    crc ^= 0x8d;
                }
                i >>= 1;
            }
        }
        crc
    }

    /// Converts a struct to bytes and copies into out_data.
    fn get_data<T: Sized>(reply: &T, out_data: &mut [u8]) -> u64 {
        let reply_size = std::mem::size_of::<T>();
        let data_size = reply_size.min(out_data.len());
        let src = unsafe { std::slice::from_raw_parts(reply as *const T as *const u8, reply_size) };
        out_data[..data_size].copy_from_slice(&src[..data_size]);
        data_size as u64
    }

    pub fn base(&self) -> &HidbusBase {
        &self.base
    }

    pub fn base_mut(&mut self) -> &mut HidbusBase {
        &mut self.base
    }
}

impl super::hidbus_base::HidbusDevice for RingController {
    fn base(&self) -> &HidbusBase { &self.base }
    fn base_mut(&mut self) -> &mut HidbusBase { &mut self.base }
    fn on_init(&mut self) { RingController::on_init(self); }
    fn on_release(&mut self) { RingController::on_release(self); }
    fn on_update(&mut self) { RingController::on_update(self); }
    fn get_device_id(&self) -> u8 { RingController::get_device_id(self) }
    fn set_command(&mut self, data: &[u8]) -> bool { RingController::set_command(self, data) }
    fn get_reply(&self, data: &mut [u8]) -> u64 { RingController::get_reply(self, data) }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_completion_signals_all_reply_paths_and_releases_owner() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Event(Arc<AtomicUsize>);
        impl super::super::hidbus_base::HidbusCommandEvent for Event {
            fn signal(&self) { self.0.fetch_add(1, Ordering::SeqCst); }
        }
        let count = Arc::new(AtomicUsize::new(0));
        let weak = Arc::downgrade(&count);
        let mut controller = RingController::new(Box::new(Event(Arc::clone(&count))), super::super::hidbus_base::test_memory());
        assert!(!controller.set_command(&[0, 1, 2]));
        assert_eq!(count.load(Ordering::SeqCst), 0);
        for (index, command) in [RingConCommands::GetFirmwareVersion,
            RingConCommands::ResetRepCount, RingConCommands::SaveCalData,
            RingConCommands::Error].into_iter().enumerate() {
            let mut data = (command as u32).to_le_bytes().to_vec();
            if command == RingConCommands::SaveCalData { data.resize(0x14, 0); }
            assert_eq!(controller.set_command(&data), command != RingConCommands::Error);
            assert_eq!(count.load(Ordering::SeqCst), index + 1);
        }
        drop(count);
        assert!(weak.upgrade().is_some());
        drop(controller);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn sixaxis_polling_updates_the_ring_lifo_like_upstream() {
        use super::super::hidbus_base::*;
        assert_eq!(std::mem::offset_of!(JoyEnableSixAxisDataAccessor, entries), 0x30);
        assert_eq!(std::mem::offset_of!(JoyEnableSixAxisPollingEntry, polling_data), 8);
        assert_eq!(std::mem::offset_of!(JoyEnableSixAxisPollingData, sampling_number), 16);
        assert_eq!(std::mem::size_of::<JoyEnableSixAxisDataAccessor>(), 0x190);
        struct Memory(Arc<Mutex<Vec<(u64, Vec<u8>)>>>);
        impl HidbusMemory for Memory {
            fn write_block(&self, address: u64, data: &[u8]) {
                self.0.lock().push((address, data.to_vec()));
            }
        }
        let writes = Arc::new(Mutex::new(Vec::new()));
        let mut controller = RingController::new(test_command_event(), Box::new(Memory(Arc::clone(&writes))));
        controller.on_update();
        assert!(writes.lock().is_empty());
        crate::hidbus::hidbus_base::HidbusDevice::activate_device(&mut controller);
        controller.base.enable(true);
        controller
            .base
            .set_polling_mode(JoyPollingMode::SixAxisSensorEnable);
        controller.base.set_transfer_memory_address(0x1000);

        controller.on_update();

        let accessor = &controller.base.enable_sixaxis_data;
        assert_eq!(accessor.header.result, common::ResultCode::SUCCESS);
        assert_eq!(accessor.header.total_entries, 10);
        assert_eq!(accessor.header.latest_entry, 1);
        assert_eq!(accessor.entries[1].sampling_number, 1);
        assert_eq!(accessor.entries[1].polling_data.sampling_number, 1);
        assert_eq!(accessor.entries[1].polling_data.out_size, 8);
        assert_eq!(
            i16::from_ne_bytes([
                accessor.entries[1].polling_data.data[4],
                accessor.entries[1].polling_data.data[5],
            ]),
            IDLE_VALUE
        );
        for _ in 0..11 { controller.on_update(); }
        let snapshots = writes.lock();
        assert_eq!(snapshots.len(), 12);
        for (i, (address, bytes)) in snapshots.iter().enumerate() {
            assert_eq!(*address, 0x1000);
            assert_eq!(bytes.len(), 0x190);
            let sample = i as u64 + 1;
            let slot = sample as usize % 10;
            assert_eq!(u64::from_ne_bytes(bytes[0x20..0x28].try_into().unwrap()), slot as u64);
            assert_eq!(u64::from_ne_bytes(bytes[0x28..0x30].try_into().unwrap()), 10);
            let offset = 0x30 + slot * 0x20;
            assert_eq!(u64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap()), sample);
            assert_eq!(u64::from_ne_bytes(bytes[offset + 24..offset + 32].try_into().unwrap()), sample);
            assert_eq!(bytes[offset + 16], 8);
            assert!(bytes[4..0x20].iter().all(|byte| *byte == 0));
            assert!(bytes[offset + 17..offset + 24].iter().all(|byte| *byte == 0));
            // Upstream allocates eleven entries but cycles through only ten.
            assert!(bytes[0x170..].iter().all(|byte| *byte == 0));
        }
        drop(snapshots);
        controller.base.disable_polling_mode();
        controller.on_update();
        assert_eq!(writes.lock().len(), 12);
    }
}
