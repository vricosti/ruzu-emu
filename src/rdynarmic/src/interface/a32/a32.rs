//! Public A32 JIT interface.
//!
//! Upstream owner: `dynarmic/interface/A32/a32.h`.

use crate::interface::a32::config::UserConfig;
use crate::interface::halt_reason::HaltReason;

#[cfg(target_arch = "x86_64")]
pub struct Jit {
    pub(crate) inner: crate::backend::x64::a32_interface::A32Jit,
    is_executing: bool,
}

#[cfg(target_arch = "x86_64")]
impl Jit {
    pub fn new(config: UserConfig) -> Result<Self, String> {
        Ok(Self {
            inner: crate::backend::x64::a32_interface::A32Jit::new(config)?,
            is_executing: false,
        })
    }

    pub fn run(&mut self) -> HaltReason {
        self.inner.run(&mut self.is_executing)
    }

    pub fn step(&mut self) -> HaltReason {
        self.inner.step(&mut self.is_executing)
    }

    pub fn clear_cache(&mut self) {
        self.inner.clear_cache();
    }

    pub fn invalidate_cache_range(&mut self, start_address: u32, length: usize) {
        self.inner.invalidate_cache_range(start_address, length);
    }

    pub fn reset(&mut self) {
        self.inner.reset(self.is_executing);
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

    pub fn regs(&self) -> &[u32; 16] {
        self.inner.regs()
    }

    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        self.inner.regs_mut()
    }

    pub fn ext_regs(&self) -> &[u32; 64] {
        self.inner.ext_regs()
    }

    pub fn ext_regs_mut(&mut self) -> &mut [u32; 64] {
        self.inner.ext_regs_mut()
    }

    pub fn get_register(&self, index: usize) -> u32 {
        self.inner.get_register(index)
    }

    pub fn set_register(&mut self, index: usize, value: u32) {
        self.inner.set_register(index, value);
    }

    pub fn get_pc(&self) -> u32 {
        self.inner.get_pc()
    }

    pub fn set_pc(&mut self, value: u32) {
        self.inner.set_pc(value);
    }

    pub fn get_cpsr(&self) -> u32 {
        self.inner.get_cpsr()
    }

    pub fn set_cpsr(&mut self, value: u32) {
        self.inner.set_cpsr(value);
    }

    pub fn get_fpscr(&self) -> u32 {
        self.inner.get_fpscr()
    }

    pub fn set_fpscr(&mut self, value: u32) {
        self.inner.set_fpscr(value);
    }

    pub fn get_ext_reg(&self, index: usize) -> u32 {
        self.inner.get_ext_reg(index)
    }

    pub fn set_ext_reg(&mut self, index: usize, value: u32) {
        self.inner.set_ext_reg(index, value);
    }

    pub fn clear_exclusive_state(&mut self) {
        self.inner.clear_exclusive_state();
    }

    pub fn is_executing(&self) -> bool {
        self.is_executing
    }

    pub fn disassemble(&self) -> String {
        self.inner.disassemble()
    }

    pub fn compile_block_only(&mut self) -> *const u8 {
        self.inner.compile_block_only()
    }

    pub fn dump_jit_block_map(&self, path: &str) -> std::io::Result<()> {
        self.inner.dump_jit_block_map(path)
    }
}

/// Public A32 JIT interface for the native AArch64 backend.
///
/// Upstream owner: `interface/A32/a32.h`; backend behavior remains in
/// `backend/arm64/a32_interface.rs`, matching Eden's host-specific `.cpp`.
#[cfg(target_arch = "aarch64")]
pub struct Jit {
    inner: crate::backend::arm64::a32_interface::A32Interface,
    is_executing: bool,
}

#[cfg(target_arch = "aarch64")]
impl Jit {
    pub fn new(config: UserConfig) -> Result<Self, String> {
        Ok(Self {
            inner: crate::backend::arm64::a32_interface::A32Interface::new(config)?,
            is_executing: false,
        })
    }

    pub fn run(&mut self) -> HaltReason {
        self.inner
            .run(&mut self.is_executing)
            .expect("A32 ARM64 run failed")
    }

    pub fn step(&mut self) -> HaltReason {
        self.inner
            .step(&mut self.is_executing)
            .expect("A32 ARM64 step failed")
    }

    pub fn clear_cache(&mut self) {
        self.inner.clear_cache();
    }

    pub fn invalidate_cache_range(&mut self, start_address: u32, length: usize) {
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

    pub fn get_register(&self, index: usize) -> u32 {
        self.inner.regs()[index]
    }

    pub fn regs(&self) -> &[u32; 16] {
        self.inner.regs()
    }

    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        self.inner.regs_mut()
    }

    pub fn set_register(&mut self, index: usize, value: u32) {
        self.inner.regs_mut()[index] = value;
    }

    pub fn get_pc(&self) -> u32 {
        self.inner.regs()[15]
    }

    pub fn set_pc(&mut self, value: u32) {
        self.inner.regs_mut()[15] = value;
    }

    pub fn get_cpsr(&self) -> u32 {
        self.inner.cpsr()
    }

    pub fn set_cpsr(&mut self, value: u32) {
        self.inner.set_cpsr(value);
    }

    pub fn get_fpscr(&self) -> u32 {
        self.inner.fpscr()
    }

    pub fn set_fpscr(&mut self, value: u32) {
        self.inner.set_fpscr(value);
    }

    pub fn get_ext_reg(&self, index: usize) -> u32 {
        self.inner.ext_regs().0[index]
    }

    pub fn ext_regs(&self) -> &[u32; 64] {
        &self.inner.ext_regs().0
    }

    pub fn ext_regs_mut(&mut self) -> &mut [u32; 64] {
        &mut self.inner.ext_regs_mut().0
    }

    pub fn set_ext_reg(&mut self, index: usize, value: u32) {
        self.inner.ext_regs_mut().0[index] = value;
    }

    pub fn clear_exclusive_state(&mut self) {
        self.inner.clear_exclusive_state();
    }

    pub fn is_executing(&self) -> bool {
        self.is_executing
    }

    pub fn disassemble(&self) -> String {
        self.inner.disassemble()
    }

    pub fn compile_block_only(&mut self) -> *const u8 {
        self.inner
            .compile_block_only()
            .expect("A32 ARM64 compile_block_only failed")
    }

    pub fn dump_jit_block_map(&self, path: &str) -> std::io::Result<()> {
        let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
        self.inner.dump_block_map(&mut file)
    }
}
