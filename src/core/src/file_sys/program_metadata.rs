// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Ported from: core/file_sys/program_metadata.h and program_metadata.cpp
// NPDM (Program Description Metadata) parsing.

use super::partition_filesystem::ResultStatus;
use super::vfs::vfs::VfsFile;
use super::vfs::vfs_types::VirtualFile;

// ============================================================================
// Enums
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProgramAddressSpaceType {
    Is32Bit = 0,
    Is36Bit = 1,
    Is32BitNoMap = 2,
    Is39Bit = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum ProgramFilePermission {
    MountContent = 1 << 0,
    SaveDataBackup = 1 << 5,
    SdCard = 1 << 21,
    Calibration = 1 << 34,
    Bit62 = 1 << 62,
    Everything = 1 << 63,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PoolPartition {
    Application = 0,
    Applet = 1,
    System = 2,
    SystemNonSecure = 3,
}

// ============================================================================
// Binary structures
// ============================================================================

#[derive(Clone, Copy)]
#[repr(C)]
struct Header {
    magic: [u8; 4],
    reserved: [u8; 8],
    flags: u8,
    reserved_3: u8,
    main_thread_priority: u8,
    main_thread_cpu: u8,
    reserved_4: [u8; 4],
    system_resource_size: u32,
    process_category: u32,
    main_stack_size: u32,
    application_name: [u8; 0x10],
    reserved_5: [u8; 0x40],
    aci_offset: u32,
    aci_size: u32,
    acid_offset: u32,
    acid_size: u32,
}

const _: () = assert!(std::mem::size_of::<Header>() == 0x80);

impl Default for Header {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl Header {
    fn has_64_bit_instructions(&self) -> bool {
        (self.flags & 0x01) != 0
    }

    fn address_space_type(&self) -> ProgramAddressSpaceType {
        match (self.flags >> 1) & 0x07 {
            0 => ProgramAddressSpaceType::Is32Bit,
            1 => ProgramAddressSpaceType::Is36Bit,
            2 => ProgramAddressSpaceType::Is32BitNoMap,
            3 => ProgramAddressSpaceType::Is39Bit,
            _ => ProgramAddressSpaceType::Is39Bit,
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
struct AcidHeader {
    signature: [u8; 0x100],
    nca_modulus: [u8; 0x100],
    magic: [u8; 4],
    nca_size: u32,
    reserved: [u8; 4],
    flags: u32,
    title_id_min: u64,
    title_id_max: u64,
    fac_offset: u32,
    fac_size: u32,
    sac_offset: u32,
    sac_size: u32,
    kac_offset: u32,
    kac_size: u32,
    _padding: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<AcidHeader>() == 0x240);

impl Default for AcidHeader {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl AcidHeader {
    fn pool_partition(&self) -> PoolPartition {
        match (self.flags >> 2) & 0xF {
            0 => PoolPartition::Application,
            1 => PoolPartition::Applet,
            2 => PoolPartition::System,
            3 => PoolPartition::SystemNonSecure,
            _ => PoolPartition::Application,
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
struct AciHeader {
    magic: [u8; 4],
    reserved: [u8; 0xC],
    title_id: u64,
    _padding: [u8; 8],
    fah_offset: u32,
    fah_size: u32,
    sac_offset: u32,
    sac_size: u32,
    kac_offset: u32,
    kac_size: u32,
    _padding2: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<AciHeader>() == 0x40);

impl Default for AciHeader {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[derive(Clone, Copy, Default)]
struct FileAccessControl {
    version: u8,
    permissions: u64,
    unknown: [u8; 0x20],
}

#[derive(Clone, Copy, Default)]
struct FileAccessHeader {
    version: u8,
    permissions: u64,
    unk_offset: u32,
    unk_size: u32,
    unk_offset_2: u32,
    unk_size_2: u32,
}

// ============================================================================
// ProgramMetadata
// ============================================================================

pub type KernelCapabilityDescriptors = Vec<u32>;

/// Program Description Metadata parser.
/// Corresponds to upstream `ProgramMetadata`.
pub struct ProgramMetadata {
    npdm_header: Header,
    aci_header: AciHeader,
    acid_header: AcidHeader,
    is_switch2: bool,
    acid_file_access: FileAccessControl,
    aci_file_access: FileAccessHeader,
    aci_kernel_capabilities: KernelCapabilityDescriptors,
}

fn read_object<T: Copy>(file: &dyn VfsFile, offset: usize) -> Option<T> {
    let size = std::mem::size_of::<T>();
    let mut buf = vec![0u8; size];
    if file.read(&mut buf, size, offset) != size {
        return None;
    }
    unsafe { Some(std::ptr::read_unaligned(buf.as_ptr() as *const T)) }
}

impl ProgramMetadata {
    pub fn new() -> Self {
        Self {
            npdm_header: Header::default(),
            aci_header: AciHeader::default(),
            acid_header: AcidHeader::default(),
            is_switch2: false,
            acid_file_access: FileAccessControl::default(),
            aci_file_access: FileAccessHeader::default(),
            aci_kernel_capabilities: Vec::new(),
        }
    }

    /// Gets a default ProgramMetadata configuration for homebrew.
    pub fn get_default() -> Self {
        const DEFAULT_THREAD_INFO_CAPABILITY: u32 = 0x30043F7;

        let mut result = Self::new();
        result.load_manual(
            true,
            ProgramAddressSpaceType::Is39Bit,
            0x2c,
            0,
            0x100000,
            0,
            0xFFFFFFFFFFFFFFFF,
            0,
            vec![DEFAULT_THREAD_INFO_CAPABILITY],
        );
        result
    }

    pub fn load(&mut self, file: VirtualFile) -> ResultStatus {
        self.is_switch2 = false; // Reload must not retain the previous ACID layout.

        let total_size = file.get_size();
        let header_size = std::mem::size_of::<Header>();

        if total_size < header_size {
            return ResultStatus::ErrorBadNPDMHeader;
        }

        match read_object::<Header>(file.as_ref(), 0) {
            Some(hdr) => self.npdm_header = hdr,
            None => return ResultStatus::ErrorBadNPDMHeader,
        }

        match read_object::<AcidHeader>(file.as_ref(), self.npdm_header.acid_offset as usize) {
            Some(hdr) => self.acid_header = hdr,
            None => return ResultStatus::ErrorBadACIDHeader,
        }

        // origin/eden-nx2: ProgramMetadata::Load alternate ACID signature area.
        if self.acid_header.magic != *b"ACID" {
            const ACID_SIG_SIZE_V1: usize = 0x200;
            const ACID_SIG_SIZE_V2: usize = 0x300;
            const ACID_BODY_SIZE: usize = std::mem::size_of::<AcidHeader>() - ACID_SIG_SIZE_V1;
            // AcidHeader contains only integers and byte arrays; all bit patterns are valid.
            let body = unsafe {
                std::slice::from_raw_parts_mut(
                    (&mut self.acid_header as *mut AcidHeader)
                        .cast::<u8>()
                        .add(ACID_SIG_SIZE_V1),
                    ACID_BODY_SIZE,
                )
            };
            if file.read(
                body,
                ACID_BODY_SIZE,
                self.npdm_header.acid_offset as usize + ACID_SIG_SIZE_V2,
            ) != ACID_BODY_SIZE
                || self.acid_header.magic != *b"ACID"
            {
                return ResultStatus::ErrorBadACIDHeader;
            }
            log::info!("NPDM uses the Switch 2 ACID layout");
            self.is_switch2 = true;
        }

        match read_object::<AciHeader>(file.as_ref(), self.npdm_header.aci_offset as usize) {
            Some(hdr) => self.aci_header = hdr,
            None => return ResultStatus::ErrorBadACIHeader,
        }

        // Load acid_file_access per-component
        let mut current_offset = self
            .npdm_header
            .acid_offset
            .wrapping_add(self.acid_header.fac_offset) as usize;
        let mut buf1 = [0u8; 1];
        if file.read(&mut buf1, 1, current_offset) != 1 {
            return ResultStatus::ErrorBadFileAccessControl;
        }
        self.acid_file_access.version = buf1[0];

        current_offset += 1 + 3; // version + 3 padding bytes
        let mut buf8 = [0u8; 8];
        if file.read(&mut buf8, 8, current_offset) != 8 {
            return ResultStatus::ErrorBadFileAccessControl;
        }
        self.acid_file_access.permissions = u64::from_le_bytes(buf8);

        if file.read(&mut self.acid_file_access.unknown, 0x20, current_offset + 8) != 0x20 {
            return ResultStatus::ErrorBadFileAccessControl;
        }

        // Load aci_file_access per-component
        current_offset = self
            .npdm_header
            .aci_offset
            .wrapping_add(self.aci_header.fah_offset) as usize;
        if file.read(&mut buf1, 1, current_offset) != 1 {
            return ResultStatus::ErrorBadFileAccessHeader;
        }
        self.aci_file_access.version = buf1[0];

        current_offset += 1 + 3;
        if file.read(&mut buf8, 8, current_offset) != 8 {
            return ResultStatus::ErrorBadFileAccessHeader;
        }
        self.aci_file_access.permissions = u64::from_le_bytes(buf8);
        current_offset += 8;

        let mut buf4 = [0u8; 4];
        if file.read(&mut buf4, 4, current_offset) != 4 {
            return ResultStatus::ErrorBadFileAccessHeader;
        }
        self.aci_file_access.unk_offset = u32::from_le_bytes(buf4);
        current_offset += 4;

        if file.read(&mut buf4, 4, current_offset) != 4 {
            return ResultStatus::ErrorBadFileAccessHeader;
        }
        self.aci_file_access.unk_size = u32::from_le_bytes(buf4);
        current_offset += 4;

        if file.read(&mut buf4, 4, current_offset) != 4 {
            return ResultStatus::ErrorBadFileAccessHeader;
        }
        self.aci_file_access.unk_offset_2 = u32::from_le_bytes(buf4);
        current_offset += 4;

        if file.read(&mut buf4, 4, current_offset) != 4 {
            return ResultStatus::ErrorBadFileAccessHeader;
        }
        self.aci_file_access.unk_size_2 = u32::from_le_bytes(buf4);

        // Read kernel capabilities
        let kac_count = self.aci_header.kac_size as usize / 4;
        self.aci_kernel_capabilities.resize(kac_count, 0);
        let read_size = self.aci_header.kac_size as usize;
        let read_offset =
            self.npdm_header.aci_offset as usize + self.aci_header.kac_offset as usize;
        let mut kac_buf = vec![0u8; read_size];
        if file.read(&mut kac_buf, read_size, read_offset) != read_size {
            return ResultStatus::ErrorBadKernelCapabilityDescriptors;
        }
        for i in 0..kac_count {
            let bytes = [
                kac_buf[i * 4],
                kac_buf[i * 4 + 1],
                kac_buf[i * 4 + 2],
                kac_buf[i * 4 + 3],
            ];
            self.aci_kernel_capabilities[i] = u32::from_le_bytes(bytes);
        }

        ResultStatus::Success
    }

    pub fn reload(&mut self, file: VirtualFile) -> ResultStatus {
        let original_program_id = self.aci_header.title_id;
        let result = self.load(file);
        self.aci_header.title_id = original_program_id;
        result
    }

    pub fn load_manual(
        &mut self,
        is_64_bit: bool,
        address_space: ProgramAddressSpaceType,
        main_thread_prio: i32,
        main_thread_core: u32,
        main_thread_stack_size: u32,
        title_id: u64,
        filesystem_permissions: u64,
        system_resource_size: u32,
        capabilities: KernelCapabilityDescriptors,
    ) {
        let mut flags = self.npdm_header.flags & 0xF0;
        if is_64_bit {
            flags |= 0x01;
        }
        flags |= (address_space as u8 & 0x07) << 1;
        self.npdm_header.flags = flags;
        self.npdm_header.main_thread_priority = main_thread_prio as u8;
        self.npdm_header.main_thread_cpu = main_thread_core as u8;
        self.npdm_header.main_stack_size = main_thread_stack_size;
        self.npdm_header.system_resource_size = system_resource_size;
        self.aci_header.title_id = title_id;
        self.aci_file_access.permissions = filesystem_permissions;
        self.aci_kernel_capabilities = capabilities;
    }

    pub fn is_switch2(&self) -> bool {
        self.is_switch2
    }

    pub fn is_64_bit_program(&self) -> bool {
        self.npdm_header.has_64_bit_instructions()
    }

    pub fn get_address_space_type(&self) -> ProgramAddressSpaceType {
        self.npdm_header.address_space_type()
    }

    pub fn get_main_thread_priority(&self) -> u8 {
        self.npdm_header.main_thread_priority
    }

    pub fn get_main_thread_core(&self) -> u8 {
        self.npdm_header.main_thread_cpu
    }

    pub fn get_main_thread_stack_size(&self) -> u32 {
        self.npdm_header.main_stack_size
    }

    pub fn get_title_id(&self) -> u64 {
        self.aci_header.title_id
    }

    pub fn get_filesystem_permissions(&self) -> u64 {
        self.aci_file_access.permissions
    }

    pub fn get_system_resource_size(&self) -> u32 {
        self.npdm_header.system_resource_size
    }

    pub fn get_pool_partition(&self) -> PoolPartition {
        self.acid_header.pool_partition()
    }

    pub fn get_kernel_capabilities(&self) -> &KernelCapabilityDescriptors {
        &self.aci_kernel_capabilities
    }

    pub fn get_name(&self) -> &[u8; 0x10] {
        &self.npdm_header.application_name
    }

    pub fn print(&self) {
        log::debug!(
            "Magic:                  {:?}",
            std::str::from_utf8(&self.npdm_header.magic)
        );
        log::debug!(
            "Main thread priority:   0x{:02X}",
            self.npdm_header.main_thread_priority
        );
        log::debug!(
            "Main thread core:       {}",
            self.npdm_header.main_thread_cpu
        );
        log::debug!(
            "Main thread stack size: 0x{:X} bytes",
            self.npdm_header.main_stack_size
        );
        log::debug!(
            "Process category:       {}",
            self.npdm_header.process_category
        );
        log::debug!("Flags:                  0x{:02X}", self.npdm_header.flags);
        log::debug!(
            " > 64-bit instructions: {}",
            if self.is_64_bit_program() {
                "YES"
            } else {
                "NO"
            }
        );

        let address_space = match self.get_address_space_type() {
            ProgramAddressSpaceType::Is36Bit => "64-bit (36-bit address space)",
            ProgramAddressSpaceType::Is39Bit => "64-bit (39-bit address space)",
            ProgramAddressSpaceType::Is32Bit => "32-bit",
            ProgramAddressSpaceType::Is32BitNoMap => "32-bit (no map region)",
        };
        log::debug!(" > Address space:       {}", address_space);

        log::debug!(
            "Magic:                  {:?}",
            std::str::from_utf8(&self.acid_header.magic)
        );
        log::debug!("Flags:                  0x{:02X}", self.acid_header.flags);
        log::debug!(
            "Title ID Min:           0x{:016X}",
            self.acid_header.title_id_min
        );
        log::debug!(
            "Title ID Max:           0x{:016X}",
            self.acid_header.title_id_max
        );
        log::debug!(
            "Filesystem Access:      0x{:016X}",
            self.acid_file_access.permissions
        );

        log::debug!(
            "Magic:                  {:?}",
            std::str::from_utf8(&self.aci_header.magic)
        );
        log::debug!(
            "Title ID:               0x{:016X}",
            self.aci_header.title_id
        );
        log::debug!(
            "Filesystem Access:      0x{:016X}",
            self.aci_file_access.permissions
        );
    }
}

impl Default for ProgramMetadata {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod nx2_tests {
    use super::*;
    use crate::file_sys::vfs::vfs_vector::VectorVfsFile;
    use std::sync::Arc;

    fn fixture(switch2: bool) -> Vec<u8> {
        let mut data = vec![0u8; 0x700];
        data[0x70..0x74].copy_from_slice(&0x80u32.to_le_bytes());
        data[0x78..0x7c].copy_from_slice(&0x200u32.to_le_bytes());
        data[0xa0..0xa4].copy_from_slice(&0x40u32.to_le_bytes());
        data[0xc4..0xcc].copy_from_slice(&0x123456789abcdef0u64.to_le_bytes());
        let body = if switch2 { 0x500 } else { 0x400 };
        data[body..body + 4].copy_from_slice(b"ACID");
        data[body + 0x20..body + 0x24].copy_from_slice(&0x400u32.to_le_bytes());
        data[0x604..0x60c].copy_from_slice(&0xfedcba9876543210u64.to_le_bytes());
        data
    }

    fn load(metadata: &mut ProgramMetadata, data: Vec<u8>) -> ResultStatus {
        metadata.load(Arc::new(VectorVfsFile::new(data, "main.npdm".into(), None)))
    }

    #[test]
    fn manual_load_preserves_reserved_flags_and_casts() {
        let mut metadata = ProgramMetadata::new();
        metadata.npdm_header.flags = 0xff;
        metadata.load_manual(
            false,
            ProgramAddressSpaceType::Is36Bit,
            -1,
            0x102,
            0x4000,
            42,
            u64::MAX,
            0,
            vec![],
        );
        assert_eq!(metadata.npdm_header.flags, 0xf2);
        assert_eq!(metadata.get_main_thread_priority(), 255);
        assert_eq!(metadata.get_main_thread_core(), 2);
    }

    #[test]
    fn layouts_and_relative_permissions() {
        assert_eq!(std::mem::offset_of!(AcidHeader, magic), 0x200);
        assert_eq!(std::mem::offset_of!(AcidHeader, fac_offset), 0x220);
        for switch2 in [false, true] {
            let mut metadata = ProgramMetadata::new();
            assert_eq!(load(&mut metadata, fixture(switch2)), ResultStatus::Success);
            assert_eq!(metadata.is_switch2(), switch2);
            assert_eq!(metadata.get_filesystem_permissions(), 0x123456789abcdef0);
            assert_eq!(metadata.acid_file_access.permissions, 0xfedcba9876543210);
        }
    }

    #[test]
    fn invalid_or_truncated_alternate_body_is_rejected() {
        for length in [0x53f, 0x700] {
            let mut data = fixture(true);
            data[0x500] = 0;
            data.truncate(length);
            assert_eq!(
                load(&mut ProgramMetadata::new(), data),
                ResultStatus::ErrorBadACIDHeader
            );
        }
    }

    #[test]
    fn hardware_profile_follows_reloaded_acid_layout() {
        let mut metadata = ProgramMetadata::new();
        assert_eq!(load(&mut metadata, fixture(true)), ResultStatus::Success);
        assert_eq!(load(&mut metadata, fixture(false)), ResultStatus::Success);
        assert!(!metadata.is_switch2());
    }
}
