//! Port of zuyu/src/core/hle/kernel/svc/svc_debug_string.cpp
//! Status: COMPLET (stubs for kernel memory access)
//! Derniere synchro: 2026-03-11
//!
//! SVC handler for outputting debug strings.

use crate::core::System;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Used to output a message on a debug hardware unit — does nothing on a retail unit.
pub fn output_debug_string(system: &System, address: u64, len: u64) -> ResultCode {
    if len == 0 {
        return RESULT_SUCCESS;
    }

    let len = len as usize;
    let msg = if let Some(memory) = system.get_svc_memory() {
        let m = memory.access().unwrap();
        let mut buf = vec![0u8; len];
        m.read_block(address, &mut buf);
        String::from_utf8_lossy(&buf).to_string()
    } else {
        let mem = system.shared_process_memory().read().unwrap();
        if mem.is_valid_range(address, len) {
            String::from_utf8_lossy(mem.read_block(address, len)).to_string()
        } else {
            String::new()
        }
    };
    log::info!(
        "OutputDebugString(addr=0x{:X}, len={}): {}",
        address,
        len,
        msg
    );

    RESULT_SUCCESS
}
