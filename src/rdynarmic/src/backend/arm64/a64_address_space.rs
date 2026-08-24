use std::ffi::c_void;
use std::ops::RangeInclusive;
use std::sync::OnceLock;

use crate::backend::block_range_information::BlockRangeInformation;
use crate::frontend::a64::translate::{translate, TranslationOptions};
use crate::interface::a64::config::{
    DataCacheOperation, Exception, InstructionCacheOperation, UserCallbacks, UserConfig,
};
use crate::interface::exclusive_monitor::ExclusiveMonitor;
use crate::interface::optimization_flags::OptimizationFlag;
use crate::ir::block::Block;
use crate::ir::location::{A64LocationDescriptor, LocationDescriptor};
use crate::ir::opt;

use super::address_space::AddressSpace;
use super::emit_arm64::{CodePtr, EmitConfig};
use super::jit_state::A64JitState;
use super::prelude::{PreludeIsa, PreludeOptions};

fn trace_a64_exclusive_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_TRACE_A64_EXCLUSIVE").is_some())
}

/// A64-specific ARM64 address-space owner.
///
/// Upstream owner: `backend/arm64/a64_address_space.h/.cpp`.
pub struct A64AddressSpace {
    address_space: AddressSpace,
    conf: UserConfig,
    block_ranges: BlockRangeInformation<u64>,
}

#[derive(Clone, Copy)]
pub struct A64CallbackFns {
    pub read_memory_8: *const c_void,
    pub read_memory_16: *const c_void,
    pub read_memory_32: *const c_void,
    pub read_memory_64: *const c_void,
    pub read_memory_128: *const c_void,
    pub exclusive_read_memory_8: *const c_void,
    pub exclusive_read_memory_16: *const c_void,
    pub exclusive_read_memory_32: *const c_void,
    pub exclusive_read_memory_64: *const c_void,
    pub exclusive_read_memory_128: *const c_void,
    pub write_memory_8: *const c_void,
    pub write_memory_16: *const c_void,
    pub write_memory_32: *const c_void,
    pub write_memory_64: *const c_void,
    pub write_memory_128: *const c_void,
    pub exclusive_write_memory_8: *const c_void,
    pub exclusive_write_memory_16: *const c_void,
    pub exclusive_write_memory_32: *const c_void,
    pub exclusive_write_memory_64: *const c_void,
    pub exclusive_write_memory_128: *const c_void,
    pub call_svc: *const c_void,
    pub exception_raised: *const c_void,
    pub isb_raised: *const c_void,
    pub ic_raised: *const c_void,
    pub dc_raised: *const c_void,
    pub get_cntpct: *const c_void,
    pub add_ticks: *const c_void,
    pub get_ticks_remaining: *const c_void,
}

