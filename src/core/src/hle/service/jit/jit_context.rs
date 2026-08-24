// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden `src/core/hle/service/jit/jit_context.{h,cpp}`.

use std::mem::{size_of, MaybeUninit};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use common::common_funcs::make_magic;
use common::elf::{
    elf64_rel_type, Elf64Dyn, Elf64Rela, Elf64Relr, ELF_AARCH64_RELATIVE, ELF_DT_NULL, ELF_DT_RELA,
    ELF_DT_RELASZ, ELF_DT_RELR, ELF_DT_RELRSZ,
};
use rdynarmic::interface::a64::config::{
    Exception as A64Exception, UserCallbacks as A64UserCallbacks, UserConfig as A64UserConfig,
    Vector as A64Vector,
};
use rdynarmic::A64Jit;
use rdynarmic::HaltReason;

use crate::memory::memory::Memory;

const SVC0_ARM64: [u8; 8] = [
    0x01, 0x00, 0x00, 0xD4, // svc #0
    0xC0, 0x03, 0x5F, 0xD6, // ret
];
const STACK_ALIGN: usize = 16;
const LOCAL_STACK_SIZE: usize = 4096 * 32;
const CODE_PAGE_SIZE: usize = 4096;

#[repr(usize)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum HelperFn {
    None,
    Stop,
    Resolve,
    Panic,
    Memcpy,
    Memmove,
    Memset,
    PanicForPlugin,
    AbortImpl,
    UnexpectedImpl,
    Count,
}

const HELPER_COUNT: usize = HelperFn::Count as usize;
const HELPER_FUNCTIONS: [HelperFn; HELPER_COUNT] = [
    HelperFn::None,
    HelperFn::Stop,
    HelperFn::Resolve,
    HelperFn::Panic,
    HelperFn::Memcpy,
    HelperFn::Memmove,
    HelperFn::Memset,
    HelperFn::PanicForPlugin,
    HelperFn::AbortImpl,
    HelperFn::UnexpectedImpl,
];

struct ContextState {
    local_memory: Vec<u8>,
    argument_stack: Vec<u64>,
    mapped_ranges: Vec<(u64, u64)>,
    helpers: [u64; HELPER_COUNT],
    top_of_stack: u64,
    heap_pointer: u64,
    relocbase: u64,
}

impl ContextState {
    fn new() -> Self {
        Self {
            local_memory: Vec::new(),
            argument_stack: Vec::new(),
            mapped_ranges: Vec::new(),
            helpers: [0; HELPER_COUNT],
            top_of_stack: 0,
            heap_pointer: 0,
            relocbase: 0,
        }
    }

    fn is_process_address(&self, address: u64) -> bool {
        self.mapped_ranges
            .iter()
            .any(|&(start, end)| start <= address && address < end)
    }

    fn get_helper(&self, name: &str) -> u64 {
        let helper = match name {
            "_resolve" => HelperFn::Resolve,
            "_panic" => HelperFn::Panic,
            "_stop" => HelperFn::Stop,
            "memset" => HelperFn::Memset,
            "memcpy" => HelperFn::Memcpy,
            "memmove" => HelperFn::Memmove,
            "PanicForPlugin" => HelperFn::PanicForPlugin,
            "_ZN2nn4diag6detail9AbortImplEPKcS3_S3_i" => HelperFn::AbortImpl,
            "_ZN2nn6detail21UnexpectedDefaultImplEPKcS2_i" => HelperFn::UnexpectedImpl,
            _ => {
                log::error!("JIT plugin: unresolved {name}");
                HelperFn::Panic
            }
        };
        self.helpers[helper as usize]
    }
}

#[repr(C)]
struct A64RegisterPrefix {
    registers: [u64; 31],
    stack_pointer: u64,
    program_counter: u64,
}

struct CodePageCache {
    address: u64,
    instructions: [u32; CODE_PAGE_SIZE / size_of::<u32>()],
}

impl CodePageCache {
    fn new() -> Self {
        Self {
            address: u64::MAX,
            instructions: [0; CODE_PAGE_SIZE / size_of::<u32>()],
        }
    }
}

struct DynarmicCallbacks64 {
    memory: Option<Arc<Mutex<Memory>>>,
    state: Arc<Mutex<ContextState>>,
    code_page: Mutex<CodePageCache>,
    halt_reason: Option<*const AtomicU32>,
    registers: Option<*mut A64RegisterPrefix>,
}

