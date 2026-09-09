// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of hid_core/hidbus/stubbed.h and stubbed.cpp

use super::hidbus_base::HidbusBase;

const DEVICE_ID: u8 = 0xFF;

pub struct HidbusStubbed {
    base: HidbusBase,
}

impl HidbusStubbed {
    pub fn new(runtime: std::sync::Arc<dyn super::hidbus_base::HidbusRuntime>) -> Self {
        Self {
            base: HidbusBase::new(runtime),
        }
    }

    pub fn on_init(&mut self) {
        // No initialization needed for stubbed device
    }

    pub fn on_release(&mut self) {
        // No release needed for stubbed device
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

        log::error!("Polling mode not supported {:?}", self.base.polling_mode);
    }

    pub fn get_device_id(&self) -> u8 {
        DEVICE_ID
    }

    pub fn set_command(&mut self, _data: &[u8]) -> bool {
        log::error!("Command not implemented");
        false
    }

    pub fn get_reply(&self, _out_data: &mut [u8]) -> u64 {
        0
    }

    pub fn base(&self) -> &HidbusBase {
        &self.base
    }

    pub fn base_mut(&mut self) -> &mut HidbusBase {
        &mut self.base
    }
}

impl super::hidbus_base::HidbusDevice for HidbusStubbed {
    fn base(&self) -> &HidbusBase { &self.base }
    fn base_mut(&mut self) -> &mut HidbusBase { &mut self.base }
    fn on_init(&mut self) { HidbusStubbed::on_init(self); }
    fn on_release(&mut self) { HidbusStubbed::on_release(self); }
    fn on_update(&mut self) { HidbusStubbed::on_update(self); }
    fn get_device_id(&self) -> u8 { HidbusStubbed::get_device_id(self) }
    fn set_command(&mut self, data: &[u8]) -> bool { HidbusStubbed::set_command(self, data) }
    fn get_reply(&self, data: &mut [u8]) -> u64 { HidbusStubbed::get_reply(self, data) }
}