/// Stable Rust context for A64 ARM64 prelude callback thunks.
///
/// Upstream stores `const A64::UserConfig& conf` in `A64AddressSpace` and emits
/// trampolines with devirtualized callback pointers. Rust cannot devirtualize a
/// trait-object member pointer, so this backend-only context preserves the same
/// ownership boundary with stable raw pointers to JIT state, callbacks, global
/// monitor, and processor id.
pub struct A64CallbackContext {
    jit_state: *mut A64JitState,
    callbacks: *mut (dyn UserCallbacks + 'static),
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    exclusive_value: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pair128 {
    pub lo: u64,
    pub hi: u64,
}

impl A64CallbackContext {
    pub fn new(
        jit_state: *mut A64JitState,
        callbacks: *mut (dyn UserCallbacks + 'static),
        global_monitor: Option<*mut ExclusiveMonitor>,
        processor_id: usize,
    ) -> Self {
        Self {
            jit_state,
            callbacks,
            global_monitor,
            processor_id,
            exclusive_value: [0; 2],
        }
    }

    pub fn callback_fns() -> A64CallbackFns {
        A64CallbackFns {
            read_memory_8: a64_arm64_memory_read_8 as *const () as *const c_void,
            read_memory_16: a64_arm64_memory_read_16 as *const () as *const c_void,
            read_memory_32: a64_arm64_memory_read_32 as *const () as *const c_void,
            read_memory_64: a64_arm64_memory_read_64 as *const () as *const c_void,
            read_memory_128: a64_arm64_memory_read_128 as *const () as *const c_void,
            exclusive_read_memory_8: a64_arm64_exclusive_read_8 as *const () as *const c_void,
            exclusive_read_memory_16: a64_arm64_exclusive_read_16 as *const () as *const c_void,
            exclusive_read_memory_32: a64_arm64_exclusive_read_32 as *const () as *const c_void,
            exclusive_read_memory_64: a64_arm64_exclusive_read_64 as *const () as *const c_void,
            exclusive_read_memory_128: a64_arm64_exclusive_read_128 as *const () as *const c_void,
            write_memory_8: a64_arm64_memory_write_8 as *const () as *const c_void,
            write_memory_16: a64_arm64_memory_write_16 as *const () as *const c_void,
            write_memory_32: a64_arm64_memory_write_32 as *const () as *const c_void,
            write_memory_64: a64_arm64_memory_write_64 as *const () as *const c_void,
            write_memory_128: a64_arm64_memory_write_128 as *const () as *const c_void,
            exclusive_write_memory_8: a64_arm64_exclusive_write_8 as *const () as *const c_void,
            exclusive_write_memory_16: a64_arm64_exclusive_write_16 as *const () as *const c_void,
            exclusive_write_memory_32: a64_arm64_exclusive_write_32 as *const () as *const c_void,
            exclusive_write_memory_64: a64_arm64_exclusive_write_64 as *const () as *const c_void,
            exclusive_write_memory_128: a64_arm64_exclusive_write_128 as *const () as *const c_void,
            call_svc: a64_arm64_call_svc as *const () as *const c_void,
            exception_raised: a64_arm64_exception_raised as *const () as *const c_void,
            isb_raised: a64_arm64_isb_raised as *const () as *const c_void,
            ic_raised: a64_arm64_instruction_cache_operation as *const () as *const c_void,
            dc_raised: a64_arm64_data_cache_operation as *const () as *const c_void,
            get_cntpct: a64_arm64_get_cntpct as *const () as *const c_void,
            add_ticks: a64_arm64_add_ticks as *const () as *const c_void,
            get_ticks_remaining: a64_arm64_get_ticks_remaining as *const () as *const c_void,
        }
    }

    fn callbacks(&self) -> &dyn UserCallbacks {
        unsafe { &*self.callbacks }
    }

    fn callbacks_mut(&mut self) -> &mut dyn UserCallbacks {
        unsafe { &mut *self.callbacks }
    }
}

extern "C" fn a64_arm64_memory_read_8(ctx: *mut A64CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    context.callbacks().memory_read_8(vaddr) as u64
}

extern "C" fn a64_arm64_memory_read_16(ctx: *mut A64CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    context.callbacks().memory_read_16(vaddr) as u64
}

extern "C" fn a64_arm64_memory_read_32(ctx: *mut A64CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    context.callbacks().memory_read_32(vaddr) as u64
}

extern "C" fn a64_arm64_memory_read_64(ctx: *mut A64CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    context.callbacks().memory_read_64(vaddr)
}

extern "C" fn a64_arm64_memory_read_128(ctx: *mut A64CallbackContext, vaddr: u64) -> Pair128 {
    let context = unsafe { &mut *ctx };
    let [lo, hi] = context.callbacks().memory_read_128(vaddr);
    Pair128 { lo, hi }
}

extern "C" fn a64_arm64_memory_write_8(ctx: *mut A64CallbackContext, vaddr: u64, value: u64) {
    let context = unsafe { &mut *ctx };
    context.callbacks_mut().memory_write_8(vaddr, value as u8);
}

extern "C" fn a64_arm64_memory_write_16(ctx: *mut A64CallbackContext, vaddr: u64, value: u64) {
    let context = unsafe { &mut *ctx };
    context.callbacks_mut().memory_write_16(vaddr, value as u16);
}

extern "C" fn a64_arm64_memory_write_32(ctx: *mut A64CallbackContext, vaddr: u64, value: u64) {
    let context = unsafe { &mut *ctx };
    context.callbacks_mut().memory_write_32(vaddr, value as u32);
}

extern "C" fn a64_arm64_memory_write_64(ctx: *mut A64CallbackContext, vaddr: u64, value: u64) {
    let context = unsafe { &mut *ctx };
    context.callbacks_mut().memory_write_64(vaddr, value);
}

extern "C" fn a64_arm64_memory_write_128(
    ctx: *mut A64CallbackContext,
    vaddr: u64,
    value_lo: u64,
    value_hi: u64,
) {
    let context = unsafe { &mut *ctx };
    context
        .callbacks_mut()
        .memory_write_128(vaddr, [value_lo, value_hi]);
}

extern "C" fn a64_arm64_call_svc(ctx: *mut A64CallbackContext, svc_num: u64) {
    let context = unsafe { &mut *ctx };
    context.callbacks_mut().call_svc(svc_num as u32);
}

extern "C" fn a64_arm64_exception_raised(ctx: *mut A64CallbackContext, pc: u64, exception: u64) {
    let context = unsafe { &mut *ctx };
    context
        .callbacks_mut()
        .exception_raised(pc, Exception::from_u32(exception as u32));
}

extern "C" fn a64_arm64_isb_raised(ctx: *mut A64CallbackContext) {
    let context = unsafe { &mut *ctx };
    context
        .callbacks_mut()
        .instruction_synchronization_barrier_raised();
}

extern "C" fn a64_arm64_instruction_cache_operation(
    ctx: *mut A64CallbackContext,
    op: u64,
    vaddr: u64,
) {
    let context = unsafe { &mut *ctx };
    context
        .callbacks_mut()
        .instruction_cache_operation_raised(InstructionCacheOperation::from_u32(op as u32), vaddr);
}

extern "C" fn a64_arm64_data_cache_operation(ctx: *mut A64CallbackContext, op: u64, vaddr: u64) {
    let context = unsafe { &mut *ctx };
    context
        .callbacks_mut()
        .data_cache_operation_raised(DataCacheOperation::from_u32(op as u32), vaddr);
}

extern "C" fn a64_arm64_get_cntpct(ctx: *mut A64CallbackContext) -> u64 {
    let context = unsafe { &mut *ctx };
    context.callbacks().get_cntpct()
}

extern "C" fn a64_arm64_add_ticks(ctx: *mut A64CallbackContext, ticks: u64) {
    let context = unsafe { &mut *ctx };
    context.callbacks_mut().add_ticks(ticks);
}

extern "C" fn a64_arm64_get_ticks_remaining(ctx: *mut A64CallbackContext) -> u64 {
    let context = unsafe { &mut *ctx };
    context.callbacks().get_ticks_remaining()
}

extern "C" fn a64_arm64_exclusive_read_8(ctx: *mut A64CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    unsafe {
        (*context.jit_state).exclusive_state = 1;
    }
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    let value = if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        unsafe {
            (&mut *monitor).read_and_mark(processor_id, vaddr, || callbacks.memory_read_8(vaddr))
        }
    } else {
        context.callbacks().memory_read_8(vaddr)
    };
    context.exclusive_value[0] = value as u64;
    value as u64
}

extern "C" fn a64_arm64_exclusive_read_16(ctx: *mut A64CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    unsafe {
        (*context.jit_state).exclusive_state = 1;
    }
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    let value = if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        unsafe {
            (&mut *monitor).read_and_mark(processor_id, vaddr, || callbacks.memory_read_16(vaddr))
        }
    } else {
        context.callbacks().memory_read_16(vaddr)
    };
    context.exclusive_value[0] = value as u64;
    value as u64
}

extern "C" fn a64_arm64_exclusive_read_32(ctx: *mut A64CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    unsafe {
        (*context.jit_state).exclusive_state = 1;
    }
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    let value = if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        unsafe {
            (&mut *monitor).read_and_mark(processor_id, vaddr, || callbacks.memory_read_32(vaddr))
        }
    } else {
        context.callbacks().memory_read_32(vaddr)
    };
    context.exclusive_value[0] = value as u64;
    if trace_a64_exclusive_enabled() {
        eprintln!(
            "[A64_EXCL_R32] vaddr=0x{vaddr:016X} value=0x{value:08X} state={}",
            unsafe { (*context.jit_state).exclusive_state }
        );
    }
    value as u64
}