// The JIT invokes this callback object synchronously on its owning thread. The
// injected state pointers remain stable for the complete A64Jit lifetime.
unsafe impl Send for DynarmicCallbacks64 {}

impl DynarmicCallbacks64 {
    fn new(memory: Option<Arc<Mutex<Memory>>>, state: Arc<Mutex<ContextState>>) -> Self {
        Self {
            memory,
            state,
            code_page: Mutex::new(CodePageCache::new()),
            halt_reason: None,
            registers: None,
        }
    }

    fn read_memory(&self, address: u64, output: &mut [u8]) {
        let is_process_address = self.state.lock().unwrap().is_process_address(address);
        if is_process_address {
            if let Some(memory) = self.memory.as_ref() {
                memory.lock().unwrap().read_block(address, output);
            } else {
                log::error!("JIT plugin: mapped read without process memory at {address:#016x}");
            }
            return;
        }

        let state = self.state.lock().unwrap();
        let Some(end) = (address as usize).checked_add(output.len()) else {
            log::error!("JIT plugin: unmapped read at {address:#016x}");
            return;
        };
        if end > state.local_memory.len() {
            log::error!("JIT plugin: unmapped read at {address:#016x}");
            return;
        }
        output.copy_from_slice(&state.local_memory[address as usize..end]);
    }

    fn write_memory(&self, address: u64, input: &[u8]) -> bool {
        let is_process_address = self.state.lock().unwrap().is_process_address(address);
        if is_process_address {
            if let Some(memory) = self.memory.as_ref() {
                memory.lock().unwrap().write_block(address, input);
            } else {
                log::error!("JIT plugin: mapped write without process memory at {address:#016x}");
            }
            return true;
        }

        let mut state = self.state.lock().unwrap();
        let Some(end) = (address as usize).checked_add(input.len()) else {
            log::error!("JIT plugin: unmapped write at {address:#016x}");
            return true;
        };
        if end > state.local_memory.len() {
            log::error!("JIT plugin: unmapped write at {address:#016x}");
            return true;
        }
        state.local_memory[address as usize..end].copy_from_slice(input);
        true
    }

    fn read_value<T: Copy + Default>(&self, address: u64) -> T {
        let mut value = T::default();
        let bytes = unsafe {
            std::slice::from_raw_parts_mut((&mut value as *mut T).cast::<u8>(), size_of::<T>())
        };
        self.read_memory(address, bytes);
        value
    }

    fn write_value<T: Copy>(&self, address: u64, value: T) -> bool {
        let bytes = unsafe {
            std::slice::from_raw_parts((&value as *const T).cast::<u8>(), size_of::<T>())
        };
        self.write_memory(address, bytes)
    }

    fn read_c_string(&self, mut address: u64) -> String {
        let mut result = Vec::new();
        loop {
            let next = self.read_value::<u8>(address);
            address = address.wrapping_add(1);
            if next == 0 {
                break;
            }
            result.push(next);
        }
        String::from_utf8_lossy(&result).into_owned()
    }

    fn register_state(&mut self) -> &mut A64RegisterPrefix {
        let pointer = self
            .registers
            .expect("rdynarmic must install the A64 register-state pointer");
        unsafe { &mut *pointer }
    }

    fn halt(&self) {
        if let Some(pointer) = self.halt_reason {
            unsafe { &*pointer }.fetch_or(HaltReason::USER_DEFINED1.bits(), Ordering::SeqCst);
        }
    }
}

impl A64UserCallbacks for DynarmicCallbacks64 {
    fn memory_read_code(&self, address: u64) -> Option<u32> {
        let aligned_address = address & !(CODE_PAGE_SIZE as u64 - 1);
        let mut code_page = self.code_page.lock().unwrap();
        if code_page.address != aligned_address {
            let mut bytes = [0u8; CODE_PAGE_SIZE];
            self.read_memory(aligned_address, &mut bytes);
            for (instruction, chunk) in code_page.instructions.iter_mut().zip(bytes.chunks_exact(4))
            {
                *instruction = u32::from_le_bytes(chunk.try_into().unwrap());
            }
            code_page.address = aligned_address;
        }
        Some(code_page.instructions[(address as usize & (CODE_PAGE_SIZE - 1)) / size_of::<u32>()])
    }

