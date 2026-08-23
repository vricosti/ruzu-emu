// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/fsa/fs_i_file.h
// Status: COMPLETE
//
// File abstraction for filesystem access. Includes public Read/Write/Flush/SetSize/
// GetSize/OperateRange methods matching upstream validation, plus protected
// DryRead/DryWrite/DrySetSize helpers for permission checks.

use crate::file_sys::errors;
use crate::file_sys::fs_file::{ReadOption, WriteOption};
use crate::file_sys::fs_filesystem::OpenMode;
use crate::file_sys::fs_operate_range::OperationId;
use crate::file_sys::vfs::vfs_types::VirtualFile;
use common::ResultCode;

/// File abstraction for filesystem access.
/// Corresponds to upstream `FileSys::Fsa::IFile`.
pub struct IFile {
    backend: VirtualFile,
}

impl IFile {
    pub fn new(backend: VirtualFile) -> Self {
        Self { backend }
    }

    /// Read from the file at the given offset.
    ///
    /// Corresponds to upstream `IFile::Read`.
    pub fn read(
        &self,
        offset: i64,
        buffer: &mut [u8],
        size: usize,
        option: &ReadOption,
    ) -> Result<usize, ResultCode> {
        // If we have nothing to read, just succeed.
        if size == 0 {
            return Ok(0);
        }

        // Check that the read is valid.
        if offset < 0 {
            return Err(errors::RESULT_OUT_OF_RANGE);
        }
        // Check for overflow: offset + size must not overflow i64.
        if (offset as u64).checked_add(size as u64).is_none()
            || (offset as u64) + (size as u64) > i64::MAX as u64
        {
            return Err(errors::RESULT_OUT_OF_RANGE);
        }

        // Do the read.
        self.do_read(offset, buffer, size, option)
    }

    /// Get the file size.
    ///
    /// Corresponds to upstream `IFile::GetSize`.
    pub fn get_size(&self) -> Result<i64, ResultCode> {
        self.do_get_size()
    }

    /// Flush the file.
    ///
    /// Corresponds to upstream `IFile::Flush`.
    pub fn flush(&self) -> Result<(), ResultCode> {
        self.do_flush()
    }

    /// Write to the file at the given offset.
    ///
    /// Corresponds to upstream `IFile::Write`.
    pub fn write(
        &self,
        offset: i64,
        buffer: &[u8],
        size: usize,
        option: &WriteOption,
    ) -> Result<(), ResultCode> {
        // Handle the zero-size case.
        if size == 0 {
            if option.has_flush_flag() {
                self.flush()?;
            }
            return Ok(());
        }

        // Check the write is valid.
        if offset < 0 {
            return Err(errors::RESULT_OUT_OF_RANGE);
        }
        // Check for overflow: offset + size must not overflow i64.
        if (offset as u64).checked_add(size as u64).is_none()
            || (offset as u64) + (size as u64) > i64::MAX as u64
        {
            return Err(errors::RESULT_OUT_OF_RANGE);
        }

        self.do_write(offset, buffer, size, option)
    }

    /// Set the file size.
    ///
    /// Corresponds to upstream `IFile::SetSize`.
    pub fn set_size(&self, size: i64) -> Result<(), ResultCode> {
        if size < 0 {
            return Err(errors::RESULT_OUT_OF_RANGE);
        }
        self.do_set_size(size)
    }

    /// Operate on a range of the file.
    ///
    /// Corresponds to upstream `IFile::OperateRange`.
    pub fn operate_range(
        &self,
        op_id: OperationId,
        offset: i64,
        size: i64,
    ) -> Result<(), ResultCode> {
        self.do_operate_range(op_id, offset, size)
    }

    // -- Protected helpers (DryRead, DryWrite, DrySetSize) --
    // These match upstream's protected interface for permission checking.

    /// Check whether a read is permitted given the open mode and current file size.
    ///
    /// Corresponds to upstream `IFile::DryRead`.
    pub fn dry_read(
        &self,
        offset: i64,
        size: usize,
        _option: &ReadOption,
        open_mode: OpenMode,
    ) -> Result<usize, ResultCode> {
        // Check that we can read.
        if !open_mode.contains(OpenMode::READ) {
            return Err(errors::RESULT_READ_NOT_PERMITTED);
        }

        // Get the file size and validate our offset.
        let file_size = self.do_get_size()?;
        if offset > file_size {
            return Err(errors::RESULT_OUT_OF_RANGE);
        }

        Ok(std::cmp::min((file_size - offset) as usize, size))
    }

    /// Check whether a set-size operation is permitted given the open mode.
    ///
    /// Corresponds to upstream `IFile::DrySetSize`.
    pub fn dry_set_size(&self, _size: i64, open_mode: OpenMode) -> Result<(), ResultCode> {
        if !open_mode.contains(OpenMode::WRITE) {
            return Err(errors::RESULT_WRITE_NOT_PERMITTED);
        }
        Ok(())
    }

