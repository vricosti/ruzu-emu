// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/
//! Status: Stubbed
//!
//! NS (Nintendo Shell) services: application management, platform fonts, language, etc.

pub mod account_proxy_interface;
pub mod application_manager_interface;
pub mod application_version_interface;
pub mod async_result;
pub mod content_management_interface;
pub mod develop_interface;
pub mod document_interface;
pub mod download_task_interface;
pub mod dynamic_rights_interface;
pub mod ecommerce_interface;
pub mod factory_reset_interface;
pub mod language;
pub mod ns;
pub mod ns_results;
pub mod ns_types;
pub mod platform_service_manager;
pub mod query_service;
pub mod read_only_application_control_data_interface;
pub mod read_only_application_record_interface;
pub mod service_getter_interface;
pub mod system_update_control;
pub mod system_update_interface;
pub mod vulnerability_manager_interface;
