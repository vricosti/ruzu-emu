// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/nca_metadata.h and nca_metadata.cpp
// CNMT (Content Meta) parsing.

use super::vfs::vfs::VfsFile;
use super::vfs::vfs_types::VirtualFile;

// ============================================================================
// Enums
// ============================================================================

/// Title type within a CNMT.
/// Corresponds to upstream `TitleType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum TitleType {
    SystemProgram = 0x01,
    SystemDataArchive = 0x02,
    SystemUpdate = 0x03,
    FirmwarePackageA = 0x04,
    FirmwarePackageB = 0x05,
    Application = 0x80,
    Update = 0x81,
    AOC = 0x82,
    DeltaTitle = 0x83,
}

/// Content record type within a CNMT.
/// Corresponds to upstream `ContentRecordType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ContentRecordType {
    Meta = 0,
    Program = 1,
    Data = 2,
    Control = 3,
    HtmlDocument = 4,
    LegalInformation = 5,
    DeltaFragment = 6,
}

// ============================================================================
// Binary structures
// ============================================================================

/// Content record — 0x38 bytes.
/// Corresponds to upstream `ContentRecord`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ContentRecord {
    pub hash: [u8; 0x20],
    pub nca_id: [u8; 0x10],
    pub size: [u8; 0x6],
    pub record_type: ContentRecordType,
    pub id_offset: u8,
}

const _: () = assert!(std::mem::size_of::<ContentRecord>() == 0x38);

impl Default for ContentRecord {
    fn default() -> Self {
        Self {
            hash: [0u8; 0x20],
            nca_id: [0u8; 0x10],
            size: [0u8; 0x6],
            record_type: ContentRecordType::Meta,
            id_offset: 0,
        }
    }
}

/// Empty meta content record constant.
pub const EMPTY_META_CONTENT_RECORD: ContentRecord = ContentRecord {
    hash: [0u8; 0x20],
    nca_id: [0u8; 0x10],
    size: [0u8; 0x6],
    record_type: ContentRecordType::Meta,
    id_offset: 0,
};

/// Meta record — 0x10 bytes.
/// Corresponds to upstream `MetaRecord`.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct MetaRecord {
    pub title_id: u64,
    pub title_version: u32,
    pub title_type: u8, // TitleType value
    pub install_byte: u8,
    pub _padding: [u8; 2],
}

const _: () = assert!(std::mem::size_of::<MetaRecord>() == 0x10);

/// Optional header — 0x10 bytes.
/// Corresponds to upstream `OptionalHeader`.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct OptionalHeader {
    pub title_id: u64,
    pub minimum_version: u64,
}

const _: () = assert!(std::mem::size_of::<OptionalHeader>() == 0x10);

/// CNMT header — 0x20 bytes.
/// Corresponds to upstream `CNMTHeader`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct CNMTHeader {
    pub title_id: u64,
    pub title_version: u32,
    pub title_type: u8, // TitleType value
    pub reserved: u8,
    pub table_offset: u16,
    pub number_content_entries: u16,
    pub number_meta_entries: u16,
    pub attributes: u8,
    pub reserved2: [u8; 2],
    pub is_committed: u8,
    pub required_download_system_version: u32,
    pub reserved3: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<CNMTHeader>() == 0x20);

impl Default for CNMTHeader {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// ============================================================================
// CNMT
// ============================================================================

/// A class representing the CNMT (Content Meta) format.
/// Corresponds to upstream `CNMT`.
pub struct CNMT {
    header: CNMTHeader,
    opt_header: OptionalHeader,
    content_records: Vec<ContentRecord>,
    meta_records: Vec<MetaRecord>,
}

/// Helper to read a repr(C) struct from a VfsFile at a given offset.
fn read_object<T: Copy>(file: &dyn VfsFile, offset: usize) -> Option<T> {
    let size = std::mem::size_of::<T>();
    let mut buf = vec![0u8; size];
    if file.read(&mut buf, size, offset) != size {
        return None;
    }
    unsafe { Some(std::ptr::read_unaligned(buf.as_ptr() as *const T)) }
}

impl CNMT {
    /// Parse CNMT from a VFS file.
    pub fn from_file(file: &VirtualFile) -> Self {
        let mut cnmt = Self {
            header: CNMTHeader::default(),
            opt_header: OptionalHeader::default(),
            content_records: Vec::new(),
            meta_records: Vec::new(),
        };

        let header_size = std::mem::size_of::<CNMTHeader>();
        if let Some(hdr) = read_object::<CNMTHeader>(file.as_ref(), 0) {
            cnmt.header = hdr;
        } else {
            return cnmt;
        }

        // If type is {Application, Update, AOC} has opt-header.
        if cnmt.header.title_type >= TitleType::Application as u8
            && cnmt.header.title_type <= TitleType::AOC as u8
        {
            if let Some(opt) = read_object::<OptionalHeader>(file.as_ref(), header_size) {
                cnmt.opt_header = opt;
            } else {
                log::warn!("Failed to read optional header.");
            }
        }

        let content_record_size = std::mem::size_of::<ContentRecord>();
        let meta_record_size = std::mem::size_of::<MetaRecord>();

        for i in 0..cnmt.header.number_content_entries as usize {
            let offset = header_size + i * content_record_size + cnmt.header.table_offset as usize;
            if let Some(rec) = read_object::<ContentRecord>(file.as_ref(), offset) {
                cnmt.content_records.push(rec);
            }
        }

        for i in 0..cnmt.header.number_meta_entries as usize {
            let offset = header_size + i * meta_record_size + cnmt.header.table_offset as usize;
            if let Some(rec) = read_object::<MetaRecord>(file.as_ref(), offset) {
                cnmt.meta_records.push(rec);
            }
        }

        cnmt
    }

