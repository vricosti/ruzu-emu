//! Port of zuyu/src/core/hle/service/filesystem/fsp/fs_i_filesystem.h and .cpp
//!
//! IFileSystem service.
//!
//! Uses the CMIF helpers (`CmifRequest` / `CmifResponse`) to mirror upstream's
//! `Result Method(Out<T>, In<T>, ...)` signatures. Each business-logic method
//! is a thin Rust port of the upstream method; per-cmd dispatch shims read
//! inputs, call the method, and serialize outputs via the CMIF response
//! builder. This matches upstream's `D<&IFileSystem::Method>` dispatch table
//! semantically — argument-count and move-slot reservations come from the
//! method signature, not hand-tuned `normal_params_size` constants.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::file_sys::fs_filesystem::{
    CreateOption, FileSystemAttribute, OpenDirectoryMode, OpenMode,
};
use crate::file_sys::fsa::fs_i_filesystem::IFileSystem as FsaIFileSystem;
use crate::file_sys::fssrv::fssrv_sf_path::Path as SfPath;
use crate::file_sys::vfs::vfs_types::{FileTimeStampRaw, VirtualDir};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};
use crate::hle::service::cmif_serialization::{CmifRequest, CmifResponse};
use crate::hle::service::hle_ipc::{HLERequestContext, SessionRequestHandler};
use crate::hle::service::service::{build_handler_map, FunctionInfo, ServiceFramework};

use super::fs_i_directory::IDirectory;
use super::fs_i_file::IFile;
use super::fsp_types::SizeGetter;

/// IPC command table for IFileSystem:
///
/// | Cmd | Name                        |
/// |-----|-----------------------------|
/// | 0   | CreateFile                  |
/// | 1   | DeleteFile                  |
/// | 2   | CreateDirectory             |
/// | 3   | DeleteDirectory             |
/// | 4   | DeleteDirectoryRecursively  |
/// | 5   | RenameFile                  |
/// | 6   | RenameDirectory             |
/// | 7   | GetEntryType                |
/// | 8   | OpenFile                    |
/// | 9   | OpenDirectory               |
/// | 10  | Commit                      |
/// | 11  | GetFreeSpaceSize            |
/// | 12  | GetTotalSpaceSize           |
/// | 13  | CleanDirectoryRecursively   |
/// | 14  | GetFileTimeStampRaw         |
/// | 15  | QueryEntry                  |
/// | 16  | GetFileSystemAttribute      |
pub struct IFileSystem {
    backend: FsaIFileSystem,
    size_getter: SizeGetter,
    handlers: BTreeMap<u32, FunctionInfo>,
    handlers_tipc: BTreeMap<u32, FunctionInfo>,
}

/// Helper: number of u32 words needed for a `Result + raw<T>` response.
const fn response_words<T>() -> u32 {
    // result occupies 2 words (status + reserved); raw payload pads to multiples of 4.
    2 + ((core::mem::size_of::<T>() as u32 + 3) / 4)
}

impl IFileSystem {
    pub fn new(dir: VirtualDir, size_getter: SizeGetter) -> Self {
        let handlers = build_handler_map(&[
            (0, Some(Self::create_file_handler), "CreateFile"),
            (1, Some(Self::delete_file_handler), "DeleteFile"),
            (2, Some(Self::create_directory_handler), "CreateDirectory"),
            (3, Some(Self::delete_directory_handler), "DeleteDirectory"),
            (
                4,
                Some(Self::delete_directory_recursively_handler),
                "DeleteDirectoryRecursively",
            ),
            (5, Some(Self::rename_file_handler), "RenameFile"),
            (6, Some(Self::rename_directory_handler), "RenameDirectory"),
            (7, Some(Self::get_entry_type_handler), "GetEntryType"),
            (8, Some(Self::open_file_handler), "OpenFile"),
            (9, Some(Self::open_directory_handler), "OpenDirectory"),
            (10, Some(Self::commit_handler), "Commit"),
            (
                11,
                Some(Self::get_free_space_size_handler),
                "GetFreeSpaceSize",
            ),
            (
                12,
                Some(Self::get_total_space_size_handler),
                "GetTotalSpaceSize",
            ),
            (
                13,
                Some(Self::clean_directory_recursively_handler),
                "CleanDirectoryRecursively",
            ),
            (
                14,
                Some(Self::get_file_time_stamp_raw_handler),
                "GetFileTimeStampRaw",
            ),
            (
                16,
                Some(Self::get_file_system_attribute_handler),
                "GetFileSystemAttribute",
            ),
        ]);

        Self {
            backend: FsaIFileSystem::new(dir),
            size_getter,
            handlers,
            handlers_tipc: BTreeMap::new(),
        }
    }

