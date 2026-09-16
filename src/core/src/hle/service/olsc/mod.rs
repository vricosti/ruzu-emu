// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/olsc/

pub mod daemon_controller;
pub mod native_handle_holder;
pub mod olsc;
pub mod olsc_service_for_application;
pub mod olsc_service_for_system_service;
pub mod remote_storage_controller;
pub mod stopper_object;
pub mod transfer_task_list_controller;
