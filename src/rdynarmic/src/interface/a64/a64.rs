//! Public A64 JIT interface.
//!
//! Upstream owner: `dynarmic/interface/A64/a64.h`.

use crate::interface::a64::config::{UserConfig, Vector};
use crate::interface::halt_reason::HaltReason;

#[cfg(target_arch = "x86_64")]
pub struct Jit {
    pub(crate) inner: crate::backend::x64::a64_interface::A64Jit,
}

#[cfg(target_arch = "x86_64")]
impl Jit {
    pub fn new(config: UserConfig) -> Result<Self, String> {
        Ok(Self {
            inner: crate::backend::x64::a64_interface::A64Jit::new(config)?,
        })
    }

    pub fn run(&mut self) -> HaltReason {
        self.inner.run()
    }

    pub fn step(&mut self) -> HaltReason {
        self.inner.step()
    }

    pub fn clear_cache(&mut self) {
        self.inner.clear_cache();
    }

    pub fn invalidate_cache_range(&mut self, start_address: u64, length: usize) {
        self.inner.invalidate_cache_range(start_address, length);
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }

    pub fn halt_execution(&self, hr: HaltReason) {
        self.inner.halt_execution(hr);
    }

    pub fn clear_halt(&self, hr: HaltReason) {
        self.inner.clear_halt(hr);
    }

    pub fn read_halt_reason(&self) -> u32 {
        self.inner.read_halt_reason()
    }

    pub fn halt_reason_ptr(&self) -> *const u32 {
        self.inner.halt_reason_ptr()
    }

    pub fn jit_state_ptr(&self) -> *const u8 {
        self.inner.jit_state_ptr()
    }

    pub fn get_sp(&self) -> u64 {
        self.inner.get_sp()
    }

    pub fn set_sp(&mut self, value: u64) {
        self.inner.set_sp(value);
    }

    pub fn get_pc(&self) -> u64 {
        self.inner.get_pc()
    }

    pub fn set_pc(&mut self, value: u64) {
        self.inner.set_pc(value);
    }

    pub fn get_register(&self, index: usize) -> u64 {
        self.inner.get_register(index)
    }

    pub fn set_register(&mut self, index: usize, value: u64) {
        self.inner.set_register(index, value);
    }

    pub fn get_registers(&self) -> [u64; 31] {
        self.inner.get_registers()
    }

    pub fn set_registers(&mut self, value: [u64; 31]) {
        self.inner.set_registers(value);
    }

    pub fn get_vector(&self, index: usize) -> Vector {
        self.inner.get_vector(index)
    }

    pub fn set_vector(&mut self, index: usize, value: Vector) {
        self.inner.set_vector(index, value);
    }

    pub fn get_vectors(&self) -> [Vector; 32] {
        self.inner.get_vectors()
    }

    pub fn set_vectors(&mut self, value: [Vector; 32]) {
        self.inner.set_vectors(value);
    }

    pub fn get_vector_parts(&self, index: usize) -> (u64, u64) {
        self.inner.get_vector_parts(index)
    }

    pub fn set_vector_parts(&mut self, index: usize, lo: u64, hi: u64) {
        self.inner.set_vector_parts(index, lo, hi);
    }

    pub fn get_fpcr(&self) -> u32 {
        self.inner.get_fpcr()
    }

    pub fn set_fpcr(&mut self, value: u32) {
        self.inner.set_fpcr(value);
    }

    pub fn get_fpsr(&self) -> u32 {
        self.inner.get_fpsr()
    }

    pub fn set_fpsr(&mut self, value: u32) {
        self.inner.set_fpsr(value);
    }

    pub fn get_pstate(&self) -> u32 {
        self.inner.get_pstate()
    }

    pub fn set_pstate(&mut self, value: u32) {
        self.inner.set_pstate(value);
    }

    pub fn clear_exclusive_state(&mut self) {
        self.inner.clear_exclusive_state();
    }

    pub fn is_executing(&self) -> bool {
        self.inner.is_executing()
    }

    pub fn disassemble(&self) -> String {
        self.inner.disassemble()
    }
}