    fn as_self(this: &dyn ServiceFramework) -> &Self {
        unsafe { &*(this as *const dyn ServiceFramework as *const Self) }
    }

    /// Decode a `FileSys::Sf::Path` payload into a Rust string.
    fn decode_path_bytes(path_bytes: &[u8]) -> Result<String, ResultCode> {
        if path_bytes.len() < core::mem::size_of::<SfPath>() {
            return Err(ResultCode::new(u32::MAX));
        }
        let mut path = SfPath::default();
        unsafe {
            core::ptr::copy_nonoverlapping(
                path_bytes.as_ptr(),
                &mut path as *mut SfPath as *mut u8,
                core::mem::size_of::<SfPath>(),
            );
        }
        Ok(path.as_str().to_string())
    }

    /// Read a `FileSys::Sf::Path` (upstream `InLargeData<Path, BufferAttr_HipcPointer>`)
    /// from the request's X descriptor at the given index.
    ///
    /// Upstream CMIF unwrap uses `ReadBufferX()` for `BufferAttr_HipcPointer`.
    /// Using the local `ReadBuffer()` auto-select path here can consume the
    /// wrong descriptor when both A and X are present.
    fn read_path(ctx: &HLERequestContext, index: usize) -> Result<String, ResultCode> {
        let path_bytes = ctx.read_buffer_x(index);
        Self::decode_path_bytes(&path_bytes)
    }

    // ---------------- Business-logic methods ----------------
    // Each method mirrors the upstream signature in `Result Method(Out<T>, In<T>, ...)`
    // form: outputs are `&mut` arguments, the return value is just the status.

