//! Port of eden/src/common/settings.h and eden/src/common/settings.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05
//!
//! Contains the global `Values` struct with ALL emulator settings, plus
//! helper functions matching the C++ free functions in `Settings` namespace.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use log::{info, warn};

use crate::settings_common::{InputSetting, Setting, Specialization, SwitchableSetting};
use crate::settings_setting::BasicSetting;

// Re-export the types that consumers most commonly need.
pub use crate::settings_enums::{
    AnisotropyMode, AntiAliasing, AppletMode, AspectRatio, AstcDecodeMode, AstcRecompression,
    AudioEngine, AudioMode, Category, ConfirmStop, ConsoleMode, CpuAccuracy, CpuBackend, CpuClock,
    DmaAccuracy, ExtendedDynamicState, FramePacingMode, FullscreenMode, GpuAccuracy, GpuClock,
    GpuFenceBehavior, GpuLogLevel, GpuUnswizzle, GpuUnswizzleChunk, GpuUnswizzleSize, Language,
    MemoryLayout, NvdecEmulation, Region, RendererBackend, ResolutionSetup, ScalingFilter,
    SpeedMode, TimeZone, VSyncMode, VramUsageMode,
};
pub use crate::settings_input::{
    AnalogsRaw, ButtonsRaw, PlayerInput, RingconRaw, TouchFromButtonMap, TouchscreenInput,
};

// Keep backward-compat aliases that the old code used.
pub type SystemLanguage = Language;
pub type SystemRegion = Region;

impl Language {
    /// Compatibility: parse from numeric index (used by config loader).
    pub fn from_index(index: u32) -> Self {
        Self::from_u32(index).unwrap_or(Self::EnglishAmerican)
    }
}

impl Region {
    /// Compatibility: parse from numeric index (used by config loader).
    pub fn from_index(index: u32) -> Self {
        Self::from_u32(index).unwrap_or(Self::Usa)
    }
}

// ── Global singleton ─────────────────────────────────────────────────────────
// Matches C++ `extern Values values;` in settings.h / `Values values;` in settings.cpp.
// Upstream accesses `Settings::values` as a plain global; here we use a
// `LazyLock<RwLock<Values>>` to satisfy Rust's thread-safety requirements.

static VALUES: LazyLock<RwLock<Values>> = LazyLock::new(|| RwLock::new(Values::default()));
static CURRENT_PROGRAM_ID: AtomicU64 = AtomicU64::new(0);

/// Obtain a read-only reference to the global settings.
/// Equivalent to reading `Settings::values` in C++.
pub fn values() -> RwLockReadGuard<'static, Values> {
    VALUES.read().expect("Settings::values lock poisoned")
}

/// Obtain a mutable reference to the global settings.
/// Equivalent to writing `Settings::values.field = …` in C++.
pub fn values_mut() -> RwLockWriteGuard<'static, Values> {
    VALUES.write().expect("Settings::values lock poisoned")
}

/// Exposes the currently running program ID to dump sites and other global readers.
///
/// Matches Eden `Settings::SetCurrentProgramID`.
pub fn set_current_program_id(program_id: u64) {
    CURRENT_PROGRAM_ID.store(program_id, Ordering::Relaxed);
}

/// Returns the program ID published by the current application load.
///
/// Matches Eden `Settings::GetCurrentProgramID`.
pub fn get_current_program_id() -> u64 {
    CURRENT_PROGRAM_ID.load(Ordering::Relaxed)
}

// ── ResolutionScalingInfo ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ResolutionScalingInfo {
    pub up_scale: u32,
    pub down_shift: u32,
    pub up_factor: f32,
    pub down_factor: f32,
    pub active: bool,
    pub downscale: bool,
}

impl Default for ResolutionScalingInfo {
    fn default() -> Self {
        Self {
            up_scale: 1,
            down_shift: 0,
            up_factor: 1.0,
            down_factor: 1.0,
            active: false,
            downscale: false,
        }
    }
}

impl ResolutionScalingInfo {
    pub fn scale_up_i32(&self, value: i32) -> i32 {
        if value == 0 {
            return 0;
        }
        ((value * self.up_scale as i32) >> self.down_shift as i32).max(1)
    }

    pub fn scale_up_u32(&self, value: u32) -> u32 {
        if value == 0 {
            return 0;
        }
        ((value * self.up_scale) >> self.down_shift).max(1)
    }
}

// ── Values ──────────────────────────────────────────────────────────────────

/// The main settings container matching C++ `Settings::Values`.
/// All emulator settings live here.
#[derive(Clone)]
pub struct Values {
    // ── Applet ──────────────────────────────────────────────────────────
    pub cabinet_applet_mode: SwitchableSetting<AppletMode>,
    pub controller_applet_mode: SwitchableSetting<AppletMode>,
    pub data_erase_applet_mode: Setting<AppletMode>,
    pub error_applet_mode: SwitchableSetting<AppletMode>,
    pub net_connect_applet_mode: Setting<AppletMode>,
    pub player_select_applet_mode: SwitchableSetting<AppletMode>,
    pub swkbd_applet_mode: SwitchableSetting<AppletMode>,
    pub mii_edit_applet_mode: SwitchableSetting<AppletMode>,
    pub web_applet_mode: SwitchableSetting<AppletMode>,
    pub shop_applet_mode: Setting<AppletMode>,
    pub photo_viewer_applet_mode: SwitchableSetting<AppletMode>,
    pub offline_web_applet_mode: SwitchableSetting<AppletMode>,
    pub login_share_applet_mode: Setting<AppletMode>,
    pub wifi_web_auth_applet_mode: Setting<AppletMode>,
    pub my_page_applet_mode: Setting<AppletMode>,
    pub enable_overlay: SwitchableSetting<bool>,

    // ── Audio ───────────────────────────────────────────────────────────
    pub sink_id: SwitchableSetting<AudioEngine>,
    pub audio_output_device_id: SwitchableSetting<String>,
    pub audio_input_device_id: SwitchableSetting<String>,
    pub sound_index: SwitchableSetting<AudioMode>,
    pub volume: SwitchableSetting<u8>,
    pub audio_muted: Setting<bool>,
    pub dump_audio_commands: Setting<bool>,

    // ── Core ────────────────────────────────────────────────────────────
    pub use_multi_core: SwitchableSetting<bool>,
    pub memory_layout_mode: SwitchableSetting<MemoryLayout>,
    pub use_speed_limit: SwitchableSetting<bool>,
    pub speed_limit: SwitchableSetting<u16>,
    pub slow_speed_limit: SwitchableSetting<u16>,
    pub turbo_speed_limit: SwitchableSetting<u16>,
    pub current_speed_mode: Setting<SpeedMode>,
    pub sync_core_speed: SwitchableSetting<bool>,

    // ── CPU ─────────────────────────────────────────────────────────────
    pub cpu_backend: SwitchableSetting<CpuBackend>,
    pub cpu_accuracy: SwitchableSetting<CpuAccuracy>,
    pub cpu_clock: SwitchableSetting<CpuClock>,
    pub use_custom_cpu_ticks: SwitchableSetting<bool>,
    pub cpu_ticks: SwitchableSetting<u32>,
    pub cpu_debug_mode: SwitchableSetting<bool>,

    pub cpuopt_page_tables: Setting<bool>,
    pub cpuopt_block_linking: Setting<bool>,
    pub cpuopt_return_stack_buffer: Setting<bool>,
    pub cpuopt_fast_dispatcher: Setting<bool>,
    pub cpuopt_context_elimination: Setting<bool>,
    pub cpuopt_const_prop: Setting<bool>,
    pub cpuopt_misc_ir: Setting<bool>,
    pub cpuopt_reduce_misalign_checks: Setting<bool>,
    pub cpuopt_fastmem: SwitchableSetting<bool>,
    pub cpuopt_fastmem_exclusives: SwitchableSetting<bool>,
    pub cpuopt_recompile_exclusives: Setting<bool>,
    pub cpuopt_ignore_memory_aborts: Setting<bool>,

    pub cpuopt_unsafe_host_mmu: SwitchableSetting<bool>,
    pub cpuopt_unsafe_unfuse_fma: SwitchableSetting<bool>,
    pub cpuopt_unsafe_reduce_fp_error: SwitchableSetting<bool>,
    pub cpuopt_unsafe_ignore_standard_fpcr: SwitchableSetting<bool>,
    pub cpuopt_unsafe_inaccurate_nan: SwitchableSetting<bool>,
    pub cpuopt_unsafe_fastmem_check: SwitchableSetting<bool>,
    pub cpuopt_unsafe_ignore_global_monitor: SwitchableSetting<bool>,

    // ── Renderer ────────────────────────────────────────────────────────
    pub renderer_backend: SwitchableSetting<RendererBackend>,
    pub vulkan_device: SwitchableSetting<u32>,

    pub use_disk_shader_cache: SwitchableSetting<bool>,
    pub use_asynchronous_gpu_emulation: SwitchableSetting<bool>,
    pub accelerate_astc: SwitchableSetting<AstcDecodeMode>,
    pub vsync_mode: SwitchableSetting<VSyncMode>,
    pub nvdec_emulation: SwitchableSetting<NvdecEmulation>,
    pub fullscreen_mode: SwitchableSetting<FullscreenMode>,
    pub aspect_ratio: SwitchableSetting<AspectRatio>,

    pub resolution_info: ResolutionScalingInfo,
    pub resolution_setup: SwitchableSetting<ResolutionSetup>,
    pub scaling_filter: SwitchableSetting<ScalingFilter>,
    pub anti_aliasing: SwitchableSetting<AntiAliasing>,
    pub fsr_sharpening_slider: SwitchableSetting<i32>,

    pub bg_red: SwitchableSetting<u8>,
    pub bg_green: SwitchableSetting<u8>,
    pub bg_blue: SwitchableSetting<u8>,

