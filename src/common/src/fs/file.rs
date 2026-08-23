// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/common/fs/file.h and zuyu/src/common/fs/file.cpp
//! IOFile class for file operations, wrapping `std::fs::File`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use log::error;

use super::fs as fs_ops;
use super::fs_types::{FileAccessMode, FileShareFlag, FileType};
use super::fs_util::path_to_utf8_string;

/// Seek origin for file pointer positioning.
///
/// Maps to upstream `SeekOrigin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekOrigin {
    /// Seeks from the start of the file.
    SetOrigin,
    /// Seeks from the current file pointer position.
    CurrentPosition,
    /// Seeks from the end of the file.
    End,
}

impl SeekOrigin {
    fn to_seek_from(self, offset: i64) -> io::SeekFrom {
        match self {
            SeekOrigin::SetOrigin => io::SeekFrom::Start(offset as u64),
            SeekOrigin::CurrentPosition => io::SeekFrom::Current(offset),
            SeekOrigin::End => io::SeekFrom::End(offset),
        }
    }
}

/// Reads an entire file at path and returns a string of the contents read from the file.
/// If the filesystem object at path is not a regular file, returns an empty string.
///
/// Maps to upstream `ReadStringFromFile`.
pub fn read_string_from_file(path: &Path, _file_type: FileType) -> String {
    if !fs_ops::is_file(path) {
        return String::new();
    }

    let mut io_file = IOFile::new(
        path,
        FileAccessMode::Read,
        FileType::BinaryFile,
        FileShareFlag::ShareReadOnly,
    );
    let size = io_file.get_size();
    io_file.read_string(size as usize)
}

/// Writes a string to a file at path and returns the number of characters successfully written.
/// If a file already exists at path, its contents will be erased.
/// If the filesystem object at path exists and is not a regular file, returns 0.
///
/// Maps to upstream `WriteStringToFile`.
pub fn write_string_to_file(path: &Path, _file_type: FileType, string: &str) -> usize {
    if fs_ops::exists(path) && !fs_ops::is_file(path) {
        return 0;
    }

    let mut io_file = IOFile::new(
        path,
        FileAccessMode::Write,
        FileType::BinaryFile,
        FileShareFlag::ShareReadOnly,
    );
    io_file.write_string(string)
}

/// Appends a string to a file at path and returns the number of characters successfully written.
/// If the filesystem object at path exists and is not a regular file, returns 0.
///
/// Maps to upstream `AppendStringToFile`.
pub fn append_string_to_file(path: &Path, _file_type: FileType, string: &str) -> usize {
    if fs_ops::exists(path) && !fs_ops::is_file(path) {
        return 0;
    }

    let mut io_file = IOFile::new(
        path,
        FileAccessMode::Append,
        FileType::BinaryFile,
        FileShareFlag::ShareReadOnly,
    );
    io_file.write_string(string)
}

/// An IOFile is a lightweight wrapper on Rust file operations.
/// Automatically closes an open file on drop.
///
/// Maps to upstream `IOFile` class.
pub struct IOFile {
    file_path: PathBuf,
    file_access_mode: FileAccessMode,
    file_type: FileType,
    file: Option<File>,
}

impl IOFile {
    /// Creates a new default (closed) IOFile.
    ///
    /// Maps to upstream `IOFile::IOFile()`.
    pub fn default() -> Self {
        Self {
            file_path: PathBuf::new(),
            file_access_mode: FileAccessMode::Read,
            file_type: FileType::BinaryFile,
            file: None,
        }
    }

    /// Creates and opens a new IOFile at the given path.
    ///
    /// Maps to upstream `IOFile::IOFile(const fs::path&, FileAccessMode, FileType, FileShareFlag)`.
    pub fn new(
        path: &Path,
        mode: FileAccessMode,
        file_type: FileType,
        _flag: FileShareFlag,
    ) -> Self {
        let mut io_file = Self {
            file_path: PathBuf::new(),
            file_access_mode: FileAccessMode::Read,
            file_type: FileType::BinaryFile,
            file: None,
        };
        io_file.open(path, mode, file_type, _flag);
        io_file
    }

