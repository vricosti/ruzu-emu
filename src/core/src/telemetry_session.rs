//! Port of zuyu/src/core/telemetry_session.h and zuyu/src/core/telemetry_session.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-11
//!
//! Telemetry session management. Creates a session that collects telemetry fields
//! during emulation and submits them on shutdown via a backend visitor.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use common::fs as common_fs;
use common::settings;
use common::settings_enums;
use common::telemetry::{self, FieldCollection, FieldType, FieldValue, VisitorInterface};

/// Telemetry session that collects and submits telemetry data.
///
/// Corresponds to the C++ `TelemetrySession` class.
/// Non-copyable, non-movable (matching upstream).
pub struct TelemetrySession {
    /// Tracks all added fields for the session.
    field_collection: FieldCollection,
    /// Whether telemetry submission is enabled (cached from settings at construction).
    enable_telemetry: bool,
}

impl TelemetrySession {
    /// Create a new telemetry session.
    /// Reads `enable_telemetry` from the global settings singleton.
    pub fn new() -> Self {
        Self {
            field_collection: FieldCollection::new(),
            enable_telemetry: *settings::values().enable_telemetry.get_value(),
        }
    }

    /// Add initial telemetry info when starting up a title.
    ///
    /// Upstream `AddInitialInfo` takes `AppLoader&`, `FileSystemController&`, and
    /// `ContentProvider&` to extract the program ID and name. This simplified version
    /// takes program_id and program_name directly; reads settings from the global singleton.
    pub fn add_initial_info_basic(&mut self, program_id: u64, program_name: Option<&str>) {
        let vals = settings::values();
        // Log one-time top-level information
        self.add_field(
            FieldType::None,
            "TelemetryId",
            FieldValue::U64(get_telemetry_id()),
        );

        // Log one-time session start information
        let init_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.add_field(FieldType::Session, "Init_Time", FieldValue::I64(init_time));

        let formatted_program_id = format!("{:016X}", program_id);
        self.add_field(
            FieldType::Session,
            "ProgramId",
            FieldValue::String(formatted_program_id),
        );

        if let Some(name) = program_name {
            if !name.is_empty() {
                self.add_field(
                    FieldType::Session,
                    "ProgramName",
                    FieldValue::String(name.to_string()),
                );
            }
        }

        // Log application information
        telemetry::append_build_info(&mut self.field_collection);

        // Log user system information
        telemetry::append_cpu_info(&mut self.field_collection);
        telemetry::append_os_info(&mut self.field_collection);

        // Log user configuration information
        let field_type = FieldType::UserConfig;

        self.add_field(
            field_type,
            "Audio_SinkId",
            FieldValue::String(format!("{:?}", vals.sink_id.get_value())),
        );
        self.add_field(
            field_type,
            "Core_UseMultiCore",
            FieldValue::Bool(*vals.use_multi_core.get_value()),
        );
        self.add_field(
            field_type,
            "Renderer_Backend",
            FieldValue::String(translate_renderer(*vals.renderer_backend.get_value()).to_string()),
        );
        self.add_field(
            field_type,
            "Renderer_UseSpeedLimit",
            FieldValue::Bool(*vals.use_speed_limit.get_value()),
        );
        self.add_field(
            field_type,
            "Renderer_SpeedLimit",
            FieldValue::U64(*vals.speed_limit.get_value() as u64),
        );
        self.add_field(
            field_type,
            "Renderer_UseDiskShaderCache",
            FieldValue::Bool(*vals.use_disk_shader_cache.get_value()),
        );
        self.add_field(
            field_type,
            "Renderer_GPUAccuracyLevel",
            FieldValue::String(
                translate_gpu_accuracy_level(*vals.gpu_accuracy.get_value()).to_string(),
            ),
        );
        self.add_field(
            field_type,
            "Renderer_UseAsynchronousGpuEmulation",
            FieldValue::Bool(*vals.use_asynchronous_gpu_emulation.get_value()),
        );
        self.add_field(
            field_type,
            "Renderer_NvdecEmulation",
            FieldValue::String(
                translate_nvdec_emulation(*vals.nvdec_emulation.get_value()).to_string(),
            ),
        );
        self.add_field(
            field_type,
            "Renderer_AccelerateASTC",
            FieldValue::String(
                translate_astc_decode_mode(*vals.accelerate_astc.get_value()).to_string(),
            ),
        );
        self.add_field(
            field_type,
            "Renderer_UseVsync",
            FieldValue::String(translate_vsync_mode(*vals.vsync_mode.get_value()).to_string()),
        );
        self.add_field(
            field_type,
            "Renderer_UseAsynchronousShaders",
            FieldValue::Bool(*vals.use_asynchronous_shaders.get_value()),
        );
        self.add_field(
            field_type,
            "System_UseDockedMode",
            FieldValue::Bool(settings::is_docked_mode(&vals)),
        );
    }

    /// Add a field to the telemetry session.
    pub fn add_field(&mut self, field_type: FieldType, name: &str, value: FieldValue) {
        self.field_collection.add_field(field_type, name, value);
    }

    /// Submit a testcase.
    /// Returns true if submission succeeded.
    pub fn submit_testcase(&mut self) -> bool {
        // Web service submission is not implemented in the Rust port.
        // Matching C++ behavior when ENABLE_WEB_SERVICE is not defined.
        false
    }

    /// Get a reference to the field collection.
    pub fn field_collection(&self) -> &FieldCollection {
        &self.field_collection
    }

