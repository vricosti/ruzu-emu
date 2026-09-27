// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/service/
//!
//! IPC service interfaces for the AM service.

pub mod all_system_applet_proxies_service;
pub mod applet_common_functions;
pub mod application_accessor;
pub mod application_creator;
pub mod application_functions;
pub mod application_proxy;
pub mod application_proxy_service;
pub mod audio_controller;
pub mod common_state_getter;
pub mod cradle_firmware_updater;
pub mod debug_functions;
pub mod display_controller;
pub mod global_state_controller;
pub mod home_menu_functions;
pub mod library_applet_accessor;
pub mod library_applet_creator;
pub mod library_applet_proxy;
pub mod library_applet_self_accessor;
pub mod lock_accessor;
pub mod overlay_functions;
pub mod overlay_applet_proxy;
pub mod process_winding_controller;
pub mod self_controller;
pub mod storage;
pub mod storage_accessor;
pub mod system_applet_proxy;
pub mod window_controller;