extern "C" fn a64_arm64_exclusive_read_64(ctx: *mut A64CallbackContext, vaddr: u64) -> u64 {
    let context = unsafe { &mut *ctx };
    unsafe {
        (*context.jit_state).exclusive_state = 1;
    }
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    let value = if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        unsafe {
            (&mut *monitor).read_and_mark(processor_id, vaddr, || callbacks.memory_read_64(vaddr))
        }
    } else {
        context.callbacks().memory_read_64(vaddr)
    };
    context.exclusive_value[0] = value;
    value
}

extern "C" fn a64_arm64_exclusive_read_128(ctx: *mut A64CallbackContext, vaddr: u64) -> Pair128 {
    let context = unsafe { &mut *ctx };
    unsafe {
        (*context.jit_state).exclusive_state = 1;
    }
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    let value = if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        unsafe {
            (&mut *monitor)
                .read_and_mark::<[u64; 2]>(processor_id, vaddr, || callbacks.memory_read_128(vaddr))
        }
    } else {
        context.callbacks().memory_read_128(vaddr)
    };
    context.exclusive_value = value;
    Pair128 {
        lo: value[0],
        hi: value[1],
    }
}

extern "C" fn a64_arm64_exclusive_write_8(
    ctx: *mut A64CallbackContext,
    vaddr: u64,
    value: u64,
) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u8| {
                callbacks.memory_write_exclusive_8(vaddr, value as u8, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = context.exclusive_value[0] as u8;
    context
        .callbacks_mut()
        .memory_write_exclusive_8(vaddr, value as u8, expected) as u64
        ^ 1
}

extern "C" fn a64_arm64_exclusive_write_16(
    ctx: *mut A64CallbackContext,
    vaddr: u64,
    value: u64,
) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u16| {
                callbacks.memory_write_exclusive_16(vaddr, value as u16, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = context.exclusive_value[0] as u16;
    context
        .callbacks_mut()
        .memory_write_exclusive_16(vaddr, value as u16, expected) as u64
        ^ 1
}

extern "C" fn a64_arm64_exclusive_write_32(
    ctx: *mut A64CallbackContext,
    vaddr: u64,
    value: u64,
) -> u64 {
    let context = unsafe { &mut *ctx };
    if trace_a64_exclusive_enabled() {
        eprintln!(
            "[A64_EXCL_W32_ENTER] vaddr=0x{vaddr:016X} value=0x{:08X} state={} expected=0x{:08X}",
            value as u32,
            unsafe { (*context.jit_state).exclusive_state },
            context.exclusive_value[0] as u32,
        );
    }
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u32| {
                callbacks.memory_write_exclusive_32(vaddr, value as u32, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = context.exclusive_value[0] as u32;
    let result = context
        .callbacks_mut()
        .memory_write_exclusive_32(vaddr, value as u32, expected);
    if trace_a64_exclusive_enabled() {
        eprintln!("[A64_EXCL_W32_EXIT] result={result}");
    }
    result as u64 ^ 1
}

extern "C" fn a64_arm64_exclusive_write_64(
    ctx: *mut A64CallbackContext,
    vaddr: u64,
    value: u64,
) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u64| {
                callbacks.memory_write_exclusive_64(vaddr, value, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = context.exclusive_value[0];
    context
        .callbacks_mut()
        .memory_write_exclusive_64(vaddr, value, expected) as u64
        ^ 1
}

extern "C" fn a64_arm64_exclusive_write_128(
    ctx: *mut A64CallbackContext,
    vaddr: u64,
    value_lo: u64,
    value_hi: u64,
) -> u64 {
    let context = unsafe { &mut *ctx };
    let global_monitor = context.global_monitor;
    let processor_id = context.processor_id;
    if let Some(monitor) = global_monitor {
        let callbacks = context.callbacks_mut();
        return if unsafe {
            (&mut *monitor).do_exclusive_operation::<[u64; 2]>(processor_id, vaddr, |expected| {
                callbacks.memory_write_exclusive_128(vaddr, [value_lo, value_hi], expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = context.exclusive_value;
    context
        .callbacks_mut()
        .memory_write_exclusive_128(vaddr, [value_lo, value_hi], expected) as u64
        ^ 1
}

impl A64AddressSpace {
    pub fn new(conf: impl Into<UserConfig>) -> Result<Self, String> {
        let conf = conf.into();
        let code_cache_size = conf.code_cache_size as usize;

        let mut address_space = AddressSpace::new(code_cache_size)?;
        emit_prelude(&mut address_space, &conf)?;

        Ok(Self {
            address_space,
            conf,
            block_ranges: BlockRangeInformation::default(),
        })
    }

    pub fn address_space(&self) -> &AddressSpace {
        &self.address_space
    }

    pub fn config(&self) -> &UserConfig {
        &self.conf
    }

    pub(crate) fn config_mut(&mut self) -> &mut UserConfig {
        &mut self.conf
    }

    pub(crate) fn address_space_mut(&mut self) -> &mut AddressSpace {
        &mut self.address_space
    }

    pub fn get_or_emit(&mut self, descriptor: LocationDescriptor) -> Result<CodePtr, String> {
        if let Some(block_entry) = self.address_space.get(descriptor) {
            return Ok(block_entry);
        }

        let block = self.generate_ir(descriptor);
        let block_for_ranges = block.clone();
        let config = self.get_emit_config();
        let block_info = self.address_space.emit(block, config)?;
        self.register_new_basic_block(&block_for_ranges);
        Ok(block_info.entry_point)
    }

    pub fn generate_ir(&self, descriptor: LocationDescriptor) -> Block {
        let a64_descriptor = A64LocationDescriptor::from_location(descriptor);
        let read_code = |vaddr: u64| self.conf.callbacks.memory_read_code(vaddr);
        let mut block = translate(
            a64_descriptor,
            &read_code,
            TranslationOptions {
                define_unpredictable_behaviour: self.conf.define_unpredictable_behaviour,
                wall_clock_cntpct: self.conf.wall_clock_cntpct,
                ..TranslationOptions::default()
            },
        );

        opt::polyfill(&mut block, opt::PolyfillOptions::default());
        opt::a64_callback_config(
            &mut block,
            self.conf.hook_data_cache_operations,
            self.conf.dczid_el0,
        );

        if self
            .conf
            .has_optimization(OptimizationFlag::GET_SET_ELIMINATION)
            && !self.conf.check_halt_on_memory_access
        {
            opt::a64_get_set_elimination(&mut block);
            block.recompute_use_counts();
            opt::dead_code_elimination(&mut block);
        }
        if self.conf.has_optimization(OptimizationFlag::CONST_PROP) {
            opt::constant_propagation(&mut block);
            block.recompute_use_counts();
            opt::dead_code_elimination(&mut block);
        }
        if self.conf.has_optimization(OptimizationFlag::MISC_IR_OPT) {}
        block.recompute_use_counts();
        #[cfg(debug_assertions)]
        opt::verification_pass(&block);

        block
    }

    pub fn get_emit_config(&self) -> EmitConfig {
        EmitConfig::from_a64_config(&self.conf)
    }

    pub fn emit_callback_trampolines(
        &mut self,
        callback_context_ptr: *const c_void,
        fns: A64CallbackFns,
    ) -> Result<(), String> {
        let read_memory_8 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.read_memory_8)?;
        let read_memory_16 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.read_memory_16)?;
        let read_memory_32 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.read_memory_32)?;
        let read_memory_64 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.read_memory_64)?;
        let read_memory_128 = self
            .address_space
            .emit_read128_call_trampoline(callback_context_ptr, fns.read_memory_128)?;

        let wrapped_read_memory_8 = self
            .address_space
            .emit_wrapped_read_call_trampoline(callback_context_ptr, fns.read_memory_8)?;
        let wrapped_read_memory_16 = self
            .address_space
            .emit_wrapped_read_call_trampoline(callback_context_ptr, fns.read_memory_16)?;
        let wrapped_read_memory_32 = self
            .address_space
            .emit_wrapped_read_call_trampoline(callback_context_ptr, fns.read_memory_32)?;
        let wrapped_read_memory_64 = self
            .address_space
            .emit_wrapped_read_call_trampoline(callback_context_ptr, fns.read_memory_64)?;
        let wrapped_read_memory_128 = self
            .address_space
            .emit_wrapped_read128_call_trampoline(callback_context_ptr, fns.read_memory_128)?;

        let exclusive_read_memory_8 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.exclusive_read_memory_8)?;
        let exclusive_read_memory_16 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.exclusive_read_memory_16)?;
        let exclusive_read_memory_32 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.exclusive_read_memory_32)?;
        let exclusive_read_memory_64 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.exclusive_read_memory_64)?;
        let exclusive_read_memory_128 = self
            .address_space
            .emit_read128_call_trampoline(callback_context_ptr, fns.exclusive_read_memory_128)?;

        let write_memory_8 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.write_memory_8)?;
        let write_memory_16 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.write_memory_16)?;
        let write_memory_32 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.write_memory_32)?;
        let write_memory_64 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.write_memory_64)?;
        let write_memory_128 = self
            .address_space
            .emit_write128_call_trampoline(callback_context_ptr, fns.write_memory_128)?;

        let wrapped_write_memory_8 = self
            .address_space
            .emit_wrapped_write_call_trampoline(callback_context_ptr, fns.write_memory_8)?;
        let wrapped_write_memory_16 = self
            .address_space
            .emit_wrapped_write_call_trampoline(callback_context_ptr, fns.write_memory_16)?;
        let wrapped_write_memory_32 = self
            .address_space
            .emit_wrapped_write_call_trampoline(callback_context_ptr, fns.write_memory_32)?;
        let wrapped_write_memory_64 = self
            .address_space
            .emit_wrapped_write_call_trampoline(callback_context_ptr, fns.write_memory_64)?;
        let wrapped_write_memory_128 = self
            .address_space
            .emit_wrapped_write128_call_trampoline(callback_context_ptr, fns.write_memory_128)?;

        let exclusive_write_memory_8 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.exclusive_write_memory_8)?;
        let exclusive_write_memory_16 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.exclusive_write_memory_16)?;
        let exclusive_write_memory_32 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.exclusive_write_memory_32)?;
        let exclusive_write_memory_64 = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.exclusive_write_memory_64)?;
        let exclusive_write_memory_128 = self
            .address_space
            .emit_write128_call_trampoline(callback_context_ptr, fns.exclusive_write_memory_128)?;

        let call_svc = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.call_svc)?;
        let exception_raised = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.exception_raised)?;
        let isb_raised = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.isb_raised)?;
        let ic_raised = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.ic_raised)?;
        let dc_raised = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.dc_raised)?;
        let get_cntpct = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.get_cntpct)?;
        let add_ticks = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.add_ticks)?;
        let get_ticks_remaining = self
            .address_space
            .emit_call_trampoline(callback_context_ptr, fns.get_ticks_remaining)?;

        let prelude_info = self.address_space.prelude_info_mut();
        prelude_info.read_memory_8 = Some(read_memory_8);
        prelude_info.read_memory_16 = Some(read_memory_16);
        prelude_info.read_memory_32 = Some(read_memory_32);
        prelude_info.read_memory_64 = Some(read_memory_64);
        prelude_info.read_memory_128 = Some(read_memory_128);
        prelude_info.wrapped_read_memory_8 = Some(wrapped_read_memory_8);
        prelude_info.wrapped_read_memory_16 = Some(wrapped_read_memory_16);
        prelude_info.wrapped_read_memory_32 = Some(wrapped_read_memory_32);
        prelude_info.wrapped_read_memory_64 = Some(wrapped_read_memory_64);
        prelude_info.wrapped_read_memory_128 = Some(wrapped_read_memory_128);
        prelude_info.exclusive_read_memory_8 = Some(exclusive_read_memory_8);
        prelude_info.exclusive_read_memory_16 = Some(exclusive_read_memory_16);
        prelude_info.exclusive_read_memory_32 = Some(exclusive_read_memory_32);
        prelude_info.exclusive_read_memory_64 = Some(exclusive_read_memory_64);
        prelude_info.exclusive_read_memory_128 = Some(exclusive_read_memory_128);
        prelude_info.write_memory_8 = Some(write_memory_8);
        prelude_info.write_memory_16 = Some(write_memory_16);
        prelude_info.write_memory_32 = Some(write_memory_32);
        prelude_info.write_memory_64 = Some(write_memory_64);
        prelude_info.write_memory_128 = Some(write_memory_128);
        prelude_info.wrapped_write_memory_8 = Some(wrapped_write_memory_8);
        prelude_info.wrapped_write_memory_16 = Some(wrapped_write_memory_16);
        prelude_info.wrapped_write_memory_32 = Some(wrapped_write_memory_32);
        prelude_info.wrapped_write_memory_64 = Some(wrapped_write_memory_64);
        prelude_info.wrapped_write_memory_128 = Some(wrapped_write_memory_128);
        prelude_info.exclusive_write_memory_8 = Some(exclusive_write_memory_8);
        prelude_info.exclusive_write_memory_16 = Some(exclusive_write_memory_16);
        prelude_info.exclusive_write_memory_32 = Some(exclusive_write_memory_32);
        prelude_info.exclusive_write_memory_64 = Some(exclusive_write_memory_64);
        prelude_info.exclusive_write_memory_128 = Some(exclusive_write_memory_128);
        prelude_info.call_svc = Some(call_svc);
        prelude_info.exception_raised = Some(exception_raised);
        prelude_info.isb_raised = Some(isb_raised);
        prelude_info.ic_raised = Some(ic_raised);
        prelude_info.dc_raised = Some(dc_raised);
        prelude_info.get_cntpct = Some(get_cntpct);
        prelude_info.add_ticks = Some(add_ticks);
        prelude_info.get_ticks_remaining = Some(get_ticks_remaining);
        Ok(())
    }

    pub fn register_new_basic_block(&mut self, block: &Block) {
        let descriptor = A64LocationDescriptor::from_location(block.location);
        let end_location = A64LocationDescriptor::from_location(block.end_location());
        self.block_ranges.add_range(
            descriptor.pc()..=end_location.pc().wrapping_sub(1),
            block.location,
        );
    }

    pub fn invalidate_cache_ranges(&mut self, ranges: &[RangeInclusive<u64>]) {
        let descriptors = self.block_ranges.invalidate_ranges(ranges);
        self.address_space.invalidate_basic_blocks(&descriptors);
    }

    #[cfg(test)]
    pub fn block_ranges(&self) -> &[(RangeInclusive<u64>, LocationDescriptor)] {
        self.block_ranges.ranges()
    }
}

