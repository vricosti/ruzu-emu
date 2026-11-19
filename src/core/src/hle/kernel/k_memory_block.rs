//! Port of zuyu/src/core/hle/kernel/k_memory_block.h
//! Status: Ported (structures and methods)
//! Derniere synchro: 2026-03-11
//!
//! Memory block types, enums, and the KMemoryBlock structure used by the kernel
//! memory block manager.

use bitflags::bitflags;

/// Page size constants (from memory_types.h).
pub const PAGE_BITS: usize = 12;
pub const PAGE_SIZE: usize = 1 << PAGE_BITS;

// ---------------------------------------------------------------------------
// Svc::MemoryState — subset needed for KMemoryState composition.
// Upstream: core/hle/kernel/svc_types.h
// ---------------------------------------------------------------------------

/// Upstream Svc::MemoryState values used to compose KMemoryState.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvcMemoryState {
    Free = 0x00,
    Io = 0x01,
    Static = 0x02,
    Code = 0x03,
    CodeData = 0x04,
    Normal = 0x05,
    Shared = 0x06,
    // Alias removed after 1.0.0
    AliasCode = 0x08,
    AliasCodeData = 0x09,
    Ipc = 0x0A,
    Stack = 0x0B,
    ThreadLocal = 0x0C,
    Transferred = 0x0D,
    SharedTransferred = 0x0E,
    SharedCode = 0x0F,
    Inaccessible = 0x10,
    NonSecureIpc = 0x11,
    NonDeviceIpc = 0x12,
    Kernel = 0x13,
    GeneratedCode = 0x14,
    CodeOut = 0x15,
    Coverage = 0x16,
    Insecure = 0x17,
}

// ---------------------------------------------------------------------------
// Svc::MemoryPermission (subset)
// ---------------------------------------------------------------------------

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SvcMemoryPermission: u8 {
        const NONE    = 0;
        const READ    = 1 << 0;
        const WRITE   = 1 << 1;
        const EXECUTE = 1 << 2;
    }
}

// ---------------------------------------------------------------------------
// Svc::MemoryAttribute (subset)
// ---------------------------------------------------------------------------

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SvcMemoryAttribute: u8 {
        const LOCKED           = 1 << 0;
        const IPC_LOCKED       = 1 << 1;
        const DEVICE_SHARED    = 1 << 2;
        const UNCACHED         = 1 << 3;
        const PERMISSION_LOCKED = 1 << 4;
    }
}

// ---------------------------------------------------------------------------
// KMemoryState
// ---------------------------------------------------------------------------