    fn create_file(&self, path: &str, option: CreateOption, size: i64) -> ResultCode {
        log::debug!(
            "IFileSystem::CreateFile called. file={}, option={:?}, size=0x{:08X}",
            path,
            option,
            size
        );
        match self.backend.create_file(path, size, option) {
            Ok(()) => RESULT_SUCCESS,
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn delete_file(&self, path: &str) -> ResultCode {
        log::debug!("IFileSystem::DeleteFile called. file={}", path);
        match self.backend.delete_file(path) {
            Ok(()) => RESULT_SUCCESS,
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn create_directory(&self, path: &str) -> ResultCode {
        log::debug!("IFileSystem::CreateDirectory called. directory={}", path);
        match self.backend.create_directory(path) {
            Ok(()) => RESULT_SUCCESS,
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn delete_directory(&self, path: &str) -> ResultCode {
        log::debug!("IFileSystem::DeleteDirectory called. directory={}", path);
        match self.backend.delete_directory(path) {
            Ok(()) => RESULT_SUCCESS,
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn delete_directory_recursively(&self, path: &str) -> ResultCode {
        log::debug!(
            "IFileSystem::DeleteDirectoryRecursively called. directory={}",
            path
        );
        match self.backend.delete_directory_recursively(path) {
            Ok(()) => RESULT_SUCCESS,
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn rename_file(&self, old_path: &str, new_path: &str) -> ResultCode {
        log::debug!(
            "IFileSystem::RenameFile called. file '{}' to file '{}'",
            old_path,
            new_path
        );
        match self.backend.rename_file(old_path, new_path) {
            Ok(()) => RESULT_SUCCESS,
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn rename_directory(&self, old_path: &str, new_path: &str) -> ResultCode {
        log::debug!("IFileSystem::RenameDirectory called. directory '{}' to directory '{}'", old_path, new_path);
        match self.backend.rename_directory(old_path, new_path) {
            Ok(()) => RESULT_SUCCESS,
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn get_entry_type(&self, out_type: &mut u32, path: &str) -> ResultCode {
        log::debug!("IFileSystem::GetEntryType called. file={}", path);
        match self.backend.get_entry_type(path) {
            Ok(entry_type) => {
                *out_type = entry_type as u32;
                RESULT_SUCCESS
            }
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn open_file(
        &self,
        out_interface: &mut Option<Arc<IFile>>,
        path: &str,
        mode: u32,
    ) -> ResultCode {
        log::debug!("IFileSystem::OpenFile called. file={}, mode={}", path, mode);
        let mode = OpenMode::from_bits_truncate(mode);
        match self.backend.open_file(path, mode) {
            Ok(vfs_file) => {
                *out_interface = Some(Arc::new(IFile::new(vfs_file)));
                RESULT_SUCCESS
            }
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn open_directory(
        &self,
        out_interface: &mut Option<Arc<IDirectory>>,
        path: &str,
        mode: u32,
    ) -> ResultCode {
        log::debug!(
            "IFileSystem::OpenDirectory called. directory={}, mode={}",
            path,
            mode
        );
        let mode = OpenDirectoryMode::from_bits_truncate(mode as u64);
        match self.backend.open_directory(path, mode) {
            Ok(vfs_dir) => {
                *out_interface = Some(Arc::new(IDirectory::new(vfs_dir, mode)));
                RESULT_SUCCESS
            }
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn get_free_space_size(&self, out_size: &mut i64) -> ResultCode {
        *out_size = (self.size_getter.get_free_size)() as i64;
        RESULT_SUCCESS
    }

    fn get_total_space_size(&self, out_size: &mut i64) -> ResultCode {
        *out_size = (self.size_getter.get_total_size)() as i64;
        RESULT_SUCCESS
    }

    fn clean_directory_recursively(&self, path: &str) -> ResultCode {
        log::debug!(
            "IFileSystem::CleanDirectoryRecursively called. directory={}",
            path
        );
        match self.backend.clean_directory_recursively(path) {
            Ok(()) => RESULT_SUCCESS,
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn get_file_time_stamp_raw(
        &self,
        out_timestamp: &mut FileTimeStampRaw,
        path: &str,
    ) -> ResultCode {
        log::warn!("IFileSystem::GetFileTimeStampRaw called. file={}", path);
        match self.backend.get_file_time_stamp_raw(path) {
            Ok(timestamp) => {
                *out_timestamp = timestamp;
                RESULT_SUCCESS
            }
            Err(rc) => ResultCode::new(rc.0),
        }
    }

    fn get_file_system_attribute(&self, out_attribute: &mut FileSystemAttribute) -> ResultCode {
        log::warn!("IFileSystem::GetFileSystemAttribute called");
        *out_attribute = FileSystemAttribute {
            dir_entry_name_length_max_defined: 1,
            file_entry_name_length_max_defined: 1,
            dir_path_name_length_max_defined: 0,
            file_path_name_length_max_defined: 0,
            _padding0: [0; 0x5],
            utf16_dir_entry_name_length_max_defined: 0,
            utf16_file_entry_name_length_max_defined: 0,
            utf16_dir_path_name_length_max_defined: 0,
            utf16_file_path_name_length_max_defined: 0,
            _padding1: [0; 0x18],
            dir_entry_name_length_max: 0x40,
            file_entry_name_length_max: 0x40,
            dir_path_name_length_max: 0,
            file_path_name_length_max: 0,
            _padding2: [0; 0x5],
            utf16_dir_entry_name_length_max: 0,
            utf16_file_entry_name_length_max: 0,
            utf16_dir_path_name_length_max: 0,
            utf16_file_path_name_length_max: 0,
            _padding3: [0; 0x18],
            _padding4: [0; 1],
        };
        RESULT_SUCCESS
    }

    // ---------------- Per-cmd dispatch shims ----------------

    fn reply_result_only(ctx: &mut HLERequestContext, result: ResultCode) {
        let mut response = CmifResponse::result_only(ctx, result);
        let _ = &mut response;
    }

    /// Reply for handlers returning `OutInterface<T>`. Upstream's CMIF reserves
    /// the move-handle slot regardless of success/failure; on success the slot
    /// gets the new session handle, on error it gets 0.
    fn reply_with_interface<T: SessionRequestHandler + 'static>(
        ctx: &mut HLERequestContext,
        result: ResultCode,
        object: Option<Arc<T>>,
    ) {
        // Capture the domain mode before constructing the response, since
        // CmifResponse::new takes a mutable borrow of `ctx`.
        let is_domain = ctx
            .get_manager()
            .map_or(false, |m| m.lock().unwrap().is_domain());
        if object.is_none() && is_domain {
            // Upstream `WriteOutArgument` always calls `AddDomainObject`, even
            // when the shared pointer was left null by an error return.
            ctx.add_null_domain_object();
        }
        let mut response = CmifResponse::new(ctx, 2, 0, 1);
        response.push_result(result);
        match object {
            Some(obj) => response.push_interface(obj),
            None => {
                if !is_domain {
                    response.push_move_objects(0);
                }
            }
        }
    }

    fn parse_create_file_arguments(ctx: &mut HLERequestContext) -> (i32, i64) {
        let mut request = CmifRequest::new(ctx);
        let option = request.raw::<i32>();
        request.align_for::<i64>();
        let size = request.raw::<i64>();
        (option, size)
    }

    fn create_file_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let (option_raw, size) = Self::parse_create_file_arguments(ctx);
        let path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let option = match option_raw {
            1 => CreateOption::BigFile,
            _ => CreateOption::None,
        };
        let result = service.create_file(&path, option, size);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            eprintln!(
                "[FS_RESULT] CreateFile path={} option_raw={} size={} result=0x{:08x}",
                path,
                option_raw,
                size,
                result.get_inner_value()
            );
        }
        Self::reply_result_only(ctx, result);
    }

    fn delete_file_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let result = service.delete_file(&path);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            eprintln!(
                "[FS_RESULT] DeleteFile path={} result=0x{:08x}",
                path,
                result.get_inner_value()
            );
        }
        Self::reply_result_only(ctx, result);
    }

    fn create_directory_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let result = service.create_directory(&path);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            eprintln!(
                "[FS_RESULT] CreateDirectory path={} result=0x{:08x}",
                path,
                result.get_inner_value()
            );
        }
        Self::reply_result_only(ctx, result);
    }

    fn delete_directory_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let result = service.delete_directory(&path);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            eprintln!(
                "[FS_RESULT] DeleteDirectory path={} result=0x{:08x}",
                path,
                result.get_inner_value()
            );
        }
        Self::reply_result_only(ctx, result);
    }

    fn delete_directory_recursively_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let result = service.delete_directory_recursively(&path);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            eprintln!(
                "[FS_RESULT] DeleteDirectoryRecursively path={} result=0x{:08x}",
                path,
                result.get_inner_value()
            );
        }
        Self::reply_result_only(ctx, result);
    }

    fn rename_file_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let old_path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let new_path = match Self::read_path(ctx, 1) {
            Ok(p) => p,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let result = service.rename_file(&old_path, &new_path);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            eprintln!(
                "[FS_RESULT] RenameFile old_path={} new_path={} result=0x{:08x}",
                old_path,
                new_path,
                result.get_inner_value()
            );
        }
        Self::reply_result_only(ctx, result);
    }

    fn rename_directory_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let old_path = match Self::read_path(ctx, 0) {
            Ok(path) => path,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let new_path = match Self::read_path(ctx, 1) {
            Ok(path) => path,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        Self::reply_result_only(ctx, service.rename_directory(&old_path, &new_path));
    }

    fn get_entry_type_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let mut out_type: u32 = 0;
        let result = service.get_entry_type(&mut out_type, &path);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            eprintln!(
                "[FS_RESULT] GetEntryType path={} result=0x{:08x} out_type={}",
                path,
                result.get_inner_value(),
                out_type
            );
        }
        if result == RESULT_SUCCESS {
            // Out<u32>: 3 words = result(2) + u32(1).
            let mut response = CmifResponse::new(ctx, response_words::<u32>(), 0, 0);
            response.push_result(result);
            response.push_u32(out_type);
        } else {
            Self::reply_result_only(ctx, result);
        }
    }

    fn open_file_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let mode_raw = request.raw::<u32>();
        let path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_with_interface::<IFile>(ctx, rc, None),
        };
        let mut out_interface: Option<Arc<IFile>> = None;
        let result = service.open_file(&mut out_interface, &path, mode_raw);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            eprintln!(
                "[FS_RESULT] OpenFile path={} mode=0x{:x} result=0x{:08x} has_iface={}",
                path,
                mode_raw,
                result.get_inner_value(),
                out_interface.is_some()
            );
        }
        Self::reply_with_interface(ctx, result, out_interface);
    }

    fn open_directory_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut request = CmifRequest::new(ctx);
        let mode_raw = request.raw::<u32>();
        let path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_with_interface::<IDirectory>(ctx, rc, None),
        };
        let mut out_interface: Option<Arc<IDirectory>> = None;
        let result = service.open_directory(&mut out_interface, &path, mode_raw);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            let tid: u64 = ctx
                .get_thread()
                .map(|t| t.lock().unwrap().get_thread_id())
                .unwrap_or(0);
            // Per-core GUEST_PC/GUEST_LR snapshot from the last SVC return.
            // For a single-threaded loop the SVC issuer's LR points at the
            // game function that called the libnx fsdev_opendir wrapper.
            let pcs: Vec<String> = (0..crate::hle::kernel::kernel::GUEST_PC.len())
                .map(|i| {
                    format!(
                        "core{}: pc=0x{:X} lr=0x{:X}",
                        i,
                        crate::hle::kernel::kernel::GUEST_PC[i]
                            .load(std::sync::atomic::Ordering::Acquire),
                        crate::hle::kernel::kernel::GUEST_LR[i]
                            .load(std::sync::atomic::Ordering::Acquire),
                    )
                })
                .collect();
            eprintln!(
                "[FS_RESULT] OpenDirectory tid={} path={} mode=0x{:x} result=0x{:08x} has_iface={} | {}",
                tid,
                path,
                mode_raw,
                result.get_inner_value(),
                out_interface.is_some(),
                pcs.join(" "),
            );
        }
        Self::reply_with_interface(ctx, result, out_interface);
    }

    fn commit_handler(_this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        Self::reply_result_only(ctx, RESULT_SUCCESS);
    }

    fn get_free_space_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        // Path is read but unused upstream — kept for protocol parity.
        let _ = Self::read_path(ctx, 0);
        let mut out_size: i64 = 0;
        let result = service.get_free_space_size(&mut out_size);
        let mut response = CmifResponse::new(ctx, response_words::<i64>(), 0, 0);
        response.push_result(result);
        response.push_u64(out_size as u64);
    }

    fn get_total_space_size_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let _ = Self::read_path(ctx, 0);
        let mut out_size: i64 = 0;
        let result = service.get_total_space_size(&mut out_size);
        let mut response = CmifResponse::new(ctx, response_words::<i64>(), 0, 0);
        response.push_result(result);
        response.push_u64(out_size as u64);
    }

    fn clean_directory_recursively_handler(
        this: &dyn ServiceFramework,
        ctx: &mut HLERequestContext,
    ) {
        let service = Self::as_self(this);
        let path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let result = service.clean_directory_recursively(&path);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            eprintln!(
                "[FS_RESULT] CleanDirectoryRecursively path={} result=0x{:08x}",
                path,
                result.get_inner_value()
            );
        }
        Self::reply_result_only(ctx, result);
    }

