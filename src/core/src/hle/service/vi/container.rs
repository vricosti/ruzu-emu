// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/vi/container.h
//! Port of zuyu/src/core/hle/service/vi/container.cpp
//!
//! The Container manages displays, layers, and the binder/surface flinger
//! infrastructure. It is the central coordinator for the VI service.

use crate::hle::result::ResultCode;
use crate::hle::service::hle_ipc::SessionRequestHandler;
use crate::hle::service::nvnflinger::buffer_queue_producer::BufferQueueProducer;
use crate::hle::service::nvnflinger::hos_binder_driver::IHosBinderDriver;
use crate::hle::service::nvnflinger::hos_binder_driver_server::HosBinderDriverServer;
use crate::hle::service::nvnflinger::surface_flinger::SurfaceFlinger;
use crate::hle::service::os::event::Event;
use crate::hle::service::sm::sm::ServiceManager;
use std::sync::{Arc, Mutex};

use super::conductor::Conductor;
use super::display_list::DisplayList;
use super::layer_list::LayerList;
use super::shared_buffer_manager::SharedBufferManager;
use super::vi_results;
use super::vi_types::DisplayName;

/// Container manages displays, layers, binder driver, and surface flinger.
///
/// Upstream constructor:
/// 1. Creates displays: Default, External, Edid, Internal, Null
/// 2. Creates HosBinderDriverServer + SurfaceFlinger
/// 3. Gets nvdrv module for SharedBufferManager
/// 4. Registers all displays with SurfaceFlinger
/// 5. Creates the Conductor (vsync manager)
pub struct Container {
    inner: Mutex<ContainerInner>,
    /// Shared `dispdrv` session handler obtained from the global ServiceManager.
    binder_driver: Arc<dyn SessionRequestHandler>,
    /// Binder server — shared with the global `dispdrv` binder driver.
    server: Arc<HosBinderDriverServer>,
    /// Surface compositor shared with the global `dispdrv` binder driver.
    surface_flinger: Arc<SurfaceFlinger>,
    /// Conductor drives vsync timing. Arc<Mutex<>> because the CoreTiming
    /// callback needs a weak reference to it.
    conductor: Arc<Mutex<Conductor>>,
    /// System reference for accessing process/scheduler from service handlers.
    system: crate::core::SystemRef,
    shared_buffer_manager: Arc<SharedBufferManager>,
}

struct ContainerInner {
    displays: DisplayList,
    layers: LayerList,
    is_shut_down: bool,
}

impl Container {
    /// Create a new Container with the standard display set.
    pub fn new(system: crate::core::SystemRef) -> Arc<Self> {
        Arc::new_cyclic(|weak_self| {
            let mut displays = DisplayList::default();
            let default_name = Self::make_display_name(b"Default");
            let external_name = Self::make_display_name(b"External");
            let edid_name = Self::make_display_name(b"Edid");
            let internal_name = Self::make_display_name(b"Internal");
            let null_name = Self::make_display_name(b"Null");

            displays.create_display(&default_name);
            displays.create_display(&external_name);
            displays.create_display(&edid_name);
            displays.create_display(&internal_name);
            displays.create_display(&null_name);

            let mut display_ids = Vec::new();
            displays.for_each_display(|d| {
                display_ids.push(d.get_id());
            });

            let service_manager = system
                .get()
                .service_manager()
                .expect("Container::new: missing ServiceManager");
            let binder_driver =
                ServiceManager::get_service_blocking(&service_manager, system, "dispdrv");
            let binder_driver_impl = binder_driver
                .as_any()
                .downcast_ref::<IHosBinderDriver>()
                .expect("Container::new: dispdrv is not IHosBinderDriver");
            let server = Arc::clone(binder_driver_impl.get_server());
            let surface_flinger = binder_driver_impl.get_surface_flinger();

            let nvdrv = ServiceManager::get_service_blocking(&service_manager, system, "nvdrv:s");
            let nvdrv = nvdrv
                .as_any()
                .downcast_ref::<crate::hle::service::nvdrv::nvdrv_interface::NvdrvService>()
                .expect("Container::new: nvdrv:s is not NvdrvService")
                .get_module();
            let shared_buffer_manager =
                Arc::new(SharedBufferManager::new(system, weak_self.clone(), nvdrv));

            for &id in &display_ids {
                surface_flinger.add_display(id);
            }

            let conductor = Arc::new(Mutex::new(Conductor::new(
                system,
                &display_ids,
                Arc::clone(&surface_flinger),
            )));
            Conductor::start(&conductor);

            Self {
                inner: Mutex::new(ContainerInner {
                    displays,
                    layers: LayerList::default(),
                    is_shut_down: false,
                }),
                binder_driver,
                server,
                surface_flinger,
                conductor,
                system,
                shared_buffer_manager,
            }
        })
    }