    /// Gets the path of the file.
    ///
    /// Maps to upstream `IOFile::GetPath`.
    pub fn get_path(&self) -> &Path {
        &self.file_path
    }

    /// Gets the access mode of the file.
    ///
    /// Maps to upstream `IOFile::GetAccessMode`.
    pub fn get_access_mode(&self) -> FileAccessMode {
        self.file_access_mode
    }

    /// Gets the type of the file.
    ///
    /// Maps to upstream `IOFile::GetType`.
    pub fn get_type(&self) -> FileType {
        self.file_type
    }

    /// Opens a file at path with the specified file access mode.
    ///
    /// Maps to upstream `IOFile::Open`.
    pub fn open(
        &mut self,
        path: &Path,
        mode: FileAccessMode,
        file_type: FileType,
        _flag: FileShareFlag,
    ) {
        self.close();

        self.file_path = path.to_path_buf();
        self.file_access_mode = mode;
        self.file_type = file_type;

        let result = match mode {
            FileAccessMode::Read => OpenOptions::new().read(true).open(path),
            FileAccessMode::Write => OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path),
            FileAccessMode::ReadWrite => OpenOptions::new().read(true).write(true).open(path),
            FileAccessMode::Append => OpenOptions::new().append(true).create(true).open(path),
            FileAccessMode::ReadAppend => OpenOptions::new()
                .read(true)
                .append(true)
                .create(true)
                .open(path),
        };