bitflags! {
    /// Port of Kernel::KMemoryState.
    /// Preserves all flag values and composite states from upstream.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KMemoryState: u32 {
        const NONE = 0;
        const ALL = u32::MAX;
        const MASK = 0xFF;

        const FLAG_CAN_REPROTECT             = 1 << 8;
        const FLAG_CAN_DEBUG                 = 1 << 9;
        const FLAG_CAN_USE_IPC               = 1 << 10;
        const FLAG_CAN_USE_NON_DEVICE_IPC    = 1 << 11;
        const FLAG_CAN_USE_NON_SECURE_IPC    = 1 << 12;
        const FLAG_MAPPED                    = 1 << 13;
        const FLAG_CODE                      = 1 << 14;
        const FLAG_CAN_ALIAS                 = 1 << 15;
        const FLAG_CAN_CODE_ALIAS            = 1 << 16;
        const FLAG_CAN_TRANSFER              = 1 << 17;
        const FLAG_CAN_QUERY_PHYSICAL        = 1 << 18;
        const FLAG_CAN_DEVICE_MAP            = 1 << 19;
        const FLAG_CAN_ALIGNED_DEVICE_MAP    = 1 << 20;
        const FLAG_CAN_IPC_USER_BUFFER       = 1 << 21;
        const FLAG_REFERENCE_COUNTED         = 1 << 22;
        const FLAG_CAN_MAP_PROCESS           = 1 << 23;
        const FLAG_CAN_CHANGE_ATTRIBUTE      = 1 << 24;
        const FLAG_CAN_CODE_MEMORY           = 1 << 25;
        const FLAG_LINEAR_MAPPED             = 1 << 26;
        const FLAG_CAN_PERMISSION_LOCK       = 1 << 27;

        // Composite flag groups
        const FLAGS_DATA = Self::FLAG_CAN_REPROTECT.bits() | Self::FLAG_CAN_USE_IPC.bits()
            | Self::FLAG_CAN_USE_NON_DEVICE_IPC.bits() | Self::FLAG_CAN_USE_NON_SECURE_IPC.bits()
            | Self::FLAG_MAPPED.bits() | Self::FLAG_CAN_ALIAS.bits() | Self::FLAG_CAN_TRANSFER.bits()
            | Self::FLAG_CAN_QUERY_PHYSICAL.bits() | Self::FLAG_CAN_DEVICE_MAP.bits()
            | Self::FLAG_CAN_ALIGNED_DEVICE_MAP.bits() | Self::FLAG_CAN_IPC_USER_BUFFER.bits()
            | Self::FLAG_REFERENCE_COUNTED.bits() | Self::FLAG_CAN_CHANGE_ATTRIBUTE.bits()
            | Self::FLAG_LINEAR_MAPPED.bits();

        const FLAGS_CODE = Self::FLAG_CAN_DEBUG.bits() | Self::FLAG_CAN_USE_IPC.bits()
            | Self::FLAG_CAN_USE_NON_DEVICE_IPC.bits() | Self::FLAG_CAN_USE_NON_SECURE_IPC.bits()
            | Self::FLAG_MAPPED.bits() | Self::FLAG_CODE.bits() | Self::FLAG_CAN_QUERY_PHYSICAL.bits()
            | Self::FLAG_CAN_DEVICE_MAP.bits() | Self::FLAG_CAN_ALIGNED_DEVICE_MAP.bits()
            | Self::FLAG_REFERENCE_COUNTED.bits() | Self::FLAG_LINEAR_MAPPED.bits();

        const FLAGS_MISC = Self::FLAG_MAPPED.bits() | Self::FLAG_REFERENCE_COUNTED.bits()
            | Self::FLAG_CAN_QUERY_PHYSICAL.bits() | Self::FLAG_CAN_DEVICE_MAP.bits()
            | Self::FLAG_LINEAR_MAPPED.bits();

        // Named states
        const FREE = 0x00000000;
        const IO_MEMORY = 0x00182001;
        const IO_REGISTER = 0x00180001;
        const STATIC = 0x00042002;
        const CODE = 0x04DC7E03;
        const CODE_DATA = 0x0FFEBD04;
        const NORMAL = 0x077EBD05;
        const SHARED = 0x04402006;
        const ALIAS_CODE = 0x04DD7E08;
        const ALIAS_CODE_DATA = 0x0FFFBD09;
        const IPC = 0x045C3C0A;
        const STACK = 0x045C3C0B;
        const THREAD_LOCAL = 0x0400000C;
        const TRANSFERRED = 0x055C3C0D;
        const SHARED_TRANSFERRED = 0x045C380E;
        const SHARED_CODE = 0x0440380F;
        const INACCESSIBLE = 0x00000010;
        const NON_SECURE_IPC = 0x045C3811;
        const NON_DEVICE_IPC = 0x044C2812;
        const KERNEL = 0x00000013;
        const GENERATED_CODE = 0x04402214;
        const CODE_OUT = 0x04402015;
        const COVERAGE = 0x00002016;
        const INSECURE = 0x055C3817;
    }
}

