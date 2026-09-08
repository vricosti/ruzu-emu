// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Counterpart of Eden core/hle/service/prepo/prepo.{h,cpp}.
//!
//! PlayReport service -- "prepo:a", "prepo:a2", "prepo:m", "prepo:s", "prepo:u".

use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::ipc_helpers::{RequestParser, ResponseBuilder};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::collections::BTreeMap;
use std::sync::Arc;

pub use crate::reporter::PlayReportType;

/// PlayReport service ("prepo:a", "prepo:a2", "prepo:m", "prepo:s", "prepo:u").
///
/// Corresponds to `PlayReport` in upstream prepo.cpp.
pub struct PlayReport {
    name: String,
    system: crate::core::SystemRef,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl PlayReport {
    pub fn new(name: &str, system: crate::core::SystemRef) -> Self {
        let handlers = build_handler_map(&[
            (
                10100,
                Some(PlayReport::save_report_old_handler),
                "SaveReportOld",
            ),
            (
                10101,
                Some(PlayReport::save_report_with_user_old_handler),
                "SaveReportWithUserOld",
            ),
            (
                10102,
                Some(PlayReport::save_report_old2_handler),
                "SaveReportOld2",
            ),
            (
                10103,
                Some(PlayReport::save_report_with_user_old2_handler),
                "SaveReportWithUserOld2",
            ),
            (
                10106,
                Some(PlayReport::save_report_new_handler),
                "SaveReport",
            ),
            (
                10107,
                Some(PlayReport::save_report_with_user_new_handler),
                "SaveReportWithUser",
            ),
            (
                10104,
                Some(|this, ctx| PlayReport::save_report_handler(this, ctx, PlayReportType::Old3)),
                "SaveReportOld3",
            ),
            (
                10105,
                Some(|this, ctx| {
                    PlayReport::save_report_with_user_handler(this, ctx, PlayReportType::Old3)
                }),
                "SaveReportWithUserOld3",
            ),
            (
                10200,
                Some(PlayReport::request_immediate_transmission_handler),
                "RequestImmediateTransmission",
            ),
            (
                10300,
                Some(PlayReport::get_transmission_status_handler),
                "GetTransmissionStatus",
            ),
            (
                10400,
                Some(PlayReport::get_system_session_id_handler),
                "GetSystemSessionId",
            ),
            (
                20100,
                Some(PlayReport::save_system_report_old_handler),
                "SaveSystemReport",
            ),
            (
                20101,
                Some(PlayReport::save_system_report_with_user_old_handler),
                "SaveSystemReportWithUser",
            ),
            (
                20102,
                Some(PlayReport::save_system_report_handler),
                "SaveSystemReport",
            ),
            (
                20103,
                Some(PlayReport::save_system_report_with_user_handler),
                "SaveSystemReportWithUser",
            ),
            (20200, None, "SetOperationMode"),
            (30100, None, "ClearStorage"),
            (30200, None, "ClearStatistics"),
            (30300, None, "GetStorageUsage"),
            (30400, None, "GetStatistics"),
            (30401, None, "GetThroughputHistory"),
            (30500, None, "GetLastUploadError"),
            (30600, None, "GetApplicationUploadSummary"),
            (40100, None, "IsUserAgreementCheckEnabled"),
            (40101, None, "SetUserAgreementCheckEnabled"),
            (50100, None, "ReadAllApplicationReportFiles"),
            (90100, None, "ReadAllReportFiles"),
            (90101, None, "Unknown90101"),
            (90102, None, "Unknown90102"),
            (90200, None, "GetStatistics"),
            (90201, None, "GetThroughputHistory"),
            (90300, None, "GetLastUploadError"),
        ]);

        Self {
            name: name.to_string(),
            system,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    /// SaveReport -- saves a play report of the given type.
    ///
    /// Corresponds to `PlayReport::SaveReport<Type>` in upstream prepo.cpp.
    pub fn save_report(
        &self,
        report_type: PlayReportType,
        title_id: u64,
        process_id: u64,
        data1: &[u8],
        data2: &[u8],
    ) {
        log::debug!(
            "PlayReport({})::save_report called, type={:02X}, process_id={:016X}, data1_size={:016X}, data2_size={:016X}",
            self.name,
            report_type as u8,
            process_id,
            data1.len(),
            data2.len()
        );
        let data = [data1, data2];
        self.system.get_reporter().save_play_report(
            report_type,
            title_id,
            &data,
            Some(process_id),
            None,
        );
    }

    /// SaveReportWithUser -- saves a play report with a user ID.
    ///
    /// Corresponds to `PlayReport::SaveReportWithUser<Type>` in upstream prepo.cpp.
    pub fn save_report_with_user(
        &self,
        report_type: PlayReportType,
        title_id: u64,
        user_id: u128,
        process_id: u64,
        data1: &[u8],
        data2: &[u8],
    ) {
        log::debug!(
            "PlayReport({})::save_report_with_user called, type={:02X}, user_id={:032X}, process_id={:016X}, data1_size={:016X}, data2_size={:016X}",
            self.name,
            report_type as u8,
            user_id,
            process_id,
            data1.len(),
            data2.len()
        );
        let data = [data1, data2];
        self.system.get_reporter().save_play_report(
            report_type,
            title_id,
            &data,
            Some(process_id),
            Some(user_id),
        );
    }

    /// RequestImmediateTransmission (cmd 10200).
    ///
    /// Corresponds to `PlayReport::RequestImmediateTransmission` in upstream prepo.cpp.
    pub fn request_immediate_transmission(&self) {
        log::warn!("(STUBBED) PlayReport::request_immediate_transmission called");
    }

    /// GetTransmissionStatus (cmd 10300).
    ///
    /// Corresponds to `PlayReport::GetTransmissionStatus` in upstream prepo.cpp.
    pub fn get_transmission_status(&self) -> i32 {
        log::warn!("(STUBBED) PlayReport::get_transmission_status called");
        0
    }

    /// GetSystemSessionId (cmd 10400).
    ///
    /// Corresponds to `PlayReport::GetSystemSessionId` in upstream prepo.cpp.
    pub fn get_system_session_id(&self) -> u64 {
        log::warn!("(STUBBED) PlayReport::get_system_session_id called");
        0
    }

    /// SaveSystemReport (cmd 20102).
    ///
    /// Corresponds to `PlayReport::SaveSystemReport` in upstream prepo.cpp.
    pub fn save_system_report(&self, title_id: u64, data1: &[u8], data2: &[u8]) {
        log::debug!(
            "PlayReport({})::save_system_report called, title_id={:016X}, data1_size={:016X}, data2_size={:016X}",
            self.name,
            title_id,
            data1.len(),
            data2.len()
        );
        let data = [data1, data2];
        self.system.get_reporter().save_play_report(
            PlayReportType::System,
            title_id,
            &data,
            None,
            None,
        );
    }

    /// SaveSystemReportWithUser (cmd 20101 and 20103).
    ///
    /// Corresponds to `PlayReport::SaveSystemReportWithUser` in upstream prepo.cpp.
    pub fn save_system_report_with_user(
        &self,
        user_id: u128,
        title_id: u64,
        data1: &[u8],
        data2: &[u8],
    ) {
        log::debug!(
            "PlayReport({})::save_system_report_with_user called, user_id={:032X}, title_id={:016X}, data1_size={:016X}, data2_size={:016X}",
            self.name,
            user_id,
            title_id,
            data1.len(),
            data2.len()
        );
        let data = [data1, data2];
        self.system.get_reporter().save_play_report(
            PlayReportType::System,
            title_id,
            &data,
            None,
            Some(user_id),
        );
    }

    // --- Handler bridge functions ---

    // Runtime enum arguments replace C++ SaveReport<Type> instantiations.
    // Both parsing paths remain in the upstream owner.
    fn save_report_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
        report_type: PlayReportType,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const PlayReport) };
        let mut rp = RequestParser::new(ctx);
        let process_id = rp.pop_u64();
        let data1 = ctx.read_buffer_a(0);
        let data2 = ctx.read_buffer_x(0);
        let title_id = service.system.get().get_application_process_program_id();
        service.save_report(report_type, title_id, process_id, &data1, &data2);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn save_report_with_user_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
        report_type: PlayReportType,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const PlayReport) };
        let mut rp = RequestParser::new(ctx);
        let user_id = rp.pop_raw::<u128>();
        let process_id = rp.pop_u64();
        let data1 = ctx.read_buffer_a(0);
        let data2 = ctx.read_buffer_x(0);
        let title_id = service.system.get().get_application_process_program_id();
        service.save_report_with_user(report_type, title_id, user_id, process_id, &data1, &data2);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn save_report_old_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::save_report_handler(this, ctx, PlayReportType::Old);
    }

    fn save_report_with_user_old_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::save_report_with_user_handler(this, ctx, PlayReportType::Old);
    }

    fn save_report_old2_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::save_report_handler(this, ctx, PlayReportType::Old2);
    }

    fn save_report_with_user_old2_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        Self::save_report_with_user_handler(this, ctx, PlayReportType::Old2);
    }

    fn save_report_new_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::save_report_handler(this, ctx, PlayReportType::New);
    }

    fn save_report_with_user_new_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::save_report_with_user_handler(this, ctx, PlayReportType::New);
    }

    fn save_system_report_old_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const PlayReport) };
        let mut rp = RequestParser::new(ctx);
        let title_id = rp.pop_u64();

        let data1 = ctx.read_buffer_a(0);
        let data2 = ctx.read_buffer_x(0);

        log::debug!(
            "PlayReport({})::SaveSystemReport called, title_id={:016X}, data1_size={:016X}, data2_size={:016X}",
            service.name, title_id, data1.len(), data2.len()
        );

        // SaveSystemReportOld only logs upstream; it does not save a report.

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn save_system_report_with_user_old_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const PlayReport) };
        let mut rp = RequestParser::new(ctx);
        let user_id = rp.pop_raw::<u128>();
        let title_id = rp.pop_u64();

        let data1 = ctx.read_buffer_a(0);
        let data2 = ctx.read_buffer_x(0);

        log::debug!(
            "PlayReport({})::SaveSystemReportWithUser called, user_id={:032X}, title_id={:016X}, data1_size={:016X}, data2_size={:016X}",
            service.name, user_id, title_id, data1.len(), data2.len()
        );

        service.save_system_report_with_user(user_id, title_id, &data1, &data2);

        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn save_system_report_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const PlayReport) };
        let mut rp = RequestParser::new(ctx);
        let field0 = rp.pop_u64();
        let title_id = rp.pop_u64();
        let data_x = ctx.read_buffer_x(0);
        let data_a = ctx.read_buffer_a(0);
        log::debug!("SaveSystemReport field0={field0:016X}");
        service.save_system_report(title_id, &data_a, &data_x);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn save_system_report_with_user_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const PlayReport) };
        let mut rp = RequestParser::new(ctx);
        let field0 = rp.pop_u64();
        let user_id = rp.pop_raw::<u128>();
        let title_id = rp.pop_u64();
        let data_x = ctx.read_buffer_x(0);
        let data_a = ctx.read_buffer_a(0);
        log::debug!("SaveSystemReportWithUser field0={field0:016X}");
        service.save_system_report_with_user(user_id, title_id, &data_a, &data_x);
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn request_immediate_transmission_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const PlayReport) };
        service.request_immediate_transmission();
        let mut rb = ResponseBuilder::new(ctx, 2, 0, 0);
        rb.push_result(RESULT_SUCCESS);
    }

    fn get_transmission_status_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const PlayReport) };
        let status = service.get_transmission_status();
        let mut rb = ResponseBuilder::new(ctx, 3, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_i32(status);
    }

    fn get_system_session_id_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = unsafe { &*(this as *const dyn ServiceFramework as *const PlayReport) };
        let id = service.get_system_session_id();
        let mut rb = ResponseBuilder::new(ctx, 4, 0, 0);
        rb.push_result(RESULT_SUCCESS);
        rb.push_u64(id);
    }
}