    fn memory_read_8(&self, address: u64) -> u8 {
        self.read_value(address)
    }
    fn memory_read_16(&self, address: u64) -> u16 {
        self.read_value(address)
    }
    fn memory_read_32(&self, address: u64) -> u32 {
        self.read_value(address)
    }
    fn memory_read_64(&self, address: u64) -> u64 {
        self.read_value(address)
    }
    fn memory_read_128(&self, address: u64) -> A64Vector {
        let mut bytes = [0; 16];
        self.read_memory(address, &mut bytes);
        [
            u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            u64::from_le_bytes(bytes[8..].try_into().unwrap()),
        ]
    }

    fn memory_write_8(&mut self, address: u64, value: u8) {
        self.write_value(address, value);
    }
    fn memory_write_16(&mut self, address: u64, value: u16) {
        self.write_value(address, value);
    }
    fn memory_write_32(&mut self, address: u64, value: u32) {
        self.write_value(address, value);
    }
    fn memory_write_64(&mut self, address: u64, value: u64) {
        self.write_value(address, value);
    }
    fn memory_write_128(&mut self, address: u64, value: A64Vector) {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&value[0].to_le_bytes());
        bytes[8..].copy_from_slice(&value[1].to_le_bytes());
        self.write_memory(address, &bytes);
    }

    fn memory_write_exclusive_8(&mut self, address: u64, value: u8, _expected: u8) -> bool {
        self.write_value(address, value)
    }
    fn memory_write_exclusive_16(&mut self, address: u64, value: u16, _expected: u16) -> bool {
        self.write_value(address, value)
    }
    fn memory_write_exclusive_32(&mut self, address: u64, value: u32, _expected: u32) -> bool {
        self.write_value(address, value)
    }
    fn memory_write_exclusive_64(&mut self, address: u64, value: u64, _expected: u64) -> bool {
        self.write_value(address, value)
    }
    fn memory_write_exclusive_128(
        &mut self,
        address: u64,
        value: A64Vector,
        _expected: A64Vector,
    ) -> bool {
        self.memory_write_128(address, value);
        true
    }
    fn call_svc(&mut self, swi: u32) {
        if swi != 0 {
            log::error!("JIT plugin issued unknown service call {swi}");
            self.halt();
            return;
        }

        let (pc, registers) = {
            let state = self.register_state();
            (state.program_counter.wrapping_sub(4), state.registers)
        };
        let helpers = self.state.lock().unwrap().helpers;

        if pc == helpers[HelperFn::Memcpy as usize] || pc == helpers[HelperFn::Memmove as usize] {
            let destination = registers[0];
            let source = registers[1];
            let size = registers[2] as usize;
            if destination < source {
                for index in 0..size {
                    let value = self.memory_read_8(source.wrapping_add(index as u64));
                    self.memory_write_8(destination.wrapping_add(index as u64), value);
                }
            } else {
                for index in (0..size).rev() {
                    let value = self.memory_read_8(source.wrapping_add(index as u64));
                    self.memory_write_8(destination.wrapping_add(index as u64), value);
                }
            }
        } else if pc == helpers[HelperFn::Memset as usize] {
            let destination = registers[0];
            let value = registers[1] as u8;
            let size = registers[2] as usize;
            for index in 0..size {
                self.memory_write_8(destination.wrapping_add(index as u64), value);
            }
        } else if pc == helpers[HelperFn::Resolve as usize] {
            let name = self.read_c_string(registers[0]);
            let resolved = self.state.lock().unwrap().get_helper(&name);
            self.register_state().registers[0] = resolved;
        } else if pc == helpers[HelperFn::Stop as usize] {
            self.halt();
        } else if pc == helpers[HelperFn::Panic as usize]
            || pc == helpers[HelperFn::PanicForPlugin as usize]
            || pc == helpers[HelperFn::AbortImpl as usize]
            || pc == helpers[HelperFn::UnexpectedImpl as usize]
        {
            log::error!("JIT plugin panicked");
            self.halt();
        } else {
            log::error!("JIT plugin issued syscall at unknown address {pc:#x}");
            self.halt();
        }
    }

    fn exception_raised(&mut self, pc: u64, exception: A64Exception) {
        let instruction = self.memory_read_32(pc);
        log::error!("JIT plugin exception {exception:?} at {pc:08x}, data={instruction:08x}");
        self.halt();
    }

    fn instruction_synchronization_barrier_raised(&mut self) {
        self.code_page.lock().unwrap().address = u64::MAX;
    }

    fn get_cntpct(&self) -> u64 {
        0
    }
    fn add_ticks(&mut self, _ticks: u64) {}
    fn get_ticks_remaining(&self) -> u64 {
        u32::MAX as u64
    }

    fn set_halt_reason_ptr(&mut self, pointer: *const u32) {
        self.halt_reason = Some(pointer.cast::<AtomicU32>());
    }

    fn set_pc_ptr(&mut self, pointer: *const u32) {
        let pc_offset = std::mem::offset_of!(A64RegisterPrefix, program_counter);
        self.registers = Some(unsafe {
            pointer
                .cast::<u8>()
                .sub(pc_offset)
                .cast_mut()
                .cast::<A64RegisterPrefix>()
        });
    }
}