// Static assertions matching upstream.
const _: () = assert!(KMemoryState::FREE.bits() == 0x00000000);
const _: () = assert!(KMemoryState::IO_MEMORY.bits() == 0x00182001);
const _: () = assert!(KMemoryState::IO_REGISTER.bits() == 0x00180001);
const _: () = assert!(KMemoryState::STATIC.bits() == 0x00042002);
const _: () = assert!(KMemoryState::CODE.bits() == 0x04DC7E03);
const _: () = assert!(KMemoryState::CODE_DATA.bits() == 0x0FFEBD04);
const _: () = assert!(KMemoryState::NORMAL.bits() == 0x077EBD05);
const _: () = assert!(KMemoryState::SHARED.bits() == 0x04402006);
const _: () = assert!(KMemoryState::ALIAS_CODE.bits() == 0x04DD7E08);
const _: () = assert!(KMemoryState::ALIAS_CODE_DATA.bits() == 0x0FFFBD09);
const _: () = assert!(KMemoryState::IPC.bits() == 0x045C3C0A);
const _: () = assert!(KMemoryState::STACK.bits() == 0x045C3C0B);
const _: () = assert!(KMemoryState::THREAD_LOCAL.bits() == 0x0400000C);
const _: () = assert!(KMemoryState::TRANSFERRED.bits() == 0x055C3C0D);
const _: () = assert!(KMemoryState::SHARED_TRANSFERRED.bits() == 0x045C380E);
const _: () = assert!(KMemoryState::SHARED_CODE.bits() == 0x0440380F);
const _: () = assert!(KMemoryState::INACCESSIBLE.bits() == 0x00000010);
const _: () = assert!(KMemoryState::NON_SECURE_IPC.bits() == 0x045C3811);
const _: () = assert!(KMemoryState::NON_DEVICE_IPC.bits() == 0x044C2812);
const _: () = assert!(KMemoryState::KERNEL.bits() == 0x00000013);
const _: () = assert!(KMemoryState::GENERATED_CODE.bits() == 0x04402214);
const _: () = assert!(KMemoryState::CODE_OUT.bits() == 0x04402015);
const _: () = assert!(KMemoryState::COVERAGE.bits() == 0x00002016);
const _: () = assert!(KMemoryState::INSECURE.bits() == 0x055C3817);

// ---------------------------------------------------------------------------
// KMemoryPermission
// ---------------------------------------------------------------------------

bitflags! {
    /// Port of Kernel::KMemoryPermission.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KMemoryPermission: u8 {
        const NONE = 0;

        const KERNEL_READ    = 0x08; // SvcRead << 3
        const KERNEL_WRITE   = 0x10; // SvcWrite << 3
        const KERNEL_EXECUTE = 0x20; // SvcExecute << 3

        const NOT_MAPPED = 1 << 6; // 1 << (2 * KernelShift)

        const KERNEL_READ_WRITE   = Self::KERNEL_READ.bits() | Self::KERNEL_WRITE.bits();
        const KERNEL_READ_EXECUTE = Self::KERNEL_READ.bits() | Self::KERNEL_EXECUTE.bits();

        const USER_READ    = 0x01 | Self::KERNEL_READ.bits();    // SvcRead | KernelRead
        const USER_WRITE   = 0x02 | Self::KERNEL_WRITE.bits();   // SvcWrite | KernelWrite
        const USER_EXECUTE = 0x04;                                // SvcExecute (no kernel bit)

        const USER_READ_WRITE   = Self::USER_READ.bits() | Self::USER_WRITE.bits();
        const USER_READ_EXECUTE = Self::USER_READ.bits() | Self::USER_EXECUTE.bits();

        const USER_MASK = 0x07; // Read | Write | Execute

        const IPC_LOCK_CHANGE_MASK = Self::NOT_MAPPED.bits() | Self::USER_READ_WRITE.bits();
    }
}

/// Kernel shift constant for permission conversions.
pub const KERNEL_SHIFT: u8 = 3;

/// Convert Svc::MemoryPermission to KMemoryPermission.
/// Port of ConvertToKMemoryPermission.
pub fn convert_to_k_memory_permission(perm: SvcMemoryPermission) -> KMemoryPermission {
    let user_mask = KMemoryPermission::USER_MASK;
    let raw = perm.bits();
    let k_raw = (raw & user_mask.bits())
        | KMemoryPermission::KERNEL_READ.bits()
        | ((raw & SvcMemoryPermission::WRITE.bits()) << KERNEL_SHIFT)
        | if perm.is_empty() {
            KMemoryPermission::NOT_MAPPED.bits()
        } else {
            0
        };
    KMemoryPermission::from_bits_truncate(k_raw)
}

// ---------------------------------------------------------------------------
// KMemoryAttribute
// ---------------------------------------------------------------------------

