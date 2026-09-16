// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nvnflinger/surface_flinger.h
//! Port of zuyu/src/core/hle/service/nvnflinger/surface_flinger.cpp

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::core::SystemRef;
use crate::hle::service::kernel_helpers::ServiceContext;
use crate::hle::service::nvdrv::core::container::SessionId;
use crate::hle::service::nvdrv::nvdrv::Module;
use crate::hle::service::nvdrv::nvdrv_interface::NvdrvService;
use crate::hle::service::sm::sm::ServiceManager;

use super::binder::IBinder;
use super::buffer_item_consumer::BufferItemConsumer;
use super::buffer_queue_consumer::BufferQueueConsumer;
use super::buffer_queue_core::BufferQueueCore;
use super::buffer_queue_producer::BufferQueueProducer;
use super::display::{Display, Layer, LayerStack};
use super::hardware_composer::HardwareComposer;
use super::hos_binder_driver_server::HosBinderDriverServer;
use super::hwc_layer::LayerBlending;

pub struct SurfaceFlinger {
    system: SystemRef,
    server: Arc<HosBinderDriverServer>,
    inner: Mutex<SurfaceFlingerInner>,
    service_context: Arc<Mutex<ServiceContext>>,
    nvdrv: Arc<Module>,
    disp_fd: i32,
    composer: Mutex<HardwareComposer>,
}

struct SurfaceFlingerInner {
    displays: Vec<Display>,
    layers: LayerStack,
    consumers: HashMap<i32, Arc<BufferQueueConsumer>>,
}

fn should_trace_compose() -> bool {
    std::env::var_os("RUZU_TRACE_PRESENT").is_some()
        || std::env::var_os("RUZU_TRACE_SF_COMPOSE").is_some()
        || std::env::var_os("RUZU_TRACE_SF_COMPOSE_DENSE").is_some()
}

impl SurfaceFlinger {
    pub fn new(system: SystemRef, server: Arc<HosBinderDriverServer>) -> Arc<Self> {
        let service_manager = system
            .get()
            .service_manager()
            .expect("SurfaceFlinger requires ServiceManager");
        let nvdrv_service =
            ServiceManager::get_service_blocking(&service_manager, system, "nvdrv:s");
        let nvdrv = nvdrv_service
            .as_any()
            .downcast_ref::<NvdrvService>()
            .expect("nvdrv:s handler must be NvdrvService")
            .get_module();
        let disp_fd = nvdrv.open("/dev/nvdisp_disp0", SessionId { id: 0 });

        Arc::new(Self {
            system,
            server,
            inner: Mutex::new(SurfaceFlingerInner {
                displays: Vec::new(),
                layers: LayerStack::new(),
                consumers: HashMap::new(),
            }),
            service_context: Arc::new(Mutex::new(ServiceContext::new(
                "SurfaceFlinger".to_string(),
            ))),
            nvdrv,
            disp_fd,
            composer: Mutex::new(HardwareComposer::new()),
        })
    }

    pub fn add_display(&self, display_id: u64) {
        let mut inner = self.inner.lock().unwrap();
        super::diagnostics::record_surface_flinger(
            "add_display",
            [display_id, inner.displays.len() as u64, 0, 0, 0, 0],
        );
        log::info!(
            "[SF_ADD_DISPLAY] display_id={} existing_displays={:?}",
            display_id,
            inner
                .displays
                .iter()
                .map(|d| (d.id, d.stack.layers.len()))
                .collect::<Vec<_>>()
        );
        inner.displays.push(Display::new(display_id));
    }

    pub fn remove_display(&self, display_id: u64) {
        let mut inner = self.inner.lock().unwrap();
        super::diagnostics::record_surface_flinger(
            "remove_display",
            [display_id, inner.displays.len() as u64, 0, 0, 0, 0],
        );
        log::info!(
            "[SF_REMOVE_DISPLAY] display_id={} existing_displays={:?}",
            display_id,
            inner
                .displays
                .iter()
                .map(|d| (d.id, d.stack.layers.len()))
                .collect::<Vec<_>>()
        );
        inner.displays.retain(|display| display.id != display_id);
    }