pub struct JitContext {
    jit: A64Jit,
    state: Arc<Mutex<ContextState>>,
}

// `A64Jit` owns every allocation referenced by its backend raw pointers, so
// moving the outer value does not invalidate them. IJitEnvironment serializes
// all execution through one mutex, matching Eden's single service-object call
// boundary; no JIT operation can run concurrently after the move.
unsafe impl Send for JitContext {}

impl JitContext {
    pub fn new(memory: Arc<Mutex<Memory>>) -> Result<Self, String> {
        Self::new_impl(Some(memory))
    }

    fn new_impl(memory: Option<Arc<Mutex<Memory>>>) -> Result<Self, String> {
        let state = Arc::new(Mutex::new(ContextState::new()));
        let callbacks = DynarmicCallbacks64::new(memory, Arc::clone(&state));
        let jit = A64Jit::new(A64UserConfig::new(Box::new(callbacks)))?;
        Ok(Self { jit, state })
    }

    pub fn load_nro(&mut self, data: &[u8]) -> bool {
        {
            let mut state = self.state.lock().unwrap();
            state.relocbase = state.local_memory.len() as u64;
            state.local_memory.extend_from_slice(data);
        }
        if self.fixup_relocations() {
            self.insert_helper_functions();
            self.insert_stack();
            true
        } else {
            false
        }
    }