        match result {
            Ok(f) => self.file = Some(f),
            Err(e) => {
                error!(
                    "Failed to open the file at path={}, ec_message={}",
                    path_to_utf8_string(&self.file_path),
                    e
                );
                self.file = None;
            }
        }
    }

    /// Closes the file if it is opened.
    ///
    /// Maps to upstream `IOFile::Close`.
    pub fn close(&mut self) {
        // Dropping the File handle closes it.
        self.file = None;
    }

    /// Checks whether the file is open.
    ///
    /// Maps to upstream `IOFile::IsOpen`.
    pub fn is_open(&self) -> bool {
        self.file.is_some()
    }

    /// Reads raw bytes from the file into the provided buffer.
    /// Returns the number of bytes actually read.
    ///
    /// Maps to upstream `IOFile::ReadSpan`.
    pub fn read_bytes(&mut self, buf: &mut [u8]) -> usize {
        if let Some(ref mut f) = self.file {
            match f.read(buf) {
                Ok(n) => n,
                Err(_) => 0,
            }
        } else {
            0
        }
    }

    /// Writes raw bytes to the file from the provided buffer.
    /// Returns the number of bytes actually written.
    ///
    /// Maps to upstream `IOFile::WriteSpan`.
    pub fn write_bytes(&mut self, buf: &[u8]) -> usize {
        if let Some(ref mut f) = self.file {
            match f.write(buf) {
                Ok(n) => n,
                Err(_) => 0,
            }
        } else {
            0
        }
    }

    /// Reads a trivially copyable object from the file.
    /// Returns `true` if the full object was read successfully.
    ///
    /// Maps to upstream `IOFile::ReadObject`.
    ///
    /// # Safety
    /// T must be a plain-old-data type safe to read from raw bytes.
    pub unsafe fn read_object<T: Copy>(&mut self, object: &mut T) -> bool {
        if !self.is_open() {
            return false;
        }
        let size = std::mem::size_of::<T>();
        let ptr = object as *mut T as *mut u8;
        let buf = std::slice::from_raw_parts_mut(ptr, size);
        self.read_bytes(buf) == size
    }

    /// Writes a trivially copyable object to the file.
    /// Returns `true` if the full object was written successfully.
    ///
    /// Maps to upstream `IOFile::WriteObject`.
    ///
    /// # Safety
    /// T must be a plain-old-data type safe to write as raw bytes.
    pub unsafe fn write_object<T: Copy>(&mut self, object: &T) -> bool {
        if !self.is_open() {
            return false;
        }
        let size = std::mem::size_of::<T>();
        let ptr = object as *const T as *const u8;
        let buf = std::slice::from_raw_parts(ptr, size);
        self.write_bytes(buf) == size
    }

    /// Reads a string of a given length from the file.
    ///
    /// Maps to upstream `IOFile::ReadString`.
    pub fn read_string(&mut self, length: usize) -> String {
        let mut buf = vec![0u8; length];
        let chars_read = self.read_bytes(&mut buf);
        let string_size = if chars_read != length {
            chars_read
        } else {
            length
        };
        String::from_utf8_lossy(&buf[..string_size]).into_owned()
    }

    /// Writes a string to the file.
    /// Returns the number of bytes successfully written.
    ///
    /// Maps to upstream `IOFile::WriteString`.
    pub fn write_string(&mut self, string: &str) -> usize {
        self.write_bytes(string.as_bytes())
    }

    /// Attempts to flush any unwritten buffered data into the file.
    ///
    /// Maps to upstream `IOFile::Flush`.
    pub fn flush(&mut self) -> bool {
        if let Some(ref mut f) = self.file {
            match f.flush() {
                Ok(()) => true,
                Err(e) => {
                    error!(
                        "Failed to flush the file at path={}, ec_message={}",
                        path_to_utf8_string(&self.file_path),
                        e
                    );
                    false
                }
            }
        } else {
            false
        }
    }

    /// Attempts to commit the file to disk (flush + fsync).
    ///
    /// Maps to upstream `IOFile::Commit`.
    pub fn commit(&mut self) -> bool {
        if let Some(ref mut f) = self.file {
            if f.flush().is_err() {
                error!(
                    "Failed to commit the file at path={}",
                    path_to_utf8_string(&self.file_path)
                );
                return false;
            }
            match f.sync_all() {
                Ok(()) => true,
                Err(e) => {
                    error!(
                        "Failed to commit the file at path={}, ec_message={}",
                        path_to_utf8_string(&self.file_path),
                        e
                    );
                    false
                }
            }
        } else {
            false
        }
    }

    /// Resizes the file to a given size.
    ///
    /// Maps to upstream `IOFile::SetSize`.
    pub fn set_size(&self, size: u64) -> bool {
        if let Some(ref f) = self.file {
            match f.set_len(size) {
                Ok(()) => true,
                Err(e) => {
                    error!(
                        "Failed to resize the file at path={}, size={}, ec_message={}",
                        path_to_utf8_string(&self.file_path),
                        size,
                        e
                    );
                    false
                }
            }
        } else {
            false
        }
    }

    /// Gets the size of the file.
    /// Returns 0 on failure.
    ///
    /// Maps to upstream `IOFile::GetSize`.
    pub fn get_size(&self) -> u64 {
        if !self.is_open() {
            return 0;
        }

        match fs::metadata(&self.file_path) {
            Ok(metadata) => metadata.len(),
            Err(e) => {
                error!(
                    "Failed to retrieve the file size of path={}, ec_message={}",
                    path_to_utf8_string(&self.file_path),
                    e
                );
                0
            }
        }
    }

    /// Moves the current position of the file pointer.
    ///
    /// Maps to upstream `IOFile::Seek`.
    pub fn seek(&mut self, offset: i64, origin: SeekOrigin) -> bool {
        if let Some(ref mut f) = self.file {
            match f.seek(origin.to_seek_from(offset)) {
                Ok(_) => true,
                Err(e) => {
                    error!(
                        "Failed to seek the file at path={}, offset={}, origin={:?}, ec_message={}",
                        path_to_utf8_string(&self.file_path),
                        offset,
                        origin,
                        e
                    );
                    false
                }
            }
        } else {
            false
        }
    }

    /// Gets the current position of the file pointer.
    ///
    /// Maps to upstream `IOFile::Tell`.
    pub fn tell(&mut self) -> i64 {
        if let Some(ref mut f) = self.file {
            match f.stream_position() {
                Ok(pos) => pos as i64,
                Err(_) => 0,
            }
        } else {
            0
        }
    }
}