    fn make_display_name(name: &[u8]) -> DisplayName {
        let mut display_name = [0u8; 0x40];
        let len = name.len().min(0x40);
        display_name[..len].copy_from_slice(&name[..len]);
        display_name
    }

    /// Get the binder driver service.
    /// Upstream: Container owns IHOSBinderDriver and returns it via GetBinderDriver().
    pub fn get_binder_driver(&self) -> Arc<dyn SessionRequestHandler> {
        Arc::clone(&self.binder_driver)
    }

    /// Upstream `Container::GetLayerProducerHandle(...)`.
    pub fn get_layer_producer_handle(
        &self,
        layer_id: u64,
    ) -> Result<Arc<BufferQueueProducer>, ResultCode> {
        let inner = self.inner.lock().unwrap();
        let layer = inner
            .layers
            .get_layer_by_id(layer_id)
            .ok_or(vi_results::RESULT_NOT_FOUND)?;

        let binder = self
            .server
            .try_get_buffer_queue_producer(layer.get_producer_binder_id())
            .ok_or(vi_results::RESULT_NOT_FOUND)?;

        Ok(binder)
    }

    /// Get the system reference.
    pub fn system(&self) -> crate::core::SystemRef {
        self.system
    }

    pub fn get_shared_buffer_manager(&self) -> Arc<SharedBufferManager> {
        Arc::clone(&self.shared_buffer_manager)
    }

    pub fn on_terminate(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.is_shut_down = true;

        let mut layer_ids = Vec::new();
        inner.layers.for_each_layer(|layer| {
            layer_ids.push(layer.get_id());
        });
        for layer_id in layer_ids {
            let _ = Self::destroy_layer_locked(&self.surface_flinger, &mut inner, layer_id);
        }

        let mut display_ids = Vec::new();
        inner.displays.for_each_display(|display| {
            display_ids.push(display.get_id());
        });
        for display_id in display_ids {
            self.surface_flinger.remove_display(display_id);
        }
    }

    pub fn open_display(&self, display_name: &DisplayName) -> Result<u64, ResultCode> {
        let inner = self.inner.lock().unwrap();
        match inner.displays.get_display_by_name(display_name) {
            Some(display) => Ok(display.get_id()),
            None => Err(vi_results::RESULT_NOT_FOUND),
        }
    }

    pub fn close_display(&self, _display_id: u64) -> Result<(), ResultCode> {
        Ok(())
    }

    pub fn create_managed_layer(
        &self,
        display_id: u64,
        owner_aruid: u64,
    ) -> Result<u64, ResultCode> {
        let mut inner = self.inner.lock().unwrap();
        Self::create_layer_locked(&self.surface_flinger, &mut inner, display_id, owner_aruid)
    }

    pub fn destroy_managed_layer(&self, layer_id: u64) -> Result<(), ResultCode> {
        let mut inner = self.inner.lock().unwrap();
        let _ = Self::close_layer_locked(&self.surface_flinger, &mut inner, layer_id);
        Self::destroy_layer_locked(&self.surface_flinger, &mut inner, layer_id)
    }

    pub fn open_layer(&self, layer_id: u64, aruid: u64) -> Result<i32, ResultCode> {
        let mut inner = self.inner.lock().unwrap();
        Self::open_layer_locked(&self.surface_flinger, &mut inner, layer_id, aruid)
    }

    pub fn close_layer(&self, layer_id: u64) -> Result<(), ResultCode> {
        let mut inner = self.inner.lock().unwrap();
        Self::close_layer_locked(&self.surface_flinger, &mut inner, layer_id)
    }