    // ── Renderer Advanced ───────────────────────────────────────────────
    pub gpu_accuracy: SwitchableSetting<GpuAccuracy>,
    pub current_gpu_accuracy: GpuAccuracy,
    pub dma_accuracy: SwitchableSetting<DmaAccuracy>,
    pub gpu_fence_behavior: SwitchableSetting<GpuFenceBehavior>,
    pub frame_pacing_mode: SwitchableSetting<FramePacingMode>,
    pub max_anisotropy: SwitchableSetting<AnisotropyMode>,
    pub astc_recompression: SwitchableSetting<AstcRecompression>,
    pub vram_usage_mode: SwitchableSetting<VramUsageMode>,
    pub sync_memory_operations: SwitchableSetting<bool>,
    pub renderer_force_max_clock: SwitchableSetting<bool>,
    pub use_reactive_flushing: SwitchableSetting<bool>,
    pub gpu_clock: SwitchableSetting<GpuClock>,
    pub use_vulkan_driver_pipeline_cache: SwitchableSetting<bool>,
    pub enable_compute_pipelines: SwitchableSetting<bool>,
    pub use_video_framerate: SwitchableSetting<bool>,
    pub barrier_feedback_loops: SwitchableSetting<bool>,
    pub enable_buffer_history: SwitchableSetting<bool>,
    pub enable_gpu_buffer_readback: SwitchableSetting<bool>,

    // ── Renderer Hacks ──────────────────────────────────────────────────
    pub skip_cpu_inner_invalidation: SwitchableSetting<bool>,
    pub async_presentation: SwitchableSetting<bool>,
    pub fix_bloom_effects: SwitchableSetting<bool>,
    pub emulate_bgr565: SwitchableSetting<bool>,
    pub rescale_hack: SwitchableSetting<bool>,
    pub use_asynchronous_shaders: SwitchableSetting<bool>,
    pub gpu_unswizzle_texture_size: SwitchableSetting<GpuUnswizzleSize>,
    pub gpu_unswizzle_stream_size: SwitchableSetting<GpuUnswizzle>,
    pub gpu_unswizzle_chunk_size: SwitchableSetting<GpuUnswizzleChunk>,
    pub gpu_unswizzle_enabled: SwitchableSetting<bool>,

    // ── Renderer Extensions ─────────────────────────────────────────────
    pub dyna_state: SwitchableSetting<ExtendedDynamicState>,
    pub sample_shading: SwitchableSetting<u32>,
    pub vertex_input_dynamic_state: SwitchableSetting<bool>,

    // ── Renderer Debug ──────────────────────────────────────────────────
    pub renderer_debug: Setting<bool>,
    pub renderer_shader_feedback: Setting<bool>,
    pub enable_nsight_aftermath: Setting<bool>,
    pub disable_shader_loop_safety_checks: Setting<bool>,
    pub enable_renderdoc_hotkey: Setting<bool>,
    pub disable_buffer_reorder: Setting<bool>,

    // ── System ──────────────────────────────────────────────────────────
    pub language_index: SwitchableSetting<Language>,
    pub region_index: SwitchableSetting<Region>,
    pub time_zone_index: SwitchableSetting<TimeZone>,
    pub custom_rtc_enabled: SwitchableSetting<bool>,
    pub custom_rtc: SwitchableSetting<i64>,
    pub custom_rtc_offset: SwitchableSetting<i64>,
    pub rng_seed_enabled: SwitchableSetting<bool>,
    pub rng_seed: SwitchableSetting<u32>,
    pub device_name: Setting<String>,
    pub current_user: Setting<i32>,
    pub use_docked_mode: SwitchableSetting<ConsoleMode>,

    // ── Controls ────────────────────────────────────────────────────────
    pub players: InputSetting<[PlayerInput; 10]>,

    pub disable_wgi_xinput: Setting<bool>,
    pub enable_raw_input: Setting<bool>,
    pub controller_navigation: Setting<bool>,
    pub enable_joycon_driver: Setting<bool>,
    pub enable_procon_driver: Setting<bool>,

    pub vibration_enabled: SwitchableSetting<bool>,
    pub enable_accurate_vibrations: SwitchableSetting<bool>,
    pub motion_enabled: SwitchableSetting<bool>,

    pub udp_input_servers: Setting<String>,
    pub enable_udp_controller: Setting<bool>,

    pub pause_tas_on_load: Setting<bool>,
    pub tas_enable: Setting<bool>,
    pub tas_loop: Setting<bool>,

    pub mouse_panning: Setting<bool>,
    pub mouse_panning_sensitivity: Setting<u8>,
    pub mouse_enabled: Setting<bool>,
    pub mouse_panning_x_sensitivity: Setting<u8>,
    pub mouse_panning_y_sensitivity: Setting<u8>,
    pub mouse_panning_deadzone_counterweight: Setting<u8>,
    pub mouse_panning_decay_strength: Setting<u8>,
    pub mouse_panning_min_decay: Setting<u8>,

    pub emulate_analog_keyboard: Setting<bool>,
    pub keyboard_enabled: Setting<bool>,

    pub debug_pad_enabled: Setting<bool>,
    pub debug_pad_buttons: ButtonsRaw,
    pub debug_pad_analogs: AnalogsRaw,

    pub touchscreen: TouchscreenInput,
    pub touch_device: Setting<String>,
    pub touch_from_button_map_index: Setting<i32>,
    pub touch_from_button_maps: Vec<TouchFromButtonMap>,

    pub enable_ring_controller: Setting<bool>,
    pub ringcon_analogs: RingconRaw,

    pub enable_ir_sensor: Setting<bool>,
    pub ir_sensor_device: Setting<String>,

    pub random_amiibo_id: Setting<bool>,

    // ── Data Storage ────────────────────────────────────────────────────
    pub use_virtual_sd: Setting<bool>,
    pub gamecard_inserted: Setting<bool>,
    pub gamecard_current_game: Setting<bool>,
    pub gamecard_path: Setting<String>,
    pub ext_content_from_game_dirs: Setting<bool>,
    /// Host directories scanned recursively for update and DLC containers.
    /// Upstream stores this outside the generic setting linkage and serializes
    /// it as the `Paths\\external_content_dirs` QSettings array.
    pub external_content_dirs: Vec<String>,

    // ── Debugging ───────────────────────────────────────────────────────
    pub record_frame_times: bool,
    pub use_gdbstub: Setting<bool>,
    pub gdbstub_port: Setting<u16>,
    pub program_args: SwitchableSetting<String>,
    pub dump_exefs: Setting<bool>,
    pub dump_nso: Setting<bool>,
    pub dump_guest_shaders: Setting<bool>,
    pub dump_macros: Setting<bool>,
    pub enable_fs_access_log: Setting<bool>,
    pub reporting_services: Setting<bool>,
    pub quest_flag: Setting<bool>,
    pub disable_macro_jit: Setting<bool>,
    pub disable_macro_hle: Setting<bool>,
    pub extended_logging: Setting<bool>,
    pub use_debug_asserts: Setting<bool>,
    pub use_auto_stub: Setting<bool>,
    pub enable_all_controllers: Setting<bool>,
    pub perform_vulkan_check: Setting<bool>,
    pub disable_web_applet: Setting<bool>,

    // ── GPU Logging ────────────────────────────────────────────────────
    pub gpu_log_level: Setting<GpuLogLevel>,
    pub gpu_log_vulkan_calls: Setting<bool>,
    pub gpu_log_shader_dumps: Setting<bool>,
    pub gpu_log_memory_tracking: Setting<bool>,
    pub gpu_log_driver_debug: Setting<bool>,
    pub gpu_log_ring_buffer_size: Setting<i32>,

    // ── Miscellaneous ───────────────────────────────────────────────────
    pub log_filter: Setting<String>,
    pub use_dev_keys: Setting<bool>,

    // ── Network ─────────────────────────────────────────────────────────
    pub network_interface: Setting<String>,
    pub airplane_mode: SwitchableSetting<bool>,

    // ── WebService ──────────────────────────────────────────────────────
    pub enable_telemetry: Setting<bool>,
    pub web_api_url: Setting<String>,
    pub yuzu_username: Setting<String>,
    pub yuzu_token: Setting<String>,

    // ── Add-Ons ─────────────────────────────────────────────────────────
    pub disabled_addons: HashMap<u64, Vec<String>>,

    // ── Per-game overrides ──────────────────────────────────────────────
    pub use_squashed_iterated_blend: bool,

    // ── Extra fields (not in C++ Values but kept for backward compat) ──
    pub title_id: u64,
    pub keys_dir: Option<std::path::PathBuf>,
    pub games_dir: Option<std::path::PathBuf>,
}

