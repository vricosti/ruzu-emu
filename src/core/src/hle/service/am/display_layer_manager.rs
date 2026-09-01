// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/am/display_layer_manager.h
//! Port of zuyu/src/core/hle/service/am/display_layer_manager.cpp

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::core::SystemRef;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::vi::application_display_service::IApplicationDisplayService;
use crate::hle::service::vi::manager_display_service::IManagerDisplayService;
use crate::hle::service::vi::manager_root_service::IManagerRootService;
use crate::hle::service::vi::vi_results;
use crate::hle::service::vi::vi_types::Policy;

use super::am_types::{AppletId, LibraryAppletMode};
use crate::hle::kernel::k_process::ProcessLock;

/// Port of DisplayLayerManager
///
/// Manages VI display layers for an applet.
pub struct DisplayLayerManager {
    display_service: Option<Arc<IApplicationDisplayService>>,
    manager_display_service: Option<Arc<IManagerDisplayService>>,
    process: Option<Arc<ProcessLock>>,
    managed_display_layers: BTreeSet<u64>,
    managed_display_recording_layers: BTreeSet<u64>,
    system_shared_buffer_id: u64,
    system_shared_layer_id: u64,
    applet_id: AppletId,
    buffer_sharing_enabled: bool,
    blending_enabled: bool,
    visible: bool,
}

impl Default for DisplayLayerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayLayerManager {
    pub fn new() -> Self {
        Self {
            display_service: None,
            manager_display_service: None,
            process: None,
            managed_display_layers: BTreeSet::new(),
            managed_display_recording_layers: BTreeSet::new(),
            system_shared_buffer_id: 0,
            system_shared_layer_id: 0,
            applet_id: AppletId::default(),
            buffer_sharing_enabled: false,
            blending_enabled: false,
            visible: true,
        }
    }

    fn default_display_name() -> [u8; 0x40] {
        let mut display_name = [0u8; 0x40];
        display_name[..7].copy_from_slice(b"Default");
        display_name
    }

    pub fn initialize(
        &mut self,
        system: SystemRef,
        process: Arc<ProcessLock>,
        applet_id: AppletId,
        mode: LibraryAppletMode,
    ) {
        self.display_service = None;
        self.manager_display_service = None;

        if let Some(service_manager) = system.get().service_manager() {
            let manager_root = crate::hle::service::sm::sm::ServiceManager::get_service_blocking(
                &service_manager,
                system,
                "vi:m",
            );
            let manager_root = manager_root
                .as_any()
                .downcast_ref::<IManagerRootService>()
                .expect("vi:m must be IManagerRootService");
            if let Ok(display_service) = manager_root.create_display_service(Policy::Compositor) {
                self.manager_display_service = Some(display_service.get_manager_display_service());
                self.display_service = Some(display_service);
            }
        }

        self.process = Some(process);
        self.system_shared_buffer_id = 0;
        self.system_shared_layer_id = 0;
        self.applet_id = applet_id;
        self.buffer_sharing_enabled = false;
        self.blending_enabled = mode == LibraryAppletMode::PartialForeground
            || mode == LibraryAppletMode::PartialForegroundIndirectDisplay;
    }

    pub fn finalize(&mut self) {
        if let Some(manager_display_service) = self.manager_display_service.as_ref() {
            for &layer_id in &self.managed_display_layers {
                let _ = manager_display_service.destroy_managed_layer(layer_id);
            }
            for &layer_id in &self.managed_display_recording_layers {
                let _ = manager_display_service.destroy_managed_layer(layer_id);
            }
            if self.buffer_sharing_enabled {
                if let Some(process) = self.process.as_ref() {
                    manager_display_service.destroy_shared_layer_session(process);
                }
            }
        }
        self.managed_display_layers.clear();
        self.managed_display_recording_layers.clear();
        self.manager_display_service = None;
        self.display_service = None;
        self.process = None;
    }

    pub fn create_managed_display_layer(&mut self) -> Result<u64, ResultCode> {
        let Some(display_service) = self.display_service.as_ref() else {
            return Err(vi_results::RESULT_OPERATION_FAILED);
        };
        let Some(manager_display_service) = self.manager_display_service.as_ref() else {
            return Err(vi_results::RESULT_OPERATION_FAILED);
        };
        let Some(process) = self.process.as_ref() else {
            return Err(vi_results::RESULT_OPERATION_FAILED);
        };

        let display_id = display_service.open_display(&Self::default_display_name())?;
        let layer_id = manager_display_service.create_managed_layer(
            0,
            display_id,
            process.lock().unwrap().get_process_id(),
        )?;
        manager_display_service.set_layer_visibility(self.visible, layer_id)?;

        if self.applet_id != AppletId::Application {
            let _ = manager_display_service.set_layer_blending(self.blending_enabled, layer_id);
            if self.applet_id == AppletId::OverlayDisplay {
                let _ = manager_display_service.set_layer_z_index(-1, layer_id);
                let _ = display_service
                    .get_container()
                    .set_layer_is_overlay(layer_id, true);
            } else {
                let _ = manager_display_service.set_layer_z_index(1, layer_id);
            }
        }

        self.managed_display_layers.insert(layer_id);
        Ok(layer_id)
    }