    pub fn compose_display(
        &self,
        out_swap_interval: &mut i32,
        out_compose_speed_scale: &mut f32,
        display_id: u64,
    ) -> bool {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COMPOSE_COUNT: AtomicU64 = AtomicU64::new(0);
        static NO_DISPLAY: AtomicU64 = AtomicU64::new(0);
        static NO_LAYERS: AtomicU64 = AtomicU64::new(0);
        let n = COMPOSE_COUNT.fetch_add(1, Ordering::Relaxed);
        let trace_compose = should_trace_compose();
        let inner = self.inner.lock().unwrap();
        let Some(display) = Self::find_display(&inner.displays, display_id) else {
            let c = NO_DISPLAY.fetch_add(1, Ordering::Relaxed);
            super::diagnostics::record_surface_flinger(
                "compose_no_display",
                [n, display_id, c, inner.displays.len() as u64, 0, 0],
            );
            if trace_compose && (c < 8 || c.is_power_of_two()) {
                log::info!("[SF_COMPOSE] #{} NO_DISPLAY display_id={}", n, display_id);
            }
            return false;
        };
        if std::env::var_os("RUZU_TRACE_SF_COMPOSE_DENSE").is_some() {
            log::info!(
                "[SF_COMPOSE_DENSE] self=0x{:X} #{} display_id={} layers={}",
                self as *const Self as usize,
                n,
                display_id,
                display.stack.layers.len()
            );
        }
        if !display.stack.has_layers() {
            let c = NO_LAYERS.fetch_add(1, Ordering::Relaxed);
            super::diagnostics::record_surface_flinger(
                "compose_no_layers",
                [n, display_id, c, display.stack.layers.len() as u64, 0, 0],
            );
            // Log first 8, every power-of-2, AND every 60 ticks (~1s at 60Hz
            // vsync) so we can detect post-add NO_LAYERS regressions.
            if trace_compose && (c < 8 || c.is_power_of_two() || c % 60 == 0) {
                log::info!(
                    "[SF_COMPOSE] #{} NO_LAYERS display_id={} display_layers_count={}",
                    n,
                    display_id,
                    display.stack.layers.len()
                );
            }
            return false;
        }
        if trace_compose && (n < 8 || n.is_power_of_two() || n % 60 == 0) {
            log::info!(
                "[SF_COMPOSE] #{} COMPOSING display_id={} layers={}",
                n,
                display_id,
                display.stack.layers.len()
            );
        }
        super::diagnostics::record_surface_flinger(
            "compose_layers",
            [n, display_id, display.stack.layers.len() as u64, 0, 0, 0],
        );

        let device = self
            .nvdrv
            .get_disp_device(self.disp_fd)
            .expect("nvdisp_disp0 device must be open while SurfaceFlinger lives");
        *out_swap_interval =
            self.composer
                .lock()
                .unwrap()
                .compose_locked(out_compose_speed_scale, display, &device) as i32;
        true
    }

    pub fn create_layer(&self, consumer_binder_id: i32) {
        let consumer = {
            self.inner
                .lock()
                .unwrap()
                .consumers
                .get(&consumer_binder_id)
                .cloned()
        };
        let Some(consumer) = consumer else {
            super::diagnostics::record_surface_flinger(
                "create_layer_no_consumer",
                [consumer_binder_id as i64 as u64, 0, 0, 0, 0, 0],
            );
            return;
        };
        super::diagnostics::record_surface_flinger(
            "create_layer",
            [consumer_binder_id as i64 as u64, 0, 0, 0, 0, 0],
        );
        let buffer_item_consumer = Arc::new(BufferItemConsumer::new(consumer));
        let _ = buffer_item_consumer.connect(false);

        self.inner
            .lock()
            .unwrap()
            .layers
            .layers
            .push(Arc::new(Mutex::new(Layer::new(
                buffer_item_consumer,
                consumer_binder_id,
            ))));
    }

    pub fn destroy_layer(&self, consumer_binder_id: i32) {
        self.inner
            .lock()
            .unwrap()
            .layers
            .layers
            .retain(|layer| layer.lock().unwrap().consumer_id != consumer_binder_id);
    }

    pub fn add_layer_to_display_stack(&self, display_id: u64, consumer_binder_id: i32) {
        let mut inner = self.inner.lock().unwrap();
        let layer = Self::find_layer_in_stack(&inner.layers, consumer_binder_id);
        let Some(display) = Self::find_display_mut(&mut inner.displays, display_id) else {
            super::diagnostics::record_surface_flinger(
                "add_layer_no_display",
                [display_id, consumer_binder_id as i64 as u64, 0, 0, 0, 0],
            );
            log::info!(
                "[SF_ADD_LAYER] NO_DISPLAY display_id={} consumer_id={}",
                display_id,
                consumer_binder_id
            );
            return;
        };
        let Some(layer) = layer else {
            super::diagnostics::record_surface_flinger(
                "add_layer_no_layer",
                [
                    display_id,
                    consumer_binder_id as i64 as u64,
                    display.stack.layers.len() as u64,
                    0,
                    0,
                    0,
                ],
            );
            log::info!(
                "[SF_ADD_LAYER] NO_LAYER display_id={} consumer_id={}",
                display_id,
                consumer_binder_id
            );
            return;
        };
        log::info!(
            "[SF_ADD_LAYER] SUCCESS display_id={} consumer_id={} stack_len_after_push={}",
            display_id,
            consumer_binder_id,
            display.stack.layers.len() + 1,
        );
        super::diagnostics::record_surface_flinger(
            "add_layer",
            [
                display_id,
                consumer_binder_id as i64 as u64,
                display.stack.layers.len() as u64,
                display.stack.layers.len() as u64 + 1,
                0,
                0,
            ],
        );
        display.stack.layers.push(layer);
    }