bitflags! {
    /// Port of Kernel::KMemoryAttribute.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KMemoryAttribute: u8 {
        const NONE              = 0x00;
        const LOCKED            = 0x01;
        const IPC_LOCKED        = 0x02;
        const DEVICE_SHARED     = 0x04;
        const UNCACHED          = 0x08;
        const PERMISSION_LOCKED = 0x10;
        const GPU_ACCESSIBLE    = 0x20;

        const USER_MASK = 0xFF;
        const SET_MASK  = Self::UNCACHED.bits() | Self::PERMISSION_LOCKED.bits() | Self::GPU_ACCESSIBLE.bits();
    }
}

// ---------------------------------------------------------------------------
// KMemoryBlockDisableMergeAttribute
// ---------------------------------------------------------------------------

bitflags! {
    /// Port of Kernel::KMemoryBlockDisableMergeAttribute.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KMemoryBlockDisableMergeAttribute: u8 {
        const NONE         = 0;
        const NORMAL       = 1 << 0;
        const DEVICE_LEFT  = 1 << 1;
        const IPC_LEFT     = 1 << 2;
        const LOCKED       = 1 << 3;
        const DEVICE_RIGHT = 1 << 4;

        const ALL_LEFT  = Self::NORMAL.bits() | Self::DEVICE_LEFT.bits()
                        | Self::IPC_LEFT.bits() | Self::LOCKED.bits();
        const ALL_RIGHT = Self::DEVICE_RIGHT.bits();
    }
}

// ---------------------------------------------------------------------------
// KMemoryInfo
// ---------------------------------------------------------------------------

/// Port of Kernel::KMemoryInfo.
#[derive(Debug, Clone, Copy)]
pub struct KMemoryInfo {
    pub m_address: usize,
    pub m_size: usize,
    pub m_state: KMemoryState,
    pub m_device_disable_merge_left_count: u16,
    pub m_device_disable_merge_right_count: u16,
    pub m_ipc_lock_count: u16,
    pub m_device_use_count: u16,
    pub m_ipc_disable_merge_count: u16,
    pub m_permission: KMemoryPermission,
    pub m_attribute: KMemoryAttribute,
    pub m_original_permission: KMemoryPermission,
    pub m_disable_merge_attribute: KMemoryBlockDisableMergeAttribute,
}

impl KMemoryInfo {
    pub fn get_address(&self) -> usize {
        self.m_address
    }

    pub fn get_size(&self) -> usize {
        self.m_size
    }

    pub fn get_num_pages(&self) -> usize {
        self.get_size() / PAGE_SIZE
    }

    pub fn get_end_address(&self) -> usize {
        self.get_address() + self.get_size()
    }

    pub fn get_last_address(&self) -> usize {
        self.get_end_address() - 1
    }

    pub fn get_ipc_lock_count(&self) -> u16 {
        self.m_ipc_lock_count
    }

    pub fn get_ipc_disable_merge_count(&self) -> u16 {
        self.m_ipc_disable_merge_count
    }

    pub fn get_state(&self) -> KMemoryState {
        self.m_state
    }

    pub fn get_svc_state(&self) -> u32 {
        (self.m_state & KMemoryState::MASK).bits()
    }

    pub fn get_permission(&self) -> KMemoryPermission {
        self.m_permission
    }

    pub fn get_original_permission(&self) -> KMemoryPermission {
        self.m_original_permission
    }

    pub fn get_attribute(&self) -> KMemoryAttribute {
        self.m_attribute
    }

    pub fn get_disable_merge_attribute(&self) -> KMemoryBlockDisableMergeAttribute {
        self.m_disable_merge_attribute
    }

