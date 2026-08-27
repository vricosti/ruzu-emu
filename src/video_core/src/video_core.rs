// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/video_core.h` and `video_core/video_core.cpp`.

use ruzu_core::core::SystemRef;

use crate::gpu::Gpu;
use crate::renderer_base::RendererBase;

/// Creates the emulated GPU and binds the renderer selected by the frontend.
///
/// This is the Rust counterpart of `VideoCore::CreateGPU`. Concrete renderer
/// construction receives frontend-owned window/context handles, so it remains
/// in the supplied factory; the upstream lifecycle and settings ownership stay
/// here: update derived rescaling state, create the GPU, create the renderer,
/// then bind it. Returning an error drops the unbound GPU, matching upstream's
/// `gpu.reset()` failure path.
pub fn create_gpu<E>(
    system: SystemRef,
    create_renderer: impl FnOnce(
        common::settings_enums::RendererBackend,
        &Gpu,
    ) -> Result<Box<dyn RendererBase>, E>,
) -> Result<Box<Gpu>, E> {
    common::settings::update_rescaling_info(&mut common::settings::values_mut());

    let use_nvdec = *common::settings::values().nvdec_emulation.get_value()
        != common::settings_enums::NvdecEmulation::Off;
    let use_async = *common::settings::values()
        .use_asynchronous_gpu_emulation
        .get_value()
        && std::env::var_os("RUZU_DISABLE_ASYNC_GPU").is_none();
    let gpu = Box::new(Gpu::new(use_async, use_nvdec));
    gpu.set_system_ref(system);

    let renderer_backend = *common::settings::values().renderer_backend.get_value();
    let renderer = create_renderer(renderer_backend, &gpu)?;
    gpu.bind_renderer(renderer);
    Ok(gpu)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};

    use super::*;

    #[test]
    fn create_gpu_reads_settings_before_calling_renderer_factory() {
        let _settings_guard = crate::test_support::RESOLUTION_SETTINGS_MUTEX
            .lock()
            .unwrap();
        let expected_async = *common::settings::values()
            .use_asynchronous_gpu_emulation
            .get_value()
            && std::env::var_os("RUZU_DISABLE_ASYNC_GPU").is_none();
        let expected_nvdec = *common::settings::values().nvdec_emulation.get_value()
            != common::settings_enums::NvdecEmulation::Off;

        let expected_backend = *common::settings::values().renderer_backend.get_value();
        let result = create_gpu(SystemRef::null(), |renderer_backend, gpu| {
            assert_eq!(renderer_backend, expected_backend);
            assert_eq!(gpu.is_async(), expected_async);
            assert_eq!(gpu.use_nvdec(), expected_nvdec);
            Err::<Box<dyn RendererBase>, _>("renderer construction failed")
        });
        match result {
            Ok(_) => panic!("renderer construction unexpectedly succeeded"),
            Err(error) => assert_eq!(error, "renderer construction failed"),
        }
    }

    #[test]
    fn create_gpu_binds_the_renderer_before_returning() {
        let _settings_guard = crate::test_support::RESOLUTION_SETTINGS_MUTEX
            .lock()
            .unwrap();
        let system = ruzu_core::core::System::new();
        let gpu = create_gpu(SystemRef::from_ref(&system), |_, _| {
            Ok::<Box<dyn RendererBase>, ()>(Box::new(
                crate::renderer_null::renderer_null::RendererNull::new(
                    Arc::new(crate::host1x::syncpoint_manager::SyncpointManager::new()),
                    Arc::new(RwLock::new(Default::default())),
                    Arc::new(|| {}),
                    Arc::new(|| {}),
                ),
            ))
        })
        .unwrap_or_else(|()| panic!("null renderer construction failed"));

        assert!(gpu.renderer().is_some());
    }
}