    pub fn remove_layer_from_display_stack(&self, display_id: u64, consumer_binder_id: i32) {
        let mut inner = self.inner.lock().unwrap();
        let stack_len_before = inner
            .displays
            .iter()
            .find(|d| d.id == display_id)
            .map(|d| d.stack.layers.len())
            .unwrap_or(0);
        let Some(display) = Self::find_display_mut(&mut inner.displays, display_id) else {
            log::info!(
                "[SF_REMOVE_LAYER] NO_DISPLAY display_id={} consumer_id={}",
                display_id,
                consumer_binder_id
            );
            return;
        };

        self.composer
            .lock()
            .unwrap()
            .remove_layer_locked(display, consumer_binder_id);
        display
            .stack
            .layers
            .retain(|layer| layer.lock().unwrap().consumer_id != consumer_binder_id);
        log::info!(
            "[SF_REMOVE_LAYER] display_id={} consumer_id={} stack_len_before={} stack_len_after={}",
            display_id,
            consumer_binder_id,
            stack_len_before,
            display.stack.layers.len()
        );
    }

    pub fn set_layer_visibility(&self, consumer_binder_id: i32, visible: bool) {
        if let Some(layer) = self.find_layer(consumer_binder_id) {
            layer.lock().unwrap().visible = visible;
        }
    }

    pub fn set_layer_blending(&self, consumer_binder_id: i32, blending: LayerBlending) {
        if let Some(layer) = self.find_layer(consumer_binder_id) {
            layer.lock().unwrap().blending = blending;
        }
    }

    pub fn set_layer_stack_mask(&self, consumer_binder_id: i32, layer_stack_mask: u32) {
        if let Some(layer) = self.find_layer(consumer_binder_id) {
            layer.lock().unwrap().layer_stack_mask = layer_stack_mask;
            log::debug!("Layer {} stack mask set to {:#x}", consumer_binder_id, layer_stack_mask);
        }
    }

    pub fn set_layer_is_overlay(&self, consumer_binder_id: i32, is_overlay: bool) {
        if let Some(layer) = self.find_layer(consumer_binder_id) {
            layer.lock().unwrap().is_overlay = is_overlay;
        }
    }

    pub fn find_layer(&self, consumer_binder_id: i32) -> Option<Arc<Mutex<Layer>>> {
        Self::find_layer_in_stack(&self.inner.lock().unwrap().layers, consumer_binder_id)
    }

    pub fn create_buffer_queue(&self) -> (i32, i32) {
        let core = BufferQueueCore::new();
        let consumer = Arc::new(BufferQueueConsumer::new(Arc::clone(&core)));
        let producer_binder = Arc::new(BufferQueueProducer::new(
            Arc::clone(&self.service_context),
            Arc::clone(&core),
            self.nvdrv.get_container().get_nv_map_file_handle(),
        ));
        let consumer_binder: Arc<dyn IBinder> = consumer.clone();

        let consumer_binder_id = self.server.register_binder(consumer_binder);
        let producer_binder_id = self.server.register_buffer_queue_producer(producer_binder);
        super::diagnostics::record_surface_flinger(
            "create_buffer_queue",
            [
                consumer_binder_id as i64 as u64,
                producer_binder_id as i64 as u64,
                0,
                0,
                0,
                0,
            ],
        );
        self.inner
            .lock()
            .unwrap()
            .consumers
            .insert(consumer_binder_id, consumer);
        (consumer_binder_id, producer_binder_id)
    }

    pub fn destroy_buffer_queue(&self, consumer_binder_id: i32, producer_binder_id: i32) {
        self.inner
            .lock()
            .unwrap()
            .consumers
            .remove(&consumer_binder_id);
        self.server.unregister_binder(producer_binder_id);
        self.server.unregister_binder(consumer_binder_id);
    }

    fn find_display(displays: &[Display], display_id: u64) -> Option<&Display> {
        displays.iter().find(|display| display.id == display_id)
    }

    fn find_display_mut(displays: &mut [Display], display_id: u64) -> Option<&mut Display> {
        displays.iter_mut().find(|display| display.id == display_id)
    }

    fn find_layer_in_stack(
        layers: &LayerStack,
        consumer_binder_id: i32,
    ) -> Option<Arc<Mutex<Layer>>> {
        layers.find_layer(consumer_binder_id)
    }
}

impl Drop for SurfaceFlinger {
    fn drop(&mut self) {
        let _ = self.nvdrv.close(self.disp_fd);
        let _ = self.system;
    }
}