    fn read_local<T: Copy>(&self, address: u64) -> T {
        let state = self.state.lock().unwrap();
        let mut value = MaybeUninit::<T>::zeroed();
        let start = address as usize;
        let end = start.checked_add(size_of::<T>());
        if end.is_some_and(|end| end <= state.local_memory.len()) {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    state.local_memory.as_ptr().add(start),
                    value.as_mut_ptr().cast::<u8>(),
                    size_of::<T>(),
                );
            }
        }
        unsafe { value.assume_init() }
    }

    fn write_local<T: Copy>(&self, address: u64, value: T) {
        let mut state = self.state.lock().unwrap();
        let start = address as usize;
        let end = start.checked_add(size_of::<T>());
        if end.is_some_and(|end| end <= state.local_memory.len()) {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&value as *const T).cast::<u8>(),
                    state.local_memory.as_mut_ptr().add(start),
                    size_of::<T>(),
                );
            }
        }
    }

    fn fixup_relocations(&self) -> bool {
        let module_offset = self.read_local::<u32>(4) as u64;
        if self.read_local::<u32>(module_offset) != make_magic(b'M', b'O', b'D', b'0') {
            return false;
        }

        let mut dynamic_offset = module_offset + self.read_local::<u32>(module_offset + 4) as u64;
        let mut rela_dynamic = 0;
        let mut relr_dynamic = 0;
        let mut num_rela = 0;
        let mut num_relr = 0;
        loop {
            let dynamic = self.read_local::<Elf64Dyn>(dynamic_offset);
            dynamic_offset += size_of::<Elf64Dyn>() as u64;
            match dynamic.d_tag as u32 {
                ELF_DT_NULL => break,
                ELF_DT_RELA => rela_dynamic = dynamic.d_ptr(),
                ELF_DT_RELASZ => num_rela = dynamic.d_val() as usize / size_of::<Elf64Rela>(),
                ELF_DT_RELR => relr_dynamic = dynamic.d_ptr(),
                ELF_DT_RELRSZ => num_relr = dynamic.d_val() as usize / size_of::<Elf64Relr>(),
                _ => {}
            }
        }

        for index in 0..num_rela {
            let rela = self
                .read_local::<Elf64Rela>(rela_dynamic + (index * size_of::<Elf64Rela>()) as u64);
            if elf64_rel_type(rela.r_info) == ELF_AARCH64_RELATIVE {
                let contents = self.read_local::<u64>(rela.r_offset);
                self.write_local(rela.r_offset, contents.wrapping_add(rela.r_addend as u64));
            }
        }

        let relocbase = self.state.lock().unwrap().relocbase;
        let mut relr_where = 0;
        for index in 0..num_relr {
            let relr = self
                .read_local::<Elf64Relr>(relr_dynamic + (index * size_of::<Elf64Relr>()) as u64);
            let increment = |where_: u64| {
                let contents = self.read_local::<u64>(where_);
                self.write_local(where_, contents.wrapping_add(relocbase));
            };
            if relr & 1 == 0 {
                relr_where = relocbase.wrapping_add(relr);
                increment(relr_where);
                relr_where += size_of::<u64>() as u64;
            } else {
                for bit in 1..64 {
                    if relr & (1u64 << bit) != 0 {
                        // Preserve Eden's current index-based address expression.
                        increment(relr_where + (index * size_of::<u64>()) as u64);
                    }
                }
                relr_where += 63 * size_of::<u64>() as u64;
            }
        }
        true
    }

    fn insert_helper_functions(&mut self) {
        let mut state = self.state.lock().unwrap();
        for helper in HELPER_FUNCTIONS {
            state.helpers[helper as usize] = state.local_memory.len() as u64;
            state.local_memory.extend_from_slice(&SVC0_ARM64);
        }
    }

    fn insert_stack(&mut self) {
        let mut state = self.state.lock().unwrap();
        let padding = (STACK_ALIGN - state.local_memory.len() % STACK_ALIGN) % STACK_ALIGN;
        let new_len = state.local_memory.len() + LOCAL_STACK_SIZE + padding;
        state.local_memory.resize(new_len, 0);
        state.top_of_stack = state.local_memory.len() as u64;
        state.heap_pointer = state.top_of_stack;
    }

    pub fn map_process_memory(&mut self, destination_address: u64, size: usize) {
        self.state.lock().unwrap().mapped_ranges.push((
            destination_address,
            destination_address.wrapping_add(size as u64),
        ));
    }

    fn push_argument_bytes(&mut self, data: &[u8]) {
        let word_count = data.len().div_ceil(size_of::<u64>());
        let mut state = self.state.lock().unwrap();
        let current_position = state.argument_stack.len();
        state
            .argument_stack
            .resize(current_position + word_count, 0);
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                state
                    .argument_stack
                    .as_mut_ptr()
                    .add(current_position)
                    .cast::<u8>(),
                data.len(),
            );
        }
    }

    fn setup_arguments(&mut self) {
        let mut state = self.state.lock().unwrap();
        for index in 0..8.min(state.argument_stack.len()) {
            self.jit.set_register(index, state.argument_stack[index]);
        }
        if state.argument_stack.len() > 8 {
            let stack_bytes = (state.argument_stack.len() - 8) * size_of::<u64>();
            let new_stack_pointer = (state.top_of_stack - stack_bytes as u64) & !0xF;
            for index in 8..state.argument_stack.len() {
                let address = new_stack_pointer + ((index - 8) * size_of::<u64>()) as u64;
                let offset = address as usize;
                let value = state.argument_stack[index].to_ne_bytes();
                state.local_memory[offset..offset + size_of::<u64>()].copy_from_slice(&value);
            }
            self.jit.set_sp(new_stack_pointer);
        }
        state.argument_stack.clear();
        state.heap_pointer = state.top_of_stack;
    }

    pub fn call_function(&mut self, function: u64, arguments: &[u64]) -> u64 {
        for argument in arguments {
            self.push_argument_bytes(&argument.to_ne_bytes());
        }
        let stop = self.state.lock().unwrap().helpers[HelperFn::Stop as usize];
        let top_of_stack = self.state.lock().unwrap().top_of_stack;
        self.jit.set_register(30, stop);
        self.jit.set_sp(top_of_stack);
        self.setup_arguments();
        self.jit.clear_halt(HaltReason::USER_DEFINED1);
        self.jit.set_pc(function);
        self.jit.run();
        self.jit.get_register(0)
    }

    pub fn get_helper(&self, name: &str) -> u64 {
        self.state.lock().unwrap().get_helper(name)
    }

    pub fn add_heap<T: Copy>(&mut self, argument: T) -> u64 {
        let data = unsafe {
            std::slice::from_raw_parts((&argument as *const T).cast::<u8>(), size_of::<T>())
        };
        self.add_heap_bytes(data)
    }

    pub fn add_heap_bytes(&mut self, data: &[u8]) -> u64 {
        let aligned_size = (data.len() + STACK_ALIGN - 1) & !(STACK_ALIGN - 1);
        let mut state = self.state.lock().unwrap();
        let heap_pointer = state.heap_pointer as usize;
        if heap_pointer + aligned_size > state.local_memory.len() {
            state.local_memory.resize(heap_pointer + aligned_size, 0);
        }
        state.local_memory[heap_pointer..heap_pointer + data.len()].copy_from_slice(data);
        state.heap_pointer += aligned_size as u64;
        heap_pointer as u64
    }

    pub fn get_heap<T: Copy>(&self, location: u64) -> T {
        let mut result = MaybeUninit::<T>::uninit();
        self.get_heap_into(location, unsafe {
            std::slice::from_raw_parts_mut(result.as_mut_ptr().cast::<u8>(), size_of::<T>())
        });
        unsafe { result.assume_init() }
    }

    pub fn get_heap_into(&self, location: u64, output: &mut [u8]) {
        let state = self.state.lock().unwrap();
        let start = location as usize;
        output.copy_from_slice(&state.local_memory[start..start + output.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callbacks_implement_the_architecture_owned_a64_interface() {
        fn assert_a64_callbacks<T: A64UserCallbacks>() {}

        assert_a64_callbacks::<DynarmicCallbacks64>();
    }

    fn minimal_nro(code_offset: usize, code: &[u32]) -> Vec<u8> {
        let mut data = vec![0; (code_offset + code.len() * 4).max(0x38)];
        data[4..8].copy_from_slice(&0x10u32.to_le_bytes());
        data[0x10..0x14].copy_from_slice(&make_magic(b'M', b'O', b'D', b'0').to_le_bytes());
        data[0x14..0x18].copy_from_slice(&8u32.to_le_bytes());
        for (index, instruction) in code.iter().enumerate() {
            let start = code_offset + index * 4;
            data[start..start + 4].copy_from_slice(&instruction.to_le_bytes());
        }
        data
    }

    #[test]
    fn load_nro_and_call_function_use_helpers_and_aarch64_argument_abi() {
        // LDR X1, [SP]; ADD X0, X0, X1; RET
        let code = [0xF940_03E1, 0x8B01_0000, 0xD65F_03C0];
        let mut context = JitContext::new_impl(None).unwrap();
        assert!(context.load_nro(&minimal_nro(0x40, &code)));

        let result = context.call_function(0x40, &[5, 0, 0, 0, 0, 0, 0, 0, 7]);
        assert_eq!(result, 12);
    }

    #[test]
    fn instruction_barrier_invalidates_the_cached_code_page() {
        let state = Arc::new(Mutex::new(ContextState::new()));
        state.lock().unwrap().local_memory.resize(CODE_PAGE_SIZE, 0);
        state.lock().unwrap().local_memory[0..4].copy_from_slice(&1u32.to_le_bytes());
        let mut callbacks = DynarmicCallbacks64::new(None, Arc::clone(&state));

        assert_eq!(callbacks.memory_read_code(0), Some(1));
        state.lock().unwrap().local_memory[0..4].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(callbacks.memory_read_code(0), Some(1));
        callbacks.instruction_synchronization_barrier_raised();
        assert_eq!(callbacks.memory_read_code(0), Some(2));
    }

    #[test]
    fn heap_allocations_keep_upstream_sixteen_byte_alignment() {
        let mut context = JitContext::new_impl(None).unwrap();
        context.state.lock().unwrap().local_memory.resize(32, 0);
        context.state.lock().unwrap().top_of_stack = 32;
        context.state.lock().unwrap().heap_pointer = 32;

        let first = context.add_heap(0x1122_3344u32);
        let second = context.add_heap(0x5566_7788u32);
        assert_eq!(first, 32);
        assert_eq!(second, 48);
        assert_eq!(context.get_heap::<u32>(first), 0x1122_3344);
    }
}