    /// Port of upstream `KMemoryInfo::GetSvcMemoryInfo()`.
    pub fn get_svc_memory_info(&self) -> crate::hle::kernel::svc::svc_types::MemoryInfo {
        crate::hle::kernel::svc::svc_types::MemoryInfo {
            base_address: self.m_address as u64,
            size: self.m_size as u64,
            state: self.get_svc_state(),
            attribute: (self.m_attribute & KMemoryAttribute::USER_MASK).bits() as u32,
            permission: (self.m_permission & KMemoryPermission::USER_MASK).bits() as u32,
            ipc_count: self.m_ipc_lock_count as u32,
            device_count: self.m_device_use_count as u32,
            padding: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// KMemoryBlock
// ---------------------------------------------------------------------------

/// Port of Kernel::KMemoryBlock.
///
/// Upstream inherits a 0x1c-byte packed intrusive red-black-tree node. Rust's
/// block manager owns ordering separately, but the reserved base bytes remain
/// present so the dynamic slab retains Eden's 0x40-byte object geometry.
#[repr(C, align(8))]
#[derive(Debug, Clone)]
pub struct KMemoryBlock {
    _tree_node: [u8; 0x1c],
    m_device_disable_merge_left_count: u16,
    m_device_disable_merge_right_count: u16,
    m_address: usize,
    m_num_pages: usize,
    m_memory_state: KMemoryState,
    m_ipc_lock_count: u16,
    m_device_use_count: u16,
    m_ipc_disable_merge_count: u16,
    m_permission: KMemoryPermission,
    m_original_permission: KMemoryPermission,
    m_attribute: KMemoryAttribute,
    m_disable_merge_attribute: KMemoryBlockDisableMergeAttribute,
}

impl KMemoryBlock {
    pub fn new() -> Self {
        Self {
            _tree_node: [0; 0x1c],
            m_device_disable_merge_left_count: 0,
            m_device_disable_merge_right_count: 0,
            m_address: 0,
            m_num_pages: 0,
            m_memory_state: KMemoryState::NONE,
            m_ipc_lock_count: 0,
            m_device_use_count: 0,
            m_ipc_disable_merge_count: 0,
            m_permission: KMemoryPermission::NONE,
            m_original_permission: KMemoryPermission::NONE,
            m_attribute: KMemoryAttribute::NONE,
            m_disable_merge_attribute: KMemoryBlockDisableMergeAttribute::NONE,
        }
    }

    pub fn new_with(
        addr: usize,
        np: usize,
        ms: KMemoryState,
        p: KMemoryPermission,
        attr: KMemoryAttribute,
    ) -> Self {
        Self {
            _tree_node: [0; 0x1c],
            m_device_disable_merge_left_count: 0,
            m_device_disable_merge_right_count: 0,
            m_address: addr,
            m_num_pages: np,
            m_memory_state: ms,
            m_ipc_lock_count: 0,
            m_device_use_count: 0,
            m_ipc_disable_merge_count: 0,
            m_permission: p,
            m_original_permission: KMemoryPermission::NONE,
            m_attribute: attr,
            m_disable_merge_attribute: KMemoryBlockDisableMergeAttribute::NONE,
        }
    }

    pub fn initialize(
        &mut self,
        addr: usize,
        np: usize,
        ms: KMemoryState,
        p: KMemoryPermission,
        attr: KMemoryAttribute,
    ) {
        self.m_device_disable_merge_left_count = 0;
        self.m_device_disable_merge_right_count = 0;
        self.m_address = addr;
        self.m_num_pages = np;
        self.m_memory_state = ms;
        self.m_ipc_lock_count = 0;
        self.m_device_use_count = 0;
        self.m_permission = p;
        self.m_original_permission = KMemoryPermission::NONE;
        self.m_attribute = attr;
        self.m_disable_merge_attribute = KMemoryBlockDisableMergeAttribute::NONE;
    }

    pub fn compare(lhs: &KMemoryBlock, rhs: &KMemoryBlock) -> i32 {
        if lhs.get_address() < rhs.get_address() {
            -1
        } else if lhs.get_address() <= rhs.get_last_address() {
            0
        } else {
            1
        }
    }

    pub fn get_address(&self) -> usize {
        self.m_address
    }

    pub fn get_num_pages(&self) -> usize {
        self.m_num_pages
    }

    pub fn get_size(&self) -> usize {
        self.get_num_pages() * PAGE_SIZE
    }

    pub fn get_end_address(&self) -> usize {
        self.get_address() + self.get_size()
    }

    pub fn get_last_address(&self) -> usize {
        self.get_end_address() - 1
    }

    pub fn get_state(&self) -> KMemoryState {
        self.m_memory_state
    }

    pub fn get_ipc_lock_count(&self) -> u16 {
        self.m_ipc_lock_count
    }

    pub fn get_ipc_disable_merge_count(&self) -> u16 {
        self.m_ipc_disable_merge_count
    }

    pub fn get_permission(&self) -> KMemoryPermission {
        self.m_permission
    }

    pub fn get_original_permission(&self) -> KMemoryPermission {
        self.m_original_permission
    }

    pub fn get_attribute(&self) -> KMemoryAttribute {
        self.m_attribute
    }

    pub fn get_disable_merge_attribute(&self) -> KMemoryBlockDisableMergeAttribute {
        self.m_disable_merge_attribute
    }

    pub fn get_memory_info(&self) -> KMemoryInfo {
        KMemoryInfo {
            m_address: self.get_address(),
            m_size: self.get_size(),
            m_state: self.m_memory_state,
            m_device_disable_merge_left_count: self.m_device_disable_merge_left_count,
            m_device_disable_merge_right_count: self.m_device_disable_merge_right_count,
            m_ipc_lock_count: self.m_ipc_lock_count,
            m_device_use_count: self.m_device_use_count,
            m_ipc_disable_merge_count: self.m_ipc_disable_merge_count,
            m_permission: self.m_permission,
            m_attribute: self.m_attribute,
            m_original_permission: self.m_original_permission,
            m_disable_merge_attribute: self.m_disable_merge_attribute,
        }
    }

    pub fn has_properties(
        &self,
        s: KMemoryState,
        p: KMemoryPermission,
        a: KMemoryAttribute,
    ) -> bool {
        let attribute_ignore_mask = KMemoryAttribute::IPC_LOCKED | KMemoryAttribute::DEVICE_SHARED;
        self.m_memory_state == s
            && self.m_permission == p
            && (self.m_attribute | attribute_ignore_mask) == (a | attribute_ignore_mask)
    }

    pub fn has_same_properties(&self, rhs: &KMemoryBlock) -> bool {
        self.m_memory_state == rhs.m_memory_state
            && self.m_permission == rhs.m_permission
            && self.m_original_permission == rhs.m_original_permission
            && self.m_attribute == rhs.m_attribute
            && self.m_ipc_lock_count == rhs.m_ipc_lock_count
            && self.m_device_use_count == rhs.m_device_use_count
    }

    pub fn can_merge_with(&self, rhs: &KMemoryBlock) -> bool {
        self.has_same_properties(rhs)
            && (self.m_disable_merge_attribute & KMemoryBlockDisableMergeAttribute::ALL_RIGHT)
                == KMemoryBlockDisableMergeAttribute::NONE
            && (rhs.m_disable_merge_attribute & KMemoryBlockDisableMergeAttribute::ALL_LEFT)
                == KMemoryBlockDisableMergeAttribute::NONE
    }

    pub fn contains(&self, addr: usize) -> bool {
        self.get_address() <= addr && addr <= self.get_end_address()
    }

    pub fn add(&mut self, added_block: &KMemoryBlock) {
        debug_assert!(added_block.get_num_pages() > 0);
        self.m_num_pages += added_block.get_num_pages();
        self.m_disable_merge_attribute |= added_block.m_disable_merge_attribute;
        self.m_device_disable_merge_right_count = added_block.m_device_disable_merge_right_count;
    }

    pub fn update(
        &mut self,
        s: KMemoryState,
        p: KMemoryPermission,
        a: KMemoryAttribute,
        set_disable_merge_attr: bool,
        set_mask: u8,
        clear_mask: u8,
    ) {
        debug_assert!(self.m_original_permission == KMemoryPermission::NONE);
        debug_assert!((self.m_attribute & KMemoryAttribute::IPC_LOCKED) == KMemoryAttribute::NONE);

        self.m_memory_state = s;
        self.m_permission = p;
        self.m_attribute = KMemoryAttribute::from_bits_truncate(
            a.bits()
                | (self.m_attribute
                    & (KMemoryAttribute::IPC_LOCKED | KMemoryAttribute::DEVICE_SHARED))
                    .bits(),
        );

        if set_disable_merge_attr && set_mask != 0 {
            self.m_disable_merge_attribute |=
                KMemoryBlockDisableMergeAttribute::from_bits_truncate(set_mask);
        }
        if clear_mask != 0 {
            self.m_disable_merge_attribute &=
                KMemoryBlockDisableMergeAttribute::from_bits_truncate(!clear_mask);
        }
    }

    pub fn update_attribute(&mut self, mask: KMemoryAttribute, attr: KMemoryAttribute) {
        debug_assert!((mask & KMemoryAttribute::IPC_LOCKED).is_empty());
        debug_assert!((mask & KMemoryAttribute::DEVICE_SHARED).is_empty());
        self.m_attribute = (self.m_attribute & !mask) | attr;
    }

    pub fn split(&mut self, block: &mut KMemoryBlock, addr: usize) {
        debug_assert!(self.get_address() < addr);
        debug_assert!(self.contains(addr));
        debug_assert!(addr % PAGE_SIZE == 0);

        block.m_address = self.m_address;
        block.m_num_pages = (addr - self.get_address()) / PAGE_SIZE;
        block.m_memory_state = self.m_memory_state;
        block.m_ipc_lock_count = self.m_ipc_lock_count;
        block.m_device_use_count = self.m_device_use_count;
        block.m_permission = self.m_permission;
        block.m_original_permission = self.m_original_permission;
        block.m_attribute = self.m_attribute;
        block.m_disable_merge_attribute =
            self.m_disable_merge_attribute & KMemoryBlockDisableMergeAttribute::ALL_LEFT;
        block.m_ipc_disable_merge_count = self.m_ipc_disable_merge_count;
        block.m_device_disable_merge_left_count = self.m_device_disable_merge_left_count;
        block.m_device_disable_merge_right_count = 0;

        self.m_address = addr;
        self.m_num_pages -= block.m_num_pages;
        self.m_ipc_disable_merge_count = 0;
        self.m_device_disable_merge_left_count = 0;
        self.m_disable_merge_attribute =
            self.m_disable_merge_attribute & KMemoryBlockDisableMergeAttribute::ALL_RIGHT;
    }

    pub fn share_to_device(&mut self, _new_perm: KMemoryPermission, left: bool, right: bool) {
        debug_assert!(
            (self.m_attribute & KMemoryAttribute::DEVICE_SHARED) == KMemoryAttribute::DEVICE_SHARED
                || self.m_device_use_count == 0
        );
        self.m_device_use_count += 1;
        debug_assert!(self.m_device_use_count > 0);
        self.m_attribute |= KMemoryAttribute::DEVICE_SHARED;
        self.update_device_disable_merge_state_for_share(_new_perm, left, right);
    }

    pub fn unshare_to_device(&mut self, new_perm: KMemoryPermission, left: bool, right: bool) {
        debug_assert!(
            (self.m_attribute & KMemoryAttribute::DEVICE_SHARED) == KMemoryAttribute::DEVICE_SHARED
        );
        let old_count = self.m_device_use_count;
        self.m_device_use_count -= 1;
        debug_assert!(old_count > 0);
        if old_count == 1 {
            self.m_attribute &= !KMemoryAttribute::DEVICE_SHARED;
        }
        self.update_device_disable_merge_state_for_unshare(new_perm, left, right);
    }

    pub fn lock_for_ipc(&mut self, new_perm: KMemoryPermission, left: bool, _right: bool) {
        debug_assert!(
            (self.m_attribute & KMemoryAttribute::IPC_LOCKED) == KMemoryAttribute::IPC_LOCKED
                || self.m_ipc_lock_count == 0
        );
        self.m_ipc_lock_count += 1;
        let new_lock_count = self.m_ipc_lock_count;
        debug_assert!(new_lock_count > 0);

        if new_lock_count == 1 {
            debug_assert!(self.m_original_permission == KMemoryPermission::NONE);
            self.m_original_permission = self.m_permission;
            self.m_permission = KMemoryPermission::from_bits_truncate(
                (new_perm & KMemoryPermission::IPC_LOCK_CHANGE_MASK).bits()
                    | (self.m_original_permission & !KMemoryPermission::IPC_LOCK_CHANGE_MASK)
                        .bits(),
            );
        }
        self.m_attribute |= KMemoryAttribute::IPC_LOCKED;

        if left {
            self.m_disable_merge_attribute |= KMemoryBlockDisableMergeAttribute::IPC_LEFT;
            self.m_ipc_disable_merge_count += 1;
            debug_assert!(self.m_ipc_disable_merge_count > 0);
        }
    }

    pub fn unlock_for_ipc(&mut self, _new_perm: KMemoryPermission, left: bool, _right: bool) {
        debug_assert!(
            (self.m_attribute & KMemoryAttribute::IPC_LOCKED) == KMemoryAttribute::IPC_LOCKED
        );
        let old_lock_count = self.m_ipc_lock_count;
        self.m_ipc_lock_count -= 1;
        debug_assert!(old_lock_count > 0);

        if old_lock_count == 1 {
            debug_assert!(self.m_original_permission != KMemoryPermission::NONE);
            self.m_permission = self.m_original_permission;
            self.m_original_permission = KMemoryPermission::NONE;
            self.m_attribute &= !KMemoryAttribute::IPC_LOCKED;
        }

        if left {
            let old_count = self.m_ipc_disable_merge_count;
            self.m_ipc_disable_merge_count -= 1;
            debug_assert!(old_count > 0);
            if old_count == 1 {
                self.m_disable_merge_attribute &= !KMemoryBlockDisableMergeAttribute::IPC_LEFT;
            }
        }
    }

    // Device merge state helpers.

    pub(crate) fn update_device_disable_merge_state_for_share(
        &mut self,
        new_perm: KMemoryPermission,
        left: bool,
        right: bool,
    ) {
        self.update_device_disable_merge_state_for_share_left(new_perm, left, right);
        self.update_device_disable_merge_state_for_share_right(new_perm, left, right);
    }

    pub(crate) fn update_device_disable_merge_state_for_share_left(
        &mut self,
        _new_perm: KMemoryPermission,
        left: bool,
        _right: bool,
    ) {
        if left {
            self.m_disable_merge_attribute |= KMemoryBlockDisableMergeAttribute::DEVICE_LEFT;
            self.m_device_disable_merge_left_count += 1;
            debug_assert!(self.m_device_disable_merge_left_count > 0);
        }
    }

    pub(crate) fn update_device_disable_merge_state_for_share_right(
        &mut self,
        _new_perm: KMemoryPermission,
        _left: bool,
        right: bool,
    ) {
        if right {
            self.m_disable_merge_attribute |= KMemoryBlockDisableMergeAttribute::DEVICE_RIGHT;
            self.m_device_disable_merge_right_count += 1;
            debug_assert!(self.m_device_disable_merge_right_count > 0);
        }
    }

    pub(crate) fn update_device_disable_merge_state_for_unshare(
        &mut self,
        new_perm: KMemoryPermission,
        left: bool,
        right: bool,
    ) {
        self.update_device_disable_merge_state_for_unshare_left(new_perm, left, right);
        self.update_device_disable_merge_state_for_unshare_right(new_perm, left, right);
    }

    pub(crate) fn update_device_disable_merge_state_for_unshare_left(
        &mut self,
        _new_perm: KMemoryPermission,
        left: bool,
        _right: bool,
    ) {
        if left {
            if self.m_device_disable_merge_left_count == 0 {
                return;
            }
            self.m_device_disable_merge_left_count -= 1;
        }
        self.m_device_disable_merge_left_count = self
            .m_device_disable_merge_left_count
            .min(self.m_device_use_count);
        if self.m_device_disable_merge_left_count == 0 {
            self.m_disable_merge_attribute &= !KMemoryBlockDisableMergeAttribute::DEVICE_LEFT;
        }
    }

    pub(crate) fn update_device_disable_merge_state_for_unshare_right(
        &mut self,
        _new_perm: KMemoryPermission,
        _left: bool,
        right: bool,
    ) {
        if right {
            let old_count = self.m_device_disable_merge_right_count;
            self.m_device_disable_merge_right_count -= 1;
            debug_assert!(old_count > 0);
            if old_count == 1 {
                self.m_disable_merge_attribute &= !KMemoryBlockDisableMergeAttribute::DEVICE_RIGHT;
            }
        }
    }
}

const _: () = assert!(std::mem::size_of::<KMemoryBlock>() == 0x40);

impl Default for KMemoryBlock {
    fn default() -> Self {
        Self::new()
    }
}