fn emit_prelude(address_space: &mut AddressSpace, conf: &UserConfig) -> Result<(), String> {
    address_space.emit_bootstrap_prelude_with_options(PreludeOptions {
        isa: PreludeIsa::A64,
        dispatcher: None,
        return_stack_buffer: conf.has_optimization(OptimizationFlag::RETURN_STACK_BUFFER),
        page_table_pointer: conf.page_table.map_or(0, |p| p as u64),
        fastmem_pointer: conf.fastmem_pointer.map_or(0, |p| p as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCallbacks {
        code: Vec<u32>,
        memory: [u8; 64],
        svc_num: u32,
        exception: (u64, u64),
        data_cache_op: (u64, u64),
        instruction_cache_op: (u64, u64),
        ticks_added: u64,
        cntpct: u64,
    }

    impl TestCallbacks {
        fn read_le<const N: usize>(&self, vaddr: u64) -> [u8; N] {
            let start = vaddr as usize;
            self.memory[start..start + N].try_into().unwrap()
        }

        fn write_le<const N: usize>(&mut self, vaddr: u64, value: [u8; N]) {
            let start = vaddr as usize;
            self.memory[start..start + N].copy_from_slice(&value);
        }
    }

    impl UserCallbacks for TestCallbacks {
        fn memory_read_code(&self, vaddr: u64) -> Option<u32> {
            let index = (vaddr / 4) as usize;
            self.code.get(index).copied()
        }

        fn memory_read_8(&self, vaddr: u64) -> u8 {
            self.memory[vaddr as usize]
        }

        fn memory_read_16(&self, vaddr: u64) -> u16 {
            u16::from_le_bytes(self.read_le(vaddr))
        }

        fn memory_read_32(&self, vaddr: u64) -> u32 {
            u32::from_le_bytes(self.read_le(vaddr))
        }

        fn memory_read_64(&self, vaddr: u64) -> u64 {
            u64::from_le_bytes(self.read_le(vaddr))
        }

        fn memory_read_128(&self, vaddr: u64) -> [u64; 2] {
            let lo = u64::from_le_bytes(self.read_le(vaddr));
            let hi = u64::from_le_bytes(self.read_le(vaddr + 8));
            [lo, hi]
        }

        fn memory_write_8(&mut self, vaddr: u64, value: u8) {
            self.memory[vaddr as usize] = value;
        }

        fn memory_write_16(&mut self, vaddr: u64, value: u16) {
            self.write_le(vaddr, value.to_le_bytes());
        }

        fn memory_write_32(&mut self, vaddr: u64, value: u32) {
            self.write_le(vaddr, value.to_le_bytes());
        }

        fn memory_write_64(&mut self, vaddr: u64, value: u64) {
            self.write_le(vaddr, value.to_le_bytes());
        }

        fn memory_write_128(&mut self, vaddr: u64, value: [u64; 2]) {
            self.write_le(vaddr, value[0].to_le_bytes());
            self.write_le(vaddr + 8, value[1].to_le_bytes());
        }

        fn memory_write_exclusive_8(&mut self, vaddr: u64, value: u8, expected: u8) -> bool {
            if self.memory_read_8(vaddr) == expected {
                self.memory_write_8(vaddr, value);
                true
            } else {
                false
            }
        }

        fn memory_write_exclusive_16(&mut self, vaddr: u64, value: u16, expected: u16) -> bool {
            if self.memory_read_16(vaddr) == expected {
                self.memory_write_16(vaddr, value);
                true
            } else {
                false
            }
        }

        fn memory_write_exclusive_32(&mut self, vaddr: u64, value: u32, expected: u32) -> bool {
            if self.memory_read_32(vaddr) == expected {
                self.memory_write_32(vaddr, value);
                true
            } else {
                false
            }
        }

        fn memory_write_exclusive_64(&mut self, vaddr: u64, value: u64, expected: u64) -> bool {
            if self.memory_read_64(vaddr) == expected {
                self.memory_write_64(vaddr, value);
                true
            } else {
                false
            }
        }

        fn memory_write_exclusive_128(
            &mut self,
            vaddr: u64,
            value: [u64; 2],
            expected: [u64; 2],
        ) -> bool {
            if self.memory_read_128(vaddr) == expected {
                self.memory_write_128(vaddr, value);
                true
            } else {
                false
            }
        }

        fn call_svc(&mut self, svc_num: u32) {
            self.svc_num = svc_num;
        }

        fn exception_raised(&mut self, pc: u64, exception: Exception) {
            self.exception = (pc, exception as u32 as u64);
        }

        fn data_cache_operation_raised(&mut self, op: DataCacheOperation, value: u64) {
            self.data_cache_op = (op as u32 as u64, value);
        }

        fn instruction_cache_operation_raised(
            &mut self,
            op: InstructionCacheOperation,
            value: u64,
        ) {
            self.instruction_cache_op = (op as u32 as u64, value);
        }

        fn get_cntpct(&self) -> u64 {
            self.cntpct
        }

        fn add_ticks(&mut self, ticks: u64) {
            self.ticks_added = self.ticks_added.wrapping_add(ticks);
        }

        fn get_ticks_remaining(&self) -> u64 {
            0x1234
        }
    }

    fn config(code: Vec<u32>) -> UserConfig {
        let callbacks = TestCallbacks {
            code,
            memory: [0; 64],
            svc_num: 0,
            exception: (0, 0),
            data_cache_op: (0, 0),
            instruction_cache_op: (0, 0),
            ticks_added: 0,
            cntpct: 0xfeed_beef,
        };
        let mut config = UserConfig::new(Box::new(callbacks));
        config.enable_cycle_counting = false;
        config.code_cache_size = 4096;
        config.optimizations = OptimizationFlag::NO_OPTIMIZATIONS;
        config
    }

    extern "C" fn dummy_callback() {}

    fn dummy_callback_fns() -> A64CallbackFns {
        let ptr = dummy_callback as *const () as *const c_void;
        A64CallbackFns {
            read_memory_8: ptr,
            read_memory_16: ptr,
            read_memory_32: ptr,
            read_memory_64: ptr,
            read_memory_128: ptr,
            exclusive_read_memory_8: ptr,
            exclusive_read_memory_16: ptr,
            exclusive_read_memory_32: ptr,
            exclusive_read_memory_64: ptr,
            exclusive_read_memory_128: ptr,
            write_memory_8: ptr,
            write_memory_16: ptr,
            write_memory_32: ptr,
            write_memory_64: ptr,
            write_memory_128: ptr,
            exclusive_write_memory_8: ptr,
            exclusive_write_memory_16: ptr,
            exclusive_write_memory_32: ptr,
            exclusive_write_memory_64: ptr,
            exclusive_write_memory_128: ptr,
            call_svc: ptr,
            exception_raised: ptr,
            isb_raised: ptr,
            ic_raised: ptr,
            dc_raised: ptr,
            get_cntpct: ptr,
            add_ticks: ptr,
            get_ticks_remaining: ptr,
        }
    }

    #[test]
    fn generate_ir_uses_a64_location_descriptor_and_callbacks() {
        let address_space = A64AddressSpace::new(config(vec![0xd65f_03c0])).unwrap();
        let descriptor = A64LocationDescriptor::new(0, 0, false).to_location();

        let block = address_space.generate_ir(descriptor);

        assert_eq!(block.location, descriptor);
        assert_eq!(
            A64LocationDescriptor::from_location(block.end_location()).pc(),
            4
        );
    }

    #[test]
    fn register_and_invalidate_cache_ranges_track_a64_pc_ranges() {
        let mut address_space = A64AddressSpace::new(config(vec![0xd65f_03c0])).unwrap();
        let descriptor = A64LocationDescriptor::new(0, 0, false).to_location();
        let block = address_space.generate_ir(descriptor);

        address_space.register_new_basic_block(&block);
        assert_eq!(address_space.block_ranges().len(), 1);
        assert_eq!(address_space.block_ranges()[0].0, 0..=3);
        assert_eq!(address_space.block_ranges()[0].1, descriptor);

        address_space.invalidate_cache_ranges(&[2..=2]);
        assert_eq!(address_space.block_ranges().len(), 1);
    }

    #[test]
    fn callback_trampolines_populate_upstream_a64_prelude_callback_subset() {
        let mut address_space = A64AddressSpace::new(config(vec![0xd65f_03c0])).unwrap();
        let bootstrap_end = address_space.address_space().prelude_info().end_of_prelude;
        let context_ptr = 0x1234usize as *const c_void;

        address_space
            .emit_callback_trampolines(context_ptr, dummy_callback_fns())
            .unwrap();

        let prelude = address_space.address_space().prelude_info();
        assert!(prelude.end_of_prelude > bootstrap_end);
        assert!(prelude.read_memory_8.is_some());
        assert!(prelude.read_memory_16.is_some());
        assert!(prelude.read_memory_32.is_some());
        assert!(prelude.read_memory_64.is_some());
        assert!(prelude.read_memory_128.is_some());
        assert!(prelude.wrapped_read_memory_8.is_some());
        assert!(prelude.wrapped_read_memory_16.is_some());
        assert!(prelude.wrapped_read_memory_32.is_some());
        assert!(prelude.wrapped_read_memory_64.is_some());
        assert!(prelude.wrapped_read_memory_128.is_some());
        assert!(prelude.exclusive_read_memory_8.is_some());
        assert!(prelude.exclusive_read_memory_16.is_some());
        assert!(prelude.exclusive_read_memory_32.is_some());
        assert!(prelude.exclusive_read_memory_64.is_some());
        assert!(prelude.exclusive_read_memory_128.is_some());
        assert!(prelude.write_memory_8.is_some());
        assert!(prelude.write_memory_16.is_some());
        assert!(prelude.write_memory_32.is_some());
        assert!(prelude.write_memory_64.is_some());
        assert!(prelude.write_memory_128.is_some());
        assert!(prelude.wrapped_write_memory_8.is_some());
        assert!(prelude.wrapped_write_memory_16.is_some());
        assert!(prelude.wrapped_write_memory_32.is_some());
        assert!(prelude.wrapped_write_memory_64.is_some());
        assert!(prelude.wrapped_write_memory_128.is_some());
        assert!(prelude.exclusive_write_memory_8.is_some());
        assert!(prelude.exclusive_write_memory_16.is_some());
        assert!(prelude.exclusive_write_memory_32.is_some());
        assert!(prelude.exclusive_write_memory_64.is_some());
        assert!(prelude.exclusive_write_memory_128.is_some());
        assert!(prelude.call_svc.is_some());
        assert!(prelude.exception_raised.is_some());
        assert!(prelude.isb_raised.is_some());
        assert!(prelude.ic_raised.is_some());
        assert!(prelude.dc_raised.is_some());
        assert!(prelude.get_cntpct.is_some());
        assert!(prelude.add_ticks.is_some());
        assert!(prelude.get_ticks_remaining.is_some());

        let end_of_prelude = prelude.end_of_prelude;
        address_space.address_space.clear_cache().unwrap();
        assert_eq!(
            address_space.address_space().code().code_size(),
            end_of_prelude
        );
    }

    #[test]
    fn callback_context_thunks_forward_scalar_memory_system_and_exclusive_state() {
        type ReadFn = extern "C" fn(*mut A64CallbackContext, u64) -> u64;
        type WriteFn = extern "C" fn(*mut A64CallbackContext, u64, u64);
        type Read128Fn = extern "C" fn(*mut A64CallbackContext, u64) -> Pair128;
        type Write128Fn = extern "C" fn(*mut A64CallbackContext, u64, u64, u64);
        type ExclusiveWriteFn = extern "C" fn(*mut A64CallbackContext, u64, u64) -> u64;
        type ExclusiveWrite128Fn = extern "C" fn(*mut A64CallbackContext, u64, u64, u64) -> u64;
        type System2Fn = extern "C" fn(*mut A64CallbackContext, u64, u64);
        type System1Fn = extern "C" fn(*mut A64CallbackContext, u64);
        type System0RetFn = extern "C" fn(*mut A64CallbackContext) -> u64;

        let mut state = A64JitState::new();
        let mut callbacks = TestCallbacks {
            code: vec![],
            memory: [0; 64],
            svc_num: 0,
            exception: (0, 0),
            data_cache_op: (0, 0),
            instruction_cache_op: (0, 0),
            ticks_added: 0,
            cntpct: 0x5678,
        };
        UserCallbacks::memory_write_32(&mut callbacks, 4, 0x1122_3344);
        UserCallbacks::memory_write_128(
            &mut callbacks,
            16,
            [0x0011_2233_4455_6677, 0x8899_aabb_ccdd_eeff],
        );
        let a64_callbacks: &mut dyn crate::interface::a64::config::UserCallbacks = &mut callbacks;
        let callbacks_ptr = a64_callbacks as *mut dyn crate::interface::a64::config::UserCallbacks;
        let mut context = A64CallbackContext::new(&mut state, callbacks_ptr, None, 0);
        let fns = A64CallbackContext::callback_fns();

        let read_32: ReadFn = unsafe { std::mem::transmute(fns.read_memory_32) };
        let write_32: WriteFn = unsafe { std::mem::transmute(fns.write_memory_32) };
        let read_128: Read128Fn = unsafe { std::mem::transmute(fns.read_memory_128) };
        let write_128: Write128Fn = unsafe { std::mem::transmute(fns.write_memory_128) };
        let exclusive_read_32: ReadFn =
            unsafe { std::mem::transmute(fns.exclusive_read_memory_32) };
        let exclusive_write_32: ExclusiveWriteFn =
            unsafe { std::mem::transmute(fns.exclusive_write_memory_32) };
        let exclusive_read_128: Read128Fn =
            unsafe { std::mem::transmute(fns.exclusive_read_memory_128) };
        let exclusive_write_128: ExclusiveWrite128Fn =
            unsafe { std::mem::transmute(fns.exclusive_write_memory_128) };
        let exception_raised: System2Fn = unsafe { std::mem::transmute(fns.exception_raised) };
        let data_cache_op: System2Fn = unsafe { std::mem::transmute(fns.dc_raised) };
        let instruction_cache_op: System2Fn = unsafe { std::mem::transmute(fns.ic_raised) };
        let call_svc: System1Fn = unsafe { std::mem::transmute(fns.call_svc) };
        let add_ticks: System1Fn = unsafe { std::mem::transmute(fns.add_ticks) };
        let get_cntpct: System0RetFn = unsafe { std::mem::transmute(fns.get_cntpct) };
        let get_ticks_remaining: System0RetFn =
            unsafe { std::mem::transmute(fns.get_ticks_remaining) };

        assert_eq!(read_32(&mut context, 4), 0x1122_3344);
        write_32(&mut context, 8, 0xaabb_ccdd);
        assert_eq!(read_32(&mut context, 8), 0xaabb_ccdd);
        assert_eq!(
            read_128(&mut context, 16),
            Pair128 {
                lo: 0x0011_2233_4455_6677,
                hi: 0x8899_aabb_ccdd_eeff
            }
        );
        write_128(
            &mut context,
            32,
            0x1111_2222_3333_4444,
            0x5555_6666_7777_8888,
        );
        assert_eq!(
            read_128(&mut context, 32),
            Pair128 {
                lo: 0x1111_2222_3333_4444,
                hi: 0x5555_6666_7777_8888
            }
        );

        assert_eq!(exclusive_read_32(&mut context, 8), 0xaabb_ccdd);
        assert_eq!(state.exclusive_state, 1);
        state.exclusive_state = 0;
        assert_eq!(exclusive_write_32(&mut context, 8, 0x5566_7788), 0);
        assert_eq!(state.exclusive_state, 0);
        assert_eq!(read_32(&mut context, 8), 0x5566_7788);
        assert_eq!(
            exclusive_read_128(&mut context, 32),
            Pair128 {
                lo: 0x1111_2222_3333_4444,
                hi: 0x5555_6666_7777_8888
            }
        );
        assert_eq!(state.exclusive_state, 1);
        state.exclusive_state = 0;
        assert_eq!(
            exclusive_write_128(
                &mut context,
                32,
                0xaaaa_bbbb_cccc_dddd,
                0xeeee_ffff_0000_1111
            ),
            0
        );
        assert_eq!(state.exclusive_state, 0);
        assert_eq!(
            read_128(&mut context, 32),
            Pair128 {
                lo: 0xaaaa_bbbb_cccc_dddd,
                hi: 0xeeee_ffff_0000_1111
            }
        );

        call_svc(&mut context, 0x42);
        exception_raised(&mut context, 0x1000, Exception::Breakpoint as u32 as u64);
        data_cache_op(
            &mut context,
            DataCacheOperation::ZeroByVa as u32 as u64,
            0x22,
        );
        instruction_cache_op(
            &mut context,
            InstructionCacheOperation::InvalidateAllToPoU as u32 as u64,
            0x44,
        );
        add_ticks(&mut context, 9);
        assert_eq!(get_cntpct(&mut context), 0x5678);
        assert_eq!(get_ticks_remaining(&mut context), 0x1234);

        assert_eq!(callbacks.svc_num, 0x42);
        assert_eq!(
            callbacks.exception,
            (0x1000, Exception::Breakpoint as u32 as u64)
        );
        assert_eq!(
            callbacks.data_cache_op,
            (DataCacheOperation::ZeroByVa as u32 as u64, 0x22)
        );
        assert_eq!(
            callbacks.instruction_cache_op,
            (
                InstructionCacheOperation::InvalidateAllToPoU as u32 as u64,
                0x44
            )
        );
        assert_eq!(callbacks.ticks_added, 9);
    }
}