    pub fn create_managed_display_separable_layer(&mut self) -> Result<(u64, u64), ResultCode> {
        let layer_id = self.create_managed_display_layer()?;
        Ok((layer_id, 0))
    }

    pub fn is_system_buffer_sharing_enabled(&mut self) -> ResultCode {
        if self.buffer_sharing_enabled {
            return RESULT_SUCCESS;
        }

        if self.manager_display_service.is_none()
            || self.display_service.is_none()
            || self.process.is_none()
        {
            return vi_results::RESULT_OPERATION_FAILED;
        }

        if self.applet_id == AppletId::Application {
            return vi_results::RESULT_PERMISSION_DENIED;
        }

        let display_service = self.display_service.as_ref().unwrap();
        let manager_display_service = self.manager_display_service.as_ref().unwrap();
        let process = self.process.as_ref().unwrap();

        let Ok(display_id) = display_service.open_display(&Self::default_display_name()) else {
            return vi_results::RESULT_OPERATION_FAILED;
        };

        let Ok((buffer_id, layer_id)) = manager_display_service.create_shared_layer_session(
            process,
            display_id,
            self.blending_enabled,
        ) else {
            return vi_results::RESULT_OPERATION_FAILED;
        };

        self.system_shared_buffer_id = buffer_id;
        self.system_shared_layer_id = layer_id;
        self.buffer_sharing_enabled = true;
        let _ =
            manager_display_service.set_layer_visibility(self.visible, self.system_shared_layer_id);
        let _ = manager_display_service
            .set_layer_blending(self.blending_enabled, self.system_shared_layer_id);
        let initial_z = if self.applet_id == AppletId::OverlayDisplay {
            let _ = display_service
                .get_container()
                .set_layer_is_overlay(self.system_shared_layer_id, true);
            -1
        } else {
            1
        };
        let _ = manager_display_service.set_layer_z_index(initial_z, self.system_shared_layer_id);
        RESULT_SUCCESS
    }

    pub fn get_system_shared_layer_handle(&mut self) -> Result<(u64, u64), ResultCode> {
        if self.is_system_buffer_sharing_enabled().is_error() {
            return Err(vi_results::RESULT_OPERATION_FAILED);
        }
        Ok((self.system_shared_buffer_id, self.system_shared_layer_id))
    }

    pub fn set_window_visibility(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if let Some(manager_display_service) = self.manager_display_service.as_ref() {
            if self.system_shared_layer_id != 0 {
                let _ = manager_display_service
                    .set_layer_visibility(visible, self.system_shared_layer_id);
            }
            for &layer_id in &self.managed_display_layers {
                let _ = manager_display_service.set_layer_visibility(visible, layer_id);
            }
        }
    }

    pub fn get_window_visibility(&self) -> bool {
        self.visible
    }

    pub fn set_overlay_z_index(&mut self, z_index: i32) {
        let Some(manager_display_service) = self.manager_display_service.as_ref() else {
            return;
        };

        if self.system_shared_layer_id != 0 {
            let _ = manager_display_service.set_layer_z_index(z_index, self.system_shared_layer_id);
            log::info!(
                "called, shared_layer={} z={}",
                self.system_shared_layer_id,
                z_index
            );
        }

        for &layer_id in &self.managed_display_layers {
            let _ = manager_display_service.set_layer_z_index(z_index, layer_id);
            log::info!("called, managed_layer={} z={}", layer_id, z_index);
        }
    }

    pub fn write_applet_capture_buffer(&mut self) -> Result<(bool, i32), ResultCode> {
        if !self.buffer_sharing_enabled {
            return Err(vi_results::RESULT_PERMISSION_DENIED);
        }
        let Some(display_service) = self.display_service.as_ref() else {
            return Err(vi_results::RESULT_OPERATION_FAILED);
        };
        display_service
            .get_container()
            .get_shared_buffer_manager()
            .write_applet_capture_buffer()
    }
}

impl Drop for DisplayLayerManager {
    fn drop(&mut self) {
        self.finalize();
    }
}