    /// Construct from pre-parsed components.
    pub fn from_parts(
        header: CNMTHeader,
        opt_header: OptionalHeader,
        content_records: Vec<ContentRecord>,
        meta_records: Vec<MetaRecord>,
    ) -> Self {
        Self {
            header,
            opt_header,
            content_records,
            meta_records,
        }
    }

    pub fn get_header(&self) -> &CNMTHeader {
        &self.header
    }

    pub fn get_title_id(&self) -> u64 {
        self.header.title_id
    }

    pub fn get_title_version(&self) -> u32 {
        self.header.title_version
    }

    pub fn get_type(&self) -> TitleType {
        // SAFETY: The value was read from a valid CNMT. If it is out of range,
        // this is a corrupted file, but we match upstream behavior of casting.
        unsafe { std::mem::transmute(self.header.title_type) }
    }

    pub fn get_content_records(&self) -> &[ContentRecord] {
        &self.content_records
    }

    pub fn get_meta_records(&self) -> &[MetaRecord] {
        &self.meta_records
    }

    /// Union the content and meta records from another CNMT into this one.
    /// Returns true if anything changed.
    pub fn union_records(&mut self, other: &CNMT) -> bool {
        let mut change = false;

        for rec in &other.content_records {
            let exists = self
                .content_records
                .iter()
                .any(|r| r.nca_id == rec.nca_id && r.record_type as u8 == rec.record_type as u8);
            if !exists {
                self.content_records.push(*rec);
                self.header.number_content_entries += 1;
                change = true;
            }
        }

        for rec in &other.meta_records {
            let exists = self.meta_records.iter().any(|r| {
                r.title_id == rec.title_id
                    && r.title_version == rec.title_version
                    && r.title_type == rec.title_type
            });
            if !exists {
                self.meta_records.push(*rec);
                self.header.number_meta_entries += 1;
                change = true;
            }
        }

        change
    }

    /// Serialize the CNMT to a byte vector.
    pub fn serialize(&self) -> Vec<u8> {
        let header_size = std::mem::size_of::<CNMTHeader>();
        let opt_header_size = std::mem::size_of::<OptionalHeader>();
        let content_record_size = std::mem::size_of::<ContentRecord>();
        let meta_record_size = std::mem::size_of::<MetaRecord>();

        let has_opt_header = self.header.title_type >= TitleType::Application as u8
            && self.header.title_type <= TitleType::AOC as u8;

        let dead_zone = self.header.table_offset as usize + header_size;
        let min_header = header_size + if has_opt_header { opt_header_size } else { 0 };
        let total_size = dead_zone.max(min_header)
            + self.content_records.len() * content_record_size
            + self.meta_records.len() * meta_record_size;

        let mut out = vec![0u8; total_size];

        // Copy header
        unsafe {
            std::ptr::copy_nonoverlapping(
                &self.header as *const CNMTHeader as *const u8,
                out.as_mut_ptr(),
                header_size,
            );
        }

        // Optional header
        if has_opt_header {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    &self.opt_header as *const OptionalHeader as *const u8,
                    out.as_mut_ptr().add(header_size),
                    opt_header_size,
                );
            }
        }

        let mut offset = self.header.table_offset as usize;

        for rec in &self.content_records {
            let dst = header_size + offset;
            if dst + content_record_size <= out.len() {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        rec as *const ContentRecord as *const u8,
                        out.as_mut_ptr().add(dst),
                        content_record_size,
                    );
                }
            }
            offset += content_record_size;
        }

        for rec in &self.meta_records {
            let dst = header_size + offset;
            if dst + meta_record_size <= out.len() {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        rec as *const MetaRecord as *const u8,
                        out.as_mut_ptr().add(dst),
                        meta_record_size,
                    );
                }
            }
            offset += meta_record_size;
        }

        out
    }
}