impl Values {
    /// Visit settings owned by one upstream category.
    ///
    /// This is the Rust counterpart of `Settings::values.linkage.by_category`.
    /// Keeping the field-to-category mapping here preserves `Values` ownership
    /// while allowing `frontend_common::Config::{Read,Write}Category` to remain
    /// generic like upstream.
    pub fn for_each_setting_in_category_mut(
        &mut self,
        category: Category,
        mut visit: impl FnMut(&mut dyn BasicSetting),
    ) {
        macro_rules! visit {
            ($($field:ident),+ $(,)?) => {{
                $(visit(&mut self.$field);)+
            }};
        }

        match category {
            Category::LibraryApplet => visit!(
                cabinet_applet_mode,
                controller_applet_mode,
                data_erase_applet_mode,
                error_applet_mode,
                net_connect_applet_mode,
                player_select_applet_mode,
                swkbd_applet_mode,
                mii_edit_applet_mode,
                web_applet_mode,
                shop_applet_mode,
                photo_viewer_applet_mode,
                offline_web_applet_mode,
                login_share_applet_mode,
                wifi_web_auth_applet_mode,
                my_page_applet_mode,
                enable_overlay,
            ),
            Category::Audio => visit!(
                sink_id,
                audio_output_device_id,
                audio_input_device_id,
                volume,
                audio_muted,
                dump_audio_commands,
            ),
            Category::Core => visit!(
                use_multi_core,
                memory_layout_mode,
                use_speed_limit,
                speed_limit,
                slow_speed_limit,
                turbo_speed_limit,
                current_speed_mode,
                sync_core_speed,
            ),
            Category::Cpu => visit!(cpu_backend, cpu_accuracy, use_custom_cpu_ticks, cpu_ticks,),
            Category::CpuDebug => visit!(
                cpu_debug_mode,
                cpuopt_page_tables,
                cpuopt_block_linking,
                cpuopt_return_stack_buffer,
                cpuopt_fast_dispatcher,
                cpuopt_context_elimination,
                cpuopt_const_prop,
                cpuopt_misc_ir,
                cpuopt_reduce_misalign_checks,
                cpuopt_fastmem,
                cpuopt_fastmem_exclusives,
                cpuopt_recompile_exclusives,
                cpuopt_ignore_memory_aborts,
            ),
            Category::CpuUnsafe => visit!(
                cpuopt_unsafe_host_mmu,
                cpuopt_unsafe_unfuse_fma,
                cpuopt_unsafe_reduce_fp_error,
                cpuopt_unsafe_ignore_standard_fpcr,
                cpuopt_unsafe_inaccurate_nan,
                cpuopt_unsafe_fastmem_check,
                cpuopt_unsafe_ignore_global_monitor,
            ),
            Category::Renderer => visit!(
                renderer_backend,
                vulkan_device,
                use_asynchronous_gpu_emulation,
                vsync_mode,
                fullscreen_mode,
                aspect_ratio,
                resolution_setup,
                scaling_filter,
                anti_aliasing,
                fsr_sharpening_slider,
                bg_red,
                bg_green,
                bg_blue,
            ),
            Category::RendererAdvanced => visit!(
                gpu_accuracy,
                dma_accuracy,
                gpu_fence_behavior,
                vram_usage_mode,
                nvdec_emulation,
                max_anisotropy,
                accelerate_astc,
                frame_pacing_mode,
                astc_recompression,
                sync_memory_operations,
                renderer_force_max_clock,
                use_disk_shader_cache,
                use_vulkan_driver_pipeline_cache,
                enable_compute_pipelines,
                use_video_framerate,
                use_reactive_flushing,
                barrier_feedback_loops,
                enable_buffer_history,
                enable_gpu_buffer_readback,
            ),
            Category::RendererHacks => visit!(
                skip_cpu_inner_invalidation,
                async_presentation,
                fix_bloom_effects,
                emulate_bgr565,
                rescale_hack,
                use_asynchronous_shaders,
                gpu_unswizzle_texture_size,
                gpu_unswizzle_stream_size,
                gpu_unswizzle_chunk_size,
                gpu_unswizzle_enabled,
            ),
            Category::RendererExtensions => {
                visit!(dyna_state, sample_shading, vertex_input_dynamic_state,)
            }
            Category::RendererDebug => visit!(
                renderer_debug,
                renderer_shader_feedback,
                enable_nsight_aftermath,
                disable_shader_loop_safety_checks,
                enable_renderdoc_hotkey,
                disable_buffer_reorder,
            ),
            Category::Debugging => visit!(
                use_gdbstub,
                gdbstub_port,
                dump_exefs,
                dump_nso,
                enable_fs_access_log,
                reporting_services,
                quest_flag,
                use_dev_keys,
                extended_logging,
                use_debug_asserts,
                use_auto_stub,
                enable_all_controllers,
                perform_vulkan_check,
                disable_web_applet,
                gpu_log_level,
                gpu_log_vulkan_calls,
                gpu_log_shader_dumps,
                gpu_log_memory_tracking,
                gpu_log_driver_debug,
                gpu_log_ring_buffer_size,
            ),
            Category::DebuggingGraphics => visit!(
                dump_guest_shaders,
                dump_macros,
                disable_macro_jit,
                disable_macro_hle,
            ),
            Category::Miscellaneous => visit!(log_filter),
            Category::WebService => {
                visit!(enable_telemetry, web_api_url, yuzu_username, yuzu_token,)
            }
            Category::System => visit!(
                cpu_clock,
                gpu_clock,
                language_index,
                region_index,
                time_zone_index,
                custom_rtc_enabled,
                custom_rtc,
                custom_rtc_offset,
                rng_seed_enabled,
                rng_seed,
                device_name,
                current_user,
                use_docked_mode,
                program_args,
            ),
            Category::SystemAudio => visit!(sound_index),
            Category::DataStorage => visit!(
                use_virtual_sd,
                gamecard_inserted,
                gamecard_current_game,
                gamecard_path,
                ext_content_from_game_dirs,
            ),
            Category::Controls => visit!(
                disable_wgi_xinput,
                enable_raw_input,
                vibration_enabled,
                enable_accurate_vibrations,
                motion_enabled,
            ),
            Category::Network => visit!(network_interface, airplane_mode,),
            _ => {}
        }
    }
}

