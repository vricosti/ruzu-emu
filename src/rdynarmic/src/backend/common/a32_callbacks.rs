use crate::exclusive_monitor::ExclusiveMonitor;
use crate::jit_config::UserCallbacks;

/// Minimal A32 exclusive-monitor state needed by host callback thunks.
///
/// x64 and arm64 JIT states do not have identical layouts, but both expose
/// upstream's `exclusive_state` contract. x64 additionally stores the fallback
/// expected value locally; arm64 uses the global monitor for the upstream path.
pub trait A32ExclusiveState {
    fn exclusive_state(&self) -> u32;
    fn set_exclusive_state(&mut self, value: u32);
    fn exclusive_value(&self, index: usize) -> u64;
    fn set_exclusive_value(&mut self, index: usize, value: u64);
}

#[inline]
pub fn add_ticks(callbacks: &mut dyn UserCallbacks, ticks: u64) {
    callbacks.add_ticks(ticks);
}

#[inline]
pub fn get_ticks_remaining(callbacks: &dyn UserCallbacks) -> u64 {
    callbacks.get_ticks_remaining()
}

#[inline]
pub fn memory_read_8(callbacks: &dyn UserCallbacks, vaddr: u64) -> u64 {
    callbacks.memory_read_8(vaddr) as u64
}

#[inline]
pub fn memory_read_16(callbacks: &dyn UserCallbacks, vaddr: u64) -> u64 {
    callbacks.memory_read_16(vaddr) as u64
}

#[inline]
pub fn memory_read_32(callbacks: &dyn UserCallbacks, vaddr: u64) -> u64 {
    callbacks.memory_read_32(vaddr) as u64
}

#[inline]
pub fn memory_read_64(callbacks: &dyn UserCallbacks, vaddr: u64) -> u64 {
    callbacks.memory_read_64(vaddr)
}

#[inline]
pub fn memory_read_128(callbacks: &dyn UserCallbacks, vaddr: u64, ret_ptr: u64) {
    let (lo, hi) = callbacks.memory_read_128(vaddr);
    unsafe {
        let ptr = ret_ptr as *mut u64;
        *ptr = lo;
        *ptr.add(1) = hi;
    }
}

#[inline]
pub fn memory_write_8(callbacks: &mut dyn UserCallbacks, vaddr: u64, value: u64) {
    callbacks.memory_write_8(vaddr, value as u8);
}

#[inline]
pub fn memory_write_16(callbacks: &mut dyn UserCallbacks, vaddr: u64, value: u64) {
    callbacks.memory_write_16(vaddr, value as u16);
}

#[inline]
pub fn memory_write_32(callbacks: &mut dyn UserCallbacks, vaddr: u64, value: u64) {
    callbacks.memory_write_32(vaddr, value as u32);
}

#[inline]
pub fn memory_write_64(callbacks: &mut dyn UserCallbacks, vaddr: u64, value: u64) {
    callbacks.memory_write_64(vaddr, value);
}

#[inline]
pub fn memory_write_128(
    callbacks: &mut dyn UserCallbacks,
    vaddr: u64,
    value_lo: u64,
    value_hi: u64,
) {
    callbacks.memory_write_128(vaddr, value_lo, value_hi);
}

#[inline]
pub fn call_supervisor(callbacks: &mut dyn UserCallbacks, svc_num: u64) {
    callbacks.call_supervisor(svc_num as u32);
}

#[inline]
pub fn exception_raised(callbacks: &mut dyn UserCallbacks, pc: u64, exception: u64) {
    callbacks.exception_raised(pc, exception);
}

#[inline]
pub fn data_cache_operation(callbacks: &mut dyn UserCallbacks, op: u64, vaddr: u64) {
    callbacks.data_cache_operation(op, vaddr);
}

#[inline]
pub fn instruction_cache_operation(callbacks: &mut dyn UserCallbacks, op: u64, vaddr: u64) {
    callbacks.instruction_cache_operation(op, vaddr);
}

#[inline]
pub fn get_cntpct(callbacks: &dyn UserCallbacks) -> u64 {
    callbacks.get_cntpct()
}

#[inline]
pub fn exclusive_clear(state: &mut impl A32ExclusiveState) {
    state.set_exclusive_state(0);
}