    fn get_file_time_stamp_raw_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let path = match Self::read_path(ctx, 0) {
            Ok(p) => p,
            Err(rc) => return Self::reply_result_only(ctx, rc),
        };
        let mut out_timestamp = FileTimeStampRaw::default();
        let result = service.get_file_time_stamp_raw(&mut out_timestamp, &path);
        if std::env::var_os("RUZU_TRACE_FS_RESULTS").is_some() {
            let tid: u64 = ctx
                .get_thread()
                .map(|t| t.lock().unwrap().get_thread_id())
                .unwrap_or(0);
            let pcs: Vec<String> = (0..crate::hle::kernel::kernel::GUEST_PC.len())
                .map(|i| {
                    format!(
                        "core{}: pc=0x{:X} lr=0x{:X}",
                        i,
                        crate::hle::kernel::kernel::GUEST_PC[i]
                            .load(std::sync::atomic::Ordering::Acquire),
                        crate::hle::kernel::kernel::GUEST_LR[i]
                            .load(std::sync::atomic::Ordering::Acquire),
                    )
                })
                .collect();
            eprintln!(
                "[FS_RESULT] GetFileTimeStampRaw tid={} path={} result=0x{:08x} | {}",
                tid,
                path,
                result.get_inner_value(),
                pcs.join(" "),
            );
        }
        if result == RESULT_SUCCESS {
            let mut response = CmifResponse::new(ctx, response_words::<FileTimeStampRaw>(), 0, 0);
            response.push_result(result);
            response.push_raw(&out_timestamp);
        } else {
            Self::reply_result_only(ctx, result);
        }
    }

    fn get_file_system_attribute_handler(this: &dyn ServiceFramework, ctx: &mut HLERequestContext) {
        let service = Self::as_self(this);
        let mut out_attribute = unsafe { core::mem::zeroed::<FileSystemAttribute>() };
        let result = service.get_file_system_attribute(&mut out_attribute);
        let mut response = CmifResponse::new(ctx, response_words::<FileSystemAttribute>(), 0, 0);
        response.push_result(result);
        response.push_raw(&out_attribute);
    }
}