/// Public A64 JIT interface for the native AArch64 backend.
///
/// Upstream owner: `interface/A64/a64.h`; backend behavior remains in
/// `backend/arm64/a64_interface.rs`, matching Eden's host-specific `.cpp`.
#[cfg(target_arch = "aarch64")]
pub struct Jit {
    inner: crate::backend::arm64::a64_interface::A64Interface,
}

#[cfg(target_arch = "aarch64")]
impl Jit {
    pub fn new(config: UserConfig) -> Result<Self, String> {
        Ok(Self {
            inner: crate::backend::arm64::a64_interface::A64Interface::new(config)?,
        })
    }

    pub fn run(&mut self) -> HaltReason {
        self.inner.run().expect("A64 ARM64 run failed")
    }

    pub fn step(&mut self) -> HaltReason {
        self.inner.step().expect("A64 ARM64 step failed")
    }

    pub fn clear_cache(&mut self) {
        self.inner.clear_cache();
    }

    pub fn invalidate_cache_range(&mut self, start_address: u64, length: usize) {
        self.inner.invalidate_cache_range(start_address, length);
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }

    pub fn halt_execution(&self, hr: HaltReason) {
        self.inner.halt_execution(hr);
    }

    pub fn clear_halt(&self, hr: HaltReason) {
        self.inner.clear_halt(hr);
    }

    pub fn read_halt_reason(&self) -> u32 {
        self.inner.current_halt_reason().bits()
    }

    pub fn halt_reason_ptr(&self) -> *const u32 {
        self.inner.halt_reason_ptr()
    }

    pub fn jit_state_ptr(&self) -> *const u8 {
        self.inner.jit_state_ptr()
    }

    pub fn get_sp(&self) -> u64 {
        self.inner.sp()
    }

    pub fn set_sp(&mut self, value: u64) {
        self.inner.set_sp(value);
    }

    pub fn get_pc(&self) -> u64 {
        self.inner.pc()
    }

    pub fn set_pc(&mut self, value: u64) {
        self.inner.set_pc(value);
    }

    pub fn get_register(&self, index: usize) -> u64 {
        self.inner.get_register(index)
    }

    pub fn set_register(&mut self, index: usize, value: u64) {
        self.inner.set_register(index, value);
    }

    pub fn get_registers(&self) -> [u64; 31] {
        self.inner.get_registers()
    }

    pub fn set_registers(&mut self, value: [u64; 31]) {
        self.inner.set_registers(value);
    }

    pub fn get_vector(&self, index: usize) -> Vector {
        self.inner.get_vector(index)
    }

    pub fn set_vector(&mut self, index: usize, value: Vector) {
        self.inner.set_vector(index, value);
    }

    pub fn get_vectors(&self) -> [Vector; 32] {
        self.inner.get_vectors()
    }

    pub fn set_vectors(&mut self, value: [Vector; 32]) {
        self.inner.set_vectors(value);
    }

    pub fn get_vector_parts(&self, index: usize) -> (u64, u64) {
        let value = self.get_vector(index);
        (value[0], value[1])
    }

    pub fn set_vector_parts(&mut self, index: usize, lo: u64, hi: u64) {
        self.set_vector(index, [lo, hi]);
    }

    pub fn get_fpcr(&self) -> u32 {
        self.inner.fpcr()
    }

    pub fn set_fpcr(&mut self, value: u32) {
        self.inner.set_fpcr(value);
    }

    pub fn get_fpsr(&self) -> u32 {
        self.inner.fpsr()
    }

    pub fn set_fpsr(&mut self, value: u32) {
        self.inner.set_fpsr(value);
    }

    pub fn get_pstate(&self) -> u32 {
        self.inner.pstate()
    }

    pub fn set_pstate(&mut self, value: u32) {
        self.inner.set_pstate(value);
    }

    pub fn clear_exclusive_state(&mut self) {
        self.inner.clear_exclusive_state();
    }

    pub fn is_executing(&self) -> bool {
        self.inner.is_executing()
    }

    pub fn disassemble(&self) -> String {
        self.inner.disassemble()
    }
}