pub fn exclusive_read_8(
    state: &mut impl A32ExclusiveState,
    callbacks: &mut dyn UserCallbacks,
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    vaddr: u64,
) -> u64 {
    state.set_exclusive_state(1);
    let value = if let Some(monitor) = global_monitor {
        unsafe {
            (&mut *monitor).read_and_mark(processor_id, vaddr, || callbacks.memory_read_8(vaddr))
        }
    } else {
        callbacks.memory_read_8(vaddr)
    };
    state.set_exclusive_value(0, value as u64);
    value as u64
}

pub fn exclusive_read_16(
    state: &mut impl A32ExclusiveState,
    callbacks: &mut dyn UserCallbacks,
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    vaddr: u64,
) -> u64 {
    state.set_exclusive_state(1);
    let value = if let Some(monitor) = global_monitor {
        unsafe {
            (&mut *monitor).read_and_mark(processor_id, vaddr, || callbacks.memory_read_16(vaddr))
        }
    } else {
        callbacks.memory_read_16(vaddr)
    };
    state.set_exclusive_value(0, value as u64);
    value as u64
}

pub fn exclusive_read_32(
    state: &mut impl A32ExclusiveState,
    callbacks: &mut dyn UserCallbacks,
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    vaddr: u64,
) -> u64 {
    state.set_exclusive_state(1);
    let value = if let Some(monitor) = global_monitor {
        unsafe {
            (&mut *monitor).read_and_mark(processor_id, vaddr, || callbacks.memory_read_32(vaddr))
        }
    } else {
        callbacks.memory_read_32(vaddr)
    };
    state.set_exclusive_value(0, value as u64);
    value as u64
}

pub fn exclusive_read_64(
    state: &mut impl A32ExclusiveState,
    callbacks: &mut dyn UserCallbacks,
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    vaddr: u64,
) -> u64 {
    state.set_exclusive_state(1);
    let value = if let Some(monitor) = global_monitor {
        unsafe {
            (&mut *monitor).read_and_mark(processor_id, vaddr, || callbacks.memory_read_64(vaddr))
        }
    } else {
        callbacks.memory_read_64(vaddr)
    };
    state.set_exclusive_value(0, value);
    value
}

pub fn exclusive_read_128(
    state: &mut impl A32ExclusiveState,
    callbacks: &mut dyn UserCallbacks,
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    vaddr: u64,
    ret_ptr: u64,
) {
    state.set_exclusive_state(1);
    let (lo, hi) = if let Some(monitor) = global_monitor {
        let value: [u64; 2] = unsafe {
            (&mut *monitor).read_and_mark(processor_id, vaddr, || {
                let (lo, hi) = callbacks.memory_read_128(vaddr);
                [lo, hi]
            })
        };
        (value[0], value[1])
    } else {
        callbacks.memory_read_128(vaddr)
    };
    state.set_exclusive_value(0, lo);
    state.set_exclusive_value(1, hi);
    unsafe {
        let ptr = ret_ptr as *mut u64;
        *ptr = lo;
        *ptr.add(1) = hi;
    }
}