impl SessionRequestHandler for IFileSystem {
    fn handle_sync_request(&self, context: &mut HLERequestContext) -> ResultCode {
        ServiceFramework::handle_sync_request_impl(self, context)
    }

    fn service_name(&self) -> &str {
        "IFileSystem"
    }
}

impl ServiceFramework for IFileSystem {
    fn get_service_name(&self) -> &str {
        "IFileSystem"
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
    use super::IFileSystem;
    use crate::file_sys::fssrv::fssrv_sf_path::Path as SfPath;
    use crate::hle::service::hle_ipc::HLERequestContext;

    #[test]
    fn rename_directory_is_registered_and_preserves_backend_results() {
        use crate::file_sys::vfs::vfs_vector::VectorVfsDirectory;
        use crate::file_sys::vfs::vfs_types::VirtualDir;
        use crate::hle::service::service::ServiceFramework;
        use super::SizeGetter;
        use std::sync::Arc;
        let child: VirtualDir = Arc::new(VectorVfsDirectory::new(vec![], vec![], "old".into(), None));
        let root: VirtualDir = Arc::new(VectorVfsDirectory::new(vec![], vec![child.clone()], "root".into(), None));
        let service = IFileSystem::new(root, SizeGetter {
            get_free_size: Box::new(|| 0), get_total_size: Box::new(|| 0),
        });
        let entry = &service.handlers()[&6];
        assert_eq!(entry.name, "RenameDirectory");
        assert!(entry.handler_callback.is_some());
        assert!(service.rename_directory("/old", "/new").is_success());
        assert_eq!(child.get_name(), "new");
        assert_eq!(service.rename_directory("/absent", "/new").get_inner_value(),
            crate::file_sys::errors::RESULT_PATH_NOT_FOUND.0);
        let mut ctx = HLERequestContext::new();
        entry.handler_callback.unwrap()(&service, &mut ctx);
        assert_ne!(ctx.cmd_buf[6], 0); // Missing X paths must not return success.
    }

    #[test]
    fn decode_path_bytes_round_trips_full_nested_path() {
        let path = SfPath::encode("//./supertuxkart/cached-textures/");
        let bytes = unsafe {
            core::slice::from_raw_parts(
                &path as *const SfPath as *const u8,
                core::mem::size_of::<SfPath>(),
            )
        };
        let decoded = IFileSystem::decode_path_bytes(bytes).unwrap();
        assert_eq!(decoded, "//./supertuxkart/cached-textures/");
    }

    #[test]
    fn decode_path_bytes_rejects_short_buffer() {
        assert!(IFileSystem::decode_path_bytes(&[0u8; 8]).is_err());
    }

    #[test]
    fn create_file_size_is_aligned_after_s32_option() {
        let mut ctx = HLERequestContext::new();
        ctx.data_payload_offset = 10;
        let command_buffer = ctx.command_buffer_mut();
        command_buffer[12] = 1;
        command_buffer[13] = 0;
        command_buffer[14] = 0x59e;
        command_buffer[15] = 0;

        let (option, size) = IFileSystem::parse_create_file_arguments(&mut ctx);

        assert_eq!(option, 1);
        assert_eq!(size, 0x59e);
    }
}
