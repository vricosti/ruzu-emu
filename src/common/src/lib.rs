// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Utility types
pub mod alignment;
pub mod bit_field;
pub mod bit_set;
pub mod bit_util;
pub mod container_hash;
pub mod div_ceil;
pub mod elf;
pub mod env_flag;
pub mod error;
pub mod fixed_point;
pub mod hash;
pub mod math_util;
pub mod overflow;
pub mod point;
pub mod quaternion;
pub mod random;
pub mod stream;
pub mod swap;
pub mod typed_address;
pub mod types;
pub mod uint128;
pub mod vector_math;

// Settings
pub mod param_package;
pub mod settings;
pub mod settings_common;
pub mod settings_enums;
pub mod settings_input;
pub mod settings_setting;

// Container types
pub mod bounded_threadsafe_queue;
pub mod intrusive_list;
pub mod intrusive_red_black_tree;
pub mod lru_cache;
pub mod range_map;
pub mod range_sets;
pub mod ring_buffer;
pub mod scratch_buffer;
pub mod slot_vector;
pub mod thread_queue_list;
pub mod threadsafe_queue;
pub mod tree;

// Memory subsystem
pub mod address_space;
pub mod free_region_manager;
pub mod heap_tracker;
pub mod host_memory;
pub mod memory_detect;
pub mod multi_level_page_table;
pub mod page_table;
pub mod sparse_large_vector;

// Threading subsystem
pub mod detached_tasks;
pub mod fiber;
pub mod range_mutex;
pub mod spin_lock;
pub mod thread;
pub mod thread_worker;

// Compression
pub mod lz4_compression;
pub mod zstd_compression;

// System utilities
pub mod announce_multiplayer_room;
pub mod cityhash;
pub mod common_funcs;
pub mod cpu_features;
pub mod dynamic_library;
pub mod fastmem_registry;
pub mod hex_util;
pub mod input;
pub mod lock_order;
pub mod logging;
pub mod scm_rev;
pub mod assert;
pub mod steady_clock;
pub mod string_util;
pub mod telemetry;
pub mod time_zone;
pub mod tiny_mt;
pub mod trace;
pub mod uuid;
pub mod wall_clock;

// Filesystem
pub mod fs;

// Platform-specific
#[cfg(target_arch = "aarch64")]
pub mod arm64;
#[cfg(target_arch = "x86_64")]
pub mod x64;

pub use error::ResultCode;
pub use types::*;