impl Default for Values {
    fn default() -> Self {
        use Category::*;

        Self {
            // Applet
            cabinet_applet_mode: SwitchableSetting::new(
                AppletMode::LLE,
                "cabinet_applet_mode",
                LibraryApplet,
            ),
            controller_applet_mode: SwitchableSetting::new(
                AppletMode::HLE,
                "controller_applet_mode",
                LibraryApplet,
            ),
            data_erase_applet_mode: Setting::new(
                AppletMode::HLE,
                "data_erase_applet_mode",
                LibraryApplet,
            ),
            error_applet_mode: SwitchableSetting::new(
                AppletMode::LLE,
                "error_applet_mode",
                LibraryApplet,
            ),
            net_connect_applet_mode: Setting::new(
                AppletMode::LLE,
                "net_connect_applet_mode",
                LibraryApplet,
            ),
            player_select_applet_mode: SwitchableSetting::new(
                AppletMode::LLE,
                "player_select_applet_mode",
                LibraryApplet,
            ),
            swkbd_applet_mode: SwitchableSetting::new(
                AppletMode::HLE,
                "swkbd_applet_mode",
                LibraryApplet,
            ),
            mii_edit_applet_mode: SwitchableSetting::new(
                AppletMode::LLE,
                "mii_edit_applet_mode",
                LibraryApplet,
            ),
            web_applet_mode: SwitchableSetting::new(
                AppletMode::HLE,
                "web_applet_mode",
                LibraryApplet,
            ),
            shop_applet_mode: Setting::new(AppletMode::HLE, "shop_applet_mode", LibraryApplet),
            photo_viewer_applet_mode: SwitchableSetting::new(
                AppletMode::LLE,
                "photo_viewer_applet_mode",
                LibraryApplet,
            ),
            offline_web_applet_mode: SwitchableSetting::new(
                AppletMode::LLE,
                "offline_web_applet_mode",
                LibraryApplet,
            ),
            login_share_applet_mode: Setting::new(
                AppletMode::HLE,
                "login_share_applet_mode",
                LibraryApplet,
            ),
            wifi_web_auth_applet_mode: Setting::new(
                AppletMode::HLE,
                "wifi_web_auth_applet_mode",
                LibraryApplet,
            ),
            my_page_applet_mode: Setting::new(
                AppletMode::LLE,
                "my_page_applet_mode",
                LibraryApplet,
            ),
            enable_overlay: SwitchableSetting::new(false, "enable_overlay", LibraryApplet),

            // Audio
            sink_id: SwitchableSetting::with_options(
                AudioEngine::Auto,
                "output_engine",
                Audio,
                Specialization::RUNTIME_LIST,
                true,
                false,
            ),
            audio_output_device_id: SwitchableSetting::with_options(
                "auto".to_string(),
                "output_device",
                Audio,
                Specialization::RUNTIME_LIST,
                true,
                false,
            ),
            audio_input_device_id: SwitchableSetting::with_options(
                "auto".to_string(),
                "input_device",
                Audio,
                Specialization::RUNTIME_LIST,
                true,
                false,
            ),
            sound_index: SwitchableSetting::ranged(
                AudioMode::Stereo,
                AudioMode::Mono,
                AudioMode::Surround,
                "sound_index",
                SystemAudio,
            ),
            volume: SwitchableSetting::ranged_with_options(
                100,
                0,
                200,
                "volume",
                Audio,
                Specialization::SCALAR | Specialization::PERCENTAGE,
                true,
                true,
            ),
            audio_muted: Setting::with_options(
                false,
                "audio_muted",
                Audio,
                Specialization::DEFAULT,
                true,
                true,
            ),
            dump_audio_commands: Setting::new(false, "dump_audio_commands", Audio),

            // Core
            use_multi_core: SwitchableSetting::new(true, "use_multi_core", Core),
            memory_layout_mode: SwitchableSetting::ranged(
                MemoryLayout::Memory4Gb,
                MemoryLayout::Memory4Gb,
                MemoryLayout::Memory12Gb,
                "memory_layout_mode",
                Core,
            ),
            use_speed_limit: SwitchableSetting::with_options(
                true,
                "use_speed_limit",
                Core,
                Specialization::PAIRED,
                true,
                true,
            ),
            speed_limit: SwitchableSetting::ranged_with_options(
                100,
                0,
                9999,
                "speed_limit",
                Core,
                Specialization::COUNTABLE | Specialization::PERCENTAGE,
                true,
                true,
            ),
            slow_speed_limit: SwitchableSetting::ranged_with_options(
                50,
                0,
                9999,
                "slow_speed_limit",
                Core,
                Specialization::COUNTABLE | Specialization::PERCENTAGE,
                true,
                true,
            ),
            turbo_speed_limit: SwitchableSetting::ranged_with_options(
                200,
                0,
                9999,
                "turbo_speed_limit",
                Core,
                Specialization::COUNTABLE | Specialization::PERCENTAGE,
                true,
                true,
            ),
            current_speed_mode: Setting::with_options(
                SpeedMode::Standard,
                "current_speed_mode",
                Core,
                Specialization::DEFAULT,
                false,
                true,
            ),
            sync_core_speed: SwitchableSetting::new(false, "sync_core_speed", Core),

            // CPU
            cpu_backend: SwitchableSetting::ranged(
                CpuBackend::Dynarmic,
                CpuBackend::Dynarmic,
                CpuBackend::Dynarmic,
                "cpu_backend",
                Cpu,
            ),
            cpu_accuracy: SwitchableSetting::ranged(
                CpuAccuracy::Auto,
                CpuAccuracy::Auto,
                CpuAccuracy::Paranoid,
                "cpu_accuracy",
                Cpu,
            ),
            cpu_clock: SwitchableSetting::with_options(
                CpuClock::Normal,
                "fast_cpu_time",
                System,
                Specialization::DEFAULT,
                true,
                true,
            ),
            use_custom_cpu_ticks: SwitchableSetting::with_options(
                false,
                "use_custom_cpu_ticks",
                Cpu,
                Specialization::PAIRED,
                true,
                true,
            ),
            cpu_ticks: SwitchableSetting::ranged_with_options(
                16_000,
                77,
                65_535,
                "cpu_ticks",
                Cpu,
                Specialization::COUNTABLE,
                true,
                true,
            ),
            cpu_debug_mode: SwitchableSetting::new(false, "cpu_debug_mode", CpuDebug),

            cpuopt_page_tables: Setting::new(true, "cpuopt_page_tables", CpuDebug),
            cpuopt_block_linking: Setting::new(true, "cpuopt_block_linking", CpuDebug),
            cpuopt_return_stack_buffer: Setting::new(true, "cpuopt_return_stack_buffer", CpuDebug),
            cpuopt_fast_dispatcher: Setting::new(true, "cpuopt_fast_dispatcher", CpuDebug),
            cpuopt_context_elimination: Setting::new(true, "cpuopt_context_elimination", CpuDebug),
            cpuopt_const_prop: Setting::new(true, "cpuopt_const_prop", CpuDebug),
            cpuopt_misc_ir: Setting::new(true, "cpuopt_misc_ir", CpuDebug),
            cpuopt_reduce_misalign_checks: Setting::new(
                true,
                "cpuopt_reduce_misalign_checks",
                CpuDebug,
            ),
            cpuopt_fastmem: SwitchableSetting::new(true, "cpuopt_fastmem", CpuDebug),
            cpuopt_fastmem_exclusives: SwitchableSetting::new(
                true,
                "cpuopt_fastmem_exclusives",
                CpuDebug,
            ),
            cpuopt_recompile_exclusives: Setting::new(
                true,
                "cpuopt_recompile_exclusives",
                CpuDebug,
            ),
            cpuopt_ignore_memory_aborts: Setting::new(
                true,
                "cpuopt_ignore_memory_aborts",
                CpuDebug,
            ),

            cpuopt_unsafe_host_mmu: SwitchableSetting::new(
                cfg!(any(
                    target_vendor = "apple",
                    target_os = "linux",
                    target_os = "android",
                    target_os = "windows"
                )),
                "cpuopt_unsafe_host_mmu",
                CpuUnsafe,
            ),
            cpuopt_unsafe_unfuse_fma: SwitchableSetting::new(
                true,
                "cpuopt_unsafe_unfuse_fma",
                CpuUnsafe,
            ),
            cpuopt_unsafe_reduce_fp_error: SwitchableSetting::new(
                true,
                "cpuopt_unsafe_reduce_fp_error",
                CpuUnsafe,
            ),
            cpuopt_unsafe_ignore_standard_fpcr: SwitchableSetting::new(
                true,
                "cpuopt_unsafe_ignore_standard_fpcr",
                CpuUnsafe,
            ),
            cpuopt_unsafe_inaccurate_nan: SwitchableSetting::new(
                true,
                "cpuopt_unsafe_inaccurate_nan",
                CpuUnsafe,
            ),
            cpuopt_unsafe_fastmem_check: SwitchableSetting::new(
                true,
                "cpuopt_unsafe_fastmem_check",
                CpuUnsafe,
            ),
            cpuopt_unsafe_ignore_global_monitor: SwitchableSetting::new(
                true,
                "cpuopt_unsafe_ignore_global_monitor",
                CpuUnsafe,
            ),

            // Renderer
            renderer_backend: SwitchableSetting::ranged(
                if cfg!(target_os = "solaris") {
                    RendererBackend::OpenGlGlsl
                } else {
                    RendererBackend::Vulkan
                },
                RendererBackend::OpenGlGlsl,
                RendererBackend::OpenGlSpirV,
                "backend",
                Renderer,
            ),
            vulkan_device: SwitchableSetting::with_options(
                0,
                "vulkan_device",
                Renderer,
                Specialization::RUNTIME_LIST,
                true,
                false,
            ),

            use_disk_shader_cache: SwitchableSetting::new(
                true,
                "use_disk_shader_cache",
                RendererAdvanced,
            ),
            use_asynchronous_gpu_emulation: SwitchableSetting::new(
                !cfg!(target_os = "android"),
                "use_asynchronous_gpu_emulation",
                Renderer,
            ),
            accelerate_astc: SwitchableSetting::ranged(
                AstcDecodeMode::Gpu,
                AstcDecodeMode::Cpu,
                AstcDecodeMode::CpuAsynchronous,
                "accelerate_astc",
                RendererAdvanced,
            ),
            vsync_mode: SwitchableSetting::ranged_with_options(
                VSyncMode::Fifo,
                VSyncMode::Immediate,
                VSyncMode::FifoRelaxed,
                "use_vsync",
                Renderer,
                Specialization::RUNTIME_LIST,
                true,
                true,
            ),
            nvdec_emulation: SwitchableSetting::new(
                NvdecEmulation::Gpu,
                "nvdec_emulation",
                RendererAdvanced,
            ),
            fullscreen_mode: SwitchableSetting::ranged_with_options(
                if cfg!(target_os = "windows") {
                    FullscreenMode::Borderless
                } else {
                    FullscreenMode::Exclusive
                },
                FullscreenMode::Borderless,
                FullscreenMode::Exclusive,
                "fullscreen_mode",
                Renderer,
                Specialization::DEFAULT,
                true,
                true,
            ),
            aspect_ratio: SwitchableSetting::ranged_with_options(
                AspectRatio::R16_9,
                AspectRatio::R16_9,
                AspectRatio::Stretch,
                "aspect_ratio",
                Renderer,
                Specialization::DEFAULT,
                true,
                true,
            ),

            resolution_info: ResolutionScalingInfo::default(),
            resolution_setup: SwitchableSetting::new(
                ResolutionSetup::Res1X,
                "resolution_setup",
                Renderer,
            ),
            scaling_filter: SwitchableSetting::with_options(
                ScalingFilter::Bilinear,
                "scaling_filter",
                Renderer,
                Specialization::DEFAULT,
                true,
                true,
            ),
            anti_aliasing: SwitchableSetting::with_options(
                AntiAliasing::None,
                "anti_aliasing",
                Renderer,
                Specialization::DEFAULT,
                true,
                true,
            ),
            fsr_sharpening_slider: SwitchableSetting::ranged_with_options(
                if cfg!(target_os = "android") { 0 } else { 25 },
                0,
                200,
                "fsr_sharpening_slider",
                Renderer,
                Specialization::SCALAR | Specialization::PERCENTAGE,
                true,
                true,
            ),

            bg_red: SwitchableSetting::with_options(
                0,
                "bg_red",
                Renderer,
                Specialization::DEFAULT,
                true,
                true,
            ),
            bg_green: SwitchableSetting::with_options(
                0,
                "bg_green",
                Renderer,
                Specialization::DEFAULT,
                true,
                true,
            ),
            bg_blue: SwitchableSetting::with_options(
                0,
                "bg_blue",
                Renderer,
                Specialization::DEFAULT,
                true,
                true,
            ),

            // Renderer Advanced
            gpu_accuracy: SwitchableSetting::ranged_with_options(
                if cfg!(target_os = "android") {
                    GpuAccuracy::Low
                } else {
                    GpuAccuracy::High
                },
                GpuAccuracy::Low,
                GpuAccuracy::High,
                "gpu_accuracy",
                RendererAdvanced,
                Specialization::DEFAULT,
                true,
                true,
            ),
            current_gpu_accuracy: GpuAccuracy::High,
            dma_accuracy: SwitchableSetting::ranged_with_options(
                DmaAccuracy::Default,
                DmaAccuracy::Default,
                DmaAccuracy::Safe,
                "dma_accuracy",
                RendererAdvanced,
                Specialization::DEFAULT,
                true,
                true,
            ),
            gpu_fence_behavior: SwitchableSetting::ranged_with_options(
                GpuFenceBehavior::Default,
                GpuFenceBehavior::Default,
                GpuFenceBehavior::Strict,
                "gpu_fence_behavior",
                RendererAdvanced,
                Specialization::DEFAULT,
                true,
                true,
            ),
            frame_pacing_mode: SwitchableSetting::ranged_with_options(
                FramePacingMode::Target_Auto,
                FramePacingMode::Target_Auto,
                FramePacingMode::Target_120,
                "frame_pacing_mode",
                RendererAdvanced,
                Specialization::DEFAULT,
                true,
                true,
            ),
            max_anisotropy: SwitchableSetting::ranged(
                if cfg!(target_os = "android") {
                    AnisotropyMode::Default
                } else {
                    AnisotropyMode::Automatic
                },
                AnisotropyMode::Automatic,
                AnisotropyMode::None,
                "max_anisotropy",
                RendererAdvanced,
            ),
            astc_recompression: SwitchableSetting::ranged(
                AstcRecompression::Uncompressed,
                AstcRecompression::Uncompressed,
                AstcRecompression::Bc3,
                "astc_recompression",
                RendererAdvanced,
            ),
            vram_usage_mode: SwitchableSetting::ranged(
                VramUsageMode::Conservative,
                VramUsageMode::Conservative,
                VramUsageMode::Aggressive,
                "vram_usage_mode",
                RendererAdvanced,
            ),
            sync_memory_operations: SwitchableSetting::with_options(
                false,
                "sync_memory_operations",
                RendererAdvanced,
                Specialization::DEFAULT,
                true,
                true,
            ),
            renderer_force_max_clock: SwitchableSetting::new(
                false,
                "force_max_clock",
                RendererAdvanced,
            ),
            use_reactive_flushing: SwitchableSetting::new(
                !cfg!(target_os = "android"),
                "use_reactive_flushing",
                RendererAdvanced,
            ),
            gpu_clock: SwitchableSetting::with_options(
                GpuClock::Boost,
                "fast_gpu_time",
                System,
                Specialization::DEFAULT,
                true,
                true,
            ),
            use_vulkan_driver_pipeline_cache: SwitchableSetting::with_options(
                true,
                "use_vulkan_driver_pipeline_cache",
                RendererAdvanced,
                Specialization::DEFAULT,
                true,
                true,
            ),
            enable_compute_pipelines: SwitchableSetting::new(
                false,
                "enable_compute_pipelines",
                RendererAdvanced,
            ),
            use_video_framerate: SwitchableSetting::new(
                false,
                "use_video_framerate",
                RendererAdvanced,
            ),
            barrier_feedback_loops: SwitchableSetting::new(
                true,
                "barrier_feedback_loops",
                RendererAdvanced,
            ),
            enable_buffer_history: SwitchableSetting::with_options(
                false,
                "enable_buffer_history",
                RendererAdvanced,
                Specialization::DEFAULT,
                true,
                true,
            ),
            enable_gpu_buffer_readback: SwitchableSetting::with_options(
                false,
                "enable_gpu_buffer_readback",
                RendererAdvanced,
                Specialization::DEFAULT,
                true,
                true,
            ),

            // Renderer Hacks
            skip_cpu_inner_invalidation: SwitchableSetting::with_options(
                false,
                "skip_cpu_inner_invalidation",
                RendererHacks,
                Specialization::DEFAULT,
                true,
                true,
            ),
            async_presentation: SwitchableSetting::new(false, "async_presentation", RendererHacks),
            fix_bloom_effects: SwitchableSetting::new(false, "fix_bloom_effects", RendererHacks),
            emulate_bgr565: SwitchableSetting::new(false, "emulate_bgr565", RendererHacks),
            rescale_hack: SwitchableSetting::new(
                cfg!(target_os = "android"),
                "rescale_hack",
                RendererHacks,
            ),
            use_asynchronous_shaders: SwitchableSetting::new(
                false,
                "use_asynchronous_shaders",
                RendererHacks,
            ),
            gpu_unswizzle_texture_size: SwitchableSetting::new(
                GpuUnswizzleSize::Large,
                "gpu_unswizzle_texture_size",
                RendererHacks,
            ),
            gpu_unswizzle_stream_size: SwitchableSetting::new(
                GpuUnswizzle::Medium,
                "gpu_unswizzle_stream_size",
                RendererHacks,
            ),
            gpu_unswizzle_chunk_size: SwitchableSetting::new(
                GpuUnswizzleChunk::Medium,
                "gpu_unswizzle_chunk_size",
                RendererHacks,
            ),
            gpu_unswizzle_enabled: SwitchableSetting::new(
                false,
                "gpu_unswizzle_enabled",
                RendererHacks,
            ),

            // Renderer Extensions
            dyna_state: SwitchableSetting::new(
                if cfg!(any(target_os = "android", target_os = "macos")) {
                    ExtendedDynamicState::Disabled
                } else {
                    ExtendedDynamicState::EDS2
                },
                "dyna_state",
                RendererExtensions,
            ),
            sample_shading: SwitchableSetting::ranged(
                0,
                0,
                100,
                "sample_shading_fraction",
                RendererExtensions,
            ),
            vertex_input_dynamic_state: SwitchableSetting::new(
                !cfg!(target_os = "android"),
                "vertex_input_dynamic_state",
                RendererExtensions,
            ),

            // Renderer Debug
            renderer_debug: Setting::new(false, "debug", RendererDebug),
            renderer_shader_feedback: Setting::new(false, "shader_feedback", RendererDebug),
            enable_nsight_aftermath: Setting::new(false, "nsight_aftermath", RendererDebug),
            disable_shader_loop_safety_checks: Setting::new(
                false,
                "disable_shader_loop_safety_checks",
                RendererDebug,
            ),
            enable_renderdoc_hotkey: Setting::new(false, "renderdoc_hotkey", RendererDebug),
            disable_buffer_reorder: Setting::new(false, "disable_buffer_reorder", RendererDebug),

            // System
            language_index: SwitchableSetting::ranged(
                Language::EnglishAmerican,
                Language::Japanese,
                Language::PortugueseBrazilian,
                "language_index",
                System,
            ),
            region_index: SwitchableSetting::ranged(
                Region::Usa,
                Region::Japan,
                Region::Taiwan,
                "region_index",
                System,
            ),
            time_zone_index: SwitchableSetting::ranged(
                TimeZone::Auto,
                TimeZone::Auto,
                TimeZone::Zulu,
                "time_zone_index",
                System,
            ),
            custom_rtc_enabled: SwitchableSetting::with_options(
                false,
                "custom_rtc_enabled",
                System,
                Specialization::PAIRED,
                true,
                true,
            ),
            custom_rtc: SwitchableSetting::with_options(
                0i64,
                "custom_rtc",
                System,
                Specialization::TIME,
                false,
                true,
            ),
            custom_rtc_offset: SwitchableSetting::ranged_with_options(
                0i64,
                i32::MIN as i64,
                i32::MAX as i64,
                "custom_rtc_offset",
                System,
                Specialization::COUNTABLE,
                true,
                true,
            ),
            rng_seed_enabled: SwitchableSetting::with_options(
                false,
                "rng_seed_enabled",
                System,
                Specialization::PAIRED,
                true,
                true,
            ),
            rng_seed: SwitchableSetting::with_options(
                0u32,
                "rng_seed",
                System,
                Specialization::HEX,
                true,
                true,
            ),
            device_name: Setting::with_options(
                "yuzu".to_string(),
                "device_name",
                System,
                Specialization::DEFAULT,
                true,
                true,
            ),
            current_user: Setting::new(0i32, "current_user", System),
            use_docked_mode: SwitchableSetting::with_options(
                ConsoleMode::Docked,
                "use_docked_mode",
                System,
                Specialization::RADIO,
                true,
                true,
            ),

            // Controls
            players: InputSetting::new(),

            disable_wgi_xinput: Setting::with_options(
                false,
                "disable_wgi_xinput",
                Controls,
                Specialization::DEFAULT,
                cfg!(target_os = "windows"),
                false,
            ),
            enable_raw_input: Setting::with_options(
                false,
                "enable_raw_input",
                Controls,
                Specialization::DEFAULT,
                cfg!(target_os = "windows"),
                false,
            ),
            controller_navigation: Setting::new(true, "controller_navigation", Controls),
            enable_joycon_driver: Setting::new(true, "enable_joycon_driver", Controls),
            enable_procon_driver: Setting::new(false, "enable_procon_driver", Controls),

            vibration_enabled: SwitchableSetting::new(true, "vibration_enabled", Controls),
            enable_accurate_vibrations: SwitchableSetting::new(
                false,
                "enable_accurate_vibrations",
                Controls,
            ),
            motion_enabled: SwitchableSetting::new(true, "motion_enabled", Controls),

            udp_input_servers: Setting::new(
                "127.0.0.1:26760".to_string(),
                "udp_input_servers",
                Controls,
            ),
            enable_udp_controller: Setting::new(false, "enable_udp_controller", Controls),

            pause_tas_on_load: Setting::new(true, "pause_tas_on_load", Controls),
            tas_enable: Setting::new(false, "tas_enable", Controls),
            tas_loop: Setting::new(false, "tas_loop", Controls),

            mouse_panning: Setting::with_options(
                false,
                "mouse_panning",
                Controls,
                Specialization::DEFAULT,
                false,
                false,
            ),
            mouse_panning_sensitivity: Setting::ranged(
                50,
                1,
                100,
                "mouse_panning_sensitivity",
                Controls,
            ),
            mouse_enabled: Setting::new(false, "mouse_enabled", Controls),
            mouse_panning_x_sensitivity: Setting::ranged(
                50,
                1,
                100,
                "mouse_panning_x_sensitivity",
                Controls,
            ),
            mouse_panning_y_sensitivity: Setting::ranged(
                50,
                1,
                100,
                "mouse_panning_y_sensitivity",
                Controls,
            ),
            mouse_panning_deadzone_counterweight: Setting::ranged(
                20,
                0,
                100,
                "mouse_panning_deadzone_counterweight",
                Controls,
            ),
            mouse_panning_decay_strength: Setting::ranged(
                18,
                0,
                100,
                "mouse_panning_decay_strength",
                Controls,
            ),
            mouse_panning_min_decay: Setting::ranged(
                6,
                0,
                100,
                "mouse_panning_min_decay",
                Controls,
            ),

            emulate_analog_keyboard: Setting::new(false, "emulate_analog_keyboard", Controls),
            keyboard_enabled: Setting::new(false, "keyboard_enabled", Controls),

            debug_pad_enabled: Setting::new(false, "debug_pad_enabled", Controls),
            debug_pad_buttons: Default::default(),
            debug_pad_analogs: Default::default(),

            touchscreen: TouchscreenInput::default(),
            touch_device: Setting::new(
                "min_x:100,min_y:50,max_x:1800,max_y:850".to_string(),
                "touch_device",
                Controls,
            ),
            touch_from_button_map_index: Setting::new(0, "touch_from_button_map", Controls),
            touch_from_button_maps: Vec::new(),

            enable_ring_controller: Setting::new(true, "enable_ring_controller", Controls),
            ringcon_analogs: String::new(),

            enable_ir_sensor: Setting::new(false, "enable_ir_sensor", Controls),
            ir_sensor_device: Setting::new("auto".to_string(), "ir_sensor_device", Controls),

            random_amiibo_id: Setting::new(false, "random_amiibo_id", Controls),

            // Data Storage
            use_virtual_sd: Setting::new(true, "use_virtual_sd", DataStorage),
            gamecard_inserted: Setting::new(false, "gamecard_inserted", DataStorage),
            gamecard_current_game: Setting::new(false, "gamecard_current_game", DataStorage),
            gamecard_path: Setting::new(String::new(), "gamecard_path", DataStorage),
            ext_content_from_game_dirs: Setting::new(
                true,
                "ext_content_from_game_dirs",
                DataStorage,
            ),
            external_content_dirs: Vec::new(),

            // Debugging
            record_frame_times: false,
            use_gdbstub: Setting::new(false, "use_gdbstub", Debugging),
            gdbstub_port: Setting::new(6543, "gdbstub_port", Debugging),
            program_args: SwitchableSetting::with_options(
                String::new(),
                "program_args",
                System,
                Specialization::DEFAULT,
                true,
                false,
            ),
            dump_exefs: Setting::new(false, "dump_exefs", Debugging),
            dump_nso: Setting::new(false, "dump_nso", Debugging),
            dump_guest_shaders: Setting::with_options(
                false,
                "dump_guest_shaders",
                DebuggingGraphics,
                Specialization::DEFAULT,
                false,
                false,
            ),
            dump_macros: Setting::with_options(
                false,
                "dump_macros",
                DebuggingGraphics,
                Specialization::DEFAULT,
                false,
                false,
            ),
            enable_fs_access_log: Setting::new(false, "enable_fs_access_log", Debugging),
            reporting_services: Setting::with_options(
                false,
                "reporting_services",
                Debugging,
                Specialization::DEFAULT,
                false,
                false,
            ),
            quest_flag: Setting::new(false, "quest_flag", Debugging),
            disable_macro_jit: Setting::new(false, "disable_macro_jit", DebuggingGraphics),
            disable_macro_hle: Setting::new(false, "disable_macro_hle", DebuggingGraphics),
            extended_logging: Setting::with_options(
                false,
                "extended_logging",
                Debugging,
                Specialization::DEFAULT,
                false,
                false,
            ),
            use_debug_asserts: Setting::new(false, "use_debug_asserts", Debugging),
            use_auto_stub: Setting::with_options(
                false,
                "use_auto_stub",
                Debugging,
                Specialization::DEFAULT,
                false,
                false,
            ),
            enable_all_controllers: Setting::new(false, "enable_all_controllers", Debugging),
            perform_vulkan_check: Setting::new(true, "perform_vulkan_check", Debugging),
            disable_web_applet: Setting::new(true, "disable_web_applet", Debugging),

            // GPU Logging
            gpu_log_level: Setting::new(GpuLogLevel::Off, "gpu_log_level", Debugging),
            gpu_log_vulkan_calls: Setting::new(true, "gpu_log_vulkan_calls", Debugging),
            gpu_log_shader_dumps: Setting::new(false, "gpu_log_shader_dumps", Debugging),
            gpu_log_memory_tracking: Setting::new(true, "gpu_log_memory_tracking", Debugging),
            gpu_log_driver_debug: Setting::new(true, "gpu_log_driver_debug", Debugging),
            gpu_log_ring_buffer_size: Setting::new(512, "gpu_log_ring_buffer_size", Debugging),

            // Miscellaneous
            log_filter: Setting::new("*:Info".to_string(), "log_filter", Miscellaneous),
            use_dev_keys: Setting::new(false, "use_dev_keys", Debugging),

            // Network
            network_interface: Setting::new(String::new(), "network_interface", Network),
            airplane_mode: SwitchableSetting::new(false, "airplane_mode", Network),

            // WebService
            enable_telemetry: Setting::new(true, "enable_telemetry", WebService),
            web_api_url: Setting::new(
                // Eden's announce host, verbatim from upstream
                // common/settings.h: `web_api_url{linkage, "api.ynet-fun.xyz", ...}`.
                // yuzu's api.yuzu-emu.org no longer answers.
                "api.ynet-fun.xyz".to_string(),
                "web_api_url",
                WebService,
            ),
            yuzu_username: Setting::new(String::new(), "yuzu_username", WebService),
            yuzu_token: Setting::new(String::new(), "yuzu_token", WebService),

            // Add-Ons
            disabled_addons: HashMap::new(),

            // Per-game overrides
            use_squashed_iterated_blend: false,

            // Extra
            title_id: 0,
            keys_dir: None,
            games_dir: None,
        }
    }
}

