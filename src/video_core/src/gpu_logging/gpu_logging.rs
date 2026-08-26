// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `video_core/gpu_logging/gpu_logging.{h,cpp}`.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::fs::path_util::{get_ruzu_path, RuzuPath};
use common::settings_enums::GpuLogLevel;
use log::{error, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LogLevel {
    Off = 0,
    Errors = 1,
    Standard = 2,
    Verbose = 3,
    All = 4,
}

impl From<GpuLogLevel> for LogLevel {
    fn from(value: GpuLogLevel) -> Self {
        match value {
            GpuLogLevel::Off => Self::Off,
            GpuLogLevel::Errors => Self::Errors,
            GpuLogLevel::Standard => Self::Standard,
            GpuLogLevel::Verbose => Self::Verbose,
            GpuLogLevel::All => Self::All,
        }
    }
}

impl LogLevel {
    fn from_raw(value: u8) -> Self {
        match value {
            1 => Self::Errors,
            2 => Self::Standard,
            3 => Self::Verbose,
            4 => Self::All,
            _ => Self::Off,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Errors => "Errors",
            Self::Standard => "Standard",
            Self::Verbose => "Verbose",
            Self::All => "All",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DriverType {
    Unknown = 0,
    Turnip = 1,
    Qualcomm = 2,
}

impl DriverType {
    fn from_raw(value: u8) -> Self {
        match value {
            1 => Self::Turnip,
            2 => Self::Qualcomm,
            _ => Self::Unknown,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Turnip => "Turnip (Mesa Freedreno)",
            Self::Qualcomm => "Qualcomm Proprietary",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VulkanCallEntry {
    pub timestamp: Duration,
    pub call_name: String,
    pub parameters: String,
    pub result: i32,
    pub thread_id: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryAllocationEntry {
    pub memory_handle: usize,
    pub size: u64,
    pub memory_flags: u32,
    pub timestamp: Duration,
    pub is_device_local: bool,
    pub is_host_visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuStateSnapshot {
    pub recent_calls: Vec<VulkanCallEntry>,
    pub active_shaders: Vec<String>,
    pub pipeline_state: String,
    pub memory_status: String,
    pub driver_debug_info: String,
    pub timestamp: Duration,
    pub driver_type: DriverType,
}

impl Default for GpuStateSnapshot {
    fn default() -> Self {
        Self {
            recent_calls: Vec::new(),
            active_shaders: Vec::new(),
            pipeline_state: String::new(),
            memory_status: String::new(),
            driver_debug_info: String::new(),
            timestamp: Duration::ZERO,
            driver_type: DriverType::Unknown,
        }
    }
}

#[derive(Default)]
struct RingBufferState {
    calls: Vec<Option<VulkanCallEntry>>,
    index: usize,
    total_vulkan_calls: u64,
}

#[derive(Default)]
struct MemoryState {
    allocations: HashMap<usize, MemoryAllocationEntry>,
    total_allocations: u64,
    total_deallocations: u64,
    current_allocated_bytes: u64,
    peak_allocated_bytes: u64,
}

#[derive(Default)]
struct StoredState {
    driver_debug_info: String,
    pipeline_state: String,
}

#[derive(Default)]
struct FileState {
    file: Option<File>,
    bytes_written: u64,
}

pub struct GpuLogger {
    initialized: AtomicBool,
    current_level: AtomicU8,
    detected_driver: AtomicU8,
    ring_buffer_size: AtomicUsize,
    track_vulkan_calls: AtomicBool,
    track_memory: AtomicBool,
    capture_driver_debug: AtomicBool,
    ring_buffer: Mutex<RingBufferState>,
    memory: Mutex<MemoryState>,
    file: Mutex<FileState>,
    extensions: Mutex<HashSet<String>>,
    state: Mutex<StoredState>,
}

impl Default for GpuLogger {
    fn default() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            current_level: AtomicU8::new(LogLevel::Off as u8),
            detected_driver: AtomicU8::new(DriverType::Unknown as u8),
            ring_buffer_size: AtomicUsize::new(512),
            track_vulkan_calls: AtomicBool::new(true),
            track_memory: AtomicBool::new(false),
            capture_driver_debug: AtomicBool::new(false),
            ring_buffer: Mutex::new(RingBufferState::default()),
            memory: Mutex::new(MemoryState::default()),
            file: Mutex::new(FileState::default()),
            extensions: Mutex::new(HashSet::new()),
            state: Mutex::new(StoredState::default()),
        }
    }
}

pub fn get_instance() -> &'static GpuLogger {
    static LOGGER: OnceLock<GpuLogger> = OnceLock::new();
    LOGGER.get_or_init(GpuLogger::default)
}

fn timestamp_now() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

fn thread_id() -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut hasher);
    hasher.finish() as u32
}

impl GpuLogger {
    pub fn initialize(&self, level: LogLevel, driver: DriverType) {
        if self.initialized.load(Ordering::Acquire) {
            warn!("[GPU Logging] Already initialized");
            return;
        }

        self.current_level.store(level as u8, Ordering::Release);
        self.detected_driver.store(driver as u8, Ordering::Release);
        if level == LogLevel::Off {
            return;
        }

        let log_dir = get_ruzu_path(RuzuPath::LogDir);
        if let Err(err) = std::fs::create_dir_all(log_dir.join("gpu_crashes")) {
            error!("[GPU Logging] Failed to create GPU log directories: {err}");
            return;
        }

        let gpu_log_path = log_dir.join("ruzu_gpu.log");
        let old_log_path = log_dir.join("ruzu_gpu.log.old.txt");
        let _ = std::fs::remove_file(&old_log_path);
        let _ = std::fs::rename(&gpu_log_path, &old_log_path);

        let file = match OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&gpu_log_path)
        {
            Ok(file) => file,
            Err(err) => {
                error!("[GPU Logging] Failed to open GPU log file: {err}");
                return;
            }
        };

        {
            let mut file_state = self.file.lock().expect("GPU log file lock poisoned");
            file_state.file = Some(file);
        }
        {
            let size = self.ring_buffer_size.load(Ordering::Acquire);
            let mut ring = self
                .ring_buffer
                .lock()
                .expect("GPU log ring buffer lock poisoned");
            ring.calls.resize(size, None);
        }

        self.write_to_log(&format!(
            "=== Ruzu GPU Logging Started ===\n\
             Timestamp: {}\n\
             Log Level: {}\n\
             Driver: {}\n\
             Ring Buffer Size: {}\n\
             ================================\n\n",
            self.format_timestamp(timestamp_now()),
            level.name(),
            driver.name(),
            self.ring_buffer_size.load(Ordering::Acquire),
        ));

        self.initialized.store(true, Ordering::Release);
        info!(
            "[GPU Logging] Initialized with level: {}, driver: {}",
            level.name(),
            driver.name()
        );
    }

    pub fn shutdown(&self) {
        if !self.initialized.load(Ordering::Acquire) {
            return;
        }

        let stats = {
            let ring = self
                .ring_buffer
                .lock()
                .expect("GPU log ring buffer lock poisoned");
            let memory = self.memory.lock().expect("GPU memory log lock poisoned");
            let bytes_written = self
                .file
                .lock()
                .expect("GPU log file lock poisoned")
                .bytes_written;
            format!(
                "\n=== GPU Logging Statistics ===\n\
                 Total Vulkan Calls: {}\n\
                 Total Memory Allocations: {}\n\
                 Total Memory Deallocations: {}\n\
                 Peak Memory Usage: {}\n\
                 Current Memory Usage: {}\n\
                 Log Size: {} bytes\n\
                 ==============================\n",
                ring.total_vulkan_calls,
                memory.total_allocations,
                memory.total_deallocations,
                self.format_memory_size(memory.peak_allocated_bytes),
                self.format_memory_size(memory.current_allocated_bytes),
                bytes_written,
            )
        };
        self.write_to_log(&stats);

        let mut file_state = self.file.lock().expect("GPU log file lock poisoned");
        if let Some(mut file) = file_state.file.take() {
            let _ = file.flush();
        }
        self.initialized.store(false, Ordering::Release);
        info!("[GPU Logging] Shutdown complete");
    }

    pub fn log_vulkan_call(&self, call_name: &str, parameters: &str, result: i32) {
        let level = self.get_log_level();
        if !self.is_initialized()
            || level == LogLevel::Off
            || !self.track_vulkan_calls.load(Ordering::Acquire)
        {
            return;
        }
        if level != LogLevel::Verbose
            && level != LogLevel::All
            && !call_name.contains("vkCmd")
            && !call_name.contains("vkCreate")
            && !call_name.contains("vkDestroy")
        {
            return;
        }

        let timestamp = timestamp_now();
        let thread_id = thread_id();
        {
            let mut ring = self
                .ring_buffer
                .lock()
                .expect("GPU log ring buffer lock poisoned");
            let size = self.ring_buffer_size.load(Ordering::Acquire);
            let index = ring.index;
            ring.calls[index] = Some(VulkanCallEntry {
                timestamp,
                call_name: call_name.to_owned(),
                parameters: parameters.to_owned(),
                result,
                thread_id,
            });
            ring.index = (index + 1) % size;
            ring.total_vulkan_calls = ring.total_vulkan_calls.wrapping_add(1);
        }
        self.write_to_log(&format!(
            "[{}] [Vulkan] [Thread:{}] {}({}) -> {}\n",
            self.format_timestamp(timestamp),
            thread_id,
            call_name,
            parameters,
            result
        ));
    }

    pub fn log_memory_allocation(&self, memory: usize, size: u64, memory_flags: u32) {
        if !self.is_initialized()
            || self.get_log_level() == LogLevel::Off
            || !self.track_memory.load(Ordering::Acquire)
        {
            return;
        }
        let timestamp = timestamp_now();
        let is_device_local = memory_flags & 0x1 != 0;
        let is_host_visible = memory_flags & 0x2 != 0;
        {
            let mut state = self.memory.lock().expect("GPU memory log lock poisoned");
            state.allocations.insert(
                memory,
                MemoryAllocationEntry {
                    memory_handle: memory,
                    size,
                    memory_flags,
                    timestamp,
                    is_device_local,
                    is_host_visible,
                },
            );
            state.total_allocations = state.total_allocations.wrapping_add(1);
            state.current_allocated_bytes = state.current_allocated_bytes.wrapping_add(size);
            state.peak_allocated_bytes = state
                .peak_allocated_bytes
                .max(state.current_allocated_bytes);
        }
        self.write_to_log(&format!(
            "[{}] [Memory] Allocated {} at {:#x} (Device:{}, Host:{})\n",
            self.format_timestamp(timestamp),
            self.format_memory_size(size),
            memory,
            if is_device_local { "Yes" } else { "No" },
            if is_host_visible { "Yes" } else { "No" },
        ));
    }

    pub fn log_memory_deallocation(&self, memory: usize) {
        if !self.is_initialized()
            || self.get_log_level() == LogLevel::Off
            || !self.track_memory.load(Ordering::Acquire)
        {
            return;
        }
        let timestamp = timestamp_now();
        let size = {
            let mut state = self.memory.lock().expect("GPU memory log lock poisoned");
            match state.allocations.remove(&memory) {
                Some(entry) => {
                    state.current_allocated_bytes =
                        state.current_allocated_bytes.wrapping_sub(entry.size);
                    state.total_deallocations = state.total_deallocations.wrapping_add(1);
                    entry.size
                }
                None => 0,
            }
        };
        if size > 0 {
            self.write_to_log(&format!(
                "[{}] [Memory] Deallocated {} at {:#x}\n",
                self.format_timestamp(timestamp),
                self.format_memory_size(size),
                memory
            ));
        }
    }

    pub fn log_shader_compilation(&self, shader_name: &str, shader_info: &str) {
        if !self.is_initialized() || self.get_log_level() < LogLevel::Verbose {
            return;
        }
        self.write_to_log(&format!(
            "[{}] [Shader] Compiled: {} ({})\n",
            self.format_timestamp(timestamp_now()),
            shader_name,
            shader_info
        ));
    }

    pub fn log_pipeline_state_change(&self, state_info: &str) {
        if !self.is_initialized() || self.get_log_level() == LogLevel::Off {
            return;
        }
        self.state
            .lock()
            .expect("GPU state log lock poisoned")
            .pipeline_state = state_info.to_owned();
        if self.get_log_level() >= LogLevel::Verbose {
            self.write_to_log(&format!(
                "[{}] [Pipeline] State change: {}\n",
                self.format_timestamp(timestamp_now()),
                state_info
            ));
        }
    }

    pub fn log_driver_debug_info(&self, debug_info: &str) {
        if !self.is_initialized() || self.get_log_level() == LogLevel::Off {
            return;
        }
        self.state
            .lock()
            .expect("GPU state log lock poisoned")
            .driver_debug_info = debug_info.to_owned();
        if self.capture_driver_debug.load(Ordering::Acquire) {
            self.write_to_log(&format!(
                "[{}] [Driver] {}\n",
                self.format_timestamp(timestamp_now()),
                debug_info
            ));
        }
    }

    pub fn log_extension_usage(&self, extension_name: &str, function_name: &str) {
        if !self.is_initialized() || self.get_log_level() == LogLevel::Off {
            return;
        }
        let timestamp = timestamp_now();
        let is_first_use = self
            .extensions
            .lock()
            .expect("GPU extension log lock poisoned")
            .insert(extension_name.to_owned());
        if is_first_use {
            self.write_to_log(&format!(
                "[{}] [Extension] First use of {} in {}\n",
                self.format_timestamp(timestamp),
                extension_name,
                function_name
            ));
            info!(
                "[GPU Logging] First use of extension {} in {}",
                extension_name, function_name
            );
        } else if self.get_log_level() >= LogLevel::Verbose {
            self.write_to_log(&format!(
                "[{}] [Extension] {} used in {}\n",
                self.format_timestamp(timestamp),
                extension_name,
                function_name
            ));
        }
    }

    pub fn log_render_pass_begin(&self, render_pass_info: &str) {
        if !self.is_initialized()
            || self.get_log_level() == LogLevel::Off
            || (!self.track_vulkan_calls.load(Ordering::Acquire)
                && self.get_log_level() < LogLevel::Verbose)
        {
            return;
        }
        self.write_to_log(&format!(
            "[{}] [RenderPass] Begin: {}\n",
            self.format_timestamp(timestamp_now()),
            render_pass_info
        ));
    }

    pub fn log_render_pass_end(&self) {
        if !self.is_initialized()
            || self.get_log_level() == LogLevel::Off
            || (!self.track_vulkan_calls.load(Ordering::Acquire)
                && self.get_log_level() < LogLevel::Verbose)
        {
            return;
        }
        self.write_to_log(&format!(
            "[{}] [RenderPass] End\n",
            self.format_timestamp(timestamp_now())
        ));
    }

    pub fn log_pipeline_bind(&self, is_compute: bool, pipeline_info: &str) {
        if !self.is_initialized()
            || self.get_log_level() == LogLevel::Off
            || (!self.track_vulkan_calls.load(Ordering::Acquire)
                && self.get_log_level() < LogLevel::Verbose)
        {
            return;
        }
        let pipeline_type = if is_compute { "Compute" } else { "Graphics" };
        self.write_to_log(&format!(
            "[{}] [Pipeline] Bind {} pipeline: {}\n",
            self.format_timestamp(timestamp_now()),
            pipeline_type,
            pipeline_info
        ));
    }

    pub fn log_descriptor_set_bind(&self, descriptor_info: &str) {
        if self.is_initialized() && self.get_log_level() >= LogLevel::Verbose {
            self.write_to_log(&format!(
                "[{}] [Descriptor] Bind: {}\n",
                self.format_timestamp(timestamp_now()),
                descriptor_info
            ));
        }
    }

    pub fn log_pipeline_barrier(&self, barrier_info: &str) {
        if self.is_initialized() && self.get_log_level() >= LogLevel::Verbose {
            self.write_to_log(&format!(
                "[{}] [Barrier] {}\n",
                self.format_timestamp(timestamp_now()),
                barrier_info
            ));
        }
    }

    pub fn log_image_operation(&self, operation: &str, image_info: &str) {
        if !self.is_initialized()
            || self.get_log_level() == LogLevel::Off
            || (!self.track_vulkan_calls.load(Ordering::Acquire)
                && self.get_log_level() < LogLevel::Verbose)
        {
            return;
        }
        self.write_to_log(&format!(
            "[{}] [Image] {}: {}\n",
            self.format_timestamp(timestamp_now()),
            operation,
            image_info
        ));
    }

    pub fn log_clear_operation(&self, clear_info: &str) {
        if !self.is_initialized()
            || self.get_log_level() == LogLevel::Off
            || (!self.track_vulkan_calls.load(Ordering::Acquire)
                && self.get_log_level() < LogLevel::Verbose)
        {
            return;
        }
        self.write_to_log(&format!(
            "[{}] [Clear] {}\n",
            self.format_timestamp(timestamp_now()),
            clear_info
        ));
    }

    pub fn get_current_snapshot(&self) -> GpuStateSnapshot {
        let recent_calls = {
            let ring = self
                .ring_buffer
                .lock()
                .expect("GPU log ring buffer lock poisoned");
            let size = self.ring_buffer_size.load(Ordering::Acquire);
            let mut calls = Vec::with_capacity(size);
            for index in ring.index..size {
                if let Some(call) = ring.calls[index].as_ref() {
                    calls.push(call.clone());
                }
            }
            for index in 0..ring.index {
                if let Some(call) = ring.calls[index].as_ref() {
                    calls.push(call.clone());
                }
            }
            calls
        };
        let memory_status = {
            let memory = self.memory.lock().expect("GPU memory log lock poisoned");
            format!(
                "Total Allocations: {}\nCurrent Usage: {}\nPeak Usage: {}\nActive Allocations: {}\n",
                memory.total_allocations,
                self.format_memory_size(memory.current_allocated_bytes),
                self.format_memory_size(memory.peak_allocated_bytes),
                memory.allocations.len()
            )
        };
        let state = self.state.lock().expect("GPU state log lock poisoned");
        GpuStateSnapshot {
            recent_calls,
            active_shaders: Vec::new(),
            pipeline_state: if state.pipeline_state.is_empty() {
                "No pipeline state logged yet".to_owned()
            } else {
                state.pipeline_state.clone()
            },
            memory_status,
            driver_debug_info: if state.driver_debug_info.is_empty() {
                "No driver debug info logged yet".to_owned()
            } else {
                state.driver_debug_info.clone()
            },
            timestamp: timestamp_now(),
            driver_type: self.get_driver_type(),
        }
    }

    pub fn dump_state_to_file(&self, crash_reason: &str) {
        let crashes_dir = get_ruzu_path(RuzuPath::LogDir).join("gpu_crashes");
        if let Err(err) = std::fs::create_dir_all(&crashes_dir) {
            error!("[GPU Logging] Failed to create crash dump directory: {err}");
            return;
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let crash_dump_path = crashes_dir.join(format!("crash_{timestamp}.gpu-dump"));
        let mut crash_file = match File::create(&crash_dump_path) {
            Ok(file) => file,
            Err(err) => {
                error!("[GPU Logging] Failed to create crash dump file: {err}");
                return;
            }
        };
        let snapshot = self.get_current_snapshot();
        let _ = writeln!(
            crash_file,
            "=== GPU CRASH DUMP ===\nTimestamp: {}\nReason: {}\nDriver: {}\n",
            self.format_timestamp(snapshot.timestamp),
            crash_reason,
            snapshot.driver_type.name()
        );
        let _ = writeln!(
            crash_file,
            "=== RECENT VULKAN API CALLS (Last {}) ===",
            snapshot.recent_calls.len()
        );
        for call in &snapshot.recent_calls {
            let _ = writeln!(
                crash_file,
                "[{}] [Thread:{}] {}({}) -> {}",
                self.format_timestamp(call.timestamp),
                call.thread_id,
                call.call_name,
                call.parameters,
                call.result
            );
        }
        let _ = write!(
            crash_file,
            "\n=== MEMORY STATUS ===\n{}\n=== PIPELINE STATE ===\n{}\n\
             === DRIVER DEBUG INFO ===\n{}\n",
            snapshot.memory_status, snapshot.pipeline_state, snapshot.driver_debug_info
        );
        let _ = crash_file.flush();
        error!(
            "[GPU Logging] Crash dump written to: {}",
            crash_dump_path.display()
        );
    }

    pub fn set_log_level(&self, level: LogLevel) {
        self.current_level.store(level as u8, Ordering::Release);
    }

    pub fn enable_vulkan_call_tracking(&self, enabled: bool) {
        self.track_vulkan_calls.store(enabled, Ordering::Release);
    }

    pub fn enable_memory_tracking(&self, enabled: bool) {
        self.track_memory.store(enabled, Ordering::Release);
    }

    pub fn enable_driver_debug_info(&self, enabled: bool) {
        self.capture_driver_debug.store(enabled, Ordering::Release);
    }

    pub fn set_ring_buffer_size(&self, entries: usize) {
        let mut ring = self
            .ring_buffer
            .lock()
            .expect("GPU log ring buffer lock poisoned");
        ring.calls.resize(entries, None);
        ring.index = 0;
        self.ring_buffer_size.store(entries, Ordering::Release);
    }

    pub fn get_log_level(&self) -> LogLevel {
        LogLevel::from_raw(self.current_level.load(Ordering::Acquire))
    }

    pub fn get_driver_type(&self) -> DriverType {
        DriverType::from_raw(self.detected_driver.load(Ordering::Acquire))
    }

    pub fn get_statistics(&self) -> String {
        let ring = self
            .ring_buffer
            .lock()
            .expect("GPU log ring buffer lock poisoned");
        let memory = self.memory.lock().expect("GPU memory log lock poisoned");
        format!(
            "Vulkan Calls: {}, Allocations: {}, Deallocations: {}, Current Memory: {}, Peak Memory: {}",
            ring.total_vulkan_calls,
            memory.total_allocations,
            memory.total_deallocations,
            self.format_memory_size(memory.current_allocated_bytes),
            self.format_memory_size(memory.peak_allocated_bytes)
        )
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    fn write_to_log(&self, message: &str) {
        let mut state = self.file.lock().expect("GPU log file lock poisoned");
        let wrote_message = match state.file.as_mut() {
            Some(file) => file.write_all(message.as_bytes()).is_ok(),
            None => false,
        };
        if wrote_message {
            state.bytes_written = state.bytes_written.wrapping_add(message.len() as u64);
            if state.bytes_written % (1024 * 1024) == 0 {
                if let Some(file) = state.file.as_mut() {
                    let _ = file.flush();
                }
            }
        }
    }

    pub fn format_timestamp(&self, timestamp: Duration) -> String {
        format!("{:4}.{:06}", timestamp.as_secs(), timestamp.subsec_micros())
    }

    pub fn format_memory_size(&self, bytes: u64) -> String {
        const KIB: u64 = 1024;
        const MIB: u64 = 1024 * KIB;
        const GIB: u64 = 1024 * MIB;
        if bytes >= GIB {
            format!("{:.2} GiB", bytes as f64 / GIB as f64)
        } else if bytes >= MIB {
            format!("{:.2} MiB", bytes as f64 / MIB as f64)
        } else if bytes >= KIB {
            format!("{:.2} KiB", bytes as f64 / KIB as f64)
        } else {
            format!("{bytes} B")
        }
    }
}

pub fn is_active() -> bool {
    *common::settings::values().gpu_log_level.get_value() != GpuLogLevel::Off
}

pub fn dump_spirv_shader(shader_hash: u64, spirv_code: &[u32]) {
    if spirv_code.is_empty() {
        return;
    }
    let dump_dir = get_ruzu_path(RuzuPath::DumpDir);
    static DUMP_DIR_CREATED: OnceLock<()> = OnceLock::new();
    DUMP_DIR_CREATED.get_or_init(|| {
        let _ = std::fs::create_dir_all(&dump_dir);
    });
    let program_id = common::settings::get_current_program_id();
    let shader_path = dump_dir.join(format!("{program_id:016x}_{shader_hash:016x}.spv"));
    let bytes = bytemuck::cast_slice(spirv_code);
    if let Err(err) = std::fs::write(&shader_path, bytes) {
        warn!(
            "[Shader Dump] Failed to write {}: {}",
            shader_path.display(),
            err
        );
    }
}

pub fn get_shader_stage_name(stage_index: usize) -> &'static str {
    const STAGE_NAMES: [&str; 5] = [
        "vertex",
        "tess_control",
        "tess_eval",
        "geometry",
        "fragment",
    ];
    STAGE_NAMES.get(stage_index).copied().unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_discriminants_match_eden() {
        assert_eq!(LogLevel::Off as u8, 0);
        assert_eq!(LogLevel::All as u8, 4);
        assert_eq!(DriverType::Unknown as u8, 0);
        assert_eq!(DriverType::Qualcomm as u8, 2);
    }

    #[test]
    fn stage_names_match_eden() {
        assert_eq!(get_shader_stage_name(0), "vertex");
        assert_eq!(get_shader_stage_name(4), "fragment");
        assert_eq!(get_shader_stage_name(5), "unknown");
    }

    #[test]
    fn memory_size_format_matches_eden_units() {
        let logger = GpuLogger::default();
        assert_eq!(logger.format_memory_size(5), "5 B");
        assert_eq!(logger.format_memory_size(1536), "1.50 KiB");
        assert_eq!(logger.format_memory_size(3 * 1024 * 1024), "3.00 MiB");
    }
}
