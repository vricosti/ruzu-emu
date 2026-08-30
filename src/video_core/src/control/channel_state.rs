// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/video_core/control/channel_state.h and channel_state.cpp
//!
//! Per-GPU-channel state including engine instances and memory manager.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::engines::engine_interface::EngineTypes;
use crate::engines::fermi_2d::Fermi2D;
use crate::engines::kepler_compute::KeplerCompute;
use crate::engines::kepler_memory::KeplerMemory;
use crate::engines::maxwell_3d::Maxwell3D;
use crate::engines::maxwell_dma::MaxwellDMA;
use crate::engines::nv01_timer::Nv01Timer;
use crate::memory_manager::MemoryManager;
use crate::rasterizer_interface::RasterizerInterface;

// ---------------------------------------------------------------------------
// ChannelState
// ---------------------------------------------------------------------------

/// Per-channel GPU state.
///
/// Corresponds to `Tegra::Control::ChannelState` in upstream.
/// Not `Clone` or `Copy` (matches C++ deleted copy constructor).
pub struct ChannelState {
    pub bind_id: i32,
    pub program_id: u64,

    /// 3D engine
    pub maxwell_3d: Option<Box<Maxwell3D>>,
    /// 2D engine
    pub fermi_2d: Option<Box<Fermi2D>>,
    /// Compute engine
    pub kepler_compute: Option<Box<KeplerCompute>>,
    /// DMA engine
    pub maxwell_dma: Option<Box<MaxwellDMA>>,
    /// Inline memory engine
    pub kepler_memory: Option<Box<KeplerMemory>>,
    /// NV01 timer engine
    pub nv01_timer: Option<Box<Nv01Timer>>,

    pub memory_manager: Option<Arc<Mutex<MemoryManager>>>,

    pub dma_pusher: Option<Box<crate::dma_pusher::DmaPusher>>,

    pub initialized: bool,
}

impl ChannelState {
    /// Create a new channel state with the given bind ID.
    ///
    /// Corresponds to `ChannelState::ChannelState(s32 bind_id)`.
    pub fn new(bind_id: i32) -> Self {
        Self {
            bind_id,
            program_id: 0,
            maxwell_3d: None,
            fermi_2d: None,
            kepler_compute: None,
            maxwell_dma: None,
            kepler_memory: None,
            nv01_timer: None,
            memory_manager: None,
            dma_pusher: None,
            initialized: false,
        }
    }

    /// Initialize the channel engines.
    ///
    /// Corresponds to `ChannelState::Init(Core::System&, GPU&, u64)`.
    /// Requires `memory_manager` to be set before calling.
    ///
    /// In the full implementation, this creates all engine instances
    /// (Maxwell3D, Fermi2D, KeplerCompute, MaxwellDMA, KeplerMemory)
    /// and the DMA pusher, passing them the memory manager and system.
    pub fn init(&mut self, _gpu: &crate::gpu::Gpu, program_id: u64) {
        assert!(
            self.memory_manager.is_some(),
            "memory_manager must be set before Init"
        );
        self.program_id = program_id;

        // Match Eden's Payload member construction order: engines first, then DmaPusher.
        let mut maxwell_3d = Box::new(Maxwell3D::new_with_memory_manager(Arc::clone(
            self.memory_manager.as_ref().unwrap(),
        )));
        let gpu_ptr = _gpu as *const crate::gpu::Gpu as usize;
        maxwell_3d.set_guest_memory_reader(Arc::new(move |addr, output| unsafe {
            let gpu = &*(gpu_ptr as *const crate::gpu::Gpu);
            let _ = gpu.read_guest_memory(addr, output);
        }));
        let gpu_ptr = _gpu as *const crate::gpu::Gpu as usize;
        maxwell_3d.set_guest_memory_writer(Arc::new(move |addr, data| unsafe {
            let gpu = &*(gpu_ptr as *const crate::gpu::Gpu);
            gpu.write_guest_memory(addr, data);
        }));
        let gpu_ptr = _gpu as *const crate::gpu::Gpu as usize;
        maxwell_3d.set_gpu_ticks_getter(Arc::new(move || unsafe {
            let gpu = &*(gpu_ptr as *const crate::gpu::Gpu);
            gpu.get_ticks()
        }));
        self.maxwell_3d = Some(maxwell_3d);
        self.fermi_2d = Some(Box::new(Fermi2D::new(Arc::clone(
            self.memory_manager.as_ref().unwrap(),
        ))));
        self.kepler_compute = Some(Box::new(KeplerCompute::new(Arc::clone(
            self.memory_manager.as_ref().unwrap(),
        ))));
        self.maxwell_dma = Some(Box::new(MaxwellDMA::new(Arc::clone(
            self.memory_manager.as_ref().unwrap(),
        ))));
        let kepler_memory = Box::new(KeplerMemory::new(Arc::clone(
            self.memory_manager.as_ref().unwrap(),
        )));
        self.kepler_memory = Some(kepler_memory);
        self.nv01_timer = Some(Box::new(Nv01Timer::new(Arc::clone(
            self.memory_manager.as_ref().unwrap(),
        ))));
        self.dma_pusher = Some(Box::new(crate::dma_pusher::DmaPusher::new(
            _gpu as *const crate::gpu::Gpu,
            _gpu.system_ref(),
            Arc::clone(self.memory_manager.as_ref().unwrap()),
            self as *mut ChannelState,
        )));
        self.dma_pusher
            .as_mut()
            .expect("DmaPusher must exist immediately after construction")
            .install_self_reference();

        self.bind_nvk_default_subchannels();

        self.initialized = true;
        log::debug!(
            "ChannelState::init bind_id={} program_id={:016x}",
            self.bind_id,
            self.program_id
        );
    }