// ── Free functions matching C++ Settings:: namespace ────────────────────────

/// Update `current_gpu_accuracy` from the switchable setting.
pub fn update_gpu_accuracy(values: &mut Values) {
    values.current_gpu_accuracy = *values.gpu_accuracy.get_value();
}

/// Returns true if GPU accuracy is High.
pub fn is_gpu_level_high(values: &Values) -> bool {
    values.current_gpu_accuracy == GpuAccuracy::High
}

/// Eden `Settings::IsDMALevelDefault`.
pub fn is_dma_level_default(values: &Values) -> bool {
    *values.dma_accuracy.get_value() == DmaAccuracy::Default
}

/// Eden `Settings::IsDMALevelSafe`.
pub fn is_dma_level_safe(values: &Values) -> bool {
    *values.dma_accuracy.get_value() == DmaAccuracy::Safe
}

pub fn is_gpu_fence_behavior_default(values: &Values) -> bool {
    *values.gpu_fence_behavior.get_value() == GpuFenceBehavior::Default
}

pub fn is_gpu_fence_behavior_balanced(values: &Values) -> bool {
    *values.gpu_fence_behavior.get_value() == GpuFenceBehavior::Balanced
}

pub fn is_gpu_fence_behavior_accurate(values: &Values) -> bool {
    *values.gpu_fence_behavior.get_value() == GpuFenceBehavior::Accurate
}

