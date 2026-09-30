//! Port of zuyu/src/core/hle/service/filesystem/fsp/fs_i_storage.h and fs_i_storage.cpp
//!
//! IStorage service.
//!
//! Mirrors upstream's `D<&IStorage::Method>` dispatch + `Out<T>` / `OutBuffer<T>`
//! signatures via the CMIF helpers (`CmifRequest`, `CmifResponse`).

use std::collections::BTreeMap;

use crate::file_sys::errors;
use crate::file_sys::vfs::vfs_types::VirtualFile;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::cmif_serialization::{CmifRequest, CmifResponse};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};
use std::sync::atomic::Ordering;

#[inline]
fn to_ipc_result(r: common::ResultCode) -> ResultCode {
    ResultCode::new(r.raw())
}

/// Number of u32 words for `Result + raw<T>` CMIF response.
const fn response_words<T>() -> u32 {
    2 + ((core::mem::size_of::<T>() as u32 + 3) / 4)
}

/// IPC command table for IStorage:
///
/// | Cmd | Name         |
/// |-----|--------------|
/// | 0   | Read         |
/// | 1   | Write        |
/// | 2   | Flush        |
/// | 3   | SetSize      |
/// | 4   | GetSize      |
/// | 5   | OperateRange |
pub struct IStorage {
    backend: VirtualFile,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

impl IStorage {
    pub fn new(backend: VirtualFile) -> Self {
        if std::env::var_os("RUZU_TRACE_ISTORAGE_OPEN").is_some() {
            log::warn!(
                "IStorage::Open backend name={} path={} size={}",
                backend.get_name(),
                backend.get_full_path(),
                backend.get_size()
            );
        }
        Self {
            backend,
            handlers: build_handler_map(&[
                (0, Some(Self::read_handler), "Read"),
                (1, None, "Write"),
                (2, None, "Flush"),
                (3, None, "SetSize"),
                (4, Some(Self::get_size_handler), "GetSize"),
                (5, None, "OperateRange"),
            ]),
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    // ---------------- Business-logic methods ----------------
    // Mirror upstream `Result IStorage::Read(OutBuffer, s64, s64)` and
    // `Result IStorage::GetSize(Out<u64>)`.

    fn read(&self, out_bytes: &mut [u8], offset: i64, length: i64) -> ResultCode {
        let backend_size = self.backend.get_size();
        log::debug!(
            "IStorage::Read called, offset=0x{:X}, length={}, backend_size={}",
            offset,
            length,
            backend_size
        );
        if length < 0 {
            return to_ipc_result(errors::RESULT_INVALID_SIZE);
        }
        if offset < 0 {
            return to_ipc_result(errors::RESULT_INVALID_OFFSET);
        }
        let read_len = out_bytes.len().min(length as usize);
        let bytes_read = self.backend.read(out_bytes, read_len, offset as usize);
        if bytes_read != read_len {
            log::warn!(
                "IStorage::Read short read, offset=0x{:X}, requested={}, buffer_size={}, read={}, backend_size={}",
                offset,
                length,
                out_bytes.len(),
                bytes_read,
                backend_size
            );
        }
        // Investigation hook — mirrors zuyu's ZUYU_ISTORAGE_READ_DUMP.
        if std::env::var_os("RUZU_ISTORAGE_READ_DUMP").is_some() {
            use std::fmt::Write;
            let n = out_bytes.len().min(64);
            let mut hex = String::new();
            for b in &out_bytes[..n] {
                let _ = write!(hex, "{:02x}", b);
            }
            log::warn!(
                "IStorage::Read offset=0x{:X} length={} backend_size={} first{}B={}",
                offset,
                length,
                backend_size,
                n,
                hex
            );
        }
        RESULT_SUCCESS
    }

    fn get_size(&self, out_size: &mut u64) -> ResultCode {
        *out_size = self.backend.get_size() as u64;
        log::debug!("IStorage::GetSize called, size={}", *out_size);
        RESULT_SUCCESS
    }

    // ---------------- Per-cmd dispatch shims ----------------

    fn reply_result_only(ctx: &mut HLERequestContext, result: ResultCode) {
        let mut response = CmifResponse::result_only(ctx, result);
        let _ = &mut response;
    }

    fn read_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let storage = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let offset = request.raw::<i64>();
        let length = request.raw::<i64>();
        let buffer_size = ctx.get_write_buffer_size(0);
        let read_size = if length >= 0 {
            core::cmp::min(length as usize, buffer_size)
        } else {
            0
        };
        if std::env::var_os("RUZU_TRACE_ISTORAGE_ANOMALY").is_some()
            && (length < 0 || buffer_size < length as usize)
        {
            let tid = ctx
                .get_thread()
                .map(|thread| thread.lock().unwrap().get_thread_id())
                .unwrap_or(0);
            log::warn!(
                "IStorage::Read anomaly tid={} offset=0x{:X} length={} buffer_size={} read_size={}",
                tid,
                offset,
                length,
                buffer_size,
                read_size
            );
        }
        let mut buffer = vec![0u8; read_size];
        let result = storage.read(&mut buffer, offset, length);
        if let Ok(raw_values) = std::env::var("RUZU_ISTORAGE_SCAN_U32_VALUE") {
            let output_address = ctx
                .buffer_descriptor_b()
                .get(0)
                .filter(|desc| desc.size() > 0)
                .map(|desc| desc.address())
                .or_else(|| ctx.buffer_descriptor_c().get(0).map(|desc| desc.address()))
                .unwrap_or(0);
            let targets: Vec<u32> = raw_values
                .split(',')
                .filter_map(|raw| {
                    let raw = raw.trim();
                    if raw.is_empty() {
                        return None;
                    }
                    let raw = raw
                        .strip_prefix("0x")
                        .or_else(|| raw.strip_prefix("0X"))
                        .unwrap_or(raw);
                    u32::from_str_radix(raw, 16).ok()
                })
                .collect();
            if !targets.is_empty() {
                for (idx, word) in buffer.windows(4).enumerate() {
                    let value = u32::from_le_bytes(word.try_into().unwrap());
                    if targets.contains(&value) {
                        let (tid, thread_pc, thread_lr, thread_sp) = ctx
                            .get_thread()
                            .map(|thread| {
                                let guard = thread.lock().unwrap();
                                (
                                    guard.get_thread_id(),
                                    guard.thread_context.pc,
                                    guard.thread_context.lr,
                                    guard.thread_context.sp,
                                )
                            })
                            .unwrap_or((0, 0, 0, 0));
                        log::warn!(
                            "IStorage::ReadScanU32 tid={} thread_pc=0x{:X} thread_lr=0x{:X} thread_sp=0x{:X} backend_name={} backend_path={} backend_size={} storage_offset=0x{:X} length={} buffer_guest=0x{:X} buffer_off=0x{:X} guest_addr=0x{:X} value=0x{:08X}",
                            tid,
                            thread_pc,
                            thread_lr,
                            thread_sp,
                            storage.backend.get_name(),
                            storage.backend.get_full_path(),
                            storage.backend.get_size(),
                            offset,
                            length,
                            output_address,
                            idx,
                            output_address.saturating_add(idx as u64),
                            value
                        );
                    }
                }
            }
        }
        if std::env::var_os("RUZU_ISTORAGE_READ_CONTEXT").is_some() {
            let (tid, thread_pc, thread_lr, thread_sp) = ctx
                .get_thread()
                .map(|thread| {
                    let guard = thread.lock().unwrap();
                    (
                        guard.get_thread_id(),
                        guard.thread_context.pc,
                        guard.thread_context.lr,
                        guard.thread_context.sp,
                    )
                })
                .unwrap_or((0, 0, 0, 0));
            let pcs: Vec<String> = (0..crate::hle::kernel::kernel::GUEST_PC.len())
                .map(|i| {
                    format!(
                        "core{}:pc=0x{:X}:lr=0x{:X}:sp=0x{:X}",
                        i,
                        crate::hle::kernel::kernel::GUEST_PC[i].load(Ordering::Acquire),
                        crate::hle::kernel::kernel::GUEST_LR[i].load(Ordering::Acquire),
                        crate::hle::kernel::kernel::GUEST_SP[i].load(Ordering::Acquire),
                    )
                })
                .collect();
            log::warn!(
                "IStorage::ReadContext tid={} thread_pc=0x{:X} thread_lr=0x{:X} thread_sp=0x{:X} backend_name={} backend_path={} backend_size={} offset=0x{:X} length={} buffer_size={} read_size={} result=0x{:08X} {}",
                tid,
                thread_pc,
                thread_lr,
                thread_sp,
                storage.backend.get_name(),
                storage.backend.get_full_path(),
                storage.backend.get_size(),
                offset,
                length,
                buffer_size,
                read_size,
                result.get_inner_value(),
                pcs.join(" ")
            );
            if let (Ok(spec), Some(memory)) =
                (std::env::var("RUZU_ISTORAGE_READ_U32_AT"), ctx.get_memory())
            {
                let memory = memory.access().unwrap();
                let values: Vec<String> = spec
                    .split(',')
                    .filter_map(|raw| {
                        let raw = raw.trim();
                        if raw.is_empty() {
                            return None;
                        }
                        let addr = u64::from_str_radix(raw.trim_start_matches("0x"), 16).ok()?;
                        Some(format!("0x{:X}=0x{:08X}", addr, memory.read_32(addr)))
                    })
                    .collect();
                log::warn!("IStorage::ReadU32At {}", values.join(" "));
            }
        }
        if result == RESULT_SUCCESS {
            ctx.write_buffer(&buffer, 0);
        }
        Self::reply_result_only(ctx, result);
    }

    fn get_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let storage = Self::as_self(this);
        let mut size: u64 = 0;
        let result = storage.get_size(&mut size);
        let mut response = CmifResponse::new(ctx, response_words::<u64>(), 0, 0);
        response.push_result(result);
        response.push_u64(size);
    }
}

impl SessionRequestHandler for IStorage {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }

    fn service_name(&self) -> &str {
        "IStorage"
    }
}

impl ServiceFramework for IStorage {
    fn get_service_name(&self) -> &str {
        "IStorage"
    }

    fn handlers(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers
    }

    fn handlers_tipc(&self) -> &BTreeMap<u32, FunctionInfo> {
        &self.handlers_tipc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;
    use std::sync::Arc;

    #[test]
    fn read_never_asks_backend_for_more_than_output_buffer() {
        let backend = Arc::new(VectorVfsFile::new(
            vec![1, 2, 3, 4],
            "storage.bin".to_string(),
            None,
        ));
        let storage = IStorage::new(backend);
        let mut out = [0u8; 2];

        let result = storage.read(&mut out, 0, 4);

        assert_eq!(result, RESULT_SUCCESS);
        assert_eq!(out, [1, 2]);
    }
}