    /// Check whether a write is permitted and whether appending is needed.
    ///
    /// Corresponds to upstream `IFile::DryWrite`.
    pub fn dry_write(
        &self,
        offset: i64,
        size: usize,
        _option: &WriteOption,
        open_mode: OpenMode,
    ) -> Result<bool, ResultCode> {
        // Check that we can write.
        if !open_mode.contains(OpenMode::WRITE) {
            return Err(errors::RESULT_WRITE_NOT_PERMITTED);
        }

        // Get the file size.
        let file_size = self.do_get_size()?;

        // Determine if we need to append.
        let mut out_append = false;
        if file_size < offset + size as i64 {
            if !open_mode.contains(OpenMode::ALLOW_APPEND) {
                return Err(errors::RESULT_FILE_EXTENSION_WITHOUT_OPEN_MODE_ALLOW_APPEND);
            }
            out_append = true;
        }

        Ok(out_append)
    }

    // -- Private Do* implementations --

    fn do_read(
        &self,
        offset: i64,
        buffer: &mut [u8],
        size: usize,
        _option: &ReadOption,
    ) -> Result<usize, ResultCode> {
        let read_size = self.backend.read(buffer, size, offset as usize);
        Ok(read_size)
    }

    fn do_get_size(&self) -> Result<i64, ResultCode> {
        Ok(self.backend.get_size() as i64)
    }

    fn do_flush(&self) -> Result<(), ResultCode> {
        // Exists for SDK compatibility -- No need to flush file.
        Ok(())
    }

    fn do_write(
        &self,
        offset: i64,
        buffer: &[u8],
        size: usize,
        _option: &WriteOption,
    ) -> Result<(), ResultCode> {
        let written = self.backend.write(buffer, size, offset as usize);
        assert_eq!(
            written, size,
            "Could not write all bytes to file (requested={:016X}, actual={:016X}).",
            size, written
        );
        Ok(())
    }

    fn do_set_size(&self, size: i64) -> Result<(), ResultCode> {
        self.backend.resize(size as usize);
        Ok(())
    }

    fn do_operate_range(
        &self,
        _op_id: OperationId,
        _offset: i64,
        _size: i64,
    ) -> Result<(), ResultCode> {
        Err(errors::RESULT_NOT_IMPLEMENTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_sys::fs_file::ReadOption;
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;
    use std::sync::Arc;

    fn make_test_file(data: &[u8]) -> VirtualFile {
        Arc::new(VectorVfsFile::new(
            data.to_vec(),
            "test.bin".to_string(),
            None,
        ))
    }

    #[test]
    fn test_read_basic() {
        let ifile = IFile::new(make_test_file(&[1, 2, 3, 4, 5]));
        let mut buf = [0u8; 3];
        let read = ifile.read(1, &mut buf, 3, &ReadOption::NONE).unwrap();
        assert_eq!(read, 3);
        assert_eq!(buf, [2, 3, 4]);
    }

    #[test]
    fn test_read_zero_size() {
        let ifile = IFile::new(make_test_file(&[1, 2, 3]));
        let mut buf = [0u8; 0];
        let read = ifile.read(0, &mut buf, 0, &ReadOption::NONE).unwrap();
        assert_eq!(read, 0);
    }

    #[test]
    fn test_read_negative_offset() {
        let ifile = IFile::new(make_test_file(&[1, 2, 3]));
        let mut buf = [0u8; 3];
        let result = ifile.read(-1, &mut buf, 3, &ReadOption::NONE);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_size() {
        let ifile = IFile::new(make_test_file(&[0u8; 42]));
        assert_eq!(ifile.get_size().unwrap(), 42);
    }

    #[test]
    fn test_set_size_negative() {
        let ifile = IFile::new(make_test_file(&[]));
        assert!(ifile.set_size(-1).is_err());
    }

    #[test]
    fn test_dry_read_no_read_permission() {
        let ifile = IFile::new(make_test_file(&[1, 2, 3]));
        let result = ifile.dry_read(0, 3, &ReadOption::NONE, OpenMode::WRITE);
        assert!(result.is_err());
    }

    #[test]
    fn test_dry_read_offset_past_end() {
        let ifile = IFile::new(make_test_file(&[1, 2, 3]));
        let result = ifile.dry_read(100, 3, &ReadOption::NONE, OpenMode::READ);
        assert!(result.is_err());
    }

    #[test]
    fn test_dry_read_clamps_to_file_size() {
        let ifile = IFile::new(make_test_file(&[1, 2, 3]));
        let out = ifile
            .dry_read(1, 100, &ReadOption::NONE, OpenMode::READ)
            .unwrap();
        assert_eq!(out, 2);
    }

    #[test]
    fn test_dry_write_no_write_permission() {
        let ifile = IFile::new(make_test_file(&[1, 2, 3]));
        let result = ifile.dry_write(0, 3, &WriteOption::NONE, OpenMode::READ);
        assert!(result.is_err());
    }

    #[test]
    fn test_dry_set_size_no_write_permission() {
        let ifile = IFile::new(make_test_file(&[1, 2, 3]));
        let result = ifile.dry_set_size(10, OpenMode::READ);
        assert!(result.is_err());
    }
}