pub fn is_gpu_fence_behavior_strict(values: &Values) -> bool {
    *values.gpu_fence_behavior.get_value() == GpuFenceBehavior::Strict
}

/// Upstream `Settings::IsOpenGL()`.
pub fn is_opengl() -> bool {
    matches!(
        *values().renderer_backend.get_value(),
        RendererBackend::OpenGlGlsl | RendererBackend::OpenGlGlasm | RendererBackend::OpenGlSpirV
    )
}

/// Returns true if fastmem is effectively enabled.
pub fn is_fastmem_enabled(values: &Values) -> bool {
    if *values.cpu_debug_mode.get_value() {
        return *values.cpuopt_fastmem.get_value();
    }
    if *values.cpu_accuracy.get_value() == CpuAccuracy::Unsafe {
        return *values.cpuopt_unsafe_host_mmu.get_value();
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        // Eden supports host-MMU fastmem on Linux/AArch64 only with 4 KiB
        // host pages.
        return unsafe { libc::getpagesize() == 4096 };
    }
    #[cfg(not(any(
        target_vendor = "apple",
        target_os = "android",
        target_os = "windows",
        target_os = "linux",
        target_os = "freebsd"
    )))]
    {
        return false;
    }
    #[cfg(any(
        target_vendor = "apple",
        target_os = "android",
        target_os = "windows",
        all(target_os = "linux", not(target_arch = "aarch64")),
        target_os = "freebsd"
    ))]
    {
        true
    }
}

static IS_NCE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Configure NCE state based on CPU backend selection and address space.
pub fn set_nce_enabled(values: &Values, is_39bit: bool) {
    let is_nce_selected = *values.cpu_backend.get_value() == CpuBackend::Nce;
    if is_nce_selected && !is_fastmem_enabled(values) {
        warn!(
            "Fastmem is required to natively execute code in a performant manner, \
             falling back to Dynarmic"
        );
    }
    if is_nce_selected && !is_39bit {
        warn!("Program does not utilize 39-bit address space, unable to natively execute code");
    }
    IS_NCE_ENABLED.store(
        is_fastmem_enabled(values) && is_nce_selected && is_39bit,
        Ordering::Relaxed,
    );
}

/// Returns true if NCE (native code execution) is enabled.
pub fn is_nce_enabled() -> bool {
    IS_NCE_ENABLED.load(Ordering::Relaxed)
}

/// Returns true if the console is in docked mode.
pub fn is_docked_mode(values: &Values) -> bool {
    *values.use_docked_mode.get_value() == ConsoleMode::Docked
}

/// Returns the effective audio volume as a float (0.0 to ~2.0).
pub fn volume(values: &Values) -> f32 {
    if *values.audio_muted.get_value() {
        return 0.0;
    }
    *values.volume.get_value() as f32 / *values.volume.get_default() as f32
}

/// Translate a `ResolutionSetup` into scaling info.
pub fn translate_resolution_info(setup: ResolutionSetup, info: &mut ResolutionScalingInfo) {
    info.downscale = false;
    match setup {
        ResolutionSetup::Res1_4X => {
            info.up_scale = 1;
            info.down_shift = 2;
            info.downscale = true;
        }
        ResolutionSetup::Res1_2X => {
            info.up_scale = 1;
            info.down_shift = 1;
            info.downscale = true;
        }
        ResolutionSetup::Res3_4X => {
            info.up_scale = 3;
            info.down_shift = 2;
            info.downscale = true;
        }
        ResolutionSetup::Res1X => {
            info.up_scale = 1;
            info.down_shift = 0;
        }
        ResolutionSetup::Res5_4X => {
            info.up_scale = 5;
            info.down_shift = 2;
        }
        ResolutionSetup::Res3_2X => {
            info.up_scale = 3;
            info.down_shift = 1;
        }
        ResolutionSetup::Res2X => {
            info.up_scale = 2;
            info.down_shift = 0;
        }
        ResolutionSetup::Res3X => {
            info.up_scale = 3;
            info.down_shift = 0;
        }
        ResolutionSetup::Res4X => {
            info.up_scale = 4;
            info.down_shift = 0;
        }
        ResolutionSetup::Res5X => {
            info.up_scale = 5;
            info.down_shift = 0;
        }
        ResolutionSetup::Res6X => {
            info.up_scale = 6;
            info.down_shift = 0;
        }
        ResolutionSetup::Res7X => {
            info.up_scale = 7;
            info.down_shift = 0;
        }
        ResolutionSetup::Res8X => {
            info.up_scale = 8;
            info.down_shift = 0;
        }
    }
    info.up_factor = info.up_scale as f32 / (1u32 << info.down_shift) as f32;
    info.down_factor = (1u32 << info.down_shift) as f32 / info.up_scale as f32;
    info.active = info.up_scale != 1 || info.down_shift != 0;
}

/// Update the global resolution scaling info from the current resolution_setup setting.
pub fn update_rescaling_info(values: &mut Values) {
    let setup = *values.resolution_setup.get_value();
    translate_resolution_info(setup, &mut values.resolution_info);
}

/// Return the active speed percentage selected by `current_speed_mode`.
/// This is Eden `Settings::SpeedLimit`.
pub fn speed_limit() -> u16 {
    let values = values();
    speed_limit_for_mode(
        *values.current_speed_mode.get_value(),
        *values.speed_limit.get_value(),
        *values.turbo_speed_limit.get_value(),
        *values.slow_speed_limit.get_value(),
    )
}