    pub fn create_stray_layer(&self, display_id: u64) -> Result<(i32, u64), ResultCode> {
        let mut inner = self.inner.lock().unwrap();
        let layer_id = Self::create_layer_locked(&self.surface_flinger, &mut inner, display_id, 0)?;
        let producer_binder_id =
            Self::open_layer_locked(&self.surface_flinger, &mut inner, layer_id, 0)?;
        Ok((producer_binder_id, layer_id))
    }

    pub fn destroy_stray_layer(&self, layer_id: u64) -> Result<(), ResultCode> {
        let mut inner = self.inner.lock().unwrap();
        Self::close_layer_locked(&self.surface_flinger, &mut inner, layer_id)?;
        Self::destroy_layer_locked(&self.surface_flinger, &mut inner, layer_id)
    }

    pub fn set_layer_visibility(&self, layer_id: u64, visible: bool) -> Result<(), ResultCode> {
        let inner = self.inner.lock().unwrap();
        let layer = inner
            .layers
            .get_layer_by_id(layer_id)
            .ok_or(vi_results::RESULT_NOT_FOUND)?;
        self.surface_flinger
            .set_layer_visibility(layer.get_consumer_binder_id(), visible);
        Ok(())
    }

    pub fn set_layer_blending(&self, layer_id: u64, enabled: bool) -> Result<(), ResultCode> {
        let inner = self.inner.lock().unwrap();
        let layer = inner
            .layers
            .get_layer_by_id(layer_id)
            .ok_or(vi_results::RESULT_NOT_FOUND)?;
        self.surface_flinger.set_layer_blending(
            layer.get_consumer_binder_id(),
            if enabled {
                crate::hle::service::nvnflinger::hwc_layer::LayerBlending::Coverage
            } else {
                crate::hle::service::nvnflinger::hwc_layer::LayerBlending::None
            },
        );
        Ok(())
    }

    pub fn set_layer_z_index(&self, layer_id: u64, z_index: i32) -> Result<(), ResultCode> {
        let inner = self.inner.lock().unwrap();
        let layer = inner
            .layers
            .get_layer_by_id(layer_id)
            .ok_or(vi_results::RESULT_NOT_FOUND)?;
        if let Some(layer) = self
            .surface_flinger
            .find_layer(layer.get_consumer_binder_id())
        {
            layer.lock().unwrap().z_index = z_index;
        }
        Ok(())
    }

    pub fn get_layer_z_index(&self, layer_id: u64) -> Result<i32, ResultCode> {
        let inner = self.inner.lock().unwrap();
        let layer = inner
            .layers
            .get_layer_by_id(layer_id)
            .ok_or(vi_results::RESULT_NOT_FOUND)?;
        let layer = self
            .surface_flinger
            .find_layer(layer.get_consumer_binder_id())
            .ok_or(vi_results::RESULT_NOT_FOUND)?;
        let z_index = layer.lock().unwrap().z_index;
        Ok(z_index)
    }

    pub fn set_layer_stack_mask(&self, layer_id: u64, layer_stack_mask: u32) -> Result<(), ResultCode> {
        let inner = self.inner.lock().unwrap();
        let layer = inner
            .layers
            .get_layer_by_id(layer_id)
            .ok_or(vi_results::RESULT_NOT_FOUND)?;
        self.surface_flinger
            .set_layer_stack_mask(layer.get_consumer_binder_id(), layer_stack_mask);
        Ok(())
    }

    pub fn set_layer_is_overlay(&self, layer_id: u64, is_overlay: bool) -> Result<(), ResultCode> {
        let inner = self.inner.lock().unwrap();
        let layer = inner
            .layers
            .get_layer_by_id(layer_id)
            .ok_or(vi_results::RESULT_NOT_FOUND)?;
        self.surface_flinger
            .set_layer_is_overlay(layer.get_consumer_binder_id(), is_overlay);
        Ok(())
    }

