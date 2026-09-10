//! Port of zuyu/src/core/hle/service/audio/audio.h and audio.cpp
//!
//! Entry point for the Audio service module. Registers all audio service endpoints.

use std::collections::BTreeMap;

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::{
    HLERequestContext, SessionRequestHandler, SessionRequestHandlerPtr,
};
use crate::hle::service::server_manager::ServerManager;
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

pub struct IAudioOutManagerForApplet {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAudioOutManagerForApplet {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "RequestSuspend"),
                (1, None, "RequestResume"),
                (2, None, "GetProcessMasterVolume"),
                (3, None, "SetProcessMasterVolume"),
                (4, None, "GetProcessRecordVolume"),
                (5, None, "SetProcessRecordVolume"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IAudioOutManagerForApplet {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "audout:a"
    }
}

impl ServiceFramework for IAudioOutManagerForApplet {
    fn get_service_name(&self) -> &str {
        "audout:a"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IAudioSnoopManager {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAudioSnoopManager {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "GetDspStatistics"),
                (1, None, "GetAppletStateSummaries"),
                (2, None, "SetDspStatisticsParameter"),
                (3, None, "GetDspStatisticsParameter"),
                (6, None, "GetDspUsage"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IAudioSnoopManager {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "auddev"
    }
}

impl ServiceFramework for IAudioSnoopManager {
    fn get_service_name(&self) -> &str {
        "auddev"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IAudioInManagerForApplet {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAudioInManagerForApplet {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "RequestSuspend"),
                (1, None, "RequestResume"),
                (2, None, "GetProcessMasterVolume"),
                (3, None, "SetProcessMasterVolume"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IAudioInManagerForApplet {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "audin:a"
    }
}

impl ServiceFramework for IAudioInManagerForApplet {
    fn get_service_name(&self) -> &str {
        "audin:a"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IAudioRendererManagerForApplet {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAudioRendererManagerForApplet {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "RequestSuspend"),
                (1, None, "RequestResume"),
                (2, None, "GetProcessMasterVolume"),
                (3, None, "SetProcessMasterVolume"),
                (4, None, "RegisterAppletResourceUserId"),
                (5, None, "UnregisterAppletResourceUserId"),
                (6, None, "GetProcessRecordVolume"),
                (7, None, "SetProcessRecordVolume"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IAudioRendererManagerForApplet {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "audren:a"
    }
}

impl ServiceFramework for IAudioRendererManagerForApplet {
    fn get_service_name(&self) -> &str {
        "audren:a"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IAudioOutManagerForDebugger {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAudioOutManagerForDebugger {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "RequestSuspend"), (1, None, "RequestResume")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IAudioOutManagerForDebugger {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "audout:d"
    }
}

impl ServiceFramework for IAudioOutManagerForDebugger {
    fn get_service_name(&self) -> &str {
        "audout:d"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IAudioInManagerForDebugger {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAudioInManagerForDebugger {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "RequestSuspend"), (1, None, "RequestResume")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IAudioInManagerForDebugger {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "audin:d"
    }
}

impl ServiceFramework for IAudioInManagerForDebugger {
    fn get_service_name(&self) -> &str {
        "audin:d"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IFinalOutputRecorderManagerForDebugger {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IFinalOutputRecorderManagerForDebugger {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "RequestSuspend"), (1, None, "RequestResume")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IFinalOutputRecorderManagerForDebugger {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "audrec:d"
    }
}

impl ServiceFramework for IFinalOutputRecorderManagerForDebugger {
    fn get_service_name(&self) -> &str {
        "audrec:d"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IAudioRendererManagerForDebugger {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAudioRendererManagerForDebugger {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[(0, None, "RequestSuspend"), (1, None, "RequestResume")]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IAudioRendererManagerForDebugger {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "audren:d"
    }
}

impl ServiceFramework for IAudioRendererManagerForDebugger {
    fn get_service_name(&self) -> &str {
        "audren:d"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IAudioSystemManagerForApplet {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAudioSystemManagerForApplet {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "RegisterAppletResourceUserId"),
                (1, None, "UnregisterAppletResourceUserId"),
                (2, None, "RequestSuspendAudio"),
                (3, None, "RequestResumeAudio"),
                (4, None, "GetAudioOutputProcessMasterVolume"),
                (5, None, "SetAudioOutputProcessMasterVolume"),
                (6, None, "GetAudioInputProcessMasterVolume"),
                (7, None, "SetAudioInputProcessMasterVolume"),
                (8, None, "GetAudioOutputProcessRecordVolume"),
                (9, None, "SetAudioOutputProcessRecordVolume"),
                (10, None, "GetAppletStateSummaries"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IAudioSystemManagerForApplet {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "aud:a"
    }
}

impl ServiceFramework for IAudioSystemManagerForApplet {
    fn get_service_name(&self) -> &str {
        "aud:a"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

pub struct IAudioSystemManagerForDebugger {
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IAudioSystemManagerForDebugger {
    pub fn new() -> Self {
        Self {
            handlers: build_handler_map(&[
                (0, None, "RequestSuspendAudioForDebug"),
                (1, None, "RequestResumeAudioForDebug"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }
}

impl SessionRequestHandler for IAudioSystemManagerForDebugger {
    fn handle_sync_request(&self, ctx: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, ctx)
    }

    fn service_name(&self) -> &str {
        "aud:d"
    }
}

impl ServiceFramework for IAudioSystemManagerForDebugger {
    fn get_service_name(&self) -> &str {
        "aud:d"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

/// Registers all audio services and runs the server.
///
/// Matches upstream `Audio::LoopProcess(Core::System& system)` in audio.cpp:
/// ```cpp
/// server_manager->RegisterNamedService("audctl", std::make_shared<IAudioController>(system));
/// server_manager->RegisterNamedService("audin:u", std::make_shared<IAudioInManager>(system));
/// server_manager->RegisterNamedService("audout:u", std::make_shared<IAudioOutManager>(system));
/// server_manager->RegisterNamedService("audrec:a", std::make_shared<IFinalOutputRecorderManagerForApplet>(system));
/// server_manager->RegisterNamedService("audrec:u", std::make_shared<IFinalOutputRecorderManager>(system));
/// server_manager->RegisterNamedService("audren:u", std::make_shared<IAudioRendererManager>(system));
/// server_manager->RegisterNamedService("hwopus", std::make_shared<IHardwareOpusDecoderManager>(system));
/// ```
pub fn loop_process(system: crate::core::SystemRef) {
    let server_manager = ServerManager::new_shared(system);

    {
        let mut server_manager = server_manager.lock().unwrap();

        server_manager.register_named_service(
            "aud:a",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IAudioSystemManagerForApplet::new())
            }),
            16,
        );
        server_manager.register_named_service(
            "aud:d",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IAudioSystemManagerForDebugger::new())
            }),
            16,
        );
        server_manager.register_named_service(
            "audout:d",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IAudioOutManagerForDebugger::new())
            }),
            16,
        );
        server_manager.register_named_service(
            "audin:d",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IAudioInManagerForDebugger::new())
            }),
            16,
        );
        server_manager.register_named_service(
            "audrec:d",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IFinalOutputRecorderManagerForDebugger::new())
            }),
            16,
        );
        // This surprising factory is literal upstream behavior: `audren:d`
        // is registered with IAudioInManager, not IAudioRendererManagerForDebugger.
        server_manager.register_named_service(
            "audren:d",
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(super::audio_in_manager::IAudioInManager::new(system))
            }),
            16,
        );

        server_manager.register_named_service(
            "audin:u",
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(super::audio_in_manager::IAudioInManager::new(system))
            }),
            16,
        );

        server_manager.register_named_service(
            "audin:a",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IAudioInManagerForApplet::new())
            }),
            16,
        );

        server_manager.register_named_service(
            "audout:u",
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(super::audio_out_manager::IAudioOutManager::new(system))
            }),
            16,
        );

        server_manager.register_named_service(
            "audout:a",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IAudioOutManagerForApplet::new())
            }),
            16,
        );
        server_manager.register_named_service(
            "auddev",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IAudioSnoopManager::new())
            }),
            16,
        );
    }
    // Construct after audout:u/audin:u, as upstream. The constructor waits for
    // set:sys: do not run it inside the SM-locked registration factory.
    let audio_controller: SessionRequestHandlerPtr =
        std::sync::Arc::new(super::audio_controller::IAudioController::new(system));
    {
        let mut server_manager = server_manager.lock().unwrap();
        server_manager.register_named_service(
            "audctl",
            Box::new(move || std::sync::Arc::clone(&audio_controller)),
            16,
        );

        server_manager.register_named_service(
            "audrec:a",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(super::final_output_recorder_manager_for_applet::IFinalOutputRecorderManagerForApplet::new())
            }),
            16,
        );

        server_manager.register_named_service(
            "audrec:u",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(
                    super::final_output_recorder_manager::IFinalOutputRecorderManager::new(),
                )
            }),
            16,
        );

        server_manager.register_named_service(
            "audren:u",
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(super::audio_renderer_manager::IAudioRendererManager::new(
                    system,
                ))
            }),
            16,
        );

        server_manager.register_named_service(
            "audren:a",
            Box::new(|| -> SessionRequestHandlerPtr {
                std::sync::Arc::new(IAudioRendererManagerForApplet::new())
            }),
            16,
        );

        server_manager.register_named_service(
            "hwopus",
            Box::new(move || -> SessionRequestHandlerPtr {
                std::sync::Arc::new(
                    super::hardware_opus_decoder_manager::IHardwareOpusDecoderManager::new(system),
                )
            }),
            16,
        );
    }

    ServerManager::run_server_shared(server_manager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_audio_service_tables_match_upstream() {
        assert_eq!(IAudioSystemManagerForApplet::new().handlers().len(), 11);
        assert_eq!(IAudioRendererManagerForApplet::new().handlers().len(), 8);
        assert_eq!(IAudioOutManagerForApplet::new().handlers().len(), 6);
        assert_eq!(IAudioSnoopManager::new().handlers().len(), 5);
    }
}
