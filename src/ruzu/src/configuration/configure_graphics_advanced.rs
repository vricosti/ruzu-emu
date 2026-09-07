// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/eden/src/yuzu/configuration/configure_graphics_advanced.cpp`
// (`ConfigureGraphicsAdvanced`), whose widget tree lives in
// `configure_graphics_advanced.ui`.
//
// A single "Advanced Graphics Settings" group populated in upstream setting-id
// order. Upstream's `ExposeComputeOption()` additionally reveals the
// "Enable compute pipelines" check box when the selected Vulkan driver needs it;
// the row is built here but stays hidden until that call, matching upstream's
// default state.

use gtk::prelude::*;

use super::configure_dialog::Page;
use super::shared_translation as tr;
use super::shared_widget as w;

/// The page plus upstream's `ExposeComputeOption` callback. `ConfigurePerGame`
/// passes the callback to `ConfigureGraphics`, preserving the same ownership
/// and construction order as the C++ dialog.
pub struct BuildResult {
    pub page: Page,
    pub expose_compute_option: Box<dyn Fn()>,
}

/// Build the Graphics "Advanced" tab — upstream `ConfigureGraphicsAdvanced`.
pub fn page(runtime_lock: bool) -> BuildResult {
    let (scroller, column) = w::page();

    let (group, content) = w::group("Advanced Graphics Settings");

    let accuracy_value = *common::settings::values().gpu_accuracy.get_value();
    let (accuracy_row, accuracy) = w::combo_row(
        "GPU Mode:",
        &tr::labels(tr::GPU_ACCURACY),
        tr::index_of(tr::GPU_ACCURACY, &accuracy_value),
    );
    content.append(&accuracy_row);

    let dma_value = *common::settings::values().dma_accuracy.get_value();
    let (dma_row, dma) = w::combo_row(
        "DMA Accuracy:",
        &tr::labels(tr::DMA_ACCURACY),
        tr::index_of(tr::DMA_ACCURACY, &dma_value),
    );
    content.append(&dma_row);

    let fence_behavior_value = *common::settings::values().gpu_fence_behavior.get_value();
    let (fence_behavior_row, fence_behavior) = w::combo_row(
        "GPU Fence Behavior:",
        &tr::labels(tr::GPU_FENCE_BEHAVIOR),
        tr::index_of(tr::GPU_FENCE_BEHAVIOR, &fence_behavior_value),
    );
    content.append(&fence_behavior_row);

    let vram_value = *common::settings::values().vram_usage_mode.get_value();
    let (vram_row, vram) = w::combo_row(
        "VRAM Usage Mode:",
        &tr::labels(tr::VRAM_USAGE_MODE),
        tr::index_of(tr::VRAM_USAGE_MODE, &vram_value),
    );
    content.append(&vram_row);

    let nvdec_value = *common::settings::values().nvdec_emulation.get_value();
    let (nvdec_row, nvdec) = w::combo_row(
        "NVDEC emulation:",
        &tr::labels(tr::NVDEC_EMULATION),
        tr::index_of(tr::NVDEC_EMULATION, &nvdec_value),
    );
    content.append(&nvdec_row);

    let aniso_value = *common::settings::values().max_anisotropy.get_value();
    let (aniso_row, aniso) = w::combo_row(
        "Anisotropic Filtering:",
        &tr::labels(tr::ANISOTROPY_MODE),
        tr::index_of(tr::ANISOTROPY_MODE, &aniso_value),
    );
    content.append(&aniso_row);

    let astc_value = *common::settings::values().accelerate_astc.get_value();
    let (astc_row, astc) = w::combo_row(
        "ASTC Decoding Method:",
        &tr::labels(tr::ASTC_DECODE_MODE),
        tr::index_of(tr::ASTC_DECODE_MODE, &astc_value),
    );
    content.append(&astc_row);

    let frame_pacing_value = *common::settings::values().frame_pacing_mode.get_value();
    let (frame_pacing_row, frame_pacing) = w::combo_row(
        "Frame Pacing Mode (Vulkan only)",
        &tr::labels(tr::FRAME_PACING_MODE),
        tr::index_of(tr::FRAME_PACING_MODE, &frame_pacing_value),
    );
    content.append(&frame_pacing_row);

    let recompression_value = *common::settings::values().astc_recompression.get_value();
    let (recompression_row, recompression) = w::combo_row(
        "ASTC Recompression Method:",
        &tr::labels(tr::ASTC_RECOMPRESSION),
        tr::index_of(tr::ASTC_RECOMPRESSION, &recompression_value),
    );
    content.append(&recompression_row);

    let sync_memory = w::check_row(
        "Sync Memory Operations",
        *common::settings::values()
            .sync_memory_operations
            .get_value(),
    );
    content.append(&sync_memory);

    let force_max_clock = w::check_row(
        "Force maximum clocks (Vulkan only)",
        *common::settings::values()
            .renderer_force_max_clock
            .get_value(),
    );
    content.append(&force_max_clock);

    let disk_pipeline_cache = w::check_row(
        "Use persistent pipeline cache",
        *common::settings::values().use_disk_shader_cache.get_value(),
    );
    content.append(&disk_pipeline_cache);

    let vulkan_pipeline_cache = w::check_row(
        "Use Vulkan pipeline cache",
        *common::settings::values()
            .use_vulkan_driver_pipeline_cache
            .get_value(),
    );
    content.append(&vulkan_pipeline_cache);

    // Upstream builds this row in setting-id order but leaves it hidden until
    // `ExposeComputeOption()` is called by `ConfigureGraphics` for a driver
    // that reports broken compute support.
    let compute_pipelines = w::check_row(
        "Enable compute pipelines (Intel Vulkan only)",
        *common::settings::values()
            .enable_compute_pipelines
            .get_value(),
    );
    compute_pipelines.set_visible(false);
    content.append(&compute_pipelines);

    let video_framerate = w::check_row(
        "Sync to framerate of video playback",
        *common::settings::values().use_video_framerate.get_value(),
    );
    content.append(&video_framerate);

    let reactive_flushing = w::check_row(
        "Enable Reactive Flushing",
        *common::settings::values().use_reactive_flushing.get_value(),
    );
    content.append(&reactive_flushing);

    let barrier_feedback_loops = w::check_row(
        "Barrier feedback loops",
        *common::settings::values()
            .barrier_feedback_loops
            .get_value(),
    );
    content.append(&barrier_feedback_loops);

    let buffer_history = w::check_row(
        "Enable buffer history",
        *common::settings::values().enable_buffer_history.get_value(),
    );
    content.append(&buffer_history);

    let gpu_buffer_readback = w::check_row(
        "Enable GPU buffer readback",
        *common::settings::values()
            .enable_gpu_buffer_readback
            .get_value(),
    );
    content.append(&gpu_buffer_readback);

    // Apply the same per-setting policy as Eden's shared Widget builder.
    // Snapshot before ConfigurePerGame prepares its custom values for saving.
    let configuring_global = common::settings::is_configuring_global();
    let accuracy_policy = w::SettingEditPolicy::new(
        &common::settings::values().gpu_accuracy,
        runtime_lock,
        configuring_global,
    );
    accuracy_row.set_sensitive(accuracy_policy.sensitive);
    let dma_policy = w::SettingEditPolicy::new(
        &common::settings::values().dma_accuracy,
        runtime_lock,
        configuring_global,
    );
    dma_row.set_sensitive(dma_policy.sensitive);
    let fence_behavior_policy = w::SettingEditPolicy::new(
        &common::settings::values().gpu_fence_behavior,
        runtime_lock,
        configuring_global,
    );
    fence_behavior_row.set_sensitive(fence_behavior_policy.sensitive);
    let vram_policy = w::SettingEditPolicy::new(
        &common::settings::values().vram_usage_mode,
        runtime_lock,
        configuring_global,
    );
    vram_row.set_sensitive(vram_policy.sensitive);
    let nvdec_policy = w::SettingEditPolicy::new(
        &common::settings::values().nvdec_emulation,
        runtime_lock,
        configuring_global,
    );
    nvdec_row.set_sensitive(nvdec_policy.sensitive);
    let aniso_policy = w::SettingEditPolicy::new(
        &common::settings::values().max_anisotropy,
        runtime_lock,
        configuring_global,
    );
    aniso_row.set_sensitive(aniso_policy.sensitive);
    let astc_policy = w::SettingEditPolicy::new(
        &common::settings::values().accelerate_astc,
        runtime_lock,
        configuring_global,
    );
    astc_row.set_sensitive(astc_policy.sensitive);
    let frame_pacing_policy = w::SettingEditPolicy::new(
        &common::settings::values().frame_pacing_mode,
        runtime_lock,
        configuring_global,
    );
    frame_pacing_row.set_sensitive(frame_pacing_policy.sensitive);
    let recompression_policy = w::SettingEditPolicy::new(
        &common::settings::values().astc_recompression,
        runtime_lock,
        configuring_global,
    );
    recompression_row.set_sensitive(recompression_policy.sensitive);
    let sync_memory_policy = w::SettingEditPolicy::new(
        &common::settings::values().sync_memory_operations,
        runtime_lock,
        configuring_global,
    );
    sync_memory.set_sensitive(sync_memory_policy.sensitive);
    let max_clock_policy = w::SettingEditPolicy::new(
        &common::settings::values().renderer_force_max_clock,
        runtime_lock,
        configuring_global,
    );
    force_max_clock.set_sensitive(max_clock_policy.sensitive);
    let disk_cache_policy = w::SettingEditPolicy::new(
        &common::settings::values().use_disk_shader_cache,
        runtime_lock,
        configuring_global,
    );
    disk_pipeline_cache.set_sensitive(disk_cache_policy.sensitive);
    let pipeline_cache_policy = w::SettingEditPolicy::new(
        &common::settings::values().use_vulkan_driver_pipeline_cache,
        runtime_lock,
        configuring_global,
    );
    vulkan_pipeline_cache.set_sensitive(pipeline_cache_policy.sensitive);
    let compute_policy = w::SettingEditPolicy::new(
        &common::settings::values().enable_compute_pipelines,
        runtime_lock,
        configuring_global,
    );
    compute_pipelines.set_sensitive(compute_policy.sensitive);
    let framerate_policy = w::SettingEditPolicy::new(
        &common::settings::values().use_video_framerate,
        runtime_lock,
        configuring_global,
    );
    video_framerate.set_sensitive(framerate_policy.sensitive);
    let reactive_policy = w::SettingEditPolicy::new(
        &common::settings::values().use_reactive_flushing,
        runtime_lock,
        configuring_global,
    );
    reactive_flushing.set_sensitive(reactive_policy.sensitive);
    let barriers_policy = w::SettingEditPolicy::new(
        &common::settings::values().barrier_feedback_loops,
        runtime_lock,
        configuring_global,
    );
    barrier_feedback_loops.set_sensitive(barriers_policy.sensitive);
    let history_policy = w::SettingEditPolicy::new(
        &common::settings::values().enable_buffer_history,
        runtime_lock,
        configuring_global,
    );
    buffer_history.set_sensitive(history_policy.sensitive);
    let readback_policy = w::SettingEditPolicy::new(
        &common::settings::values().enable_gpu_buffer_readback,
        runtime_lock,
        configuring_global,
    );
    gpu_buffer_readback.set_sensitive(readback_policy.sensitive);

    column.append(&group);

    let expose_compute_pipelines = compute_pipelines.clone();
    let page = Page::new("Advanced", scroller, move || {
        let accuracy_value = tr::value_at(tr::GPU_ACCURACY, accuracy.selected());
        let dma_value = tr::value_at(tr::DMA_ACCURACY, dma.selected());
        let fence_behavior_value = tr::value_at(tr::GPU_FENCE_BEHAVIOR, fence_behavior.selected());
        let vram_value = tr::value_at(tr::VRAM_USAGE_MODE, vram.selected());
        let nvdec_value = tr::value_at(tr::NVDEC_EMULATION, nvdec.selected());
        let aniso_value = tr::value_at(tr::ANISOTROPY_MODE, aniso.selected());
        let astc_value = tr::value_at(tr::ASTC_DECODE_MODE, astc.selected());
        let frame_pacing_value = tr::value_at(tr::FRAME_PACING_MODE, frame_pacing.selected());
        let recompression_value = tr::value_at(tr::ASTC_RECOMPRESSION, recompression.selected());
        let sync_memory_value = sync_memory.is_active();
        let max_clock = force_max_clock.is_active();
        let disk_cache = disk_pipeline_cache.is_active();
        let pipeline_cache = vulkan_pipeline_cache.is_active();
        let compute = compute_pipelines.is_active();
        let framerate = video_framerate.is_active();
        let reactive = reactive_flushing.is_active();
        let barriers = barrier_feedback_loops.is_active();
        let history = buffer_history.is_active();
        let readback = gpu_buffer_readback.is_active();

        let mut values = common::settings::values_mut();
        accuracy_policy.apply(&mut values.gpu_accuracy, accuracy_value);
        dma_policy.apply(&mut values.dma_accuracy, dma_value);
        fence_behavior_policy.apply(&mut values.gpu_fence_behavior, fence_behavior_value);
        vram_policy.apply(&mut values.vram_usage_mode, vram_value);
        nvdec_policy.apply(&mut values.nvdec_emulation, nvdec_value);
        aniso_policy.apply(&mut values.max_anisotropy, aniso_value);
        astc_policy.apply(&mut values.accelerate_astc, astc_value);
        frame_pacing_policy.apply(&mut values.frame_pacing_mode, frame_pacing_value);
        recompression_policy.apply(&mut values.astc_recompression, recompression_value);
        sync_memory_policy.apply(&mut values.sync_memory_operations, sync_memory_value);
        max_clock_policy.apply(&mut values.renderer_force_max_clock, max_clock);
        disk_cache_policy.apply(&mut values.use_disk_shader_cache, disk_cache);
        pipeline_cache_policy.apply(&mut values.use_vulkan_driver_pipeline_cache, pipeline_cache);
        compute_policy.apply(&mut values.enable_compute_pipelines, compute);
        framerate_policy.apply(&mut values.use_video_framerate, framerate);
        reactive_policy.apply(&mut values.use_reactive_flushing, reactive);
        barriers_policy.apply(&mut values.barrier_feedback_loops, barriers);
        history_policy.apply(&mut values.enable_buffer_history, history);
        readback_policy.apply(&mut values.enable_gpu_buffer_readback, readback);
    });

    BuildResult {
        page,
        expose_compute_option: Box::new(move || expose_compute_pipelines.set_visible(true)),
    }
}