    /// Link a vsync event for a display.
    /// Port of upstream `Container::LinkVsyncEvent`.
    pub fn link_vsync_event(&self, display_id: u64, event: Arc<Event>) {
        self.conductor
            .lock()
            .unwrap()
            .link_vsync_event(display_id, event);
    }

    /// Unlink a vsync event for a display.
    /// Port of upstream `Container::UnlinkVsyncEvent`.
    pub fn unlink_vsync_event(&self, display_id: u64, event: &Arc<Event>) {
        self.conductor
            .lock()
            .unwrap()
            .unlink_vsync_event(display_id, event);
    }

    pub fn compose_on_display(
        &self,
        out_swap_interval: &mut i32,
        out_compose_speed_scale: &mut f32,
        display_id: u64,
    ) -> bool {
        let _inner = self.inner.lock().unwrap();
        self.surface_flinger
            .compose_display(out_swap_interval, out_compose_speed_scale, display_id)
    }

    fn create_layer_locked(
        surface_flinger: &Arc<SurfaceFlinger>,
        inner: &mut ContainerInner,
        display_id: u64,
        owner_aruid: u64,
    ) -> Result<u64, ResultCode> {
        if inner.displays.get_display_by_id(display_id).is_none() {
            return Err(vi_results::RESULT_NOT_FOUND);
        }

        // Create buffer queues via surface flinger (matches upstream).
        let (consumer_binder_id, producer_binder_id) = surface_flinger.create_buffer_queue();

        let layer = inner
            .layers
            .create_layer(
                owner_aruid,
                display_id,
                consumer_binder_id,
                producer_binder_id,
            )
            .ok_or(vi_results::RESULT_NOT_FOUND)?;

        surface_flinger.create_layer(consumer_binder_id);

        Ok(layer.get_id())
    }

    fn destroy_layer_locked(
        surface_flinger: &Arc<SurfaceFlinger>,
        inner: &mut ContainerInner,
        layer_id: u64,
    ) -> Result<(), ResultCode> {
        let layer = inner
            .layers
            .get_layer_by_id(layer_id)
            .ok_or(vi_results::RESULT_NOT_FOUND)?;
        let consumer_binder_id = layer.get_consumer_binder_id();
        let producer_binder_id = layer.get_producer_binder_id();

        surface_flinger.destroy_layer(consumer_binder_id);
        surface_flinger.destroy_buffer_queue(consumer_binder_id, producer_binder_id);

        inner.layers.destroy_layer(layer_id);
        Ok(())
    }

    fn open_layer_locked(
        surface_flinger: &Arc<SurfaceFlinger>,
        inner: &mut ContainerInner,
        layer_id: u64,
        aruid: u64,
    ) -> Result<i32, ResultCode> {
        if inner.is_shut_down {
            return Err(vi_results::RESULT_OPERATION_FAILED);
        }

        let layer = inner
            .layers
            .get_layer_by_id_mut(layer_id)
            .ok_or(vi_results::RESULT_NOT_FOUND)?;

        if layer.is_open() {
            return Err(vi_results::RESULT_OPERATION_FAILED);
        }

        if layer.get_owner_aruid() != aruid {
            return Err(vi_results::RESULT_PERMISSION_DENIED);
        }

        let producer_binder_id = layer.get_producer_binder_id();
        let consumer_binder_id = layer.get_consumer_binder_id();
        let display_id = layer.get_display_id();
        layer.open();
        surface_flinger.add_layer_to_display_stack(display_id, consumer_binder_id);
        Ok(producer_binder_id)
    }

    fn close_layer_locked(
        surface_flinger: &Arc<SurfaceFlinger>,
        inner: &mut ContainerInner,
        layer_id: u64,
    ) -> Result<(), ResultCode> {
        let layer = inner
            .layers
            .get_layer_by_id_mut(layer_id)
            .ok_or(vi_results::RESULT_NOT_FOUND)?;

        if !layer.is_open() {
            return Err(vi_results::RESULT_OPERATION_FAILED);
        }

        let consumer_binder_id = layer.get_consumer_binder_id();
        let display_id = layer.get_display_id();
        surface_flinger.remove_layer_from_display_stack(display_id, consumer_binder_id);
        layer.close();
        Ok(())
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        self.on_terminate();
    }
}
