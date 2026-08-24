//! ARM64 emission context.
//!
//! Upstream owner: `backend/arm64/emit_context.h`.

use crate::ir::block::Block;
use crate::ir::location::LocationDescriptor;

use super::emit_arm64::{EmitConfig, EmittedBlockInfo};
use super::fastmem::FastmemManager;
use super::fpsr_manager::FpsrManager;
use super::reg_alloc::RegAlloc;

const FPCR_MASK: u32 = 0x07ff_9f00;
const FPCR_AHP_BIT: u32 = 26;
const FPCR_DN_BIT: u32 = 25;
const FPCR_FZ_BIT: u32 = 24;
const FPCR_FZ16_BIT: u32 = 19;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fpcr(u32);

impl Fpcr {
    pub fn new(value: u32) -> Self {
        Self(value & FPCR_MASK)
    }

    pub fn value(self) -> u32 {
        self.0
    }

    pub fn asimd_standard_value(self) -> Self {
        let mut value = (1 << FPCR_FZ_BIT) | (1 << FPCR_DN_BIT);
        value |= self.0 & (1 << FPCR_AHP_BIT);
        value |= self.0 & (1 << FPCR_FZ16_BIT);
        Self::new(value)
    }
}

pub type DescriptorToFpcr = fn(LocationDescriptor) -> Fpcr;

pub struct EmitContext<'a> {
    pub block: &'a mut Block,
    pub reg_alloc: &'a mut RegAlloc,
    pub conf: &'a EmitConfig,
    pub emitted_block_info: &'a mut EmittedBlockInfo,
    pub fpsr: &'a mut FpsrManager,
    pub fastmem: &'a mut FastmemManager<'a>,
    pub deferred_emits: Vec<Box<dyn FnMut() -> Result<(), String> + 'a>>,
}

impl EmitContext<'_> {
    pub fn fpcr(&self, fpcr_controlled: bool) -> Fpcr {
        let fpcr = (self.conf.descriptor_to_fpcr)(self.block.location);
        if fpcr_controlled {
            fpcr
        } else {
            fpcr.asimd_standard_value()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::arm64::emit_arm64::{CodePtr, EmittedBlockInfo};
    use crate::interface::a64::config::{
        Exception as A64Exception, UserCallbacks as A64UserCallbacks, UserConfig as A64UserConfig,
        Vector as A64Vector,
    };
    use crate::ir::location::A64LocationDescriptor;

    struct DummyCallbacks;

    impl A64UserCallbacks for DummyCallbacks {
        fn memory_read_code(&self, _vaddr: u64) -> Option<u32> {
            None
        }

        fn memory_read_8(&self, _vaddr: u64) -> u8 {
            0
        }

        fn memory_read_16(&self, _vaddr: u64) -> u16 {
            0
        }

        fn memory_read_32(&self, _vaddr: u64) -> u32 {
            0
        }

        fn memory_read_64(&self, _vaddr: u64) -> u64 {
            0
        }

        fn memory_read_128(&self, _vaddr: u64) -> A64Vector {
            [0, 0]
        }

        fn memory_write_8(&mut self, _vaddr: u64, _value: u8) {}
        fn memory_write_16(&mut self, _vaddr: u64, _value: u16) {}
        fn memory_write_32(&mut self, _vaddr: u64, _value: u32) {}
        fn memory_write_64(&mut self, _vaddr: u64, _value: u64) {}
        fn memory_write_128(&mut self, _vaddr: u64, _value: A64Vector) {}

        fn call_svc(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: A64Exception) {}
        fn add_ticks(&mut self, _ticks: u64) {}

        fn get_ticks_remaining(&self) -> u64 {
            0
        }

        fn get_cntpct(&self) -> u64 {
            0
        }
    }

    fn config() -> A64UserConfig {
        let mut config = A64UserConfig::new(Box::new(DummyCallbacks));
        config.enable_cycle_counting = false;
        config.code_cache_size = 0;
        config
    }

    fn empty_block_info() -> EmittedBlockInfo {
        EmittedBlockInfo {
            entry_point: core::ptr::null::<u8>() as CodePtr,
            size: 0,
            relocations: Vec::new(),
            block_relocations: crate::backend::arm64::fast_hash::FastHashMap::default(),
            fastmem_patch_info: crate::backend::arm64::fast_hash::FastHashMap::default(),
        }
    }

    #[test]
    fn fpcr_masks_reserved_bits_and_builds_asimd_standard_value() {
        let fpcr = Fpcr::new(0xffff_ffff);
        assert_eq!(fpcr.value(), 0x07ff_9f00);

        let source = Fpcr::new((1 << FPCR_AHP_BIT) | (1 << FPCR_FZ16_BIT) | (3 << 22) | 0x1f00);
        let standard = source.asimd_standard_value();
        assert_eq!(
            standard.value(),
            (1 << FPCR_AHP_BIT) | (1 << FPCR_FZ16_BIT) | (1 << FPCR_FZ_BIT) | (1 << FPCR_DN_BIT)
        );
    }

    #[test]
    fn emit_context_fpcr_uses_descriptor_callback() {
        let location = A64LocationDescriptor::new(0x1000, 0x07c8_0000, false).to_location();
        let mut block = Block::new(location);
        let mut reg_alloc = RegAlloc::default();
        let mut emitted_block_info = empty_block_info();
        let mut fpsr = FpsrManager::default();
        let mut fastmem = FastmemManager::default();
        let conf = crate::backend::arm64::emit_arm64::EmitConfig::from_a64_config(&config());

        let ctx = EmitContext {
            block: &mut block,
            reg_alloc: &mut reg_alloc,
            conf: &conf,
            emitted_block_info: &mut emitted_block_info,
            fpsr: &mut fpsr,
            fastmem: &mut fastmem,
            deferred_emits: Vec::new(),
        };

        assert_eq!(ctx.fpcr(true).value(), 0x07c8_0000);
        assert_eq!(
            ctx.fpcr(false).value(),
            (1 << FPCR_AHP_BIT) | (1 << FPCR_FZ16_BIT) | (1 << FPCR_FZ_BIT) | (1 << FPCR_DN_BIT)
        );
    }
}