    /// Match NVK/Nouveau's initial pushbuffer subchannel layout.
    ///
    /// Corresponds to Eden's anonymous `BindNvkDefaultSubchannels` helper in
    /// `channel_state.cpp`.
    fn bind_nvk_default_subchannels(&mut self) {
        const NVK_3D_SUBCHANNEL: u32 = 0;
        const NVK_COMPUTE_SUBCHANNEL: u32 = 1;
        const NVK_2D_SUBCHANNEL: u32 = 3;
        const NVK_COPY_SUBCHANNEL: u32 = 4;

        let Self {
            dma_pusher,
            maxwell_3d,
            kepler_compute,
            fermi_2d,
            maxwell_dma,
            ..
        } = self;
        let dma_pusher = dma_pusher
            .as_mut()
            .expect("DmaPusher must exist before binding NVK subchannels");
        dma_pusher.bind_subchannel(
            maxwell_3d
                .as_mut()
                .expect("Maxwell3D must exist before NVK binding")
                .as_mut(),
            NVK_3D_SUBCHANNEL,
            EngineTypes::Maxwell3D,
        );
        dma_pusher.bind_subchannel(
            kepler_compute
                .as_mut()
                .expect("KeplerCompute must exist before NVK binding")
                .as_mut(),
            NVK_COMPUTE_SUBCHANNEL,
            EngineTypes::KeplerCompute,
        );
        // Subchannel 2 is M2MF; Eden does not expose a 0x9039 engine yet.
        dma_pusher.bind_subchannel(
            fermi_2d
                .as_mut()
                .expect("Fermi2D must exist before NVK binding")
                .as_mut(),
            NVK_2D_SUBCHANNEL,
            EngineTypes::Fermi2D,
        );
        dma_pusher.bind_subchannel(
            maxwell_dma
                .as_mut()
                .expect("MaxwellDMA must exist before NVK binding")
                .as_mut(),
            NVK_COPY_SUBCHANNEL,
            EngineTypes::MaxwellDMA,
        );
    }

    /// Bind a rasterizer to all engines and the memory manager.
    ///
    /// Corresponds to `ChannelState::BindRasterizer(RasterizerInterface*)`.
    ///
    /// In the current port, this forwards the rasterizer reference through the
    /// same owner list as upstream.
    pub fn bind_rasterizer(&mut self, rasterizer: &dyn RasterizerInterface) {
        log::debug!("ChannelState::bind_rasterizer bind_id={}", self.bind_id);
        if let Some(ref mut dma) = self.dma_pusher {
            dma.bind_rasterizer(rasterizer);
        }
        if let Some(ref mut mm) = self.memory_manager {
            mm.lock().bind_rasterizer(rasterizer);
        }
        if let Some(ref mut maxwell_3d) = self.maxwell_3d {
            maxwell_3d.bind_rasterizer(rasterizer);
        }
        if let Some(ref mut fermi_2d) = self.fermi_2d {
            fermi_2d.bind_rasterizer(rasterizer);
        }
        if let Some(ref mut kepler_memory) = self.kepler_memory {
            kepler_memory.bind_rasterizer(rasterizer);
        }
        if let Some(ref mut kepler_compute) = self.kepler_compute {
            kepler_compute.bind_rasterizer(rasterizer);
        }
        if let Some(ref mut maxwell_dma) = self.maxwell_dma {
            maxwell_dma.bind_rasterizer(rasterizer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_channel_state() {
        let cs = ChannelState::new(42);
        assert_eq!(cs.bind_id, 42);
        assert_eq!(cs.program_id, 0);
        assert!(!cs.initialized);
        assert!(cs.maxwell_3d.is_none());
        assert!(cs.memory_manager.is_none());
    }

    #[test]
    fn init_binds_the_nvk_default_subchannels() {
        let gpu = crate::gpu::Gpu::new(false, false);
        let mut cs = ChannelState::new(7);
        cs.memory_manager = Some(Arc::new(Mutex::new(MemoryManager::new(1))));

        cs.init(&gpu, 0x1234);

        let dma = cs.dma_pusher.as_ref().unwrap();
        assert_eq!(
            dma.subchannel_binding_for_test(0),
            (true, EngineTypes::Maxwell3D)
        );
        assert_eq!(
            dma.subchannel_binding_for_test(1),
            (true, EngineTypes::KeplerCompute)
        );
        assert_eq!(dma.subchannel_binding_for_test(2).0, false);
        assert_eq!(
            dma.subchannel_binding_for_test(3),
            (true, EngineTypes::Fermi2D)
        );
        assert_eq!(
            dma.subchannel_binding_for_test(4),
            (true, EngineTypes::MaxwellDMA)
        );
    }
}