pub fn exclusive_write_8(
    state: &mut impl A32ExclusiveState,
    callbacks: &mut dyn UserCallbacks,
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    vaddr: u64,
    value: u64,
) -> u64 {
    if state.exclusive_state() == 0 {
        return 1;
    }
    state.set_exclusive_state(0);
    if let Some(monitor) = global_monitor {
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u8| {
                callbacks.exclusive_write_8(vaddr, value as u8, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = state.exclusive_value(0) as u8;
    callbacks.exclusive_write_8(vaddr, value as u8, expected) as u64 ^ 1
}

pub fn exclusive_write_16(
    state: &mut impl A32ExclusiveState,
    callbacks: &mut dyn UserCallbacks,
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    vaddr: u64,
    value: u64,
) -> u64 {
    if state.exclusive_state() == 0 {
        return 1;
    }
    state.set_exclusive_state(0);
    if let Some(monitor) = global_monitor {
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u16| {
                callbacks.exclusive_write_16(vaddr, value as u16, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = state.exclusive_value(0) as u16;
    callbacks.exclusive_write_16(vaddr, value as u16, expected) as u64 ^ 1
}

pub fn exclusive_write_32(
    state: &mut impl A32ExclusiveState,
    callbacks: &mut dyn UserCallbacks,
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    vaddr: u64,
    value: u64,
) -> u64 {
    if state.exclusive_state() == 0 {
        return 1;
    }
    state.set_exclusive_state(0);
    if let Some(monitor) = global_monitor {
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u32| {
                callbacks.exclusive_write_32(vaddr, value as u32, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = state.exclusive_value(0) as u32;
    callbacks.exclusive_write_32(vaddr, value as u32, expected) as u64 ^ 1
}

pub fn exclusive_write_64(
    state: &mut impl A32ExclusiveState,
    callbacks: &mut dyn UserCallbacks,
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    vaddr: u64,
    value: u64,
) -> u64 {
    if state.exclusive_state() == 0 {
        return 1;
    }
    state.set_exclusive_state(0);
    if let Some(monitor) = global_monitor {
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: u64| {
                callbacks.exclusive_write_64(vaddr, value, expected)
            })
        } {
            0
        } else {
            1
        };
    }
    let expected = state.exclusive_value(0);
    callbacks.exclusive_write_64(vaddr, value, expected) as u64 ^ 1
}

pub fn exclusive_write_128(
    state: &mut impl A32ExclusiveState,
    callbacks: &mut dyn UserCallbacks,
    global_monitor: Option<*mut ExclusiveMonitor>,
    processor_id: usize,
    vaddr: u64,
    value_lo: u64,
    value_hi: u64,
) -> u64 {
    if state.exclusive_state() == 0 {
        return 1;
    }
    state.set_exclusive_state(0);
    if let Some(monitor) = global_monitor {
        return if unsafe {
            (&mut *monitor).do_exclusive_operation(processor_id, vaddr, |expected: [u64; 2]| {
                callbacks.exclusive_write_128(vaddr, value_lo, value_hi, expected[0], expected[1])
            })
        } {
            0
        } else {
            1
        };
    }
    let expected_lo = state.exclusive_value(0);
    let expected_hi = state.exclusive_value(1);
    callbacks.exclusive_write_128(vaddr, value_lo, value_hi, expected_lo, expected_hi) as u64 ^ 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestState {
        exclusive_state: u32,
        exclusive_value: [u64; 2],
    }

    impl A32ExclusiveState for TestState {
        fn exclusive_state(&self) -> u32 {
            self.exclusive_state
        }

        fn set_exclusive_state(&mut self, value: u32) {
            self.exclusive_state = value;
        }

        fn exclusive_value(&self, index: usize) -> u64 {
            self.exclusive_value[index]
        }

        fn set_exclusive_value(&mut self, index: usize, value: u64) {
            self.exclusive_value[index] = value;
        }
    }

    struct TestCallbacks {
        memory: [u8; 32],
        ticks_remaining: u64,
        ticks_added: u64,
        last_expected: u64,
    }

    impl Default for TestCallbacks {
        fn default() -> Self {
            Self {
                memory: [0; 32],
                ticks_remaining: 123,
                ticks_added: 0,
                last_expected: 0,
            }
        }
    }

    impl UserCallbacks for TestCallbacks {
        fn memory_read_code(&self, _vaddr: u64) -> Option<u32> {
            None
        }

        fn memory_read_8(&self, vaddr: u64) -> u8 {
            self.memory[vaddr as usize]
        }

        fn memory_read_16(&self, vaddr: u64) -> u16 {
            let offset = vaddr as usize;
            u16::from_le_bytes(self.memory[offset..offset + 2].try_into().unwrap())
        }

        fn memory_read_32(&self, vaddr: u64) -> u32 {
            let offset = vaddr as usize;
            u32::from_le_bytes(self.memory[offset..offset + 4].try_into().unwrap())
        }

        fn memory_read_64(&self, vaddr: u64) -> u64 {
            let offset = vaddr as usize;
            u64::from_le_bytes(self.memory[offset..offset + 8].try_into().unwrap())
        }

        fn memory_read_128(&self, vaddr: u64) -> (u64, u64) {
            (self.memory_read_64(vaddr), self.memory_read_64(vaddr + 8))
        }

        fn memory_write_8(&mut self, vaddr: u64, value: u8) {
            self.memory[vaddr as usize] = value;
        }

        fn memory_write_16(&mut self, vaddr: u64, value: u16) {
            let offset = vaddr as usize;
            self.memory[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }

        fn memory_write_32(&mut self, vaddr: u64, value: u32) {
            let offset = vaddr as usize;
            self.memory[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }

        fn memory_write_64(&mut self, vaddr: u64, value: u64) {
            let offset = vaddr as usize;
            self.memory[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }

        fn memory_write_128(&mut self, vaddr: u64, value_lo: u64, value_hi: u64) {
            self.memory_write_64(vaddr, value_lo);
            self.memory_write_64(vaddr + 8, value_hi);
        }

        fn exclusive_write_8(&mut self, vaddr: u64, value: u8, expected: u8) -> bool {
            self.last_expected = expected as u64;
            self.memory_write_8(vaddr, value);
            true
        }

        fn exclusive_write_16(&mut self, vaddr: u64, value: u16, expected: u16) -> bool {
            self.last_expected = expected as u64;
            self.memory_write_16(vaddr, value);
            true
        }

        fn exclusive_write_32(&mut self, vaddr: u64, value: u32, expected: u32) -> bool {
            self.last_expected = expected as u64;
            self.memory_write_32(vaddr, value);
            true
        }

        fn exclusive_write_64(&mut self, vaddr: u64, value: u64, expected: u64) -> bool {
            self.last_expected = expected;
            self.memory_write_64(vaddr, value);
            true
        }

        fn exclusive_write_128(
            &mut self,
            vaddr: u64,
            value_lo: u64,
            value_hi: u64,
            expected_lo: u64,
            _expected_hi: u64,
        ) -> bool {
            self.last_expected = expected_lo;
            self.memory_write_128(vaddr, value_lo, value_hi);
            true
        }

        fn call_supervisor(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: u64) {}

        fn add_ticks(&mut self, ticks: u64) {
            self.ticks_added += ticks;
        }

        fn get_ticks_remaining(&self) -> u64 {
            self.ticks_remaining
        }
    }

    #[test]
    fn memory_helpers_forward_raw_values() {
        let mut callbacks = TestCallbacks::default();

        memory_write_32(&mut callbacks, 4, 0x89ab_cdef);
        assert_eq!(memory_read_32(&callbacks, 4), 0x89ab_cdef);

        add_ticks(&mut callbacks, 7);
        assert_eq!(callbacks.ticks_added, 7);
        assert_eq!(get_ticks_remaining(&callbacks), 123);
    }

    #[test]
    fn exclusive_read_records_expected_value_for_local_fallback() {
        let mut state = TestState::default();
        let mut callbacks = TestCallbacks::default();
        callbacks.memory_write_32(8, 0xfeed_face);

        let value = exclusive_read_32(&mut state, &mut callbacks, None, 0, 8);

        assert_eq!(value, 0xfeed_face);
        assert_eq!(state.exclusive_state, 1);
        assert_eq!(state.exclusive_value[0], 0xfeed_face);
    }

    #[test]
    fn exclusive_clear_only_resets_jit_state() {
        let mut state = TestState {
            exclusive_state: 1,
            exclusive_value: [0x1122_3344, 0x5566_7788],
        };

        exclusive_clear(&mut state);

        assert_eq!(state.exclusive_state, 0);
        assert_eq!(state.exclusive_value, [0x1122_3344, 0x5566_7788]);
    }

    #[test]
    fn exclusive_write_uses_recorded_expected_value_and_clears_state() {
        let mut state = TestState {
            exclusive_state: 1,
            exclusive_value: [0x1122_3344, 0],
        };
        let mut callbacks = TestCallbacks::default();

        let result = exclusive_write_32(&mut state, &mut callbacks, None, 0, 12, 0xaabb_ccdd);

        assert_eq!(result, 0);
        assert_eq!(state.exclusive_state, 0);
        assert_eq!(callbacks.last_expected, 0x1122_3344);
        assert_eq!(callbacks.memory_read_32(12), 0xaabb_ccdd);
    }

    #[test]
    fn exclusive_write_without_reservation_fails_without_callback() {
        let mut state = TestState::default();
        let mut callbacks = TestCallbacks::default();

        let result = exclusive_write_32(&mut state, &mut callbacks, None, 0, 12, 0xaabb_ccdd);

        assert_eq!(result, 1);
        assert_eq!(callbacks.memory_read_32(12), 0);
    }
}