    /// Get a mutable reference to the field collection.
    pub fn field_collection_mut(&mut self) -> &mut FieldCollection {
        &mut self.field_collection
    }
}

impl Drop for TelemetrySession {
    fn drop(&mut self) {
        // Log one-time session end information
        let shutdown_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.add_field(
            FieldType::Session,
            "Shutdown_Time",
            FieldValue::I64(shutdown_time),
        );

        // Use a null visitor (no web service in Rust port).
        // Matching C++ behavior when ENABLE_WEB_SERVICE is not defined.
        let mut visitor = telemetry::NullVisitor;
        self.field_collection.accept(&mut visitor);

        if self.enable_telemetry {
            visitor.complete();
        }
    }
}

// Note: No Default impl since TelemetrySession requires &Values at construction.
// The C++ version has a default constructor, but the Rust version caches settings.

// --- Static helper functions matching the C++ anonymous namespace / static functions ---

fn translate_renderer(backend: settings_enums::RendererBackend) -> &'static str {
    match backend {
        settings_enums::RendererBackend::OpenGlGlsl
        | settings_enums::RendererBackend::OpenGlGlasm
        | settings_enums::RendererBackend::OpenGlSpirV => "OpenGL",
        settings_enums::RendererBackend::Vulkan => "Vulkan",
        settings_enums::RendererBackend::Metal => "Metal",
        settings_enums::RendererBackend::Null => "Null",
    }
}

fn translate_gpu_accuracy_level(accuracy: settings_enums::GpuAccuracy) -> &'static str {
    match accuracy {
        settings_enums::GpuAccuracy::Low => "Low",
        settings_enums::GpuAccuracy::High => "High",
    }
}

fn translate_nvdec_emulation(nvdec: settings_enums::NvdecEmulation) -> &'static str {
    match nvdec {
        settings_enums::NvdecEmulation::Off => "Off",
        settings_enums::NvdecEmulation::Cpu => "CPU",
        settings_enums::NvdecEmulation::Gpu => "GPU",
    }
}

fn translate_vsync_mode(mode: settings_enums::VSyncMode) -> &'static str {
    match mode {
        settings_enums::VSyncMode::Immediate => "Immediate",
        settings_enums::VSyncMode::Mailbox => "Mailbox",
        settings_enums::VSyncMode::Fifo => "FIFO",
        settings_enums::VSyncMode::FifoRelaxed => "FIFO Relaxed",
    }
}

fn translate_astc_decode_mode(mode: settings_enums::AstcDecodeMode) -> &'static str {
    match mode {
        settings_enums::AstcDecodeMode::Cpu => "CPU",
        settings_enums::AstcDecodeMode::Gpu => "GPU",
        settings_enums::AstcDecodeMode::CpuAsynchronous => "CPU Asynchronous",
    }
}

/// Generate a random telemetry ID.
/// The C++ version uses mbedtls CSPRNG; we use a simpler approach.
fn generate_telemetry_id() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    std::process::id().hash(&mut hasher);

    let id = hasher.finish();
    if id == 0 {
        1
    } else {
        id
    }
}

fn telemetry_id_path() -> PathBuf {
    common_fs::path_util::get_ruzu_path(common_fs::path_util::RuzuPath::ConfigDir)
        .join("telemetry_id")
}

/// Get the current telemetry ID, generating one if it doesn't exist.
pub fn get_telemetry_id() -> u64 {
    let filename = telemetry_id_path();

    let mut generate_new = !filename.exists();

    if !generate_new {
        match fs::File::open(&filename) {
            Ok(mut file) => {
                let mut buf = [0u8; 8];
                match file.read_exact(&mut buf) {
                    Ok(()) => {
                        let id = u64::from_le_bytes(buf);
                        if id == 0 {
                            log::error!("telemetry_id is 0. Generating a new one.");
                            generate_new = true;
                        } else {
                            return id;
                        }
                    }
                    Err(_) => {
                        log::error!("Failed to read telemetry_id");
                        generate_new = true;
                    }
                }
            }
            Err(e) => {
                log::error!("Failed to open telemetry_id: {}: {}", filename.display(), e);
                return 0;
            }
        }
    }

    if generate_new {
        let id = generate_telemetry_id();
        if let Some(parent) = filename.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match fs::File::create(&filename) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(&id.to_le_bytes()) {
                    log::error!("Failed to write telemetry_id to file: {}", e);
                }
            }
            Err(e) => {
                log::error!("Failed to open telemetry_id: {}: {}", filename.display(), e);
                return 0;
            }
        }
        return id;
    }

    0
}

/// Regenerate the telemetry ID and return the new value.
pub fn regenerate_telemetry_id() -> u64 {
    let new_id = generate_telemetry_id();
    let filename = telemetry_id_path();

    if let Some(parent) = filename.parent() {
        let _ = fs::create_dir_all(parent);
    }

    match fs::File::create(&filename) {
        Ok(mut file) => {
            if let Err(e) = file.write_all(&new_id.to_le_bytes()) {
                log::error!("Failed to write telemetry_id to file: {}", e);
            }
        }
        Err(e) => {
            log::error!("Failed to open telemetry_id: {}: {}", filename.display(), e);
            return 0;
        }
    }

    new_id
}

/// Verify login credentials.
/// Returns false since web service is not implemented in the Rust port.
pub fn verify_login(_username: &str, _token: &str) -> bool {
    // Matching C++ behavior when ENABLE_WEB_SERVICE is not defined.
    false
}