fn speed_limit_for_mode(mode: SpeedMode, standard: u16, turbo: u16, slow: u16) -> u16 {
    match mode {
        SpeedMode::Standard => standard,
        SpeedMode::Turbo => turbo,
        SpeedMode::Slow => slow,
    }
}

/// Select Eden's runtime speed mode. Slow and turbo always enable limiting.
pub fn set_speed_mode(mode: SpeedMode) {
    let mut values = values_mut();
    values.current_speed_mode.set_value(mode);
    if matches!(mode, SpeedMode::Turbo | SpeedMode::Slow) {
        values.use_speed_limit.set_value(true);
    }
}

pub fn toggle_standard_mode() {
    let enabled = !*values().use_speed_limit.get_value();
    values_mut().use_speed_limit.set_value(enabled);
    set_speed_mode(SpeedMode::Standard);
}

pub fn toggle_turbo_mode() {
    let next = if *values().current_speed_mode.get_value() == SpeedMode::Turbo {
        SpeedMode::Standard
    } else {
        SpeedMode::Turbo
    };
    set_speed_mode(next);
}

pub fn toggle_slow_mode() {
    let next = if *values().current_speed_mode.get_value() == SpeedMode::Slow {
        SpeedMode::Standard
    } else {
        SpeedMode::Slow
    };
    set_speed_mode(next);
}

/// Restore all switchable settings to their global state.
/// Should be called when a game is not running.
pub fn restore_global_state(values: &mut Values, is_powered_on: bool) {
    if is_powered_on {
        return;
    }

    values.sink_id.set_global(true);
    values.audio_output_device_id.set_global(true);
    values.audio_input_device_id.set_global(true);
    values.sound_index.set_global(true);
    values.volume.set_global(true);
    values.use_multi_core.set_global(true);
    values.memory_layout_mode.set_global(true);
    values.use_speed_limit.set_global(true);
    values.speed_limit.set_global(true);
    values.slow_speed_limit.set_global(true);
    values.turbo_speed_limit.set_global(true);
    values.sync_core_speed.set_global(true);
    values.cpu_backend.set_global(true);
    values.cpu_accuracy.set_global(true);
    values.cpu_clock.set_global(true);
    values.use_custom_cpu_ticks.set_global(true);
    values.cpu_ticks.set_global(true);
    values.cpu_debug_mode.set_global(true);
    values.cpuopt_fastmem.set_global(true);
    values.cpuopt_fastmem_exclusives.set_global(true);
    values.cpuopt_unsafe_host_mmu.set_global(true);
    values.cpuopt_unsafe_unfuse_fma.set_global(true);
    values.cpuopt_unsafe_reduce_fp_error.set_global(true);
    values.cpuopt_unsafe_ignore_standard_fpcr.set_global(true);
    values.cpuopt_unsafe_inaccurate_nan.set_global(true);
    values.cpuopt_unsafe_fastmem_check.set_global(true);
    values.cpuopt_unsafe_ignore_global_monitor.set_global(true);
    values.renderer_backend.set_global(true);
    values.vulkan_device.set_global(true);
    values.use_disk_shader_cache.set_global(true);
    values.use_asynchronous_gpu_emulation.set_global(true);
    values.accelerate_astc.set_global(true);
    values.vsync_mode.set_global(true);
    values.nvdec_emulation.set_global(true);
    values.fullscreen_mode.set_global(true);
    values.aspect_ratio.set_global(true);
    values.resolution_setup.set_global(true);
    values.scaling_filter.set_global(true);
    values.anti_aliasing.set_global(true);
    values.fsr_sharpening_slider.set_global(true);
    values.bg_red.set_global(true);
    values.bg_green.set_global(true);
    values.bg_blue.set_global(true);
    values.gpu_accuracy.set_global(true);
    values.dma_accuracy.set_global(true);
    values.gpu_fence_behavior.set_global(true);
    values.frame_pacing_mode.set_global(true);
    values.max_anisotropy.set_global(true);
    values.astc_recompression.set_global(true);
    values.vram_usage_mode.set_global(true);
    values.sync_memory_operations.set_global(true);
    values.renderer_force_max_clock.set_global(true);
    values.use_reactive_flushing.set_global(true);
    values.gpu_clock.set_global(true);
    values.use_vulkan_driver_pipeline_cache.set_global(true);
    values.enable_compute_pipelines.set_global(true);
    values.use_video_framerate.set_global(true);
    values.barrier_feedback_loops.set_global(true);
    values.enable_buffer_history.set_global(true);
    values.enable_gpu_buffer_readback.set_global(true);
    values.skip_cpu_inner_invalidation.set_global(true);
    values.async_presentation.set_global(true);
    values.fix_bloom_effects.set_global(true);
    values.emulate_bgr565.set_global(true);
    values.rescale_hack.set_global(true);
    values.use_asynchronous_shaders.set_global(true);
    values.gpu_unswizzle_texture_size.set_global(true);
    values.gpu_unswizzle_stream_size.set_global(true);
    values.gpu_unswizzle_chunk_size.set_global(true);
    values.gpu_unswizzle_enabled.set_global(true);
    values.dyna_state.set_global(true);
    values.sample_shading.set_global(true);
    values.vertex_input_dynamic_state.set_global(true);
    values.language_index.set_global(true);
    values.region_index.set_global(true);
    values.time_zone_index.set_global(true);
    values.custom_rtc_enabled.set_global(true);
    values.custom_rtc.set_global(true);
    values.custom_rtc_offset.set_global(true);
    values.rng_seed_enabled.set_global(true);
    values.rng_seed.set_global(true);
    values.use_docked_mode.set_global(true);
    values.program_args.set_global(true);
    values.cabinet_applet_mode.set_global(true);
    values.controller_applet_mode.set_global(true);
    values.error_applet_mode.set_global(true);
    values.player_select_applet_mode.set_global(true);
    values.swkbd_applet_mode.set_global(true);
    values.mii_edit_applet_mode.set_global(true);
    values.web_applet_mode.set_global(true);
    values.photo_viewer_applet_mode.set_global(true);
    values.offline_web_applet_mode.set_global(true);
    values.enable_overlay.set_global(true);
    values.airplane_mode.set_global(true);
    values.vibration_enabled.set_global(true);
    values.enable_accurate_vibrations.set_global(true);
    values.motion_enabled.set_global(true);

    // Reset per-game flags, matching `Settings::RestoreGlobalState`.
    values.use_squashed_iterated_blend = false;
}

static CONFIGURING_GLOBAL: AtomicBool = AtomicBool::new(true);

/// Returns true if the frontend is currently configuring global (not per-game) settings.
pub fn is_configuring_global() -> bool {
    CONFIGURING_GLOBAL.load(Ordering::Relaxed)
}

/// Set the global configuration state.
pub fn set_configuring_global(is_global: bool) {
    CONFIGURING_GLOBAL.store(is_global, Ordering::Relaxed);
}

/// Log all settings. Matches C++ `Settings::LogSettings()`.
pub fn log_settings(values: &Values) {
    info!("ruzu Configuration:");
    info!("  use_multi_core: {}", values.use_multi_core.get_value());
    info!("  cpu_accuracy: {:?}", values.cpu_accuracy.get_value());
    info!("  cpu_backend: {:?}", values.cpu_backend.get_value());
    info!(
        "  renderer_backend: {:?}",
        values.renderer_backend.get_value()
    );
    info!("  vulkan_device: {}", values.vulkan_device.get_value());
    info!("  gpu_accuracy: {:?}", values.gpu_accuracy.get_value());
    info!(
        "  gpu_fence_behavior: {:?}",
        values.gpu_fence_behavior.get_value()
    );
    info!(
        "  resolution_setup: {:?}",
        values.resolution_setup.get_value()
    );
    info!("  vsync_mode: {:?}", values.vsync_mode.get_value());
    info!("  language_index: {:?}", values.language_index.get_value());
    info!("  region_index: {:?}", values.region_index.get_value());
    info!(
        "  use_docked_mode: {:?}",
        values.use_docked_mode.get_value()
    );
    info!("  volume: {}", values.volume.get_value());
}

// ── Backward-compatibility type alias ───────────────────────────────────────

