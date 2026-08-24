//! AArch64 host backend infrastructure.
//!
//! This module is intentionally separate from `backend::x64`: the existing
//! backend emits x86-64 machine code through rxbyak, while Apple Silicon needs
//! native AArch64 code generation, dispatcher stubs, and I-cache handling.

pub mod a32_address_space;
pub mod a32_core;
pub mod a32_interface;
pub mod a64_address_space;
pub mod a64_core;
pub mod a64_interface;
pub mod abi;
pub mod address_space;
pub mod block_of_code;
pub mod emit_arm64;
pub mod emit_arm64_a32;
pub mod emit_arm64_a32_coprocessor;
pub mod emit_arm64_a32_memory;
pub mod emit_arm64_a64;
pub mod emit_arm64_a64_memory;
pub mod emit_arm64_cryptography;
pub mod emit_arm64_data_processing;
pub mod emit_arm64_floating_point;
pub mod emit_arm64_memory;
pub mod emit_arm64_packed;
pub mod emit_arm64_saturation;
pub mod emit_arm64_vector;
pub mod emit_arm64_vector_floating_point;
pub mod emit_arm64_vector_saturation;
pub mod emit_context;
pub mod fast_hash;
pub mod fastmem;
pub mod fpsr_manager;
pub mod inst;
pub mod jit_state;
pub mod label;
pub mod prelude;
pub mod reg_alloc;
pub mod stack_layout;