/// IOFile closes the underlying file handle on drop.
///
/// Maps to upstream `IOFile::~IOFile`.
impl Drop for IOFile {
    fn drop(&mut self) {
        self.close();
    }
}

/// Opens a file stream at path with the specified open mode.
///
/// Maps to upstream `OpenFileStream` template function.
/// In Rust, this is simply a convenience wrapper that creates OpenOptions.
pub fn open_file_stream(
    path: &Path,
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
) -> io::Result<File> {
    OpenOptions::new()
        .read(read)
        .write(write)
        .append(append)
        .truncate(truncate)
        .create(create)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_io_file_write_and_read() {
        let dir = std::env::temp_dir().join("ruzu_test_io_file");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_write_read.bin");

        // Write
        {
            let mut f = IOFile::new(
                &path,
                FileAccessMode::Write,
                FileType::BinaryFile,
                FileShareFlag::ShareReadOnly,
            );
            assert!(f.is_open());
            let written = f.write_string("hello world");
            assert_eq!(written, 11);
        }

        // Read back
        {
            let mut f = IOFile::new(
                &path,
                FileAccessMode::Read,
                FileType::BinaryFile,
                FileShareFlag::ShareReadOnly,
            );
            assert!(f.is_open());
            assert_eq!(f.get_size(), 11);
            let s = f.read_string(11);
            assert_eq!(s, "hello world");
        }

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_io_file_seek_tell() {
        let dir = std::env::temp_dir().join("ruzu_test_io_file");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_seek_tell.bin");

        {
            let mut f = IOFile::new(
                &path,
                FileAccessMode::Write,
                FileType::BinaryFile,
                FileShareFlag::ShareReadOnly,
            );
            f.write_string("0123456789");
        }

        {
            let mut f = IOFile::new(
                &path,
                FileAccessMode::Read,
                FileType::BinaryFile,
                FileShareFlag::ShareReadOnly,
            );
            assert_eq!(f.tell(), 0);
            assert!(f.seek(5, SeekOrigin::SetOrigin));
            assert_eq!(f.tell(), 5);
            let s = f.read_string(5);
            assert_eq!(s, "56789");
        }

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_io_file_set_size() {
        let dir = std::env::temp_dir().join("ruzu_test_io_file");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_set_size.bin");

        {
            let f = IOFile::new(
                &path,
                FileAccessMode::Write,
                FileType::BinaryFile,
                FileShareFlag::ShareReadOnly,
            );
            assert!(f.set_size(1024));
            assert_eq!(f.get_size(), 1024);
        }

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_read_string_from_file() {
        let dir = std::env::temp_dir().join("ruzu_test_io_file");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_read_string.txt");

        std::fs::write(&path, "test content").unwrap();
        let result = read_string_from_file(&path, FileType::BinaryFile);
        assert_eq!(result, "test content");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_write_string_to_file() {
        let dir = std::env::temp_dir().join("ruzu_test_io_file");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_write_string.txt");

        let written = write_string_to_file(&path, FileType::BinaryFile, "hello");
        assert_eq!(written, 5);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "hello");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_append_string_to_file() {
        let dir = std::env::temp_dir().join("ruzu_test_io_file_append");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_append_string.txt");
        // Clean up any leftover from previous runs
        let _ = std::fs::remove_file(&path);

        let written = write_string_to_file(&path, FileType::BinaryFile, "hello");
        assert_eq!(written, 5);
        // Verify the first write succeeded before appending
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "hello");

        append_string_to_file(&path, FileType::BinaryFile, " world");

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "hello world");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