/// The old `Settings` struct name -- now an alias to `Values`.
pub type Settings = Values;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_graphics_defaults_match_eden_settings_h() {
        let values = Values::default();

        assert_eq!(
            *values.renderer_backend.get_value(),
            if cfg!(target_os = "solaris") {
                RendererBackend::OpenGlGlsl
            } else {
                RendererBackend::Vulkan
            }
        );
        assert_eq!(*values.vulkan_device.get_value(), 0);
        assert_eq!(*values.resolution_setup.get_value(), ResolutionSetup::Res1X);
        assert_eq!(*values.vsync_mode.get_value(), VSyncMode::Fifo);
        assert_eq!(*values.scaling_filter.get_value(), ScalingFilter::Bilinear);
        assert_eq!(
            *values.fsr_sharpening_slider.get_value(),
            if cfg!(target_os = "android") { 0 } else { 25 }
        );
        assert_eq!(*values.aspect_ratio.get_value(), AspectRatio::R16_9);
        assert_eq!(*values.anti_aliasing.get_value(), AntiAliasing::None);
        assert_eq!(
            *values.use_asynchronous_gpu_emulation.get_value(),
            !cfg!(target_os = "android")
        );
        assert_eq!(
            *values.fullscreen_mode.get_value(),
            if cfg!(target_os = "windows") {
                FullscreenMode::Borderless
            } else {
                FullscreenMode::Exclusive
            }
        );
        assert_eq!(
            (
                *values.bg_red.get_value(),
                *values.bg_green.get_value(),
                *values.bg_blue.get_value(),
            ),
            (0, 0, 0)
        );

        assert_eq!(
            *values.gpu_accuracy.get_value(),
            if cfg!(target_os = "android") {
                GpuAccuracy::Low
            } else {
                GpuAccuracy::High
            }
        );
        assert_eq!(*values.dma_accuracy.get_value(), DmaAccuracy::Default);
        assert_eq!(
            *values.gpu_fence_behavior.get_value(),
            GpuFenceBehavior::Default
        );
        assert_eq!(
            *values.vram_usage_mode.get_value(),
            VramUsageMode::Conservative
        );
        assert_eq!(*values.nvdec_emulation.get_value(), NvdecEmulation::Gpu);
        assert_eq!(
            *values.max_anisotropy.get_value(),
            if cfg!(target_os = "android") {
                AnisotropyMode::Default
            } else {
                AnisotropyMode::Automatic
            }
        );
        assert_eq!(*values.accelerate_astc.get_value(), AstcDecodeMode::Gpu);
        assert_eq!(
            *values.frame_pacing_mode.get_value(),
            FramePacingMode::Target_Auto
        );
        assert_eq!(
            *values.astc_recompression.get_value(),
            AstcRecompression::Uncompressed
        );
        assert!(!*values.sync_memory_operations.get_value());
        assert!(!*values.renderer_force_max_clock.get_value());
        assert!(*values.use_disk_shader_cache.get_value());
        assert!(*values.use_vulkan_driver_pipeline_cache.get_value());
        assert!(!*values.enable_compute_pipelines.get_value());
        assert!(!*values.use_video_framerate.get_value());
        assert_eq!(
            *values.use_reactive_flushing.get_value(),
            !cfg!(target_os = "android")
        );
        assert!(*values.barrier_feedback_loops.get_value());
        assert!(!*values.enable_buffer_history.get_value());
        assert!(!*values.enable_gpu_buffer_readback.get_value());

        assert!(!*values.skip_cpu_inner_invalidation.get_value());
        assert!(!*values.async_presentation.get_value());
        assert!(!*values.fix_bloom_effects.get_value());
        assert!(!*values.emulate_bgr565.get_value());
        assert_eq!(
            *values.rescale_hack.get_value(),
            cfg!(target_os = "android")
        );
        assert!(!*values.use_asynchronous_shaders.get_value());
        assert_eq!(
            *values.gpu_unswizzle_texture_size.get_value(),
            GpuUnswizzleSize::Large
        );
        assert_eq!(
            *values.gpu_unswizzle_stream_size.get_value(),
            GpuUnswizzle::Medium
        );
        assert_eq!(
            *values.gpu_unswizzle_chunk_size.get_value(),
            GpuUnswizzleChunk::Medium
        );
        assert!(!*values.gpu_unswizzle_enabled.get_value());

        assert_eq!(*values.gpu_log_level.get_value(), GpuLogLevel::Off);
        assert!(*values.gpu_log_vulkan_calls.get_value());
        assert!(!*values.gpu_log_shader_dumps.get_value());
        assert!(*values.gpu_log_memory_tracking.get_value());
        assert!(*values.gpu_log_driver_debug.get_value());
        assert_eq!(*values.gpu_log_ring_buffer_size.get_value(), 512);

        assert_eq!(
            *values.dyna_state.get_value(),
            if cfg!(any(target_os = "android", target_os = "macos")) {
                ExtendedDynamicState::Disabled
            } else {
                ExtendedDynamicState::EDS2
            }
        );
        assert_eq!(*values.sample_shading.get_value(), 0);
        assert_eq!(
            *values.vertex_input_dynamic_state.get_value(),
            !cfg!(target_os = "android")
        );
    }

    #[test]
    fn gpu_level_high_only_matches_high() {
        let mut values = Values::default();
        values.current_gpu_accuracy = GpuAccuracy::High;
        assert!(is_gpu_level_high(&values));

        values.current_gpu_accuracy = GpuAccuracy::Low;
        assert!(!is_gpu_level_high(&values));
    }

    #[test]
    fn fastmem_enablement_matches_upstream_accuracy_switches() {
        let mut values = Values::default();
        values.cpu_debug_mode.set_value(false);
        values.cpu_accuracy.set_value(CpuAccuracy::Unsafe);
        values.cpuopt_unsafe_host_mmu.set_value(false);
        assert!(!is_fastmem_enabled(&values));

        values.cpuopt_unsafe_host_mmu.set_value(true);
        assert!(is_fastmem_enabled(&values));

        values.cpu_debug_mode.set_value(true);
        values.cpuopt_fastmem.set_value(false);
        assert!(!is_fastmem_enabled(&values));
        values.cpuopt_fastmem.set_value(true);
        assert!(is_fastmem_enabled(&values));
    }

    #[test]
    fn update_gpu_accuracy_publishes_the_selected_level_to_the_gpu() {
        let mut values = Values::default();
        values.gpu_accuracy.set_value(GpuAccuracy::Low);
        values.current_gpu_accuracy = GpuAccuracy::High;

        update_gpu_accuracy(&mut values);

        assert_eq!(values.current_gpu_accuracy, GpuAccuracy::Low);

        values.gpu_accuracy.set_value(GpuAccuracy::High);
        update_gpu_accuracy(&mut values);

        assert_eq!(values.current_gpu_accuracy, GpuAccuracy::High);
        assert!(is_gpu_level_high(&values));
    }

    #[test]
    fn speed_limit_selects_the_active_eden_mode() {
        assert_eq!(speed_limit_for_mode(SpeedMode::Standard, 100, 200, 50), 100);
        assert_eq!(speed_limit_for_mode(SpeedMode::Turbo, 100, 200, 50), 200);
        assert_eq!(speed_limit_for_mode(SpeedMode::Slow, 100, 200, 50), 50);
    }

    #[test]
    fn update_rescaling_info_publishes_the_selected_resolution() {
        let mut values = Values::default();
        values.resolution_setup.set_value(ResolutionSetup::Res3_2X);

        update_rescaling_info(&mut values);

        assert_eq!(values.resolution_info.up_scale, 3);
        assert_eq!(values.resolution_info.down_shift, 1);
        assert_eq!(values.resolution_info.up_factor, 1.5);
        assert!(values.resolution_info.active);
    }

    #[test]
    fn resolution_setup_numeric_values_and_fractional_scales_match_upstream() {
        assert_eq!(ResolutionSetup::Res1_4X as u32, 0);
        assert_eq!(ResolutionSetup::Res1X as u32, 3);
        assert_eq!(ResolutionSetup::Res5_4X as u32, 4);
        assert_eq!(ResolutionSetup::Res8X as u32, 12);

        let mut quarter = ResolutionScalingInfo::default();
        translate_resolution_info(ResolutionSetup::Res1_4X, &mut quarter);
        assert_eq!((quarter.up_scale, quarter.down_shift), (1, 2));
        assert_eq!((quarter.up_factor, quarter.down_factor), (0.25, 4.0));
        assert!(quarter.downscale);

        let mut five_quarters = ResolutionScalingInfo::default();
        translate_resolution_info(ResolutionSetup::Res5_4X, &mut five_quarters);
        assert_eq!((five_quarters.up_scale, five_quarters.down_shift), (5, 2));
        assert_eq!(
            (five_quarters.up_factor, five_quarters.down_factor),
            (1.25, 0.8)
        );
        assert!(!five_quarters.downscale);
    }

    #[test]
    fn network_category_persists_interface_and_switchable_airplane_mode() {
        let mut values = Values::default();
        let mut labels = Vec::new();
        let mut switchable = Vec::new();
        values.for_each_setting_in_category_mut(Category::Network, |setting| {
            labels.push(setting.label().to_string());
            switchable.push(setting.switchable());
        });
        assert_eq!(labels, ["network_interface", "airplane_mode"]);
        assert_eq!(switchable, [false, true]);
    }

    #[test]
    fn windows_only_sdl_input_settings_match_upstream_defaults_and_persistence() {
        let mut values = Values::default();

        assert!(!*values.disable_wgi_xinput.get_value());
        assert!(!*values.enable_raw_input.get_value());
        assert_eq!(values.disable_wgi_xinput.save, cfg!(target_os = "windows"));
        assert_eq!(values.enable_raw_input.save, cfg!(target_os = "windows"));

        let mut labels = Vec::new();
        values.for_each_setting_in_category_mut(Category::Controls, |setting| {
            labels.push(setting.label().to_string());
        });
        assert!(labels.iter().any(|label| label == "disable_wgi_xinput"));
        assert!(labels.iter().any(|label| label == "enable_raw_input"));
    }

    #[test]
    fn library_applet_category_matches_upstream_switchability_and_defaults() {
        let mut values = Values::default();
        let mut entries = Vec::new();
        values.for_each_setting_in_category_mut(Category::LibraryApplet, |setting| {
            entries.push((setting.label().to_string(), setting.switchable()));
        });
        assert_eq!(entries.len(), 16);
        for label in [
            "cabinet_applet_mode",
            "controller_applet_mode",
            "error_applet_mode",
            "player_select_applet_mode",
            "swkbd_applet_mode",
            "mii_edit_applet_mode",
            "web_applet_mode",
            "photo_viewer_applet_mode",
            "offline_web_applet_mode",
            "enable_overlay",
        ] {
            assert!(entries
                .iter()
                .any(|entry| entry == &(label.to_string(), true)));
        }
        for label in [
            "data_erase_applet_mode",
            "net_connect_applet_mode",
            "shop_applet_mode",
            "login_share_applet_mode",
            "wifi_web_auth_applet_mode",
            "my_page_applet_mode",
        ] {
            assert!(entries
                .iter()
                .any(|entry| entry == &(label.to_string(), false)));
        }
        assert_eq!(*values.net_connect_applet_mode.get_value(), AppletMode::LLE);
        assert_eq!(
            *values.player_select_applet_mode.get_value(),
            AppletMode::LLE
        );
        assert_eq!(*values.swkbd_applet_mode.get_value(), AppletMode::HLE);
    }

    #[test]
    fn disable_web_applet_matches_upstream_default_and_category() {
        let mut values = Values::default();
        assert!(*values.disable_web_applet.get_value());

        let mut labels = Vec::new();
        values.for_each_setting_in_category_mut(Category::Debugging, |setting| {
            labels.push(setting.label().to_string());
        });
        assert!(labels.iter().any(|label| label == "disable_web_applet"));
    }

    #[test]
    fn current_program_id_round_trips_for_global_dump_readers() {
        const HOMEBREW_PROGRAM_ID: u64 = 0x0000_0000_4842_5257;
        let previous = get_current_program_id();

        set_current_program_id(HOMEBREW_PROGRAM_ID);
        assert_eq!(get_current_program_id(), HOMEBREW_PROGRAM_ID);

        set_current_program_id(previous);
    }
}