impl SessionRequestHandler for PlayReport {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }
    fn service_name(&self) -> &str {
        &self.name
    }
}

impl ServiceFramework for PlayReport {
    fn get_service_name(&self) -> &str {
        &self.name
    }
    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }
    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Registers "prepo:a", "prepo:a2", "prepo:m", "prepo:s", "prepo:u" services.
///
/// Corresponds to `LoopProcess` in upstream prepo.cpp.
pub fn loop_process(system: crate::core::SystemRef) {
    use crate::hle::service::hle_ipc::SessionRequestHandlerPtr;
    use crate::hle::service::server_manager::ServerManager;

    log::debug!("PlayReport::LoopProcess called");

    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();
        for &name in &["prepo:a", "prepo:a2", "prepo:m", "prepo:s", "prepo:u"] {
            let n = name.to_string();
            server_manager.register_named_service(
                name,
                Box::new(move || -> SessionRequestHandlerPtr {
                    Arc::new(PlayReport::new(&n, system))
                }),
                64,
            );
        }
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_commands_preserve_versions_payloads_and_reporting_gate() {
        const CHILD: &str = "RUZU_TEST_PREPO_REPORTS";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hle::service::prepo::prepo::tests::report_commands_preserve_versions_payloads_and_reporting_gate"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                use crate::core::{System, SystemRef};
                use crate::hle::kernel::k_process::{KProcess, ProcessLock};
                use common::fs::path_util::{set_ruzu_path, RuzuPath};
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let directory =
                    std::env::temp_dir().join(format!("ruzu-prepo-{}-{nonce}", std::process::id()));
                std::fs::create_dir(&directory).unwrap();
                set_ruzu_path(RuzuPath::LogDir, &directory);
                let sdmc = directory.join("sdmc");
                std::fs::create_dir(&sdmc).unwrap();
                set_ruzu_path(RuzuPath::SDMCDir, &sdmc);
                let mut system = Box::new(System::new());
                let mut process = KProcess::new();
                process.program_id = 42;
                system.set_current_process_arc(Arc::new(ProcessLock::new(process)));
                // A cached launch identifier is not the application's process ID.
                system.set_runtime_program_id(99);
                let service = PlayReport::new("prepo:u", SystemRef::from_ref(&system));
                assert_eq!(service.handlers.len(), 32);
                for (&id, entry) in &service.handlers {
                    assert_eq!(entry.handler_callback.is_some(), id < 20200);
                }
                let reports = directory.join("play_report");
                let take_report = || -> serde_json::Value {
                    let files: Vec<_> = std::fs::read_dir(&reports)
                        .unwrap()
                        .map(|e| e.unwrap().path())
                        .collect();
                    assert_eq!(files.len(), 1);
                    let value = serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
                    std::fs::remove_file(&files[0]).unwrap();
                    value
                };
                let invoke = |id: u32, values: &[u64]| {
                    let mut ctx = HLERequestContext::new();
                    for (i, value) in values.iter().enumerate() {
                        ctx.command_buffer_mut()[2 + i * 2] = *value as u32;
                        ctx.command_buffer_mut()[3 + i * 2] = (*value >> 32) as u32;
                    }
                    service.handlers[&id].handler_callback.unwrap()(&service, &mut ctx);
                    // Six CMIF header/padding words followed by Result (u64).
                    assert_eq!(ctx.write_size, 8);
                    assert_eq!(ctx.command_buffer()[6], RESULT_SUCCESS.get_inner_value());
                };
                common::settings::values_mut()
                    .reporting_services
                    .set_value(false);
                invoke(10106, &[7]);
                assert!(!reports.exists());
                common::settings::values_mut()
                    .reporting_services
                    .set_value(true);
                for (id, kind) in [(10100, "00"), (10102, "01"), (10104, "02"), (10106, "03")] {
                    for with_user in [false, true] {
                        if with_user {
                            invoke(id + 1, &[0xAA, 0xBB, 7]);
                        } else {
                            invoke(id, &[7]);
                        }
                        let report = take_report();
                        assert_eq!(report["play_report_type"], kind);
                        assert_eq!(report["report_common"]["title_id"], "000000000000002A");
                        assert_eq!(report["play_report_process_id"], "0000000000000007");
                        assert_eq!(report["play_report_data"], serde_json::json!(["", ""]));
                        if with_user {
                            assert_eq!(
                                report["report_common"]["user_id"],
                                "00000000000000BB00000000000000AA"
                            );
                        } else {
                            assert!(report["report_common"].get("user_id").is_none());
                        }
                    }
                }
                invoke(20100, &[43]); // Upstream deliberately does not save this command.
                assert_eq!(std::fs::read_dir(&reports).unwrap().count(), 0);
                for (id, values, with_user) in [
                    (20101, vec![0xAA, 0xBB, 43], true),
                    (20102, vec![99, 43], false),
                    (20103, vec![99, 0xAA, 0xBB, 43], true),
                ] {
                    invoke(id, &values);
                    let report = take_report();
                    assert_eq!(report["play_report_type"], "04");
                    assert_eq!(report["report_common"]["title_id"], "000000000000002B");
                    assert!(report.get("play_report_process_id").is_none());
                    assert_eq!(report["play_report_data"], serde_json::json!(["", ""]));
                    if with_user {
                        assert_eq!(
                            report["report_common"]["user_id"],
                            "00000000000000BB00000000000000AA"
                        );
                    } else {
                        assert!(report["report_common"].get("user_id").is_none());
                    }
                }
                service.save_report(PlayReportType::New, 42, 7, &[0xAB, 0xCD], &[]);
                assert_eq!(
                    take_report()["play_report_data"],
                    serde_json::json!(["ABCD", ""])
                );
                service.save_report_with_user(PlayReportType::Old3, 42, 1, 7, &[0xEF], &[0xAB]);
                assert_eq!(
                    take_report()["play_report_data"],
                    serde_json::json!(["EF", "AB"])
                );
                common::settings::values_mut()
                    .reporting_services
                    .set_value(false);
                service.save_system_report(43, &[0xAB], &[]);
                assert_eq!(std::fs::read_dir(&reports).unwrap().count(), 0);
                drop(service);
                drop(system);
                std::fs::remove_dir_all(directory).unwrap();
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
