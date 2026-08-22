//! Port of zuyu/src/core/hle/kernel/k_page_table_base.h / k_page_table_base.cpp
//! Status: Partial; runtime-critical map/unmap, IPC setup/cleanup, lock-memory,
//! device-address-space, code-memory, heap, page-group, and physical-memory paths
//! have local upstream-shaped lock/updater/allocator structure.
//! Derniere synchro: 2026-06-07
//!
//! The base class for virtual memory page tables.
//! Contains all address space regions, memory block manager, and virtual memory operations.
//!
//! Remaining gaps are tracked in DIFF.md and mostly concern process/handle
//! ownership boundaries outside the audited page-table traversal slice.

use bitflags::bitflags;

use std::sync::{Arc, Mutex};

use super::k_address_space_info::{AddressSpaceInfoType, KAddressSpaceInfo};
use super::k_dynamic_resource_manager::{KBlockInfoManager, KMemoryBlockSlabManager};
use super::k_light_lock::{KLightLock, KScopedLightLock};
use super::k_memory_block::*;
use super::k_memory_block_manager::KMemoryBlockManager;
use super::k_memory_layout::KERNEL_ASLR_ALIGNMENT;
use super::k_memory_manager;
use super::k_memory_region_type::K_MEMORY_REGION_ATTR_LINEAR_MAPPED;
use super::k_resource_limit::{KResourceLimit, LimitableResource};
use super::svc::svc_types::PageInfo;
use super::svc_types::{CreateProcessFlag, MemoryState as SvcMemoryState, ADDRESS_SPACE_MASK};
use crate::core::SystemRef;
use crate::hle::kernel::svc::svc_results;
use crate::memory::memory::Memory;

/// RAII wrapper for acquiring two page-table light locks in a stable address
/// order. Mirrors the helper local to upstream `k_page_table_base.cpp`.
struct KScopedLightLockPair {
    lower: Option<Arc<KLightLock>>,
    upper: Option<Arc<KLightLock>>,
}

fn invalidate_instruction_cache_for_table(table: &KPageTableBase, addr: usize, size: usize) {
    let Some(kernel) = crate::hle::kernel::kernel::get_kernel_ref() else {
        return;
    };
    let Some(process) = kernel.get_process_by_page_table_base(table as *const KPageTableBase)
    else {
        return;
    };

    let mut process = process.lock().unwrap();
    for core_id in 0..crate::hardware_properties::NUM_CPU_CORES as usize {
        if let Some(interface) = process.get_arm_interface_mut(core_id) {
            interface.invalidate_cache_range(addr as u64, size);
        }
    }
}

fn store_data_cache(_addr: u64, _size: usize) -> u32 {
    0
}

fn is_heap_physical_address_for_table(phys_addr: u64) -> bool {
    crate::hle::kernel::kernel::get_kernel_ref()
        .map(|kernel| kernel.memory_manager().contains_physical_address(phys_addr))
        .unwrap_or(true)
}

fn is_linear_mapped_physical_address_for_table(phys_addr: u64) -> bool {
    crate::hle::kernel::kernel::get_kernel_ref()
        .and_then(|kernel| kernel.get_memory_layout())
        .and_then(|layout| {
            layout
                .lock()
                .unwrap()
                .find_physical(phys_addr)
                .map(|region| region.has_type_attribute(K_MEMORY_REGION_ATTR_LINEAR_MAPPED))
        })
        .unwrap_or_else(|| is_heap_physical_address_for_table(phys_addr))
}

impl KScopedLightLockPair {
    fn new(lhs: Arc<KLightLock>, rhs: Arc<KLightLock>) -> Self {
        let (lower, upper) = if Arc::as_ptr(&lhs) <= Arc::as_ptr(&rhs) {
            (lhs, rhs)
        } else {
            (rhs, lhs)
        };

        lower.lock();
        if !Arc::ptr_eq(&lower, &upper) {
            upper.lock();
        }

        Self {
            lower: Some(lower),
            upper: Some(upper),
        }
    }

    /// Mirrors upstream `TryUnlockHalf`.
    #[allow(dead_code)]
    fn try_unlock_half(&mut self, lock: &Arc<KLightLock>) {
        let is_pair = self
            .lower
            .as_ref()
            .zip(self.upper.as_ref())
            .is_some_and(|(lower, upper)| !Arc::ptr_eq(lower, upper));
        if !is_pair {
            return;
        }

        if self
            .lower
            .as_ref()
            .is_some_and(|lower| Arc::ptr_eq(lower, lock))
        {
            if let Some(lower) = self.lower.take() {
                lower.unlock();
            }
        } else if self
            .upper
            .as_ref()
            .is_some_and(|upper| Arc::ptr_eq(upper, lock))
        {
            if let Some(upper) = self.upper.take() {
                upper.unlock();
            }
        }
    }
}

impl Drop for KScopedLightLockPair {
    fn drop(&mut self) {
        if let Some(upper) = self.upper.take() {
            if self
                .lower
                .as_ref()
                .is_none_or(|lower| !Arc::ptr_eq(lower, &upper))
            {
                upper.unlock();
            }
        }
        if let Some(lower) = self.lower.take() {
            lower.unlock();
        }
    }
}

// ---------------------------------------------------------------------------
// DisableMergeAttribute
// ---------------------------------------------------------------------------

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DisableMergeAttribute: u8 {
        const NONE                       = 0;
        const DISABLE_HEAD               = 1 << 0;
        const DISABLE_HEAD_AND_BODY      = 1 << 1;
        const ENABLE_HEAD_AND_BODY       = 1 << 2;
        const DISABLE_TAIL               = 1 << 3;
        const ENABLE_TAIL                = 1 << 4;
        const ENABLE_AND_MERGE_HEAD_BODY_TAIL = 1 << 5;

        const ENABLE_HEAD_BODY_TAIL  = Self::ENABLE_HEAD_AND_BODY.bits() | Self::ENABLE_TAIL.bits();
        const DISABLE_HEAD_BODY_TAIL = Self::DISABLE_HEAD_AND_BODY.bits() | Self::DISABLE_TAIL.bits();
    }
}

// ---------------------------------------------------------------------------
// KPageProperties
// ---------------------------------------------------------------------------

/// Port of Kernel::KPageProperties.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KPageProperties {
    pub perm: KMemoryPermission,
    pub io: bool,
    pub uncached: bool,
    pub disable_merge_attributes: DisableMergeAttribute,
}

const _: () = assert!(std::mem::size_of::<KPageProperties>() == std::mem::size_of::<u32>());

// ---------------------------------------------------------------------------
// MemoryFillValue
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryFillValue {
    Zero = 0,
    Stack = b'X',
    Ipc = b'Y',
    Heap = b'Z',
}

// ---------------------------------------------------------------------------
// OperationType
// ---------------------------------------------------------------------------

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationType {
    Map = 0,
    MapGroup = 1,
    MapFirstGroup = 2,
    Unmap = 3,
    ChangePermissions = 4,
    ChangePermissionsAndRefresh = 5,
    ChangePermissionsAndRefreshAndFlush = 6,
    Separate = 7,
    MapFirstGroupPhysical = 65000,
    UnmapPhysical = 65001,
}

// ---------------------------------------------------------------------------
// PageLinkedList
// ---------------------------------------------------------------------------

/// Stack of page-table pages awaiting deferred free at end of an update.
///
/// Port of upstream `KPageTableBase::PageLinkedList` (k_page_table_base.h:116).
/// Upstream stores intrusive `Node*` pointers (each Node is a 4 KB page);
/// ruzu stores raw physical addresses since ruzu's page table doesn't yet
/// have a kernel-side page-table-page allocator. Current Eden's
/// `FinalizeUpdate` drains the list while its manager assertions and free call
/// remain commented out; Ruzu does the same.
#[derive(Default, Debug)]
pub struct PageLinkedList {
    pages: Vec<u64>,
}

impl PageLinkedList {
    pub const fn new() -> Self {
        Self { pages: Vec::new() }
    }
    pub fn push(&mut self, page: u64) {
        debug_assert_eq!(page & (PAGE_SIZE as u64 - 1), 0);
        self.pages.push(page);
    }
    pub fn peek(&self) -> Option<u64> {
        self.pages.last().copied()
    }
    pub fn pop(&mut self) -> Option<u64> {
        self.pages.pop()
    }
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

/// RAII helper that holds a `PageLinkedList` for the duration of a page-table
/// update and frees it on destruction by calling
/// `m_pt->FinalizeUpdate(this->GetPageList())`.
///
/// Port of upstream `KPageTableBase::KScopedPageTableUpdater`
/// (k_page_table_base.h:170):
///
/// ```cpp
/// class KScopedPageTableUpdater {
///     KPageTableBase* m_pt;
///     PageLinkedList m_ll;
///   public:
///     explicit KScopedPageTableUpdater(KPageTableBase* pt) : m_pt(pt), m_ll() {}
///     ~KScopedPageTableUpdater() { m_pt->FinalizeUpdate(GetPageList()); }
/// };
/// ```
///
/// Holds a raw `*mut KPageTableBase` to mirror upstream's destructor
/// semantics — Rust's borrow checker would otherwise forbid the back-
/// reference from coexisting with the caller's `&mut self` access.
/// The pointer is valid for the helper's lifetime since the helper is
/// always a stack local in a method that already holds `&mut self`.
/// Finalize fires unconditionally on Drop.
pub struct KScopedPageTableUpdater {
    pt: *mut KPageTableBase,
    page_list: PageLinkedList,
}

impl KScopedPageTableUpdater {
    /// Construct with a raw pointer to the page table whose update is
    /// in progress. Mirrors upstream's `KScopedPageTableUpdater(KPageTableBase*)`.
    ///
    /// # Safety
    /// `pt` must remain valid for the lifetime of the returned helper.
    /// In practice always called with `self as *mut _` from inside a
    /// method that already holds `&mut self`, so the pointer is non-null
    /// and outlives the stack frame.
    pub unsafe fn new(pt: *mut KPageTableBase) -> Self {
        Self {
            pt,
            page_list: PageLinkedList::new(),
        }
    }

    /// Convenience constructor for `&mut self`-holding callers.
    pub fn from_mut(pt: &mut KPageTableBase) -> Self {
        // SAFETY: pt is exclusively borrowed for at least the lifetime
        // of this helper; we do not alias through the raw pointer.
        unsafe { Self::new(pt as *mut _) }
    }

    /// Borrow the page list, matching upstream's `GetPageList()` accessor.
    pub fn page_list(&mut self) -> &mut PageLinkedList {
        &mut self.page_list
    }
}

impl Drop for KScopedPageTableUpdater {
    fn drop(&mut self) {
        // Mirror upstream's destructor: unconditionally call
        // `m_pt->FinalizeUpdate(this->GetPageList())`.
        if !self.pt.is_null() {
            // SAFETY: `pt` is valid for the lifetime of this helper per
            // the unsafe contract on `new` / `from_mut`. The page list
            // is exclusively borrowed within this Drop scope.
            unsafe {
                (*self.pt).finalize_update(&mut self.page_list);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const PAGE_BITS: usize = 12;
pub const MAX_PHYSICAL_MAP_ALIGNMENT: usize = 1 << 30; // 1 GiB
pub const REGION_ALIGNMENT: usize = 2 << 20; // 2 MiB
const _: () = assert!(REGION_ALIGNMENT == KERNEL_ASLR_ALIGNMENT);

// ---------------------------------------------------------------------------
// KPageTableBase
// ---------------------------------------------------------------------------

/// Port of Kernel::KPageTableBase.
///
/// Owns the Rust page-table state corresponding to upstream `KPageTableBase`.
/// Remaining behavioral gaps are tracked in `DIFF.md`.
pub struct KPageTableBase {
    /// Upstream: `Core::System& m_system`.
    pub(crate) m_system: SystemRef,
    /// The page table implementation used by dynarmic.
    /// Upstream: `std::unique_ptr<Common::PageTable> m_impl`
    pub(crate) m_impl: Option<Box<common::page_table::PageTable>>,
    /// Reference to the Memory bridge (MapMemoryRegion/UnmapRegion/ProtectRegion).
    /// Upstream: `Core::Memory::Memory* m_memory`
    pub(crate) m_memory: Option<Arc<Mutex<Memory>>>,
    pub(crate) m_address_space_start: usize,
    pub(crate) m_address_space_end: usize,
    pub(crate) m_heap_region_start: usize,
    pub(crate) m_heap_region_end: usize,
    pub(crate) m_current_heap_end: usize,
    pub(crate) m_alias_region_start: usize,
    pub(crate) m_alias_region_end: usize,
    pub(crate) m_stack_region_start: usize,
    pub(crate) m_stack_region_end: usize,
    pub(crate) m_kernel_map_region_start: usize,
    pub(crate) m_kernel_map_region_end: usize,
    pub(crate) m_alias_code_region_start: usize,
    pub(crate) m_alias_code_region_end: usize,
    pub(crate) m_code_region_start: usize,
    pub(crate) m_code_region_end: usize,
    pub(crate) m_max_heap_size: usize,
    pub(crate) m_mapped_physical_memory_size: usize,
    pub(crate) m_mapped_unsafe_physical_memory: usize,
    pub(crate) m_mapped_insecure_memory: usize,
    pub(crate) m_mapped_ipc_server_memory: usize,
    /// Upstream: `mutable KLightLock m_general_lock`.
    pub(crate) m_general_lock: Arc<KLightLock>,
    /// Upstream: `mutable KLightLock m_map_physical_memory_lock`.
    #[allow(dead_code)]
    pub(crate) m_map_physical_memory_lock: Arc<KLightLock>,
    /// Upstream: `KLightLock m_device_map_lock`.
    #[allow(dead_code)]
    pub(crate) m_device_map_lock: Arc<KLightLock>,
    pub(crate) m_memory_block_manager: KMemoryBlockManager,
    pub(crate) m_allocate_option: u32,
    pub(crate) m_address_space_width: u32,
    pub(crate) m_is_kernel: bool,
    pub(crate) m_enable_aslr: bool,
    pub(crate) m_enable_device_address_space_merge: bool,
    /// Upstream: `KMemoryBlockSlabManager* m_memory_block_slab_manager`.
    pub(crate) m_memory_block_slab_manager: Option<Arc<KMemoryBlockSlabManager>>,
    /// Upstream: `KBlockInfoManager* m_block_info_manager`.
    pub(crate) m_block_info_manager: Option<Arc<KBlockInfoManager>>,
    pub(crate) m_heap_fill_value: MemoryFillValue,
    pub(crate) m_ipc_fill_value: MemoryFillValue,
    pub(crate) m_stack_fill_value: MemoryFillValue,
    pub(crate) m_process_id: u64,
    /// Upstream: `KResourceLimit* m_resource_limit`.
    pub(crate) m_resource_limit: Option<Arc<KResourceLimit>>,
}

impl KPageTableBase {
    pub fn new() -> Self {
        Self {
            m_system: SystemRef::null(),
            m_impl: None,
            m_memory: None,
            m_address_space_start: 0,
            m_address_space_end: 0,
            m_heap_region_start: 0,
            m_heap_region_end: 0,
            m_current_heap_end: 0,
            m_alias_region_start: 0,
            m_alias_region_end: 0,
            m_stack_region_start: 0,
            m_stack_region_end: 0,
            m_kernel_map_region_start: 0,
            m_kernel_map_region_end: 0,
            m_alias_code_region_start: 0,
            m_alias_code_region_end: 0,
            m_code_region_start: 0,
            m_code_region_end: 0,
            m_max_heap_size: 0,
            m_mapped_physical_memory_size: 0,
            m_mapped_unsafe_physical_memory: 0,
            m_mapped_insecure_memory: 0,
            m_mapped_ipc_server_memory: 0,
            m_general_lock: Arc::new(KLightLock::new()),
            m_map_physical_memory_lock: Arc::new(KLightLock::new()),
            m_device_map_lock: Arc::new(KLightLock::new()),
            m_memory_block_manager: KMemoryBlockManager::new(),
            m_allocate_option: 0,
            m_address_space_width: 0,
            m_is_kernel: false,
            m_enable_aslr: false,
            m_enable_device_address_space_merge: false,
            m_memory_block_slab_manager: None,
            m_block_info_manager: None,
            m_heap_fill_value: MemoryFillValue::Zero,
            m_ipc_fill_value: MemoryFillValue::Zero,
            m_stack_fill_value: MemoryFillValue::Zero,
            m_process_id: 0,
            m_resource_limit: None,
        }
    }

    // --- Accessors matching upstream ---

    pub fn is_kernel(&self) -> bool {
        self.m_is_kernel
    }

    pub fn set_process_id(&mut self, process_id: u64) {
        self.m_process_id = process_id;
    }

    /// Zero a region of physical memory. Used by `set_heap_size` and other
    /// pool-allocation callers to clear newly-allocated pages BEFORE they
    /// are mapped into virtual address space — upstream's
    /// `ClearBackingRegion(m_system, block.GetAddress(), block.GetSize(), m_heap_fill_value)`
    /// does the same. The physical address is translated to a HostMemory
    /// backing offset so Linux can discard zero-filled pages with MADV_REMOVE.
    fn clear_fresh_backing_region_phys(&self, phys_addr: u64, size: usize) {
        if size == 0 {
            return;
        }
        // The backing buffer is owned by the System's DeviceMemory. Reach it
        // through the existing Memory wrapper.
        if let Some(memory) = &self.m_memory {
            memory.lock().unwrap().zero_phys_block(phys_addr, size);
        }
    }

    /// Port of upstream `KPageTableBase::FinalizeProcess`.
    pub fn finalize_process(&mut self) -> u32 {
        debug_assert!(!self.is_kernel());
        0
    }

    /// Finalize the page table, releasing all resources.
    /// Port of upstream `KPageTableBase::Finalize`.
    pub fn finalize(&mut self) {
        let _ = self.finalize_process();

        let has_fastmem_arena = self
            .m_impl
            .as_ref()
            .is_some_and(|page_table| !page_table.fastmem_arena.is_null());
        let slab_manager = self.m_memory_block_slab_manager.clone();
        let mut block_manager = std::mem::take(&mut self.m_memory_block_manager);

        {
            let general_lock = Arc::clone(&self.m_general_lock);
            let _lock_guard = KScopedLightLock::new(general_lock.as_ref());
            block_manager.finalize(slab_manager.as_deref(), |addr, size| {
                if has_fastmem_arena && !self.m_system.is_null() {
                    self.m_system
                        .get()
                        .device_memory()
                        .buffer
                        .unmap(addr, size, false);
                }

                let mut page_group =
                    super::k_page_group::KPageGroup::with_block_info_manager(
                        self.m_block_info_manager.clone(),
                    );
                let _ = self.make_page_group(&mut page_group, addr, size / PAGE_SIZE);
                page_group.close_and_reset();
            });
        }
        self.m_memory_block_manager = block_manager;

        if std::mem::take(&mut self.m_mapped_unsafe_physical_memory) != 0 {
            log::warn!("KPageTableBase::finalize: unsafe mapped memory cleanup is unimplemented");
        }

        let mapped_insecure_memory = std::mem::take(&mut self.m_mapped_insecure_memory);
        if mapped_insecure_memory != 0 {
            if let Some(resource_limit) = crate::hle::kernel::kernel::get_kernel_ref()
                .and_then(super::board::k_system_control::get_insecure_memory_resource_limit)
            {
                resource_limit.release(
                    LimitableResource::PhysicalMemoryMax,
                    mapped_insecure_memory as i64,
                );
            }
        }

        let mapped_ipc_server_memory = std::mem::take(&mut self.m_mapped_ipc_server_memory);
        if mapped_ipc_server_memory != 0 {
            if let Some(resource_limit) = &self.m_resource_limit {
                resource_limit.release(
                    LimitableResource::PhysicalMemoryMax,
                    mapped_ipc_server_memory as i64,
                );
            }
        }

        // Upstream resets the unique_ptr directly. Its VirtualBuffer
        // destructors release the reserved arrays without walking every 4 KiB
        // entry in the process address space.
        self.m_impl = None;
        self.m_memory = None;
        self.m_memory_block_slab_manager = None;
        self.m_block_info_manager = None;
        self.m_system = SystemRef::null();
    }

    pub fn is_aslr_enabled(&self) -> bool {
        self.m_enable_aslr
    }

    pub fn contains(&self, addr: usize) -> bool {
        self.m_address_space_start <= addr && addr <= self.m_address_space_end.wrapping_sub(1)
    }

    pub fn contains_range(&self, addr: usize, size: usize) -> bool {
        self.m_address_space_start <= addr
            && addr < addr.wrapping_add(size)
            && addr.wrapping_add(size).wrapping_sub(1) <= self.m_address_space_end.wrapping_sub(1)
    }

    pub fn get_address_space_start(&self) -> usize {
        self.m_address_space_start
    }
    pub fn get_address_space_size(&self) -> usize {
        self.m_address_space_end - self.m_address_space_start
    }
    pub fn get_heap_region_start(&self) -> usize {
        self.m_heap_region_start
    }
    pub fn get_heap_region_size(&self) -> usize {
        self.m_heap_region_end - self.m_heap_region_start
    }
    pub fn get_current_heap_size(&self) -> usize {
        self.m_current_heap_end
            .saturating_sub(self.m_heap_region_start)
    }
    pub fn get_alias_region_start(&self) -> usize {
        self.m_alias_region_start
    }
    pub fn get_alias_region_size(&self) -> usize {
        self.m_alias_region_end - self.m_alias_region_start
    }
    pub fn get_stack_region_start(&self) -> usize {
        self.m_stack_region_start
    }
    pub fn get_stack_region_size(&self) -> usize {
        self.m_stack_region_end - self.m_stack_region_start
    }
    pub fn get_kernel_map_region_start(&self) -> usize {
        self.m_kernel_map_region_start
    }
    pub fn get_kernel_map_region_size(&self) -> usize {
        self.m_kernel_map_region_end - self.m_kernel_map_region_start
    }
    pub fn get_code_region_start(&self) -> usize {
        self.m_code_region_start
    }
    pub fn get_code_region_size(&self) -> usize {
        self.m_code_region_end - self.m_code_region_start
    }
    pub fn get_alias_code_region_start(&self) -> usize {
        self.m_alias_code_region_start
    }
    pub fn get_alias_code_region_size(&self) -> usize {
        self.m_alias_code_region_end - self.m_alias_code_region_start
    }

    pub fn get_allocate_option(&self) -> u32 {
        self.m_allocate_option
    }
    pub fn get_address_space_width(&self) -> u32 {
        self.m_address_space_width
    }

    pub fn get_num_guard_pages(&self) -> usize {
        if self.m_is_kernel {
            1
        } else {
            4
        }
    }

    /// Port of GetAddressSpaceWidth(Svc::CreateProcessFlag).
    /// Upstream matches on the masked enum values directly:
    ///   AddressSpace32Bit = 0<<1 = 0
    ///   AddressSpace64BitDeprecated = 1<<1 = 2
    ///   AddressSpace32BitWithoutAlias = 2<<1 = 4
    ///   AddressSpace64Bit = 3<<1 = 6
    pub fn get_address_space_width_from_flags(flags: u32) -> usize {
        use super::svc_types::{CreateProcessFlag, ADDRESS_SPACE_MASK};
        match flags & ADDRESS_SPACE_MASK {
            x if x == CreateProcessFlag::ADDRESS_SPACE_64_BIT.bits() => 39,
            x if x == CreateProcessFlag::ADDRESS_SPACE_64_BIT_DEPRECATED.bits() => 36,
            x if x == CreateProcessFlag::ADDRESS_SPACE_32_BIT.bits() => 32,
            x if x == CreateProcessFlag::ADDRESS_SPACE_32_BIT_WITHOUT_ALIAS.bits() => 32,
            other => panic!(
                "Invalid address space flag: flags={:#x}, masked={:#x}",
                flags, other
            ),
        }
    }

    pub fn get_memory_block_manager(&self) -> &KMemoryBlockManager {
        &self.m_memory_block_manager
    }

    pub fn get_memory_block_manager_mut(&mut self) -> &mut KMemoryBlockManager {
        &mut self.m_memory_block_manager
    }

    // -- Region resolution matching upstream GetRegionAddress/GetRegionSize --

    /// Get the start address for a memory region by SVC memory state.
    /// Matches upstream `KPageTableBase::GetRegionAddress(Svc::MemoryState)`.
    pub fn get_region_address(&self, state: SvcMemoryState) -> usize {
        use SvcMemoryState::*;
        match state {
            Free | Kernel => self.m_address_space_start,
            Normal => self.m_heap_region_start,
            Ipc | NonSecureIpc | NonDeviceIpc => self.m_alias_region_start,
            Stack => self.m_stack_region_start,
            Static | ThreadLocal => self.m_kernel_map_region_start,
            Io | Shared | AliasCode | AliasCodeData | Transferred | SharedTransferred
            | SharedCode | GeneratedCode | CodeOut | Coverage | Insecure => {
                self.m_alias_code_region_start
            }
            Code | CodeData => self.m_code_region_start,
            _ => {
                log::error!("GetRegionAddress: unknown state {:?}", state);
                self.m_address_space_start
            }
        }
    }

    /// Get the size of a memory region by SVC memory state.
    /// Matches upstream `KPageTableBase::GetRegionSize(Svc::MemoryState)`.
    pub fn get_region_size(&self, state: SvcMemoryState) -> usize {
        use SvcMemoryState::*;
        match state {
            Free | Kernel => self.m_address_space_end - self.m_address_space_start,
            Normal => self.m_heap_region_end - self.m_heap_region_start,
            Ipc | NonSecureIpc | NonDeviceIpc => {
                self.m_alias_region_end - self.m_alias_region_start
            }
            Stack => self.m_stack_region_end - self.m_stack_region_start,
            Static | ThreadLocal => self.m_kernel_map_region_end - self.m_kernel_map_region_start,
            Io | Shared | AliasCode | AliasCodeData | Transferred | SharedTransferred
            | SharedCode | GeneratedCode | CodeOut | Coverage | Insecure => {
                self.m_alias_code_region_end - self.m_alias_code_region_start
            }
            Code | CodeData => self.m_code_region_end - self.m_code_region_start,
            _ => {
                log::error!("GetRegionSize: unknown state {:?}", state);
                self.m_address_space_end - self.m_address_space_start
            }
        }
    }

    /// Check if an address range can be contained within a region for a given state.
    /// Matches upstream `KPageTableBase::CanContain(KProcessAddress, size_t, Svc::MemoryState)`
    /// (`zuyu/src/core/hle/kernel/k_page_table_base.cpp:574-619`).
    ///
    /// Earlier ruzu only checked `is_in_region`; it accepted addresses that
    /// overlap the heap or alias regions for memory states that upstream
    /// excludes from those (Shared, AliasCode, etc.). That caused
    /// `svcMapSharedMemory` to succeed at addresses past heap_end on
    /// AArch32, corrupting the page-table invariants and cascading into
    pub fn can_contain(&self, addr: usize, size: usize, state: SvcMemoryState) -> bool {
        use SvcMemoryState::*;

        let end = addr.saturating_add(size);
        let last = end.saturating_sub(1);

        let region_start = self.get_region_address(state);
        let region_size = self.get_region_size(state);
        let region_end = region_start.saturating_add(region_size);

        let is_in_region =
            region_start <= addr && addr < end && last <= region_end.saturating_sub(1);
        let is_in_heap = !(end <= self.m_heap_region_start
            || self.m_heap_region_end <= addr
            || self.m_heap_region_start == self.m_heap_region_end);
        let is_in_alias = !(end <= self.m_alias_region_start
            || self.m_alias_region_end <= addr
            || self.m_alias_region_start == self.m_alias_region_end);

        match state {
            Free | Kernel => is_in_region,
            Io | Static | Code | CodeData | Shared | AliasCode | AliasCodeData | Stack
            | ThreadLocal | Transferred | SharedTransferred | SharedCode | GeneratedCode
            | CodeOut | Coverage | Insecure => is_in_region && !is_in_heap && !is_in_alias,
            Normal => {
                debug_assert!(
                    is_in_heap,
                    "CanContain(Normal) called with addr=0x{:x} size=0x{:x} not in heap",
                    addr, size,
                );
                is_in_region && !is_in_alias
            }
            Ipc | NonSecureIpc | NonDeviceIpc => {
                debug_assert!(
                    is_in_alias,
                    "CanContain(Ipc) called with addr=0x{:x} size=0x{:x} not in alias",
                    addr, size,
                );
                is_in_region && !is_in_heap
            }
            _ => false,
        }
    }

    // -- KMemoryState convenience overloads --
    // Upstream: inline overloads in k_page_table_base.h that cast
    //   `static_cast<Svc::MemoryState>(state & KMemoryState::Mask)`.

    fn k_state_to_svc(state: KMemoryState) -> SvcMemoryState {
        let svc = state.bits() & KMemoryState::MASK.bits();
        // SAFETY: all valid KMemoryState lower bytes map to valid SvcMemoryState discriminants.
        unsafe { std::mem::transmute(svc) }
    }

    pub fn get_region_address_k(&self, state: KMemoryState) -> usize {
        self.get_region_address(Self::k_state_to_svc(state))
    }

    pub fn get_region_size_k(&self, state: KMemoryState) -> usize {
        self.get_region_size(Self::k_state_to_svc(state))
    }

    pub fn can_contain_k(&self, addr: usize, size: usize, state: KMemoryState) -> bool {
        self.can_contain(addr, size, Self::k_state_to_svc(state))
    }

    pub fn is_in_alias_region(&self, addr: usize, size: usize) -> bool {
        self.m_alias_region_start <= addr && addr + size <= self.m_alias_region_end
    }

    pub fn is_in_heap_region(&self, addr: usize, size: usize) -> bool {
        self.m_heap_region_start <= addr && addr + size <= self.m_heap_region_end
    }

    // -- CheckMemoryState --

    /// Default ignore attribute for CheckMemoryState.
    /// Matches upstream `DefaultMemoryIgnoreAttr`.
    pub const DEFAULT_MEMORY_IGNORE_ATTR: KMemoryAttribute = KMemoryAttribute::from_bits_truncate(
        KMemoryAttribute::IPC_LOCKED.bits() | KMemoryAttribute::DEVICE_SHARED.bits(),
    );

    /// Check that a single block's state/perm/attr match expectations.
    /// Matches upstream `CheckMemoryState(const KMemoryInfo&, ...)`.
    pub fn check_memory_state_info(
        info: &KMemoryInfo,
        state_mask: KMemoryState,
        state: KMemoryState,
        perm_mask: KMemoryPermission,
        perm: KMemoryPermission,
        attr_mask: KMemoryAttribute,
        attr: KMemoryAttribute,
    ) -> u32 {
        if (info.m_state & state_mask) != state {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }
        if (info.m_permission & perm_mask) != perm {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }
        if (info.m_attribute & attr_mask) != attr {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }
        0
    }

    /// Check memory state over a contiguous range (all blocks must match independently).
    /// Matches upstream `CheckMemoryStateContiguous`.
    pub fn check_memory_state_contiguous(
        &self,
        addr: usize,
        size: usize,
        state_mask: KMemoryState,
        state: KMemoryState,
        perm_mask: KMemoryPermission,
        perm: KMemoryPermission,
        attr_mask: KMemoryAttribute,
        attr: KMemoryAttribute,
    ) -> (u32, usize) {
        let last_addr = addr + size - 1;
        let mut blocks_needed: usize = 0;

        let mut iter = self.m_memory_block_manager.find_iterator(addr);
        let Some(first_block) = iter.next() else {
            return (
                svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                0,
            );
        };
        let mut info = first_block.get_memory_info();

        // If start address isn't aligned to block start, need a split.
        if (addr & !(PAGE_SIZE - 1)) != info.get_address() {
            blocks_needed += 1;
        }

        loop {
            let result = Self::check_memory_state_info(
                &info, state_mask, state, perm_mask, perm, attr_mask, attr,
            );
            if result != 0 {
                return (result, 0);
            }

            if last_addr <= info.get_last_address() {
                break;
            }

            let Some(next_block) = iter.next() else {
                return (
                    svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                    0,
                );
            };
            info = next_block.get_memory_info();
        }

        // If end address isn't aligned to block end, need a split.
        if ((addr + size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)) != info.get_end_address() {
            blocks_needed += 1;
        }

        (0, blocks_needed)
    }

    /// Check memory state over a range (all blocks must have the SAME state/perm/attr).
    /// Matches upstream `CheckMemoryState(out_state, out_perm, out_attr, ..., iterator, last_addr, ...)`.
    pub fn check_memory_state_range(
        &self,
        addr: usize,
        size: usize,
        state_mask: KMemoryState,
        state: KMemoryState,
        perm_mask: KMemoryPermission,
        perm: KMemoryPermission,
        attr_mask: KMemoryAttribute,
        attr: KMemoryAttribute,
        ignore_attr: KMemoryAttribute,
    ) -> (
        u32,
        Option<KMemoryState>,
        Option<KMemoryPermission>,
        Option<KMemoryAttribute>,
        usize,
    ) {
        let last_addr = addr + size - 1;
        let mut blocks_needed: usize = 0;

        let mut iter = self.m_memory_block_manager.find_iterator(addr);
        let Some(first_block) = iter.next() else {
            return (
                svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                None,
                None,
                None,
                0,
            );
        };

        // If start address isn't aligned to block start, need a split.
        if (addr & !(PAGE_SIZE - 1)) != first_block.get_address() {
            blocks_needed += 1;
        }

        let mut info = first_block.get_memory_info();
        let first_state = info.m_state;
        let first_perm = info.m_permission;
        let first_attr = info.m_attribute;

        loop {
            // All blocks must have the same state/perm/attr (ignoring ignore_attr).
            if info.m_state != first_state {
                return (
                    svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                    None,
                    None,
                    None,
                    0,
                );
            }
            if info.m_permission != first_perm {
                return (
                    svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                    None,
                    None,
                    None,
                    0,
                );
            }
            if (info.m_attribute | ignore_attr) != (first_attr | ignore_attr) {
                return (
                    svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                    None,
                    None,
                    None,
                    0,
                );
            }

            // Check against the provided masks.
            let result = Self::check_memory_state_info(
                &info, state_mask, state, perm_mask, perm, attr_mask, attr,
            );
            if result != 0 {
                return (result, None, None, None, 0);
            }

            if last_addr <= info.get_last_address() {
                break;
            }

            let Some(next_block) = iter.next() else {
                return (
                    svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                    None,
                    None,
                    None,
                    0,
                );
            };
            info = next_block.get_memory_info();
        }

        // If end address isn't aligned to block end, need a split.
        if ((last_addr & !(PAGE_SIZE - 1)) + PAGE_SIZE) != info.get_end_address() {
            blocks_needed += 1;
        }

        let out_attr = first_attr & !ignore_attr;
        (
            0,
            Some(first_state),
            Some(first_perm),
            Some(out_attr),
            blocks_needed,
        )
    }

    /// Convenience: check memory state, discarding output.
    /// Matches upstream `CheckMemoryState(addr, size, ...)`.
    pub fn check_memory_state(
        &self,
        addr: usize,
        size: usize,
        state_mask: KMemoryState,
        state: KMemoryState,
        perm_mask: KMemoryPermission,
        perm: KMemoryPermission,
        attr_mask: KMemoryAttribute,
        attr: KMemoryAttribute,
    ) -> u32 {
        let (result, _, _, _, _) = self.check_memory_state_range(
            addr,
            size,
            state_mask,
            state,
            perm_mask,
            perm,
            attr_mask,
            attr,
            Self::DEFAULT_MEMORY_IGNORE_ATTR,
        );
        result
    }

    // -- InitializeForProcess --

    /// Initialize the page table for a user process.
    /// Matches upstream `KPageTableBase::InitializeForProcess`.
    ///
    /// Computes all memory region layouts based on address space width,
    /// code address/size, and ASLR randomization.
    /// Upstream: `Result KPageTableBase::InitializeForProcess(
    ///     Svc::CreateProcessFlag as_type, bool enable_aslr, bool enable_das_merge,
    ///     bool from_back, KMemoryManager::Pool pool, KProcessAddress code_address,
    ///     size_t code_size, KSystemResource* system_resource,
    ///     KResourceLimit* resource_limit, Core::Memory::Memory& memory,
    ///     KProcessAddress aslr_space_start)`
    pub fn initialize_for_process(
        &mut self,
        as_flags: u32,
        enable_aslr: bool,
        enable_das_merge: bool,
        from_back: bool,
        pool: u32,
        code_address: usize,
        code_size: usize,
        system_resource: Option<&super::k_system_resource::KSystemResource>,
        resource_limit: Option<Arc<KResourceLimit>>,
        memory: Option<Arc<Mutex<Memory>>>,
        aslr_space_start: usize,
    ) -> u32 {
        // Store resource limit and memory references.
        // Upstream stores these for use in heap operations, mapping, etc.
        self.m_resource_limit = resource_limit;
        self.m_system = memory
            .as_ref()
            .map(|memory| memory.lock().unwrap().system_ref())
            .unwrap_or_else(SystemRef::null);
        self.m_memory = memory;
        let as_width = Self::get_address_space_width_from_flags(as_flags) as u32;
        let start: usize = 0;
        let end: usize = 1usize << as_width;

        // Validate the region.
        debug_assert!(start <= code_address);
        debug_assert!(code_address < code_address + code_size);
        debug_assert!(code_address + code_size - 1 <= end - 1);

        // Helpers for address space info lookups.
        let get_space_start = |info_type: AddressSpaceInfoType| -> usize {
            KAddressSpaceInfo::get_address_space_start(as_width as usize, info_type) as usize
        };
        let get_space_size = |info_type: AddressSpaceInfoType| -> usize {
            KAddressSpaceInfo::get_address_space_size(as_width as usize, info_type) as usize
        };

        self.m_address_space_width = as_width;
        let mut alias_region_size = get_space_size(AddressSpaceInfoType::Alias);
        let mut heap_region_size = get_space_size(AddressSpaceInfoType::Heap);

        // Adjust for 32-bit without alias.
        // Upstream: AddressSpace32BitWithoutAlias = 2 << 1 = 4
        if (as_flags & ADDRESS_SPACE_MASK)
            == CreateProcessFlag::ADDRESS_SPACE_32_BIT_WITHOUT_ALIAS.bits()
        {
            heap_region_size += alias_region_size;
            alias_region_size = 0;
        }

        // Set code regions and determine remaining sizes.
        let process_code_start: usize;
        let process_code_end: usize;
        let stack_region_size: usize;
        let kernel_map_region_size: usize;

        if as_width == 39 {
            alias_region_size = get_space_size(AddressSpaceInfoType::Alias);
            heap_region_size = get_space_size(AddressSpaceInfoType::Heap);
            stack_region_size = get_space_size(AddressSpaceInfoType::Stack);
            kernel_map_region_size = get_space_size(AddressSpaceInfoType::MapSmall);
            self.m_code_region_start = self.m_address_space_start
                + aslr_space_start
                + get_space_start(AddressSpaceInfoType::Map39Bit);
            self.m_code_region_end =
                self.m_code_region_start + get_space_size(AddressSpaceInfoType::Map39Bit);
            self.m_alias_code_region_start = self.m_code_region_start;
            self.m_alias_code_region_end = self.m_code_region_end;
            process_code_start = code_address & !(REGION_ALIGNMENT - 1);
            process_code_end =
                (code_address + code_size + REGION_ALIGNMENT - 1) & !(REGION_ALIGNMENT - 1);
        } else {
            stack_region_size = 0;
            kernel_map_region_size = 0;
            self.m_code_region_start = get_space_start(AddressSpaceInfoType::MapSmall);
            self.m_code_region_end =
                self.m_code_region_start + get_space_size(AddressSpaceInfoType::MapSmall);
            self.m_stack_region_start = self.m_code_region_start;
            self.m_alias_code_region_start = self.m_code_region_start;
            self.m_alias_code_region_end = get_space_start(AddressSpaceInfoType::MapLarge)
                + get_space_size(AddressSpaceInfoType::MapLarge);
            self.m_stack_region_end = self.m_code_region_end;
            self.m_kernel_map_region_start = self.m_code_region_start;
            self.m_kernel_map_region_end = self.m_code_region_end;
            process_code_start = self.m_code_region_start;
            process_code_end = self.m_code_region_end;
        }

        // Set other basic fields.
        self.m_enable_aslr = enable_aslr;
        self.m_enable_device_address_space_merge = enable_das_merge;
        self.m_address_space_start = start;
        self.m_address_space_end = end;
        self.m_is_kernel = false;

        // Determine the region for placing alias/heap/stack/kmap.
        let alloc_start: usize;
        let alloc_size: usize;
        if (process_code_start - self.m_code_region_start) >= (end - process_code_end) {
            alloc_start = self.m_code_region_start;
            alloc_size = process_code_start - self.m_code_region_start;
        } else {
            alloc_start = process_code_end;
            alloc_size = end - process_code_end;
        }
        let needed_size =
            alias_region_size + heap_region_size + stack_region_size + kernel_map_region_size;
        if alloc_size < needed_size {
            return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
        }
        let remaining_size = alloc_size - needed_size;

        // Determine random placements for each region.
        let (alias_rnd, heap_rnd, stack_rnd, kmap_rnd) = if enable_aslr {
            let max_units = if remaining_size / REGION_ALIGNMENT > 0 {
                remaining_size / REGION_ALIGNMENT
            } else {
                0
            };
            let gen = |max: usize| -> usize {
                if max == 0 {
                    return 0;
                }
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
                    .hash(&mut h);
                (h.finish() as usize % (max + 1)) * REGION_ALIGNMENT
            };
            (
                gen(max_units),
                gen(max_units),
                gen(max_units),
                gen(max_units),
            )
        } else {
            (0, 0, 0, 0)
        };

        // Setup alias and heap regions.
        self.m_alias_region_start = alloc_start + alias_rnd;
        self.m_alias_region_end = self.m_alias_region_start + alias_region_size;
        self.m_heap_region_start = alloc_start + heap_rnd;
        self.m_heap_region_end = self.m_heap_region_start + heap_region_size;

        if alias_rnd <= heap_rnd {
            self.m_heap_region_start += alias_region_size;
            self.m_heap_region_end += alias_region_size;
        } else {
            self.m_alias_region_start += heap_region_size;
            self.m_alias_region_end += heap_region_size;
        }

        // Setup stack region.
        if stack_region_size > 0 {
            self.m_stack_region_start = alloc_start + stack_rnd;
            self.m_stack_region_end = self.m_stack_region_start + stack_region_size;

            if alias_rnd < stack_rnd {
                self.m_stack_region_start += alias_region_size;
                self.m_stack_region_end += alias_region_size;
            } else {
                self.m_alias_region_start += stack_region_size;
                self.m_alias_region_end += stack_region_size;
            }

            if heap_rnd < stack_rnd {
                self.m_stack_region_start += heap_region_size;
                self.m_stack_region_end += heap_region_size;
            } else {
                self.m_heap_region_start += stack_region_size;
                self.m_heap_region_end += stack_region_size;
            }
        }

        // Setup kernel map region.
        if kernel_map_region_size > 0 {
            self.m_kernel_map_region_start = alloc_start + kmap_rnd;
            self.m_kernel_map_region_end = self.m_kernel_map_region_start + kernel_map_region_size;

            if alias_rnd < kmap_rnd {
                self.m_kernel_map_region_start += alias_region_size;
                self.m_kernel_map_region_end += alias_region_size;
            } else {
                self.m_alias_region_start += kernel_map_region_size;
                self.m_alias_region_end += kernel_map_region_size;
            }

            if heap_rnd < kmap_rnd {
                self.m_kernel_map_region_start += heap_region_size;
                self.m_kernel_map_region_end += heap_region_size;
            } else {
                self.m_heap_region_start += kernel_map_region_size;
                self.m_heap_region_end += kernel_map_region_size;
            }

            if stack_region_size > 0 {
                if stack_rnd < kmap_rnd {
                    self.m_kernel_map_region_start += stack_region_size;
                    self.m_kernel_map_region_end += stack_region_size;
                } else {
                    self.m_stack_region_start += kernel_map_region_size;
                    self.m_stack_region_end += kernel_map_region_size;
                }
            }
        }

        // Set heap and fill members.
        self.m_current_heap_end = self.m_heap_region_start;
        self.m_max_heap_size = 0;
        self.m_mapped_physical_memory_size = 0;
        self.m_mapped_unsafe_physical_memory = 0;
        self.m_mapped_insecure_memory = 0;
        self.m_mapped_ipc_server_memory = 0;

        self.m_heap_fill_value = MemoryFillValue::Zero;
        self.m_ipc_fill_value = MemoryFillValue::Zero;
        self.m_stack_fill_value = MemoryFillValue::Zero;

        // Set allocation option.
        let direction = if from_back {
            k_memory_manager::Direction::FromBack
        } else {
            k_memory_manager::Direction::FromFront
        };
        self.m_allocate_option = ((pool & k_memory_manager::Pool::MASK)
            << k_memory_manager::Pool::SHIFT)
            | ((direction as u32 & k_memory_manager::Direction::MASK)
                << k_memory_manager::Direction::SHIFT);

        // Validate regions inside address space.
        debug_assert!(self.m_alias_region_start >= self.m_address_space_start);
        debug_assert!(self.m_alias_region_end <= self.m_address_space_end);
        debug_assert!(self.m_heap_region_start >= self.m_address_space_start);
        debug_assert!(self.m_heap_region_end <= self.m_address_space_end);
        debug_assert!(self.m_stack_region_start >= self.m_address_space_start);
        debug_assert!(self.m_stack_region_end <= self.m_address_space_end);
        debug_assert!(self.m_kernel_map_region_start >= self.m_address_space_start);
        debug_assert!(self.m_kernel_map_region_end <= self.m_address_space_end);

        let (slab, block_info_manager) = match system_resource {
            Some(resource) => (
                Some(resource.memory_block_slab_manager_arc()),
                Some(resource.block_info_manager_arc()),
            ),
            None => {
                let kernel = crate::hle::kernel::kernel::get_kernel_ref();
                (
                    kernel.and_then(|k| k.get_memory_block_slab_manager()),
                    kernel.and_then(|k| k.get_block_info_manager()),
                )
            }
        };
        self.m_memory_block_slab_manager = slab.clone();
        self.m_block_info_manager = block_info_manager;
        let slab_ref = slab.as_deref();
        if self
            .m_memory_block_manager
            .initialize(
                self.m_address_space_start,
                self.m_address_space_end,
                slab_ref,
            )
            .is_err()
        {
            return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
        }

        log::debug!(
            "KPageTableBase::InitializeForProcess: width={}, code=[{:#x}..{:#x}], \
             alias_code=[{:#x}..{:#x}], heap=[{:#x}..{:#x}], alias=[{:#x}..{:#x}], \
             stack=[{:#x}..{:#x}], kmap=[{:#x}..{:#x}]",
            as_width,
            self.m_code_region_start,
            self.m_code_region_end,
            self.m_alias_code_region_start,
            self.m_alias_code_region_end,
            self.m_heap_region_start,
            self.m_heap_region_end,
            self.m_alias_region_start,
            self.m_alias_region_end,
            self.m_stack_region_start,
            self.m_stack_region_end,
            self.m_kernel_map_region_start,
            self.m_kernel_map_region_end,
        );

        0 // success
    }

    // -- Memory / PageTable wiring --

    /// Set the Memory bridge and initialize the page table implementation.
    /// Must be called after InitializeForProcess.
    pub fn set_memory(&mut self, memory: Arc<Mutex<Memory>>) {
        self.m_system = memory.lock().unwrap().system_ref();
        self.m_memory = Some(memory);
    }

    /// Initialize the Common::PageTable impl.
    /// Matches upstream: `m_impl = make_unique<PageTable>(); m_impl->Resize(width, PageBits)`
    pub fn initialize_impl(&mut self) {
        let mut pt = Box::new(common::page_table::PageTable::new());
        pt.resize(self.m_address_space_width as usize, PAGE_BITS);
        self.m_impl = Some(pt);
    }

    /// Get the inner page table (for dynarmic).
    pub fn get_impl(&self) -> Option<&common::page_table::PageTable> {
        self.m_impl.as_deref()
    }

    pub fn get_impl_mut(&mut self) -> Option<&mut common::page_table::PageTable> {
        self.m_impl.as_deref_mut()
    }

    // -- Operate --

    /// Convert KMemoryPermission to Common::MemoryPermission.
    /// Matches upstream `ConvertToMemoryPermission`.
    fn convert_to_memory_permission(
        perm: KMemoryPermission,
    ) -> crate::memory::memory::MemoryPermission {
        use crate::memory::memory::MemoryPermission;
        let mut perms = MemoryPermission::empty();
        if perm.contains(KMemoryPermission::USER_READ) {
            perms |= MemoryPermission::READ;
        }
        if perm.contains(KMemoryPermission::USER_WRITE) {
            perms |= MemoryPermission::WRITE;
        }
        perms
    }

    /// Core page table operation — maps, unmaps, or changes permissions.
    /// Matches upstream `KPageTableBase::Operate(PageLinkedList*, virt_addr, num_pages,
    /// phys_addr, is_pa_valid, properties, operation, reuse_ll)`.
    ///
    /// In the emulator, we don't use PageLinkedList (no guest page table entries).
    /// The operation modifies Core::Memory::Memory which updates the dynarmic PageTable.
    /// Page-table operate (single phys variant). Mirrors upstream:
    ///
    /// ```cpp
    /// Result Operate(PageLinkedList* page_list, KProcessAddress virt_addr,
    ///                size_t num_pages, KPhysicalAddress phys_addr,
    ///                bool is_pa_valid, KPageProperties properties,
    ///                OperationType operation, bool reuse_ll);
    /// ```
    ///
    /// The `page_list` parameter accumulates page-table pages that the
    /// operation has freed, for `FinalizeUpdate` to drain. Ruzu's flat page
    /// table never produces such pages today, so callers
    /// can pass `None` if they don't have a `KScopedPageTableUpdater` in
    /// scope; the principal phys-handlers pass `Some(updater.page_list())`
    /// matching upstream's call shape.
    pub fn operate(
        &mut self,
        _page_list: Option<&mut PageLinkedList>,
        virt_addr: usize,
        num_pages: usize,
        phys_addr: u64,
        _is_pa_valid: bool,
        properties: KPageProperties,
        operation: OperationType,
    ) -> u32 {
        debug_assert!(num_pages > 0);
        debug_assert!(virt_addr % PAGE_SIZE == 0);

        let size = num_pages * PAGE_SIZE;

        match operation {
            OperationType::Unmap | OperationType::UnmapPhysical => {
                let separate_heap = matches!(operation, OperationType::UnmapPhysical);
                let mut pages_to_close =
                    super::k_page_group::KPageGroup::with_block_info_manager(
                        self.m_block_info_manager.clone(),
                    );
                let close_pages = self.make_page_group(&mut pages_to_close, virt_addr, num_pages);
                if close_pages != 0 {
                    return close_pages;
                }

                if let (Some(memory), Some(impl_pt)) = (&self.m_memory, &mut self.m_impl) {
                    memory.lock().unwrap().unmap_region(
                        impl_pt,
                        virt_addr as u64,
                        size as u64,
                        separate_heap,
                    );
                }
                pages_to_close.close_and_reset();

                0 // success
            }
            OperationType::Map => {
                debug_assert!(virt_addr != 0);

                if let (Some(memory), Some(impl_pt)) = (&self.m_memory, &mut self.m_impl) {
                    memory.lock().unwrap().map_memory_region(
                        impl_pt,
                        virt_addr as u64,
                        size as u64,
                        phys_addr,
                        Self::convert_to_memory_permission(properties.perm),
                        false,
                    );
                }

                if let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut() {
                    let memory_manager = kernel.memory_manager_mut();
                    if memory_manager.contains_physical_address(phys_addr) {
                        memory_manager.open(phys_addr, num_pages);
                    }
                }

                0
            }
            OperationType::Separate => {
                // No-op in emulator.
                0
            }
            OperationType::MapGroup
            | OperationType::MapFirstGroup
            | OperationType::MapFirstGroupPhysical => {
                // The page-group flavor of Operate is invoked through the
                // dedicated `operate_with_group` entry point; if a caller
                // arrives here with the single-phys signature, it indicates
                // a port mismatch.
                log::error!(
                    "KPageTableBase::operate: {:?} requires page-group entry; \
                     call operate_with_group instead",
                    operation
                );
                svc_results::RESULT_INVALID_STATE.get_inner_value()
            }
            OperationType::ChangePermissions
            | OperationType::ChangePermissionsAndRefresh
            | OperationType::ChangePermissionsAndRefreshAndFlush => {
                if let (Some(memory), Some(impl_pt)) = (&self.m_memory, &mut self.m_impl) {
                    memory.lock().unwrap().protect_region(
                        impl_pt,
                        virt_addr as u64,
                        size as u64,
                        Self::convert_to_memory_permission(properties.perm),
                    );
                }

                0
            }
        }
    }

    /// Page-group flavor of `Operate`. Maps the blocks in `page_group` at
    /// consecutive virtual addresses starting at `virt_addr`, optionally
    /// re-opening references to each page so the kernel keeps the group
    /// alive across the mapping's lifetime.
    ///
    /// Port of upstream's second `Operate` overload taking `const KPageGroup&`
    /// (k_page_table_base.cpp). Mirrors the upstream switch:
    ///
    /// ```cpp
    /// case OperationType::MapGroup:
    /// case OperationType::MapFirstGroup:
    /// case OperationType::MapFirstGroupPhysical: {
    ///     const bool separate_heap = operation == MapFirstGroupPhysical;
    ///     KScopedPageGroup spg(page_group, operation == MapGroup);
    ///     for (const auto& node : page_group) {
    ///         m_memory->MapMemoryRegion(...);
    ///         virt_addr += node.GetSize();
    ///     }
    ///     spg.CancelClose();
    /// }
    /// ```
    pub fn operate_with_group(
        &mut self,
        _page_list: Option<&mut PageLinkedList>,
        virt_addr: usize,
        num_pages: usize,
        page_group: &super::k_page_group::KPageGroup,
        properties: KPageProperties,
        operation: OperationType,
    ) -> u32 {
        debug_assert!(num_pages > 0);
        debug_assert!(virt_addr % PAGE_SIZE == 0);
        debug_assert_eq!(num_pages, page_group.get_num_pages());
        match operation {
            OperationType::MapGroup
            | OperationType::MapFirstGroup
            | OperationType::MapFirstGroupPhysical => {}
            _ => {
                log::error!(
                    "operate_with_group: {:?} is not a page-group operation",
                    operation
                );
                return svc_results::RESULT_INVALID_STATE.get_inner_value();
            }
        }

        let separate_heap = matches!(operation, OperationType::MapFirstGroupPhysical);
        // Maintain a new reference to every page in the group. Upstream's
        // KScopedPageGroup is RAII; we mirror it manually so we can `cancel_close`
        // on the success path.
        let not_first = matches!(operation, OperationType::MapGroup);
        let mut spg = super::k_page_group::KScopedPageGroup::new(page_group, not_first);

        let mut cur_va = virt_addr as u64;
        for node in page_group.iter() {
            let block_size = (node.get_num_pages() * PAGE_SIZE) as u64;
            if let (Some(memory), Some(impl_pt)) = (&self.m_memory, &mut self.m_impl) {
                memory.lock().unwrap().map_memory_region(
                    impl_pt,
                    cur_va,
                    block_size,
                    node.get_address(),
                    Self::convert_to_memory_permission(properties.perm),
                    separate_heap,
                );
            }
            cur_va += block_size;
        }

        // Persist the references — the mapping owns them now.
        spg.cancel_close();
        0
    }

    /// Matches upstream `KPageTableBase::AllocateAndMapPagesImpl`.
    fn allocate_and_map_pages_impl(
        &mut self,
        page_list: Option<&mut PageLinkedList>,
        address: usize,
        num_pages: usize,
        perm: KMemoryPermission,
    ) -> u32 {
        let alloc_option = self.m_allocate_option;
        let mut pg = super::k_page_group::KPageGroup::with_block_info_manager(
            self.m_block_info_manager.clone(),
        );
        {
            let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut() else {
                return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
            };
            let rc =
                kernel
                    .memory_manager_mut()
                    .allocate_and_open(&mut pg, num_pages, alloc_option);
            if rc != 0 {
                return rc;
            }
        }

        struct PgCloseGuard<'a> {
            pg: &'a mut super::k_page_group::KPageGroup,
        }
        impl Drop for PgCloseGuard<'_> {
            fn drop(&mut self) {
                self.pg.close();
            }
        }
        let pg_guard = PgCloseGuard { pg: &mut pg };

        for block in pg_guard.pg.iter() {
            self.clear_fresh_backing_region_phys(block.get_address(), block.get_size());
        }

        let properties = KPageProperties {
            perm,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        self.operate_with_group(
            page_list,
            address,
            num_pages,
            pg_guard.pg,
            properties,
            OperationType::MapGroup,
        )
    }

    /// Allocator-aware shorthand for `m_memory_block_manager.update(...)`.
    /// Constructs a `KMemoryBlockManagerUpdateAllocator` inline (matching
    /// upstream's stack-local `KMemoryBlockManagerUpdateAllocator allocator`
    /// pattern), drives the update, and drops the allocator so unused/
    /// coalesced nodes return to the slab.
    pub fn update_blocks(
        &mut self,
        address: usize,
        num_pages: usize,
        state: KMemoryState,
        perm: KMemoryPermission,
        attr: KMemoryAttribute,
        set_disable_attr: KMemoryBlockDisableMergeAttribute,
        clear_disable_attr: KMemoryBlockDisableMergeAttribute,
    ) {
        match self.make_block_update_allocator(
            super::k_dynamic_resource_manager::KMemoryBlockManagerUpdateAllocator::MAX_BLOCKS,
        ) {
            Ok(mut alloc) => {
                self.m_memory_block_manager.update_with_allocator(
                    Some(&mut alloc),
                    address,
                    num_pages,
                    state,
                    perm,
                    attr,
                    set_disable_attr,
                    clear_disable_attr,
                );
            }
            Err(_) => {
                // Slab not initialized yet (very early boot). Fall back to
                // the heap-allocated path so existing call sites don't fail.
                self.m_memory_block_manager.update(
                    address,
                    num_pages,
                    state,
                    perm,
                    attr,
                    set_disable_attr,
                    clear_disable_attr,
                );
            }
        }
    }

    fn update_blocks_with_allocator(
        &mut self,
        allocator: &mut super::k_dynamic_resource_manager::KMemoryBlockManagerUpdateAllocator,
        address: usize,
        num_pages: usize,
        state: KMemoryState,
        perm: KMemoryPermission,
        attr: KMemoryAttribute,
        set_disable_attr: KMemoryBlockDisableMergeAttribute,
        clear_disable_attr: KMemoryBlockDisableMergeAttribute,
    ) {
        self.m_memory_block_manager.update_with_allocator(
            Some(allocator),
            address,
            num_pages,
            state,
            perm,
            attr,
            set_disable_attr,
            clear_disable_attr,
        );
    }

    /// Construct a `KMemoryBlockManagerUpdateAllocator` pre-reserving
    /// `num_blocks` slab nodes. Returns the allocator on success or a
    /// `RESULT_OUT_OF_RESOURCE` code if the kernel-wide slab can't satisfy
    /// the request — matching upstream's behavior where the same
    /// allocator constructor signals failure via `out_result`.
    ///
    /// Defaults to `MAX_BLOCKS` (= 2 in upstream) so each update can absorb
    /// the worst-case two splits.
    pub fn make_block_update_allocator(
        &self,
        num_blocks: usize,
    ) -> Result<super::k_dynamic_resource_manager::KMemoryBlockManagerUpdateAllocator, u32> {
        let slab = match self.m_memory_block_slab_manager.clone() {
            Some(s) => s,
            None => {
                log::error!(
                    "make_block_update_allocator: page table has no block slab manager"
                );
                return Err(svc_results::RESULT_OUT_OF_RESOURCE.get_inner_value());
            }
        };
        let (allocator, rc) =
            super::k_dynamic_resource_manager::KMemoryBlockManagerUpdateAllocator::new(
                slab, num_blocks,
            );
        if rc != 0 {
            return Err(rc);
        }
        Ok(allocator)
    }

    /// Drain the deferred-free list at the end of a page-table update.
    ///
    /// Port of upstream `KPageTableBase::FinalizeUpdate`
    /// (k_page_table_base.cpp:5787):
    ///
    /// ```cpp
    /// while (page_list->Peek()) {
    ///     auto page = page_list->Pop();
    ///     // ASSERT(this->GetPageTableManager().IsInPageTableHeap(page));
    ///     // ASSERT(this->GetPageTableManager().GetRefCount(page) == 0);
    ///     // this->GetPageTableManager().Free(page);
    /// }
    /// ```
    ///
    /// Current Eden only drains this list; freeing page-table entries remains
    /// commented out until guest-memory page-table allocation is enabled.
    pub fn finalize_update(&mut self, page_list: &mut PageLinkedList) {
        while page_list.pop().is_some() {
        }
    }

    // -- FindFreeArea --

    /// Find a free area in the given region.
    /// Matches upstream `KPageTableBase::FindFreeArea`.
    ///
    /// If ASLR is enabled, tries up to 8 random placements first,
    /// then falls back to a random-offset linear scan, then a plain linear scan.
    pub fn find_free_area(
        &self,
        region_start: usize,
        region_num_pages: usize,
        num_pages: usize,
        alignment: usize,
        offset: usize,
        guard_pages: usize,
    ) -> usize {
        let mut address: usize = 0;

        if num_pages <= region_num_pages {
            if self.m_enable_aslr {
                // Try to directly find a free area up to 8 times.
                let max_offset_pages = region_num_pages
                    .saturating_sub(num_pages)
                    .saturating_sub(guard_pages);

                for _ in 0..8 {
                    if max_offset_pages == 0 || alignment == 0 {
                        break;
                    }
                    let random_offset = {
                        use std::collections::hash_map::DefaultHasher;
                        use std::hash::{Hash, Hasher};
                        let mut h = DefaultHasher::new();
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos()
                            .hash(&mut h);
                        (h.finish() as usize % ((max_offset_pages * PAGE_SIZE / alignment) + 1))
                            * alignment
                    };
                    let candidate = ((region_start + random_offset) & !(alignment - 1)) + offset;

                    if let Some(info) = self.m_memory_block_manager.query_info(candidate) {
                        if info.m_state != KMemoryState::FREE {
                            continue;
                        }
                        if region_start > candidate {
                            continue;
                        }
                        if info.get_address() + guard_pages * PAGE_SIZE > candidate {
                            continue;
                        }
                        if candidate + (num_pages + guard_pages) * PAGE_SIZE - 1
                            > info.get_last_address()
                        {
                            continue;
                        }
                        if candidate + (num_pages + guard_pages) * PAGE_SIZE - 1
                            > region_start + region_num_pages * PAGE_SIZE - 1
                        {
                            continue;
                        }
                        address = candidate;
                        break;
                    }
                }

                // Fall back to finding free area with random offset.
                if address == 0 {
                    let offset_pages = {
                        use std::collections::hash_map::DefaultHasher;
                        use std::hash::{Hash, Hasher};
                        let mut h = DefaultHasher::new();
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos()
                            .hash(&mut h);
                        h.finish() as usize % (max_offset_pages + 1)
                    };
                    address = self
                        .m_memory_block_manager
                        .find_free_area(
                            region_start + offset_pages * PAGE_SIZE,
                            region_num_pages.saturating_sub(offset_pages),
                            num_pages,
                            alignment,
                            offset,
                            guard_pages,
                        )
                        .unwrap_or(0);
                }
            }
            // Find the first free area (no ASLR or fallback).
            if address == 0 {
                address = self
                    .m_memory_block_manager
                    .find_free_area(
                        region_start,
                        region_num_pages,
                        num_pages,
                        alignment,
                        offset,
                        guard_pages,
                    )
                    .unwrap_or(0);
            }
        }

        address
    }

    // -- SetMaxHeapSize / SetHeapSize --

    /// Matches upstream `KPageTableBase::SetMaxHeapSize`.
    pub fn set_max_heap_size(&mut self, size: usize) -> u32 {
        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        debug_assert!(!self.m_is_kernel);
        self.m_max_heap_size = size;
        0
    }

    /// Matches upstream `KPageTableBase::SetHeapSize`.
    /// Faithfully implements heap grow/shrink with Operate calls.
    pub fn set_heap_size(&mut self, size: usize) -> (u32, usize) {
        use super::k_scoped_resource_reservation::KScopedResourceReservation;

        let physical_memory_lock = self.m_map_physical_memory_lock.clone();
        let _physical_memory_lock_guard =
            super::k_light_lock::KScopedLightLock::new(physical_memory_lock.as_ref());

        let (cur_address, allocation_size) = {
            let general_lock = self.m_general_lock.clone();
            let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

            if self.m_is_kernel {
                return (svc_results::RESULT_OUT_OF_MEMORY.get_inner_value(), 0);
            }
            if size > (self.m_heap_region_end - self.m_heap_region_start) {
                return (svc_results::RESULT_OUT_OF_MEMORY.get_inner_value(), 0);
            }
            if size > self.m_max_heap_size {
                return (svc_results::RESULT_OUT_OF_MEMORY.get_inner_value(), 0);
            }

            let current_heap_size = self.m_current_heap_end - self.m_heap_region_start;
            if size < current_heap_size {
                let free_start = self.m_heap_region_start + size;
                let free_size = current_heap_size - size;
                let (result, _, _, _, num_allocator_blocks) = self.check_memory_state_range(
                    free_start,
                    free_size,
                    KMemoryState::ALL,
                    KMemoryState::NORMAL,
                    KMemoryPermission::from_bits_truncate(0xFF),
                    KMemoryPermission::USER_READ_WRITE,
                    KMemoryAttribute::from_bits_truncate(0xFF),
                    KMemoryAttribute::NONE,
                    KMemoryAttribute::NONE,
                );
                if result != 0 {
                    return (result, 0);
                }

                let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
                    Ok(allocator) => allocator,
                    Err(rc) => return (rc, 0),
                };
                let mut updater = KScopedPageTableUpdater::from_mut(self);

                let num_pages = free_size / PAGE_SIZE;
                let unmap_properties = KPageProperties {
                    perm: KMemoryPermission::NONE,
                    io: false,
                    uncached: false,
                    disable_merge_attributes: DisableMergeAttribute::NONE,
                };
                let op_result = self.operate(
                    Some(updater.page_list()),
                    free_start,
                    num_pages,
                    0,
                    false,
                    unmap_properties,
                    OperationType::Unmap,
                );
                if op_result != 0 {
                    return (op_result, 0);
                }

                if let Some(ref resource_limit) = self.m_resource_limit {
                    resource_limit
                        .release(LimitableResource::PhysicalMemoryMax, free_size as i64);
                }

                self.m_memory_block_manager.update_with_allocator(
                    Some(&mut allocator),
                    free_start,
                    num_pages,
                    KMemoryState::FREE,
                    KMemoryPermission::NONE,
                    KMemoryAttribute::NONE,
                    KMemoryBlockDisableMergeAttribute::NONE,
                    if size == 0 {
                        KMemoryBlockDisableMergeAttribute::NORMAL
                    } else {
                        KMemoryBlockDisableMergeAttribute::NONE
                    },
                );

                self.m_current_heap_end = self.m_heap_region_start + size;
                return (0, self.m_heap_region_start);
            } else if size == current_heap_size {
                return (0, self.m_heap_region_start);
            }

            (self.m_current_heap_end, size - current_heap_size)
        };

        let mut memory_reservation = KScopedResourceReservation::new(
            self.m_resource_limit.clone(),
            LimitableResource::PhysicalMemoryMax,
            allocation_size as i64,
        );
        if !memory_reservation.succeeded() {
            return (svc_results::RESULT_LIMIT_REACHED.get_inner_value(), 0);
        }

        let num_pages = allocation_size / PAGE_SIZE;

        // Allocate pages for the heap extension as a KPageGroup. Upstream:
        //
        //   KPageGroup pg(m_kernel, m_block_info_manager);
        //   R_TRY(m_kernel.MemoryManager().AllocateAndOpen(
        //       std::addressof(pg), allocation_size / PageSize, m_allocate_option));
        let alloc_option = self.m_allocate_option;
        let mut pg = super::k_page_group::KPageGroup::with_block_info_manager(
            self.m_block_info_manager.clone(),
        );
        {
            let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut() else {
                return (svc_results::RESULT_OUT_OF_MEMORY.get_inner_value(), 0);
            };
            let rc =
                kernel
                    .memory_manager_mut()
                    .allocate_and_open(&mut pg, num_pages, alloc_option);
            if rc != 0 {
                return (rc, 0);
            }
        }

        // RAII close the opened pages when we're done — if the mapping
        // succeeds, operate(MapGroup) opens an extra reference (via
        // KScopedPageGroup with not_first=true), so dropping `pg` brings
        // refcount back to 1 and the mapping's reference keeps the pages
        // alive. On failure path, dropping `pg` also frees the pages.
        struct PgCloseGuard<'a> {
            pg: &'a mut super::k_page_group::KPageGroup,
            armed: bool,
        }
        impl<'a> Drop for PgCloseGuard<'a> {
            fn drop(&mut self) {
                if self.armed {
                    self.pg.close();
                }
            }
        }
        let pg_guard = PgCloseGuard {
            pg: &mut pg,
            armed: true,
        };

        // Clear all the newly allocated pages. Upstream calls
        // ClearBackingRegion(...) per block; we do the same — operate on
        // PHYSICAL pages so it's correct even before the VA mapping exists.
        for block in pg_guard.pg.iter() {
            self.clear_fresh_backing_region_phys(block.get_address(), block.get_size());
        }

        {
            let general_lock = self.m_general_lock.clone();
            let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

            debug_assert_eq!(cur_address, self.m_current_heap_end);

            let (result, _, _, _, num_allocator_blocks) = self.check_memory_state_range(
                self.m_current_heap_end,
                allocation_size,
                KMemoryState::ALL,
                KMemoryState::FREE,
                KMemoryPermission::NONE,
                KMemoryPermission::NONE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
            );
            if result != 0 {
                return (result, 0);
            }

            let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
                Ok(allocator) => allocator,
                Err(rc) => return (rc, 0),
            };
            let mut updater = KScopedPageTableUpdater::from_mut(self);

            let map_properties = KPageProperties {
                perm: KMemoryPermission::USER_READ_WRITE,
                io: false,
                uncached: false,
                disable_merge_attributes: if self.m_current_heap_end == self.m_heap_region_start {
                    DisableMergeAttribute::DISABLE_HEAD
                } else {
                    DisableMergeAttribute::NONE
                },
            };

            let op_result = self.operate_with_group(
                Some(updater.page_list()),
                self.m_current_heap_end,
                num_pages,
                pg_guard.pg,
                map_properties,
                OperationType::MapGroup,
            );
            if op_result != 0 {
                return (op_result, 0);
            }

            memory_reservation.commit();

            self.m_memory_block_manager.update_with_allocator(
                Some(&mut allocator),
                self.m_current_heap_end,
                num_pages,
                KMemoryState::NORMAL,
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::NONE,
                if self.m_heap_region_start == self.m_current_heap_end {
                    KMemoryBlockDisableMergeAttribute::NORMAL
                } else {
                    KMemoryBlockDisableMergeAttribute::NONE
                },
                KMemoryBlockDisableMergeAttribute::NONE,
            );

            self.m_current_heap_end = self.m_heap_region_start + size;
        }

        // RUZU_TRACE_HEAP_GROW=1 — log heap-region bounds + the just-grown
        // range so we can correlate later "Unmapped Read" addresses
        // against what set_heap_size actually mapped.
        if std::env::var_os("RUZU_TRACE_HEAP_GROW").is_some() {
            eprintln!(
                "[HEAP_GROW] region=[0x{:016X}..0x{:016X}) current_end=0x{:016X} grew=[0x{:016X}..0x{:016X}) size={}MB",
                self.m_heap_region_start,
                self.m_heap_region_end,
                self.m_current_heap_end,
                cur_address,
                cur_address + allocation_size,
                allocation_size / (1024 * 1024),
            );
        }
        // Drop the close guard explicitly so its Close() runs while we still
        // hold the borrow chain we want — match upstream SCOPE_EXIT semantics.
        drop(pg_guard);
        (0, self.m_heap_region_start)
    }

    // -- QueryInfo --

    /// Query memory info at an address.
    /// Port of upstream `KPageTableBase::QueryInfoImpl`.
    fn query_info_impl(&self, addr: usize) -> Option<(KMemoryInfo, PageInfo)> {
        let info = self.m_memory_block_manager.query_info(addr)?;
        Some((info, PageInfo { flags: 0 }))
    }

    /// Query memory info and page info at an address.
    /// Matches upstream `KPageTableBase::QueryInfo`.
    pub fn query_info_with_page_info(&self, addr: usize) -> Option<(KMemoryInfo, PageInfo)> {
        if !self.contains(addr) {
            return Some((
                KMemoryInfo {
                    m_address: self.m_address_space_end,
                    m_size: 0usize.wrapping_sub(self.m_address_space_end),
                    m_state: KMemoryState::INACCESSIBLE,
                    m_device_disable_merge_left_count: 0,
                    m_device_disable_merge_right_count: 0,
                    m_ipc_lock_count: 0,
                    m_device_use_count: 0,
                    m_ipc_disable_merge_count: 0,
                    m_permission: KMemoryPermission::NONE,
                    m_attribute: KMemoryAttribute::NONE,
                    m_original_permission: KMemoryPermission::NONE,
                    m_disable_merge_attribute: KMemoryBlockDisableMergeAttribute::NONE,
                },
                PageInfo { flags: 0 },
            ));
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());
        self.query_info_impl(addr)
    }

    /// Query memory info at an address.
    /// Compatibility wrapper for owners that do not need upstream `PageInfo`.
    pub fn query_info(&self, addr: usize) -> Option<KMemoryInfo> {
        self.query_info_with_page_info(addr).map(|(info, _)| info)
    }

    // -- SetMemoryPermission --

    /// Set memory permission for a range.
    /// Matches upstream `KPageTableBase::SetMemoryPermission`.
    pub fn set_memory_permission(
        &mut self,
        addr: usize,
        size: usize,
        perm: KMemoryPermission,
    ) -> u32 {
        let num_pages = size / PAGE_SIZE;
        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        // Check current memory state — verify the region can be reprotected.
        // Upstream: state_mask = FlagCanReprotect, state = FlagCanReprotect
        let (result, out_state, _out_perm, _out_attr, num_allocator_blocks) = self
            .check_memory_state_range(
                addr,
                size,
                KMemoryState::FLAG_CAN_REPROTECT,
                KMemoryState::FLAG_CAN_REPROTECT,
                KMemoryPermission::NONE,
                KMemoryPermission::NONE,
                KMemoryAttribute::from_bits_truncate(0xFF),
                KMemoryAttribute::NONE,
                Self::DEFAULT_MEMORY_IGNORE_ATTR,
            );
        if result != 0 {
            return result;
        }

        let old_state = out_state.unwrap_or(KMemoryState::NORMAL);
        let old_perm = _out_perm.unwrap_or(KMemoryPermission::NONE);

        // If perm is already the same, nothing to do.
        if old_perm == perm {
            return 0;
        }

        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Change permissions via Operate.
        let properties = KPageProperties {
            perm,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        let op_result = self.operate(
            Some(updater.page_list()),
            addr,
            num_pages,
            0,
            false,
            properties,
            OperationType::ChangePermissions,
        );
        if op_result != 0 {
            return op_result;
        }

        // Update block manager.
        self.update_blocks_with_allocator(
            &mut allocator,
            addr,
            num_pages,
            old_state,
            perm,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        0
    }

    // -- SetMemoryAttribute --

    /// Set memory attribute for a range.
    /// Matches upstream `KPageTableBase::SetMemoryAttribute`.
    pub fn set_memory_attribute(&mut self, addr: usize, size: usize, mask: u32, attr: u32) -> u32 {
        let num_pages = size / PAGE_SIZE;
        let mask_attr = KMemoryAttribute::from_bits_truncate(mask as u8);
        let set_attr = KMemoryAttribute::from_bits_truncate(attr as u8);
        debug_assert_eq!(
            mask_attr.bits() | KMemoryAttribute::SET_MASK.bits(),
            KMemoryAttribute::SET_MASK.bits()
        );

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        // Compute state test mask based on which attributes are being changed.
        let mut state_test_mask = KMemoryState::NONE;
        if mask_attr.contains(KMemoryAttribute::UNCACHED) {
            state_test_mask = KMemoryState::from_bits_truncate(
                state_test_mask.bits() | KMemoryState::FLAG_CAN_CHANGE_ATTRIBUTE.bits(),
            );
        }
        if mask_attr.contains(KMemoryAttribute::PERMISSION_LOCKED) {
            state_test_mask = KMemoryState::from_bits_truncate(
                state_test_mask.bits() | KMemoryState::FLAG_CAN_PERMISSION_LOCK.bits(),
            );
        }

        // Check current state.
        let attr_test_mask = KMemoryAttribute::from_bits_truncate(
            !(KMemoryAttribute::SET_MASK.bits() | KMemoryAttribute::DEVICE_SHARED.bits()),
        );
        let ignore_attr = KMemoryAttribute::from_bits_truncate(!attr_test_mask.bits());
        let (result, _out_state, out_perm, out_attr, num_allocator_blocks) = self
            .check_memory_state_range(
                addr,
                size,
                state_test_mask,
                state_test_mask,
                KMemoryPermission::NONE,
                KMemoryPermission::NONE,
                attr_test_mask,
                KMemoryAttribute::NONE,
                ignore_attr,
            );
        if result != 0 {
            return result;
        }

        let old_perm = out_perm.unwrap_or(KMemoryPermission::NONE);
        let old_attr = out_attr.unwrap_or(KMemoryAttribute::NONE);

        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // If uncached attribute is changing, change via Operate.
        if mask_attr.contains(KMemoryAttribute::UNCACHED) {
            let new_attr = KMemoryAttribute::from_bits_truncate(
                (old_attr.bits() & !mask_attr.bits()) | (set_attr.bits() & mask_attr.bits()),
            );
            let properties = KPageProperties {
                perm: old_perm,
                io: false,
                uncached: new_attr.contains(KMemoryAttribute::UNCACHED),
                disable_merge_attributes: DisableMergeAttribute::NONE,
            };
            let op_result = self.operate(
                Some(updater.page_list()),
                addr,
                num_pages,
                0,
                false,
                properties,
                OperationType::ChangePermissionsAndRefreshAndFlush,
            );
            if op_result != 0 {
                return op_result;
            }
        }

        // Update the blocks via UpdateAttribute.
        self.m_memory_block_manager.update_attribute_with_allocator(
            Some(&mut allocator),
            addr,
            num_pages,
            mask_attr,
            set_attr,
        );
        0
    }

    // -- MapMemory / UnmapMemory (stack mirror) --

    /// Map memory (stack mirror): copies src page group to dst, reprotects src.
    /// Matches upstream `KPageTableBase::MapMemory`.
    pub fn map_memory(&mut self, dst: usize, src: usize, size: usize) -> u32 {
        let num_pages = size / PAGE_SIZE;
        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        // Check source state — must be aliasable and UserReadWrite.
        let (result, out_src_state, _, _, num_src_allocator_blocks) = self
            .check_memory_state_range(
                src,
                size,
                KMemoryState::FLAG_CAN_ALIAS,
                KMemoryState::FLAG_CAN_ALIAS,
                KMemoryPermission::from_bits_truncate(0xFF),
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::from_bits_truncate(0xFF),
                KMemoryAttribute::NONE,
                Self::DEFAULT_MEMORY_IGNORE_ATTR,
            );
        if result != 0 {
            return result;
        }

        // Check destination state — must be Free.
        let (result, _, _, _, num_dst_allocator_blocks) = self.check_memory_state_range(
            dst,
            size,
            KMemoryState::ALL,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryAttribute::NONE,
            Self::DEFAULT_MEMORY_IGNORE_ATTR,
        );
        if result != 0 {
            return result;
        }

        let src_state = out_src_state.unwrap_or(KMemoryState::NORMAL);
        let mut src_allocator = match self.make_block_update_allocator(num_src_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(result) => return result,
        };
        let mut dst_allocator = match self.make_block_update_allocator(num_dst_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(result) => return result,
        };

        // Build the source page group first, matching upstream
        // `MakePageGroup(pg, src_address, num_pages)` before any permission change.
        let mut pg = super::k_page_group::KPageGroup::with_block_info_manager(
            self.m_block_info_manager.clone(),
        );
        let make_pg_result = self.make_page_group(&mut pg, src, num_pages);
        if make_pg_result != 0 {
            return make_pg_result;
        }

        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Reprotect source as KernelRead | NotMapped.
        let src_new_perm = KMemoryPermission::from_bits_truncate(
            KMemoryPermission::KERNEL_READ.bits() | KMemoryPermission::NOT_MAPPED.bits(),
        );
        let src_props = KPageProperties {
            perm: src_new_perm,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD_BODY_TAIL,
        };
        let op_result = self.operate(
            Some(updater.page_list()),
            src,
            num_pages,
            0,
            false,
            src_props,
            OperationType::ChangePermissions,
        );
        if op_result != 0 {
            return op_result;
        }

        let dst_props = KPageProperties {
            perm: KMemoryPermission::USER_READ_WRITE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD,
        };
        let op_result = self.map_page_group_impl(Some(updater.page_list()), dst, &pg, dst_props);
        if op_result != 0 {
            // Revert source on failure.
            let revert_props = KPageProperties {
                perm: KMemoryPermission::USER_READ_WRITE,
                io: false,
                uncached: false,
                disable_merge_attributes: DisableMergeAttribute::ENABLE_HEAD_BODY_TAIL,
            };
            let _ = self.operate(
                Some(updater.page_list()),
                src,
                num_pages,
                0,
                false,
                revert_props,
                OperationType::ChangePermissions,
            );
            return op_result;
        }

        // Update source block.
        self.update_blocks_with_allocator(
            &mut src_allocator,
            src,
            num_pages,
            src_state,
            src_new_perm,
            KMemoryAttribute::LOCKED,
            KMemoryBlockDisableMergeAttribute::LOCKED,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        // Update destination block.
        self.update_blocks_with_allocator(
            &mut dst_allocator,
            dst,
            num_pages,
            KMemoryState::STACK,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        0
    }

    /// Unmap memory (undo stack mirror).
    /// Matches upstream `KPageTableBase::UnmapMemory`.
    pub fn unmap_memory(&mut self, dst: usize, src: usize, size: usize) -> u32 {
        let num_pages = size / PAGE_SIZE;
        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        // Check source is locked/aliased and capture original state.
        let (result, out_src_state, _, _, num_src_allocator_blocks) = self
            .check_memory_state_range(
                src,
                size,
                KMemoryState::FLAG_CAN_ALIAS,
                KMemoryState::FLAG_CAN_ALIAS,
                KMemoryPermission::from_bits_truncate(0xFF),
                KMemoryPermission::from_bits_truncate(
                    KMemoryPermission::NOT_MAPPED.bits() | KMemoryPermission::KERNEL_READ.bits(),
                ),
                KMemoryAttribute::from_bits_truncate(0xFF),
                KMemoryAttribute::LOCKED,
                Self::DEFAULT_MEMORY_IGNORE_ATTR,
            );
        if result != 0 {
            log::error!(
                "KPageTableBase::UnmapMemory source-state check failed, dst=0x{:X}, src=0x{:X}, size=0x{:X}, result=0x{:08X}",
                dst,
                src,
                size,
                result
            );
            return result;
        }
        let src_state = out_src_state.unwrap_or(KMemoryState::NORMAL);

        // Check destination is Stack.
        let (result, _, _out_dst_perm, _, num_dst_allocator_blocks) = self
            .check_memory_state_range(
                dst,
                size,
                KMemoryState::ALL,
                KMemoryState::STACK,
                KMemoryPermission::NONE,
                KMemoryPermission::NONE,
                KMemoryAttribute::from_bits_truncate(0xFF),
                KMemoryAttribute::NONE,
                Self::DEFAULT_MEMORY_IGNORE_ATTR,
            );
        if result != 0 {
            log::error!(
                "KPageTableBase::UnmapMemory destination-state check failed, dst=0x{:X}, src=0x{:X}, size=0x{:X}, result=0x{:08X}",
                dst,
                src,
                size,
                result
            );
            return result;
        }
        let mut src_allocator = match self.make_block_update_allocator(num_src_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(result) => return result,
        };
        let mut dst_allocator = match self.make_block_update_allocator(num_dst_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(result) => return result,
        };

        // Create the page group representing the destination and verify it is
        // the same physical page group as the source. Upstream:
        // `MakePageGroup(pg, dst_address, num_pages)` followed by
        // `IsValidPageGroup(pg, src_address, num_pages)`.
        let mut pg = super::k_page_group::KPageGroup::with_block_info_manager(
            self.m_block_info_manager.clone(),
        );
        let make_pg_result = self.make_page_group(&mut pg, dst, num_pages);
        if make_pg_result != 0 {
            log::error!(
                "KPageTableBase::UnmapMemory destination page-group creation failed, dst=0x{:X}, src=0x{:X}, size=0x{:X}, result=0x{:08X}",
                dst,
                src,
                size,
                make_pg_result
            );
            return make_pg_result;
        }
        if !self.is_valid_page_group(&pg, src, num_pages) {
            log::error!(
                "KPageTableBase::UnmapMemory source/destination page-group mismatch, dst=0x{:X}, src=0x{:X}, size=0x{:X}",
                dst,
                src,
                size
            );
            return svc_results::RESULT_INVALID_MEMORY_REGION.get_inner_value();
        }

        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Unmap the destination.
        let unmap_props = KPageProperties {
            perm: KMemoryPermission::NONE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        let op_result = self.operate(
            Some(updater.page_list()),
            dst,
            num_pages,
            0,
            false,
            unmap_props,
            OperationType::Unmap,
        );
        if op_result != 0 {
            log::error!(
                "KPageTableBase::UnmapMemory destination unmap operation failed, dst=0x{:X}, src=0x{:X}, size=0x{:X}, result=0x{:08X}",
                dst,
                src,
                size,
                op_result
            );
            return op_result;
        }

        // Restore source permissions.
        let restore_props = KPageProperties {
            perm: KMemoryPermission::USER_READ_WRITE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::ENABLE_AND_MERGE_HEAD_BODY_TAIL,
        };
        let op_result = self.operate(
            Some(updater.page_list()),
            src,
            num_pages,
            0,
            false,
            restore_props,
            OperationType::ChangePermissions,
        );
        if op_result != 0 {
            self.remap_page_group(Some(updater.page_list()), dst, size, &pg);
            log::error!(
                "KPageTableBase::UnmapMemory source restore operation failed, dst=0x{:X}, src=0x{:X}, size=0x{:X}, result=0x{:08X}",
                dst,
                src,
                size,
                op_result
            );
            return op_result;
        }

        // Update blocks.
        self.update_blocks_with_allocator(
            &mut src_allocator,
            src,
            num_pages,
            src_state,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::LOCKED,
        );
        self.update_blocks_with_allocator(
            &mut dst_allocator,
            dst,
            num_pages,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
        );
        0
    }

    fn physical_contiguous_ranges(
        &self,
        address: usize,
        size: usize,
    ) -> Result<Vec<(u64, usize)>, u32> {
        let Some(impl_pt) = self.m_impl.as_ref() else {
            return Err(svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value());
        };
        let Some((first_entry, mut traversal_context)) = impl_pt.begin_traversal(address as u64)
        else {
            return Err(svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value());
        };
        if first_entry.block_size == 0 {
            return Err(svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value());
        }

        let mut cur_phys = first_entry.phys_addr;
        let mut cur_size = size.min(
            first_entry
                .block_size
                .saturating_sub((cur_phys as usize) & (first_entry.block_size - 1)),
        );
        let mut consumed_size = cur_size;
        let mut ranges = Vec::new();

        while consumed_size < size {
            let Some(next_entry) = impl_pt.continue_traversal(&mut traversal_context) else {
                return Err(svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value());
            };
            let next_size = (size - consumed_size).min(next_entry.block_size);
            if next_entry.phys_addr == cur_phys + cur_size as u64 {
                cur_size += next_size;
            } else {
                ranges.push((cur_phys, cur_size));
                cur_phys = next_entry.phys_addr;
                cur_size = next_size;
            }
            consumed_size += next_size;
        }

        ranges.push((cur_phys, cur_size));
        Ok(ranges)
    }

    fn validate_process_memory_physical_ranges(
        dst_page_table: &Self,
        dst_address: usize,
        src_page_table: &Self,
        src_address: usize,
        size: usize,
    ) -> u32 {
        let src_ranges = match src_page_table.physical_contiguous_ranges(src_address, size) {
            Ok(ranges) => ranges,
            Err(result) => return result,
        };
        let dst_ranges = match dst_page_table.physical_contiguous_ranges(dst_address, size) {
            Ok(ranges) => ranges,
            Err(result) => return result,
        };
        if src_ranges != dst_ranges {
            return svc_results::RESULT_INVALID_MEMORY_REGION.get_inner_value();
        }
        0
    }

    /// Unmap process memory previously mapped as SharedCode.
    ///
    /// Upstream: `KPageTableBase::UnmapProcessMemory`.
    pub fn unmap_process_memory(
        &mut self,
        dst_address: usize,
        size: usize,
        src_page_table: &KPageTableBase,
        src_address: usize,
    ) -> u32 {
        let dst_lock = self.m_general_lock.clone();
        let src_lock = src_page_table.m_general_lock.clone();
        let mut lock_pair = KScopedLightLockPair::new(src_lock.clone(), dst_lock);

        let (result, num_allocator_blocks) = self.check_memory_state_contiguous(
            dst_address,
            size,
            KMemoryState::ALL,
            KMemoryState::SHARED_CODE,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::all(),
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        let result = src_page_table.check_memory_state(
            src_address,
            size,
            KMemoryState::FLAG_CAN_MAP_PROCESS,
            KMemoryState::FLAG_CAN_MAP_PROCESS,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::all(),
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        let result = Self::validate_process_memory_physical_ranges(
            self,
            dst_address,
            src_page_table,
            src_address,
            size,
        );
        if result != 0 {
            return result;
        }

        lock_pair.try_unlock_half(&src_lock);

        self.finish_unmap_process_memory(dst_address, size, num_allocator_blocks)
    }

    /// Same-table entry point for the current Rust SVC owner graph. Upstream
    /// reaches the same code through distinct `dst_pt`/`src_pt` references.
    pub fn unmap_process_memory_same_table(
        &mut self,
        dst_address: usize,
        size: usize,
        src_address: usize,
    ) -> u32 {
        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        let (result, num_allocator_blocks) = self.check_memory_state_contiguous(
            dst_address,
            size,
            KMemoryState::ALL,
            KMemoryState::SHARED_CODE,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::all(),
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        let result = self.check_memory_state(
            src_address,
            size,
            KMemoryState::FLAG_CAN_MAP_PROCESS,
            KMemoryState::FLAG_CAN_MAP_PROCESS,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::all(),
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        let result = Self::validate_process_memory_physical_ranges(
            self,
            dst_address,
            self,
            src_address,
            size,
        );
        if result != 0 {
            return result;
        }

        self.finish_unmap_process_memory(dst_address, size, num_allocator_blocks)
    }

    fn finish_unmap_process_memory(
        &mut self,
        dst_address: usize,
        size: usize,
        num_allocator_blocks: usize,
    ) -> u32 {
        let num_pages = size / PAGE_SIZE;
        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(result) => return result,
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        let unmap_properties = KPageProperties {
            perm: KMemoryPermission::NONE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        let result = self.operate(
            Some(updater.page_list()),
            dst_address,
            num_pages,
            0,
            false,
            unmap_properties,
            OperationType::Unmap,
        );
        if result != 0 {
            return result;
        }

        self.update_blocks_with_allocator(
            &mut allocator,
            dst_address,
            num_pages,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
        );
        0
    }

    // -- LockMemoryAndOpen / UnlockMemory --

    /// Lock a memory region, optionally changing permissions and setting lock attribute.
    /// Matches upstream `KPageTableBase::LockMemoryAndOpen`.
    pub fn lock_memory_and_open(
        &mut self,
        out_pg: Option<&mut super::k_page_group::KPageGroup>,
        out_paddr: Option<&mut u64>,
        addr: usize,
        size: usize,
        state_mask: KMemoryState,
        state: KMemoryState,
        perm_mask: KMemoryPermission,
        perm: KMemoryPermission,
        attr_mask: KMemoryAttribute,
        attr: KMemoryAttribute,
        new_perm: KMemoryPermission,
        lock_attr: KMemoryAttribute,
    ) -> u32 {
        // Port of upstream `KPageTableBase::LockMemoryAndOpen`
        // (k_page_table_base.cpp:759). Optionally returns a page group
        // describing the locked pages and/or the head physical address.
        // Upstream:
        //   ASSERT(False(lock_attr & attr));
        //   ASSERT(False(lock_attr & (IpcLocked | DeviceShared)));
        //   if (out_pg) ASSERT(out_pg->GetNumPages() == 0);
        debug_assert!(
            (lock_attr.bits() & attr.bits()) == 0,
            "lock_attr ∩ attr must be empty"
        );
        let num_pages = size / PAGE_SIZE;
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        // Check memory state with FlagReferenceCounted.
        let check_state_mask = KMemoryState::from_bits_truncate(
            state_mask.bits() | KMemoryState::FLAG_REFERENCE_COUNTED.bits(),
        );
        let check_state = KMemoryState::from_bits_truncate(
            state.bits() | KMemoryState::FLAG_REFERENCE_COUNTED.bits(),
        );
        let (result, out_state, out_old_perm, out_old_attr, _) = self.check_memory_state_range(
            addr,
            size,
            check_state_mask,
            check_state,
            perm_mask,
            perm,
            attr_mask,
            attr,
            Self::DEFAULT_MEMORY_IGNORE_ATTR,
        );
        if result != 0 {
            return result;
        }

        let old_state = out_state.unwrap_or(KMemoryState::FREE);
        let old_perm = out_old_perm.unwrap_or(KMemoryPermission::NONE);
        let old_attr = out_old_attr.unwrap_or(KMemoryAttribute::NONE);

        // Get the physical address, if requested. Upstream:
        //   ASSERT(this->GetPhysicalAddressLocked(out_paddr, addr));
        if let Some(out_paddr) = out_paddr {
            *out_paddr = match self
                .m_impl
                .as_ref()
                .and_then(|p| p.get_physical_address(addr as u64))
            {
                Some(p) => p,
                None => {
                    log::error!(
                        "lock_memory_and_open: no physical mapping for addr={:#x}",
                        addr
                    );
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
            };
        }

        // Make the page group, if requested. Upstream:
        //   if (out_pg != nullptr) {
        //       R_TRY(this->MakePageGroup(*out_pg, addr, num_pages));
        //   }
        // We hold `out_pg` past this point so we can call `Open()` after the
        // optional permission change succeeds (per upstream's tail).
        let out_pg_ref = if let Some(out_pg) = out_pg {
            let mpg = self.make_page_group(out_pg, addr, num_pages);
            if mpg != 0 {
                return mpg;
            }
            Some(out_pg)
        } else {
            None
        };

        // Determine new perm and attr.
        let effective_new_perm = if new_perm != KMemoryPermission::NONE {
            new_perm
        } else {
            old_perm
        };
        let new_attr = KMemoryAttribute::from_bits_truncate(old_attr.bits() | lock_attr.bits());

        // Change permissions if needed.
        if effective_new_perm != old_perm {
            let uncached = old_attr.contains(KMemoryAttribute::UNCACHED);
            let properties = KPageProperties {
                perm: effective_new_perm,
                io: false,
                uncached,
                disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD_BODY_TAIL,
            };
            let mut updater = KScopedPageTableUpdater::from_mut(self);
            let op_result = self.operate(
                Some(updater.page_list()),
                addr,
                num_pages,
                0,
                false,
                properties,
                OperationType::ChangePermissions,
            );
            if op_result != 0 {
                return op_result;
            }
        }

        // Update block manager.
        self.update_blocks(
            addr,
            num_pages,
            old_state,
            effective_new_perm,
            new_attr,
            KMemoryBlockDisableMergeAttribute::LOCKED,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        // If we have an output group, open. Upstream:
        //   if (out_pg) { out_pg->Open(); }
        if let Some(out_pg) = out_pg_ref {
            out_pg.open();
        }

        0
    }

    /// Unlock a previously locked memory region.
    /// Matches upstream `KPageTableBase::UnlockMemory`.
    pub fn unlock_memory(
        &mut self,
        addr: usize,
        size: usize,
        state_mask: KMemoryState,
        state: KMemoryState,
        perm_mask: KMemoryPermission,
        perm: KMemoryPermission,
        attr_mask: KMemoryAttribute,
        attr: KMemoryAttribute,
        new_perm: KMemoryPermission,
        lock_attr: KMemoryAttribute,
    ) -> u32 {
        self.unlock_memory_impl(
            addr, size, state_mask, state, perm_mask, perm, attr_mask, attr, new_perm, lock_attr,
            None,
        )
    }

    fn unlock_memory_impl(
        &mut self,
        addr: usize,
        size: usize,
        state_mask: KMemoryState,
        state: KMemoryState,
        perm_mask: KMemoryPermission,
        perm: KMemoryPermission,
        attr_mask: KMemoryAttribute,
        attr: KMemoryAttribute,
        new_perm: KMemoryPermission,
        lock_attr: KMemoryAttribute,
        pg: Option<&super::k_page_group::KPageGroup>,
    ) -> u32 {
        let num_pages = size / PAGE_SIZE;
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        // Check memory state with FlagReferenceCounted.
        let check_state_mask = KMemoryState::from_bits_truncate(
            state_mask.bits() | KMemoryState::FLAG_REFERENCE_COUNTED.bits(),
        );
        let check_state = KMemoryState::from_bits_truncate(
            state.bits() | KMemoryState::FLAG_REFERENCE_COUNTED.bits(),
        );
        let (result, out_state, out_old_perm, out_old_attr, _) = self.check_memory_state_range(
            addr,
            size,
            check_state_mask,
            check_state,
            perm_mask,
            perm,
            attr_mask,
            attr,
            Self::DEFAULT_MEMORY_IGNORE_ATTR,
        );
        if result != 0 {
            return result;
        }

        if let Some(pg) = pg {
            if !self.is_valid_page_group(pg, addr, num_pages) {
                return svc_results::RESULT_INVALID_MEMORY_REGION.get_inner_value();
            }
        }

        let old_state = out_state.unwrap_or(KMemoryState::FREE);
        let old_perm = out_old_perm.unwrap_or(KMemoryPermission::NONE);
        let old_attr = out_old_attr.unwrap_or(KMemoryAttribute::NONE);

        let effective_new_perm = if new_perm != KMemoryPermission::NONE {
            new_perm
        } else {
            old_perm
        };
        let new_attr = KMemoryAttribute::from_bits_truncate(old_attr.bits() & !lock_attr.bits());

        // Change permissions if needed.
        if effective_new_perm != old_perm {
            let uncached = old_attr.contains(KMemoryAttribute::UNCACHED);
            let properties = KPageProperties {
                perm: effective_new_perm,
                io: false,
                uncached,
                disable_merge_attributes: DisableMergeAttribute::ENABLE_AND_MERGE_HEAD_BODY_TAIL,
            };
            let mut updater = KScopedPageTableUpdater::from_mut(self);
            let op_result = self.operate(
                Some(updater.page_list()),
                addr,
                num_pages,
                0,
                false,
                properties,
                OperationType::ChangePermissions,
            );
            if op_result != 0 {
                return op_result;
            }
        }

        // Update block manager.
        self.update_blocks(
            addr,
            num_pages,
            old_state,
            effective_new_perm,
            new_attr,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::LOCKED,
        );

        0
    }

    /// Lock memory for IPC user buffer.
    /// Matches upstream `KPageTableBase::LockForIpcUserBuffer`.
    pub fn lock_for_ipc_user_buffer(
        &mut self,
        out_paddr: &mut u64,
        addr: usize,
        size: usize,
    ) -> u32 {
        self.lock_memory_and_open(
            None,
            Some(out_paddr),
            addr,
            size,
            KMemoryState::FLAG_CAN_IPC_USER_BUFFER,
            KMemoryState::FLAG_CAN_IPC_USER_BUFFER,
            KMemoryPermission::from_bits_truncate(0xFF), // All
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::from_bits_truncate(0xFF), // All
            KMemoryAttribute::NONE,
            // new_perm: NotMapped | KernelReadWrite
            KMemoryPermission::from_bits_truncate(
                KMemoryPermission::NOT_MAPPED.bits() | KMemoryPermission::KERNEL_READ_WRITE.bits(),
            ),
            KMemoryAttribute::LOCKED,
        )
    }

    /// Unlock memory for IPC user buffer.
    /// Matches upstream `KPageTableBase::UnlockForIpcUserBuffer`.
    pub fn unlock_for_ipc_user_buffer(&mut self, addr: usize, size: usize) -> u32 {
        self.unlock_memory(
            addr,
            size,
            KMemoryState::FLAG_CAN_IPC_USER_BUFFER,
            KMemoryState::FLAG_CAN_IPC_USER_BUFFER,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::from_bits_truncate(0xFF), // All
            KMemoryAttribute::LOCKED,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::LOCKED,
        )
    }

    // -- MapStatic / MapIo / MapRegion --

    /// Map static physical memory into the process address space.
    /// Matches upstream `KPageTableBase::MapStatic`.
    pub fn map_static(&mut self, phys_addr: u64, size: usize, perm: KMemoryPermission) -> u32 {
        let num_pages = size / PAGE_SIZE;
        let region_start = self.get_region_address(SvcMemoryState::Static);
        let region_size = self.get_region_size(SvcMemoryState::Static);
        let region_num_pages = region_size / PAGE_SIZE;

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        // Find a free area in the Static region.
        let addr = self.find_free_area(region_start, region_num_pages, num_pages, PAGE_SIZE, 0, 0);
        if addr == 0 {
            return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
        }

        let Ok(mut allocator) = self.make_block_update_allocator(
            super::k_dynamic_resource_manager::KMemoryBlockManagerUpdateAllocator::MAX_BLOCKS,
        ) else {
            return svc_results::RESULT_OUT_OF_RESOURCE.get_inner_value();
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Map the pages.
        let properties = KPageProperties {
            perm,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD,
        };
        let op_result = self.operate(
            Some(updater.page_list()),
            addr,
            num_pages,
            phys_addr,
            true,
            properties,
            OperationType::Map,
        );
        if op_result != 0 {
            return op_result;
        }

        // Update block manager.
        self.update_blocks_with_allocator(
            &mut allocator,
            addr,
            num_pages,
            KMemoryState::STATIC,
            perm,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        log::debug!(
            "KPageTableBase::map_static(phys={:#x}, size={:#x}) -> va={:#x}",
            phys_addr,
            size,
            addr
        );
        0
    }

    /// Map IO physical memory into the process address space.
    /// Matches upstream `KPageTableBase::MapIo`.
    pub fn map_io(&mut self, phys_addr: u64, size: usize, perm: KMemoryPermission) -> u32 {
        let num_pages = size / PAGE_SIZE;
        let region_start = self.get_region_address(SvcMemoryState::Io);
        let region_size = self.get_region_size(SvcMemoryState::Io);
        let region_num_pages = region_size / PAGE_SIZE;

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        // Find a free area.
        let addr = self.find_free_area(region_start, region_num_pages, num_pages, PAGE_SIZE, 0, 0);
        if addr == 0 {
            return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
        }

        let Ok(mut allocator) = self.make_block_update_allocator(
            super::k_dynamic_resource_manager::KMemoryBlockManagerUpdateAllocator::MAX_BLOCKS,
        ) else {
            return svc_results::RESULT_OUT_OF_RESOURCE.get_inner_value();
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Map the pages.
        let properties = KPageProperties {
            perm,
            io: true,
            uncached: true,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD,
        };
        let op_result = self.operate(
            Some(updater.page_list()),
            addr,
            num_pages,
            phys_addr,
            true,
            properties,
            OperationType::Map,
        );
        if op_result != 0 {
            return op_result;
        }

        // Update block manager.
        self.update_blocks_with_allocator(
            &mut allocator,
            addr,
            num_pages,
            KMemoryState::IO_REGISTER,
            perm,
            KMemoryAttribute::LOCKED,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        log::debug!(
            "KPageTableBase::map_io(phys={:#x}, size={:#x}) -> va={:#x}",
            phys_addr,
            size,
            addr
        );
        0
    }

    /// Map a kernel memory region by type.
    /// Matches upstream `KPageTableBase::MapRegion`.
    pub fn map_region(&mut self, region_type: u32, perm: KMemoryPermission) -> u32 {
        let Some(layout) = crate::hle::kernel::kernel::get_kernel_ref()
            .and_then(|kernel| kernel.get_memory_layout())
        else {
            return svc_results::RESULT_OUT_OF_RANGE.get_inner_value();
        };

        let region = {
            let layout = layout.lock().unwrap();
            let found = layout
                .get_physical_memory_region_tree()
                .iter()
                .find(|region| region.is_derived_from_raw(region_type))
                .map(|region| (region.get_address(), region.get_size()));
            found
        };
        let Some((phys_addr, size)) = region else {
            return svc_results::RESULT_OUT_OF_RANGE.get_inner_value();
        };

        let result = self.map_static(phys_addr, size, perm);
        if result == svc_results::RESULT_INVALID_ADDRESS.get_inner_value() {
            return svc_results::RESULT_OUT_OF_RANGE.get_inner_value();
        }
        result
    }

    // -- MapPages --

    /// Map pages at a free area in a region.
    /// Matches upstream `KPageTableBase::MapPages(out_addr, num_pages, alignment, phys_addr, ...)`.
    pub fn map_pages_find_free(
        &mut self,
        num_pages: usize,
        alignment: usize,
        phys_addr: u64,
        is_pa_valid: bool,
        region_start: usize,
        region_num_pages: usize,
        state: KMemoryState,
        perm: KMemoryPermission,
    ) -> (u32, usize) {
        debug_assert!(alignment >= PAGE_SIZE && alignment % PAGE_SIZE == 0);

        if !self.can_contain_k(region_start, region_num_pages * PAGE_SIZE, state) {
            return (
                svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                0,
            );
        }
        if num_pages >= region_num_pages {
            return (svc_results::RESULT_OUT_OF_MEMORY.get_inner_value(), 0);
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        // Find free area.
        let addr = self.find_free_area(
            region_start,
            region_num_pages,
            num_pages,
            alignment,
            0,
            self.get_num_guard_pages(),
        );
        if addr == 0 {
            return (svc_results::RESULT_OUT_OF_MEMORY.get_inner_value(), 0);
        }

        // Verify the area is free.
        let result = self.check_memory_state(
            addr,
            num_pages * PAGE_SIZE,
            KMemoryState::ALL,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return (result, 0);
        }

        let mut allocator = match self.make_block_update_allocator(
            super::k_dynamic_resource_manager::KMemoryBlockManagerUpdateAllocator::MAX_BLOCKS,
        ) {
            Ok(allocator) => allocator,
            Err(rc) => return (rc, 0),
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Map.
        if is_pa_valid {
            let properties = KPageProperties {
                perm,
                io: false,
                uncached: false,
                disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD,
            };
            let op_result = self.operate(
                Some(updater.page_list()),
                addr,
                num_pages,
                phys_addr,
                true,
                properties,
                OperationType::Map,
            );
            if op_result != 0 {
                return (op_result, 0);
            }
        } else {
            let op_result =
                self.allocate_and_map_pages_impl(Some(updater.page_list()), addr, num_pages, perm);
            if op_result != 0 {
                return (op_result, 0);
            }
        }

        // Update blocks.
        self.update_blocks_with_allocator(
            &mut allocator,
            addr,
            num_pages,
            state,
            perm,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        (0, addr)
    }

    /// Map pages at a specific address.
    /// Matches upstream `KPageTableBase::MapPages(address, num_pages, state, perm)`.
    pub fn map_pages_at_address(
        &mut self,
        addr: usize,
        num_pages: usize,
        state: KMemoryState,
        perm: KMemoryPermission,
    ) -> u32 {
        let size = num_pages * PAGE_SIZE;
        if !self.can_contain_k(addr, size, state) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        // Check the target area is free.
        let (result, _, _, _, num_allocator_blocks) = self.check_memory_state_range(
            addr,
            size,
            KMemoryState::ALL,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryAttribute::NONE,
            Self::DEFAULT_MEMORY_IGNORE_ATTR,
        );
        if result != 0 {
            return result;
        }

        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        let op_result =
            self.allocate_and_map_pages_impl(Some(updater.page_list()), addr, num_pages, perm);
        if op_result != 0 {
            return op_result;
        }

        // Update blocks.
        self.update_blocks_with_allocator(
            &mut allocator,
            addr,
            num_pages,
            state,
            perm,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        0
    }

    /// Unmap pages.
    /// Matches upstream `KPageTableBase::UnmapPages`.
    pub fn unmap_pages(&mut self, addr: usize, num_pages: usize, state: KMemoryState) -> u32 {
        let size = num_pages * PAGE_SIZE;
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        // Check the state matches.
        let (result, _, _, _, num_allocator_blocks) = self.check_memory_state_range(
            addr,
            size,
            KMemoryState::ALL,
            state,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::NONE,
            Self::DEFAULT_MEMORY_IGNORE_ATTR,
        );
        if result != 0 {
            return result;
        }

        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Unmap.
        let unmap_props = KPageProperties {
            perm: KMemoryPermission::NONE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        let op_result = self.operate(
            Some(updater.page_list()),
            addr,
            num_pages,
            0,
            false,
            unmap_props,
            OperationType::Unmap,
        );
        if op_result != 0 {
            return op_result;
        }

        // Update blocks.
        self.update_blocks_with_allocator(
            &mut allocator,
            addr,
            num_pages,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
        );
        0
    }

    // -- SetProcessMemoryPermission (for NSO loading) --

    /// Set memory permission for process code/data sections.
    /// Matches upstream `KPageTableBase::SetProcessMemoryPermission`.
    pub fn set_process_memory_permission(
        &mut self,
        addr: usize,
        size: usize,
        perm: KMemoryPermission,
    ) -> u32 {
        let num_pages = size / PAGE_SIZE;
        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        // Check current state — must be Code-flagged.
        let (result, out_state, out_perm, _out_attr, num_allocator_blocks) = self
            .check_memory_state_range(
                addr,
                size,
                KMemoryState::FLAG_CODE,
                KMemoryState::FLAG_CODE,
                KMemoryPermission::NONE,
                KMemoryPermission::NONE,
                KMemoryAttribute::from_bits_truncate(0xFF),
                KMemoryAttribute::NONE,
                Self::DEFAULT_MEMORY_IGNORE_ATTR,
            );
        if result != 0 {
            return result;
        }

        let old_state = out_state.unwrap_or(KMemoryState::CODE);
        let old_perm = out_perm.unwrap_or(KMemoryPermission::NONE);
        let mut pg = super::k_page_group::KPageGroup::with_block_info_manager(
            self.m_block_info_manager.clone(),
        );

        // Determine new state based on permission being set.
        let is_w = perm.contains(KMemoryPermission::USER_WRITE);
        let is_x = perm.contains(KMemoryPermission::USER_EXECUTE);
        let was_x = old_perm.contains(KMemoryPermission::USER_EXECUTE);
        debug_assert!(!(is_w && is_x));

        let new_state = if is_w {
            if old_state == KMemoryState::CODE {
                KMemoryState::CODE_DATA
            } else if old_state == KMemoryState::ALIAS_CODE {
                KMemoryState::ALIAS_CODE_DATA
            } else {
                old_state
            }
        } else {
            old_state
        };

        if is_x {
            let result = self.make_page_group(&mut pg, addr, num_pages);
            if result != 0 {
                return result;
            }
        }

        // Nothing to do if perm and state are unchanged.
        if old_perm == perm && old_state == new_state {
            return 0;
        }

        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Change permissions via Operate.
        let operation = if was_x {
            OperationType::ChangePermissionsAndRefreshAndFlush
        } else {
            OperationType::ChangePermissions
        };
        let properties = KPageProperties {
            perm,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        let op_result = self.operate(
            Some(updater.page_list()),
            addr,
            num_pages,
            0,
            false,
            properties,
            operation,
        );
        if op_result != 0 {
            return op_result;
        }

        // Update block manager.
        self.update_blocks_with_allocator(
            &mut allocator,
            addr,
            num_pages,
            new_state,
            perm,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        if is_x {
            for block in pg.iter() {
                let rc = store_data_cache(block.get_address(), block.get_size());
                if rc != 0 {
                    return rc;
                }
            }
            invalidate_instruction_cache_for_table(self, addr, size);
        }
        0
    }

    // -- MapPhysicalMemory / UnmapPhysicalMemory --

    /// Map physical memory into the alias region.
    /// Matches upstream `KPageTableBase::MapPhysicalMemory`.
    pub fn map_physical_memory(&mut self, addr: usize, size: usize) -> u32 {
        // Port of upstream `KPageTableBase::MapPhysicalMemory`
        // (k_page_table_base.cpp:5111). Three-phase flow:
        //   1. Walk the memory block manager to count already-mapped bytes.
        //   2. Allocate `size - mapped` pages as a KPageGroup.
        //   3. Walk the block manager again, mapping FREE ranges only,
        //      drawing pages from the page-group iterator. Skip already-
        //      mapped (Normal) ranges.
        // If concurrent mappings change between phases (mapped_size differs),
        // upstream retries — ruzu currently has no concurrency between
        // SetHeapSize/MapPhysicalMemory callers (single-threaded SVC dispatch
        // per process), but the retry loop is preserved structurally.
        use super::k_scoped_resource_reservation::KScopedResourceReservation;
        if !self.is_in_alias_region(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }
        let physical_memory_lock = self.m_map_physical_memory_lock.clone();
        let _physical_memory_lock_guard =
            super::k_light_lock::KScopedLightLock::new(physical_memory_lock.as_ref());
        let last_address = addr + size - 1;

        loop {
            // === Phase 1: count already-mapped bytes ===
            let mut mapped_size: usize = 0;
            {
                let general_lock = self.m_general_lock.clone();
                let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());
                let mut cur_address = addr;
                for info in self.m_memory_block_manager.find_iterator(cur_address) {
                    let info_state = info.get_state();
                    let info_end = info.get_end_address();
                    let info_last = info.get_last_address();
                    let in_range_end = info_last.min(last_address);
                    if info_state != KMemoryState::FREE {
                        mapped_size += in_range_end + 1 - cur_address;
                    }
                    if last_address <= info_last {
                        break;
                    }
                    cur_address = info_end;
                }
            }
            // Already entirely mapped — nothing to do.
            if mapped_size == size {
                return 0;
            }

            // === Phase 2: allocate ===
            let allocation_size = size - mapped_size;
            let mut memory_reservation = KScopedResourceReservation::new(
                self.m_resource_limit.clone(),
                LimitableResource::PhysicalMemoryMax,
                allocation_size as i64,
            );
            if !memory_reservation.succeeded() {
                return svc_results::RESULT_LIMIT_REACHED.get_inner_value();
            }

            let alloc_option = self.m_allocate_option;
            let mut pg = super::k_page_group::KPageGroup::with_block_info_manager(
                self.m_block_info_manager.clone(),
            );
            {
                let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut() else {
                    return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
                };
                let rc = kernel.memory_manager_mut().allocate_for_process(
                    &mut pg,
                    allocation_size / PAGE_SIZE,
                    alloc_option,
                    self.m_process_id,
                );
                if rc != 0 {
                    return rc;
                }
            }
            for block in pg.iter() {
                self.clear_fresh_backing_region_phys(block.get_address(), block.get_size());
            }

            // === Phase 3: re-check + map ===
            // Re-walk the block manager. If somebody mapped between phase 1
            // and phase 3 (mapped_size changed), drop pg (auto-close) and
            // retry. ruzu is single-threaded per process so this always
            // matches; the loop is structural for fidelity.
            let general_lock = self.m_general_lock.clone();
            let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());
            let mut checked_mapped: usize = 0;
            let mut num_allocator_blocks: usize = 0;
            {
                let mut cur_address = addr;
                for info in self.m_memory_block_manager.find_iterator(cur_address) {
                    let info_state = info.get_state();
                    let info_end = info.get_end_address();
                    let info_last = info.get_last_address();
                    let in_range_end = info_last.min(last_address);
                    if info_state == KMemoryState::FREE {
                        if info.get_address() < addr {
                            num_allocator_blocks += 1;
                        }
                        if last_address < info_last {
                            num_allocator_blocks += 1;
                        }
                    } else {
                        checked_mapped += in_range_end + 1 - cur_address;
                    }
                    if last_address <= info_last {
                        break;
                    }
                    cur_address = info_end;
                }
            }
            if checked_mapped != mapped_size {
                // Somebody raced with us — release the unowned page group and retry.
                Self::release_unowned_page_group(&pg);
                continue;
            }

            let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
                Ok(allocator) => allocator,
                Err(rc) => return rc,
            };
            let mut updater = KScopedPageTableUpdater::from_mut(self);
            let updater_page_list = updater.page_list();

            // Iterate FREE ranges, slicing pages off the page group as we go.
            // Snapshot block boundaries first so we don't borrow the block
            // manager mutably and immutably at the same time.
            let mut free_ranges: Vec<(usize, usize)> = Vec::new();
            {
                let mut cur_address = addr;
                for info in self.m_memory_block_manager.find_iterator(cur_address) {
                    let info_state = info.get_state();
                    let info_end = info.get_end_address();
                    let info_last = info.get_last_address();
                    let in_range_end = info_last.min(last_address);
                    if info_state == KMemoryState::FREE {
                        let range_pages = (in_range_end + 1 - cur_address) / PAGE_SIZE;
                        free_ranges.push((cur_address, range_pages));
                    }
                    if last_address <= info_last {
                        break;
                    }
                    cur_address = info_end;
                }
            }

            // Walk the page group, slicing blocks to fill each free VA range.
            let pg_blocks: Vec<(u64, usize)> = pg
                .iter()
                .map(|b| (b.get_address(), b.get_num_pages()))
                .collect();
            let mut pg_idx = 0usize;
            let mut pg_phys = pg_blocks.first().map(|(a, _)| *a).unwrap_or(0);
            let mut pg_remaining = pg_blocks.first().map(|(_, n)| *n).unwrap_or(0);

            for &(range_va, range_pages) in &free_ranges {
                let mut cur_pg = super::k_page_group::KPageGroup::with_block_info_manager(
                    self.m_block_info_manager.clone(),
                );
                let mut needed = range_pages;
                while needed > 0 {
                    if pg_remaining == 0 {
                        pg_idx += 1;
                        if pg_idx >= pg_blocks.len() {
                            // Should not happen — page group sizes match.
                            log::error!("map_physical_memory: page group ran short");
                            return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
                        }
                        pg_phys = pg_blocks[pg_idx].0;
                        pg_remaining = pg_blocks[pg_idx].1;
                    }
                    let chunk = needed.min(pg_remaining);
                    // Upstream consumes each KPageGroup block from its tail:
                    // `pg_phys_addr + ((pg_pages - cur_pages) * PageSize)`.
                    let chunk_phys = pg_phys + ((pg_remaining - chunk) * PAGE_SIZE) as u64;
                    if cur_pg.add_block(chunk_phys, chunk).is_err() {
                        self.rollback_partial_map_physical(addr, range_va, &mut *updater_page_list);
                        Self::release_unowned_physical_pages_from(
                            &pg_blocks,
                            pg_idx,
                            pg_phys,
                            pg_remaining,
                        );
                        return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
                    }
                    pg_remaining -= chunk;
                    needed -= chunk;
                }

                let cur_props = KPageProperties {
                    perm: KMemoryPermission::USER_READ_WRITE,
                    io: false,
                    uncached: false,
                    disable_merge_attributes: if range_va == self.get_alias_region_start() {
                        DisableMergeAttribute::DISABLE_HEAD
                    } else {
                        DisableMergeAttribute::NONE
                    },
                };
                let rc = self.operate_with_group(
                    Some(&mut *updater_page_list),
                    range_va,
                    range_pages,
                    &cur_pg,
                    cur_props,
                    OperationType::MapFirstGroupPhysical,
                );
                if rc != 0 {
                    self.rollback_partial_map_physical(addr, range_va, &mut *updater_page_list);
                    Self::release_unowned_physical_pages_from(
                        &pg_blocks,
                        pg_idx,
                        pg_phys,
                        pg_remaining,
                    );
                    return rc;
                }
            }

            // Commit reservation and update block manager. The per-range
            // MapFirstGroupPhysical calls opened the page references now owned
            // by the mappings; `pg` itself only finalizes its block list.
            memory_reservation.commit();
            self.m_memory_block_manager.update_if_match_with_allocator(
                Some(&mut allocator),
                addr,
                size / PAGE_SIZE,
                KMemoryState::FREE,
                KMemoryPermission::NONE,
                KMemoryAttribute::NONE,
                KMemoryState::NORMAL,
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::NONE,
                if addr == self.get_alias_region_start() {
                    KMemoryBlockDisableMergeAttribute::NORMAL
                } else {
                    KMemoryBlockDisableMergeAttribute::NONE
                },
                KMemoryBlockDisableMergeAttribute::NONE,
            );
            self.m_mapped_physical_memory_size += allocation_size;
            return 0;
        }
    }

    /// Roll back a partial `map_physical_memory` failure by unmapping the
    /// FREE ranges in `[addr, last_unmap_address+1)` that we just touched.
    /// Mirrors upstream's `ON_RESULT_FAILURE` lambda in MapPhysicalMemory.
    fn rollback_partial_map_physical(
        &mut self,
        addr: usize,
        last_unmap_address: usize,
        page_list: &mut PageLinkedList,
    ) {
        if last_unmap_address <= addr {
            return;
        }
        let last_unmap = last_unmap_address - 1;
        let mut free_ranges: Vec<(usize, usize)> = Vec::new();
        {
            let mut cur_address = addr;
            for info in self.m_memory_block_manager.find_iterator(cur_address) {
                let info_state = info.get_state();
                let info_end = info.get_address() + info.get_size();
                let info_last = info_end - 1;
                let in_range_end = info_last.min(last_unmap);
                if info_state == KMemoryState::FREE {
                    let pages = (in_range_end + 1 - cur_address) / PAGE_SIZE;
                    free_ranges.push((cur_address, pages));
                }
                if last_unmap <= info_last {
                    break;
                }
                cur_address = info_end;
            }
        }
        let unmap_props = KPageProperties {
            perm: KMemoryPermission::NONE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        for (va, pages) in free_ranges {
            let _ = self.operate(
                Some(&mut *page_list),
                va,
                pages,
                0,
                false,
                unmap_props,
                OperationType::UnmapPhysical,
            );
        }
    }

    fn release_unowned_page_group(pg: &super::k_page_group::KPageGroup) {
        let blocks: Vec<(u64, usize)> = pg
            .iter()
            .map(|block| (block.get_address(), block.get_num_pages()))
            .collect();
        Self::release_unowned_physical_pages(&blocks);
    }

    fn release_unowned_physical_pages(blocks: &[(u64, usize)]) {
        if let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut() {
            let mm = kernel.memory_manager_mut();
            for &(addr, pages) in blocks {
                if pages == 0 {
                    continue;
                }
                mm.open_first(addr, pages);
                mm.close(addr, pages);
            }
        }
    }

    fn release_unowned_physical_pages_from(
        blocks: &[(u64, usize)],
        block_index: usize,
        current_phys: u64,
        current_pages: usize,
    ) {
        if let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut() {
            let mm = kernel.memory_manager_mut();
            if current_pages != 0 {
                mm.open_first(current_phys, current_pages);
                mm.close(current_phys, current_pages);
            }
            for &(addr, pages) in blocks.iter().skip(block_index + 1) {
                if pages == 0 {
                    continue;
                }
                mm.open_first(addr, pages);
                mm.close(addr, pages);
            }
        }
    }

    /// Unmap physical memory from the alias region.
    /// Matches upstream `KPageTableBase::UnmapPhysicalMemory`.
    pub fn unmap_physical_memory(&mut self, addr: usize, size: usize) -> u32 {
        if !self.is_in_alias_region(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }
        let physical_memory_lock = self.m_map_physical_memory_lock.clone();
        let _physical_memory_lock_guard =
            super::k_light_lock::KScopedLightLock::new(physical_memory_lock.as_ref());
        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());
        let last_address = addr + size - 1;

        let mut mapped_size = 0usize;
        let mut map_start_address = 0usize;
        let mut map_last_address = 0usize;
        let mut num_allocator_blocks = 0usize;
        let mut normal_ranges: Vec<(usize, usize)> = Vec::new();
        {
            let mut cur_address = addr;
            for info in self.m_memory_block_manager.find_iterator(cur_address) {
                let info_end = info.get_end_address();
                let info_last = info.get_last_address();
                let in_range_end = info_last.min(last_address);
                let is_normal = info.get_state() == KMemoryState::NORMAL
                    && info.get_attribute() == KMemoryAttribute::NONE;
                let is_free = info.get_state() == KMemoryState::FREE;
                if !is_normal && !is_free {
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
                if is_normal {
                    if map_start_address == 0 {
                        map_start_address = cur_address;
                    }
                    map_last_address = in_range_end;
                    if info.get_address() < addr {
                        num_allocator_blocks += 1;
                    }
                    if last_address < info_last {
                        num_allocator_blocks += 1;
                    }
                    let pages = (in_range_end + 1 - cur_address) / PAGE_SIZE;
                    normal_ranges.push((cur_address, pages));
                    mapped_size += in_range_end + 1 - cur_address;
                }
                if last_address <= info_last {
                    break;
                }
                cur_address = info_end;
            }
        }

        if mapped_size == 0 {
            return 0;
        }

        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);
        let updater_page_list = updater.page_list();

        let unmap_props = KPageProperties {
            perm: KMemoryPermission::NONE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };

        let separate_result = self.operate(
            Some(&mut *updater_page_list),
            map_start_address,
            (map_last_address + 1 - map_start_address) / PAGE_SIZE,
            0,
            false,
            unmap_props,
            OperationType::Separate,
        );
        if separate_result != 0 {
            return separate_result;
        }

        for (range_addr, range_pages) in normal_ranges {
            let op_result = self.operate(
                Some(&mut *updater_page_list),
                range_addr,
                range_pages,
                0,
                false,
                unmap_props,
                OperationType::UnmapPhysical,
            );
            if op_result != 0 {
                return op_result;
            }
        }

        let clear_merge_attr = if self
            .m_memory_block_manager
            .find_iterator(addr)
            .next()
            .is_some_and(|info| {
                info.get_state() == KMemoryState::NORMAL
                    && info.get_address() == self.get_alias_region_start()
                    && info.get_address() == addr
            }) {
            KMemoryBlockDisableMergeAttribute::NORMAL
        } else {
            KMemoryBlockDisableMergeAttribute::NONE
        };
        self.m_mapped_physical_memory_size = self
            .m_mapped_physical_memory_size
            .saturating_sub(mapped_size);
        if let Some(resource_limit) = &self.m_resource_limit {
            resource_limit.release(LimitableResource::PhysicalMemoryMax, mapped_size as i64);
        }
        self.update_blocks_with_allocator(
            &mut allocator,
            addr,
            size / PAGE_SIZE,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            clear_merge_attr,
        );
        0
    }

    // -- Transfer Memory Locking --

    /// Lock memory for transfer memory.
    /// Matches upstream `KPageTableBase::LockForTransferMemory`.
    pub fn lock_for_transfer_memory(
        &mut self,
        out_pg: &mut super::k_page_group::KPageGroup,
        addr: usize,
        size: usize,
        perm: KMemoryPermission,
    ) -> u32 {
        self.lock_memory_and_open(
            Some(out_pg),
            None,
            addr,
            size,
            KMemoryState::FLAG_CAN_TRANSFER,
            KMemoryState::FLAG_CAN_TRANSFER,
            KMemoryPermission::from_bits_truncate(0xFF),
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::NONE,
            perm,
            KMemoryAttribute::LOCKED,
        )
    }

    /// Unlock memory for transfer memory.
    /// Matches upstream `KPageTableBase::UnlockForTransferMemory`.
    pub fn unlock_for_transfer_memory(
        &mut self,
        addr: usize,
        size: usize,
        pg: &super::k_page_group::KPageGroup,
    ) -> u32 {
        self.unlock_memory_impl(
            addr,
            size,
            KMemoryState::FLAG_CAN_TRANSFER,
            KMemoryState::FLAG_CAN_TRANSFER,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::LOCKED,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::LOCKED,
            Some(pg),
        )
    }
    // -- MapCodeMemory / UnmapCodeMemory --

    /// Map code memory: copies src pages to dst, reprotects src as KernelRead|NotMapped.
    /// Matches upstream `KPageTableBase::MapCodeMemory`.
    pub fn map_code_memory(&mut self, dst: usize, src: usize, size: usize) -> u32 {
        let num_pages = size / PAGE_SIZE;

        if !self.can_contain(dst, size, SvcMemoryState::AliasCode) {
            return svc_results::RESULT_INVALID_MEMORY_REGION.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        // Check source: Normal, UserReadWrite.
        let (result, src_state, src_perm, _, num_src_allocator_blocks) = self
            .check_memory_state_range(
                src,
                size,
                KMemoryState::ALL,
                KMemoryState::NORMAL,
                KMemoryPermission::from_bits_truncate(0xFF),
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::from_bits_truncate(0xFF),
                KMemoryAttribute::NONE,
                Self::DEFAULT_MEMORY_IGNORE_ATTR,
            );
        if result != 0 {
            return result;
        }
        let src_state = src_state.unwrap_or(KMemoryState::NORMAL);
        let src_perm = src_perm.unwrap_or(KMemoryPermission::USER_READ_WRITE);

        // Check destination: Free.
        let (result, _, _, _, num_dst_allocator_blocks) = self.check_memory_state_range(
            dst,
            size,
            KMemoryState::ALL,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryAttribute::NONE,
            Self::DEFAULT_MEMORY_IGNORE_ATTR,
        );
        if result != 0 {
            return result;
        }

        let mut src_allocator = match self.make_block_update_allocator(num_src_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };
        let mut dst_allocator = match self.make_block_update_allocator(num_dst_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };

        // Build a KPageGroup describing src's existing physical pages before reprotecting it.
        let mut pg = super::k_page_group::KPageGroup::with_block_info_manager(
            self.m_block_info_manager.clone(),
        );
        let mpg_rc = self.make_page_group(&mut pg, src, num_pages);
        if mpg_rc != 0 {
            return mpg_rc;
        }

        let mut updater = KScopedPageTableUpdater::from_mut(self);
        let new_perm = KMemoryPermission::from_bits_truncate(
            KMemoryPermission::KERNEL_READ.bits() | KMemoryPermission::NOT_MAPPED.bits(),
        );

        // Reprotect source.
        let src_props = KPageProperties {
            perm: new_perm,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD_BODY_TAIL,
        };
        let op = self.operate(
            Some(updater.page_list()),
            src,
            num_pages,
            0,
            false,
            src_props,
            OperationType::ChangePermissions,
        );
        if op != 0 {
            return op;
        }

        // Map dst with that page group via MapPageGroupImpl. On failure, restore src's permissions.
        let dst_props = KPageProperties {
            perm: new_perm,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD,
        };
        let map_rc = self.map_page_group_impl(Some(updater.page_list()), dst, &pg, dst_props);
        if map_rc != 0 {
            let revert = KPageProperties {
                perm: src_perm,
                io: false,
                uncached: false,
                disable_merge_attributes: DisableMergeAttribute::ENABLE_HEAD_BODY_TAIL,
            };
            let _ = self.operate(
                Some(updater.page_list()),
                src,
                num_pages,
                0,
                false,
                revert,
                OperationType::ChangePermissions,
            );
            return map_rc;
        }

        // Update blocks.
        self.update_blocks_with_allocator(
            &mut src_allocator,
            src,
            num_pages,
            src_state,
            new_perm,
            KMemoryAttribute::LOCKED,
            KMemoryBlockDisableMergeAttribute::LOCKED,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        self.update_blocks_with_allocator(
            &mut dst_allocator,
            dst,
            num_pages,
            KMemoryState::ALIAS_CODE,
            new_perm,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        0
    }

    /// Unmap code memory: unmaps dst, restores src permissions.
    /// Matches upstream `KPageTableBase::UnmapCodeMemory`.
    pub fn unmap_code_memory(&mut self, dst: usize, src: usize, size: usize) -> u32 {
        let num_pages = size / PAGE_SIZE;

        if !self.can_contain(dst, size, SvcMemoryState::AliasCode) {
            return svc_results::RESULT_INVALID_MEMORY_REGION.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        // Check source: Normal, Locked.
        let (result, num_src_allocator_blocks) = self.check_memory_state_contiguous(
            src,
            size,
            KMemoryState::ALL,
            KMemoryState::NORMAL,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::LOCKED,
        );
        if result != 0 {
            return result;
        }

        let (result, num_dst_allocator_blocks) = self.check_memory_state_contiguous(
            dst,
            size,
            KMemoryState::FLAG_CAN_CODE_ALIAS,
            KMemoryState::FLAG_CAN_CODE_ALIAS,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::from_bits_truncate(0xFF) & !KMemoryAttribute::PERMISSION_LOCKED,
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        let mut any_code_pages = false;
        let last_address = dst + size - 1;
        for info in self.m_memory_block_manager.find_iterator(dst) {
            if info.get_state().intersects(KMemoryState::FLAG_CODE) {
                any_code_pages = true;
                break;
            }
            if last_address <= info.get_last_address() {
                break;
            }
        }

        let mut pg = super::k_page_group::KPageGroup::with_block_info_manager(
            self.m_block_info_manager.clone(),
        );
        let result = self.make_page_group(&mut pg, dst, num_pages);
        if result != 0 {
            return result;
        }
        if !self.is_valid_page_group(&pg, src, num_pages) {
            return svc_results::RESULT_INVALID_MEMORY_REGION.get_inner_value();
        }

        let mut src_allocator = match self.make_block_update_allocator(num_src_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };
        let mut dst_allocator = match self.make_block_update_allocator(num_dst_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };

        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Unmap dst.
        let unmap_props = KPageProperties {
            perm: KMemoryPermission::NONE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        let op = self.operate(
            Some(updater.page_list()),
            dst,
            num_pages,
            0,
            false,
            unmap_props,
            OperationType::Unmap,
        );
        if op != 0 {
            return op;
        }

        // Restore source permissions. On failure, restore the destination alias mapping.
        let restore_props = KPageProperties {
            perm: KMemoryPermission::USER_READ_WRITE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::ENABLE_AND_MERGE_HEAD_BODY_TAIL,
        };
        let op = self.operate(
            Some(updater.page_list()),
            src,
            num_pages,
            0,
            false,
            restore_props,
            OperationType::ChangePermissions,
        );
        if op != 0 {
            self.remap_page_group(Some(updater.page_list()), dst, size, &pg);
            return op;
        }

        // Update blocks.
        self.update_blocks_with_allocator(
            &mut dst_allocator,
            dst,
            num_pages,
            KMemoryState::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
        );
        self.update_blocks_with_allocator(
            &mut src_allocator,
            src,
            num_pages,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::LOCKED,
        );
        if any_code_pages {
            invalidate_instruction_cache_for_table(self, dst, size);
        }
        0
    }

    // -- MapInsecureMemory / UnmapInsecureMemory --

    /// Matches upstream `KPageTableBase::MapInsecureMemory`.
    pub fn map_insecure_memory(&mut self, addr: usize, size: usize) -> u32 {
        // Port of upstream `KPageTableBase::MapInsecureMemory`
        // (k_page_table_base.cpp:1379). Allocates from the *insecure* pool
        // (KMemoryManager::Pool::SystemNonSecure on Nintendo NX), reserves
        // against the kernel-wide insecure resource limit, and maps the
        // resulting page group with Operate(MapGroup).
        use super::k_scoped_resource_reservation::KScopedResourceReservation;
        let _updater = KScopedPageTableUpdater::from_mut(self);
        let num_pages = size / PAGE_SIZE;

        // Get the insecure memory resource limit and pool from KSystemControl.
        let kernel_arc = crate::hle::kernel::kernel::get_kernel_ref();
        let insecure_resource_limit = kernel_arc
            .and_then(|k| super::board::k_system_control::get_insecure_memory_resource_limit(k));
        let insecure_pool_raw = super::board::k_system_control::get_insecure_memory_pool();
        let insecure_pool_option = super::k_memory_manager::KMemoryManager::encode_option(
            // Decode the raw pool id and re-encode as the (pool, FromFront)
            // option upstream uses for insecure allocations.
            match insecure_pool_raw {
                0 => super::k_memory_manager::Pool::Application,
                1 => super::k_memory_manager::Pool::Applet,
                2 => super::k_memory_manager::Pool::System,
                _ => super::k_memory_manager::Pool::SystemNonSecure,
            },
            super::k_memory_manager::Direction::FromFront,
        );

        // Reserve the insecure memory. NOTE: ResultOutOfMemory is returned
        // here instead of the usual LimitReached (matches upstream comment).
        let mut memory_reservation = KScopedResourceReservation::new(
            insecure_resource_limit,
            LimitableResource::PhysicalMemoryMax,
            size as i64,
        );
        if !memory_reservation.succeeded() {
            return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
        }

        // Allocate pages for the insecure memory.
        let mut pg = super::k_page_group::KPageGroup::with_block_info_manager(
            self.m_block_info_manager.clone(),
        );
        {
            let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut() else {
                return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
            };
            let rc = kernel.memory_manager_mut().allocate_and_open(
                &mut pg,
                num_pages,
                insecure_pool_option,
            );
            if rc != 0 {
                return rc;
            }
        }

        // RAII close: balances the OpenFirst above so refcount returns to 1
        // after Operate(MapGroup) opens its extra reference.
        struct PgCloseGuard<'a> {
            pg: &'a mut super::k_page_group::KPageGroup,
        }
        impl<'a> Drop for PgCloseGuard<'a> {
            fn drop(&mut self) {
                self.pg.close();
            }
        }
        let pg_guard = PgCloseGuard { pg: &mut pg };

        // Clear all the newly allocated pages (per-block phys clear).
        for block in pg_guard.pg.iter() {
            self.clear_fresh_backing_region_phys(block.get_address(), block.get_size());
        }

        // Validate that the address's state is valid (Free).
        let result = self.check_memory_state(
            addr,
            size,
            KMemoryState::ALL,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        // Map the pages.
        let map_properties = KPageProperties {
            perm: KMemoryPermission::USER_READ_WRITE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD,
        };
        let op = self.operate_with_group(
            None,
            addr,
            num_pages,
            pg_guard.pg,
            map_properties,
            OperationType::MapGroup,
        );
        if op != 0 {
            return op;
        }

        // Apply the memory block update.
        self.update_blocks(
            addr,
            num_pages,
            KMemoryState::INSECURE,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        self.m_mapped_insecure_memory += size;

        // Commit the memory reservation.
        memory_reservation.commit();
        drop(pg_guard);
        0
    }

    /// Matches upstream `KPageTableBase::UnmapInsecureMemory`.
    pub fn unmap_insecure_memory(&mut self, addr: usize, size: usize) -> u32 {
        let num_pages = size / PAGE_SIZE;
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let result = self.check_memory_state(
            addr,
            size,
            KMemoryState::ALL,
            KMemoryState::INSECURE,
            KMemoryPermission::from_bits_truncate(0xFF),
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        let unmap_props = KPageProperties {
            perm: KMemoryPermission::NONE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        let op = self.operate(
            None,
            addr,
            num_pages,
            0,
            false,
            unmap_props,
            OperationType::Unmap,
        );
        if op != 0 {
            return op;
        }

        self.update_blocks(
            addr,
            num_pages,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
        );
        self.m_mapped_insecure_memory = self.m_mapped_insecure_memory.saturating_sub(size);
        0
    }

    // -- GetSize --

    /// Get the total size of memory in a given state.
    /// Matches upstream `KPageTableBase::GetSize`.
    pub fn get_size_by_state(&self, state: KMemoryState) -> usize {
        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());
        let mut total = 0usize;
        for block in self
            .m_memory_block_manager
            .find_iterator(self.m_address_space_start)
        {
            let info = block.get_memory_info();
            if info.get_state() == state {
                total += info.get_size();
            }
        }
        total
    }

    pub fn get_code_size(&self) -> usize {
        self.get_size_by_state(KMemoryState::CODE)
    }
    pub fn get_code_data_size(&self) -> usize {
        self.get_size_by_state(KMemoryState::CODE_DATA)
    }
    pub fn get_alias_code_size(&self) -> usize {
        self.get_size_by_state(KMemoryState::ALIAS_CODE)
    }
    pub fn get_alias_code_data_size(&self) -> usize {
        self.get_size_by_state(KMemoryState::ALIAS_CODE_DATA)
    }
    pub fn get_normal_memory_size(&self) -> usize {
        self.m_current_heap_end
            .saturating_sub(self.m_heap_region_start)
            + self.m_mapped_physical_memory_size
    }

    // -- GetContiguousMemoryRangeWithState --

    /// Matches upstream `KPageTableBase::GetContiguousMemoryRangeWithState`.
    /// Returns (phys_addr, size, is_heap) for a contiguous range matching the state.
    pub fn get_contiguous_memory_range_with_state(
        &self,
        addr: usize,
        size: usize,
        state_mask: KMemoryState,
        state: KMemoryState,
        perm_mask: KMemoryPermission,
        perm: KMemoryPermission,
        attr_mask: KMemoryAttribute,
        attr: KMemoryAttribute,
    ) -> Option<(u64, usize, bool)> {
        let (result, _, _, _, _) = self.check_memory_state_range(
            addr,
            size,
            state_mask,
            state,
            perm_mask,
            perm,
            attr_mask,
            attr,
            Self::DEFAULT_MEMORY_IGNORE_ATTR,
        );
        if result != 0 {
            return None;
        }

        // Look the physical address up via the page table — with pool-
        // allocated mappings, identity-mapping (DRAM_BASE + addr) no longer
        // describes the actual phys for arbitrary VAs. Callers expect the
        // contiguous run starting at `addr`; we trust check_memory_state_range
        // already verified the VA range is uniformly mapped.
        let phys_addr = self
            .m_impl
            .as_ref()
            .and_then(|p| p.get_physical_address(addr as u64))?;
        let is_heap = state == KMemoryState::NORMAL;
        Some((phys_addr, size, is_heap))
    }

    // -- Data Cache --

    /// Matches upstream `KPageTableBase::InvalidateProcessDataCache`.
    pub fn invalidate_process_data_cache(&self, addr: usize, size: usize) -> u32 {
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let _lock = KScopedLightLock::new(&self.m_general_lock);
        let (result, _) = self.check_memory_state_contiguous(
            addr,
            size,
            KMemoryState::FLAG_REFERENCE_COUNTED,
            KMemoryState::FLAG_REFERENCE_COUNTED,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::UNCACHED,
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        let Some(impl_pt) = self.m_impl.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some((mut entry, mut context)) = impl_pt.begin_traversal(addr as u64) else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };

        let mut cur_addr = entry.phys_addr;
        let mut cur_size = entry.block_size - ((cur_addr as usize) & (entry.block_size - 1));
        let mut total_size = cur_size;

        while total_size < size {
            let Some(next_entry) = impl_pt.continue_traversal(&mut context) else {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };
            entry = next_entry;

            if entry.phys_addr != cur_addr.wrapping_add(cur_size as u64) {
                if !is_heap_physical_address_for_table(cur_addr) {
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
                cur_addr = entry.phys_addr;
                cur_size = entry.block_size;
            } else {
                cur_size += entry.block_size;
            }

            total_size += entry.block_size;
        }

        if total_size > size {
            cur_size -= total_size - size;
        }

        if cur_size > 0 && !is_heap_physical_address_for_table(cur_addr) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        // Host-side cache maintenance is a no-op, matching the existing emulator
        // cache model after upstream's range/state/traversal validation succeeds.
        0
    }

    /// Matches upstream `KPageTableBase::InvalidateCurrentProcessDataCache`.
    pub fn invalidate_current_process_data_cache(&self, addr: usize, size: usize) -> u32 {
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let _lock = KScopedLightLock::new(&self.m_general_lock);
        let (result, _) = self.check_memory_state_contiguous(
            addr,
            size,
            KMemoryState::FLAG_REFERENCE_COUNTED,
            KMemoryState::FLAG_REFERENCE_COUNTED,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::UNCACHED,
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        // Upstream invalidates the current-process virtual range here. ruzu's
        // host memory model has no separate data cache to invalidate.
        0
    }

    // -- Debug Memory --

    /// Read memory for debugging.
    /// Matches upstream `KPageTableBase::ReadDebugMemory`.
    pub fn read_debug_memory(&self, dst_addr: usize, src_addr: usize, size: usize) -> u32 {
        if !self.contains_range(src_addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let Some(memory) = self.m_memory.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };

        let _lock = KScopedLightLock::new(&self.m_general_lock);
        let (read_result, _) = self.check_memory_state_contiguous(
            src_addr,
            size,
            KMemoryState::NONE,
            KMemoryState::NONE,
            KMemoryPermission::USER_READ,
            KMemoryPermission::USER_READ,
            KMemoryAttribute::NONE,
            KMemoryAttribute::NONE,
        );
        if read_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            let (debug_result, _) = self.check_memory_state_contiguous(
                src_addr,
                size,
                KMemoryState::FLAG_CAN_DEBUG,
                KMemoryState::FLAG_CAN_DEBUG,
                KMemoryPermission::NONE,
                KMemoryPermission::NONE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
            );
            if debug_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return debug_result;
            }
        }

        let Some(impl_pt) = self.m_impl.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some((first_entry, mut traversal_context)) = impl_pt.begin_traversal(src_addr as u64)
        else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        if first_entry.block_size == 0 {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let mut dst_addr = dst_addr;
        let block_mask = first_entry.block_size - 1;
        let mut cur_addr = first_entry.phys_addr;
        let mut cur_size = first_entry
            .block_size
            .saturating_sub((cur_addr as usize) & block_mask);
        let mut total_size = cur_size;

        let perform_copy = |dst_addr: usize, cur_addr: u64, cur_size: usize| -> u32 {
            if !is_linear_mapped_physical_address_for_table(cur_addr) {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            }

            let mut local_dst = dst_addr;
            let mut local_phys = cur_addr;
            let mut local_size = cur_size;
            if local_size >= std::mem::size_of::<u32>() {
                let copy_size = local_size & !(std::mem::size_of::<u32>() - 1);
                if !memory.lock().unwrap().copy_phys_to_guest(
                    local_dst as u64,
                    local_phys,
                    copy_size,
                ) {
                    return svc_results::RESULT_INVALID_POINTER.get_inner_value();
                }
                local_dst += copy_size;
                local_phys += copy_size as u64;
                local_size -= copy_size;
            }

            if local_size > 0
                && !memory.lock().unwrap().copy_phys_to_guest(
                    local_dst as u64,
                    local_phys,
                    local_size,
                )
            {
                return svc_results::RESULT_INVALID_POINTER.get_inner_value();
            }

            crate::hle::result::RESULT_SUCCESS.get_inner_value()
        };

        while total_size < size {
            let Some(next_entry) = impl_pt.continue_traversal(&mut traversal_context) else {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };

            if next_entry.phys_addr != cur_addr + cur_size as u64 {
                let result = perform_copy(dst_addr, cur_addr, cur_size);
                if result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                    return result;
                }
                dst_addr += cur_size;
                cur_addr = next_entry.phys_addr;
                cur_size = next_entry.block_size;
            } else {
                cur_size += next_entry.block_size;
            }

            total_size += next_entry.block_size;
        }

        if total_size > size {
            cur_size -= total_size - size;
        }

        perform_copy(dst_addr, cur_addr, cur_size)
    }

    /// Write memory for debugging.
    /// Matches upstream `KPageTableBase::WriteDebugMemory`.
    pub fn write_debug_memory(&self, dst_addr: usize, src_addr: usize, size: usize) -> u32 {
        if !self.contains_range(dst_addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let Some(memory) = self.m_memory.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };

        let _lock = KScopedLightLock::new(&self.m_general_lock);
        let (write_result, _) = self.check_memory_state_contiguous(
            dst_addr,
            size,
            KMemoryState::NONE,
            KMemoryState::NONE,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryAttribute::NONE,
        );
        if write_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            let (debug_result, _) = self.check_memory_state_contiguous(
                dst_addr,
                size,
                KMemoryState::FLAG_CAN_DEBUG,
                KMemoryState::FLAG_CAN_DEBUG,
                KMemoryPermission::NONE,
                KMemoryPermission::NONE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
            );
            if debug_result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return debug_result;
            }
        }

        let Some(impl_pt) = self.m_impl.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some((first_entry, mut traversal_context)) = impl_pt.begin_traversal(dst_addr as u64)
        else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        if first_entry.block_size == 0 {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let original_dst_addr = dst_addr;
        let mut src_addr = src_addr;
        let block_mask = first_entry.block_size - 1;
        let mut cur_addr = first_entry.phys_addr;
        let mut cur_size = first_entry
            .block_size
            .saturating_sub((cur_addr as usize) & block_mask);
        let mut total_size = cur_size;

        let perform_copy = |src_addr: usize, cur_addr: u64, cur_size: usize| -> u32 {
            if !is_linear_mapped_physical_address_for_table(cur_addr) {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            }

            let mut local_src = src_addr;
            let mut local_phys = cur_addr;
            let mut local_size = cur_size;
            if local_size >= std::mem::size_of::<u32>() {
                let copy_size = local_size & !(std::mem::size_of::<u32>() - 1);
                if !memory.lock().unwrap().copy_guest_to_phys(
                    local_phys,
                    local_src as u64,
                    copy_size,
                ) {
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
                let result = store_data_cache(local_phys, copy_size);
                if result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                    return result;
                }
                local_src += copy_size;
                local_phys += copy_size as u64;
                local_size -= copy_size;
            }

            if local_size > 0 {
                if !memory.lock().unwrap().copy_guest_to_phys(
                    local_phys,
                    local_src as u64,
                    local_size,
                ) {
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
                let result = store_data_cache(local_phys, local_size);
                if result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                    return result;
                }
            }

            crate::hle::result::RESULT_SUCCESS.get_inner_value()
        };

        while total_size < size {
            let Some(next_entry) = impl_pt.continue_traversal(&mut traversal_context) else {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };

            if next_entry.phys_addr != cur_addr + cur_size as u64 {
                let result = perform_copy(src_addr, cur_addr, cur_size);
                if result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                    return result;
                }
                src_addr += cur_size;
                cur_addr = next_entry.phys_addr;
                cur_size = next_entry.block_size;
            } else {
                cur_size += next_entry.block_size;
            }

            total_size += next_entry.block_size;
        }

        if total_size > size {
            cur_size -= total_size - size;
        }

        let result = perform_copy(src_addr, cur_addr, cur_size);
        if result != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            return result;
        }

        invalidate_instruction_cache_for_table(self, original_dst_addr, size);
        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    // -- Copy Memory --

    /// Copy memory between linear (kernel) and user address spaces.
    /// Matches upstream `KPageTableBase::CopyMemoryFromLinearToUser`.
    pub fn copy_memory_from_linear_to_user(
        &self,
        dst_addr: usize,
        size: usize,
        src_addr: usize,
        src_state_mask: KMemoryState,
        src_state: KMemoryState,
        src_test_perm: KMemoryPermission,
        src_attr_mask: KMemoryAttribute,
        src_attr: KMemoryAttribute,
    ) -> u32 {
        if !self.contains_range(src_addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let Some(memory) = self.m_memory.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };

        let _lock = KScopedLightLock::new(&self.m_general_lock);
        let (result, _) = self.check_memory_state_contiguous(
            src_addr,
            size,
            src_state_mask,
            src_state,
            src_test_perm,
            src_test_perm,
            src_attr_mask | KMemoryAttribute::UNCACHED,
            src_attr,
        );
        if result != 0 {
            return result;
        }

        let Some(impl_pt) = self.m_impl.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some((first_entry, mut traversal_context)) = impl_pt.begin_traversal(src_addr as u64)
        else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        if first_entry.block_size == 0 {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let mut dst_addr = dst_addr;
        let block_mask = first_entry.block_size - 1;
        let mut cur_addr = first_entry.phys_addr;
        let mut cur_size = first_entry
            .block_size
            .saturating_sub((cur_addr as usize) & block_mask);
        let mut total_size = cur_size;

        let perform_copy = |dst_addr: usize, cur_addr: u64, cur_size: usize| -> u32 {
            if !is_linear_mapped_physical_address_for_table(cur_addr) {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            }

            let mut local_dst = dst_addr;
            let mut local_phys = cur_addr;
            let mut local_size = cur_size;
            if local_size >= std::mem::size_of::<u32>() {
                let copy_size = local_size & !(std::mem::size_of::<u32>() - 1);
                if !memory.lock().unwrap().copy_phys_to_guest(
                    local_dst as u64,
                    local_phys,
                    copy_size,
                ) {
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
                local_dst += copy_size;
                local_phys += copy_size as u64;
                local_size -= copy_size;
            }

            if local_size > 0
                && !memory.lock().unwrap().copy_phys_to_guest(
                    local_dst as u64,
                    local_phys,
                    local_size,
                )
            {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            }

            0
        };

        while total_size < size {
            let Some(next_entry) = impl_pt.continue_traversal(&mut traversal_context) else {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };

            if next_entry.phys_addr != cur_addr + cur_size as u64 {
                let result = perform_copy(dst_addr, cur_addr, cur_size);
                if result != 0 {
                    return result;
                }
                dst_addr += cur_size;
                cur_addr = next_entry.phys_addr;
                cur_size = next_entry.block_size;
            } else {
                cur_size += next_entry.block_size;
            }

            total_size += next_entry.block_size;
        }

        if total_size > size {
            cur_size -= total_size - size;
        }

        perform_copy(dst_addr, cur_addr, cur_size)
    }

    /// Matches upstream `KPageTableBase::CopyMemoryFromLinearToKernel`.
    pub fn copy_memory_from_linear_to_kernel(
        &self,
        buffer: usize,
        size: usize,
        src_addr: usize,
        src_state_mask: KMemoryState,
        src_state: KMemoryState,
        src_test_perm: KMemoryPermission,
        src_attr_mask: KMemoryAttribute,
        src_attr: KMemoryAttribute,
    ) -> u32 {
        if !self.contains_range(src_addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }
        if size != 0 && buffer == 0 {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let _lock = KScopedLightLock::new(&self.m_general_lock);
        let (result, _) = self.check_memory_state_contiguous(
            src_addr,
            size,
            src_state_mask,
            src_state,
            src_test_perm,
            src_test_perm,
            src_attr_mask | KMemoryAttribute::UNCACHED,
            src_attr,
        );
        if result != 0 {
            return result;
        }

        let Some(impl_pt) = self.m_impl.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some(memory) = self.m_memory.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some((first_entry, mut traversal_context)) = impl_pt.begin_traversal(src_addr as u64)
        else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        if first_entry.block_size == 0 {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let mut dst = buffer;
        let block_mask = first_entry.block_size - 1;
        let mut cur_addr = first_entry.phys_addr;
        let mut cur_size = first_entry
            .block_size
            .saturating_sub((cur_addr as usize) & block_mask);
        let mut total_size = cur_size;

        let perform_copy = |dst: usize, cur_addr: u64, cur_size: usize| -> u32 {
            if !is_linear_mapped_physical_address_for_table(cur_addr) {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            }
            let dst_slice = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, cur_size) };
            if !memory.lock().unwrap().read_phys_block(cur_addr, dst_slice) {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            }
            0
        };

        while total_size < size {
            let Some(next_entry) = impl_pt.continue_traversal(&mut traversal_context) else {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };

            if next_entry.phys_addr != cur_addr + cur_size as u64 {
                let result = perform_copy(dst, cur_addr, cur_size);
                if result != 0 {
                    return result;
                }
                dst += cur_size;
                cur_addr = next_entry.phys_addr;
                cur_size = next_entry.block_size;
            } else {
                cur_size += next_entry.block_size;
            }

            total_size += next_entry.block_size;
        }

        if total_size > size {
            cur_size -= total_size - size;
        }

        perform_copy(dst, cur_addr, cur_size)
    }

    /// Matches upstream `KPageTableBase::CopyMemoryFromUserToLinear`.
    pub fn copy_memory_from_user_to_linear(
        &self,
        dst_addr: usize,
        size: usize,
        dst_state_mask: KMemoryState,
        dst_state: KMemoryState,
        dst_test_perm: KMemoryPermission,
        dst_attr_mask: KMemoryAttribute,
        dst_attr: KMemoryAttribute,
        src_addr: usize,
    ) -> u32 {
        if !self.contains_range(dst_addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let Some(memory) = self.m_memory.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };

        let _lock = KScopedLightLock::new(&self.m_general_lock);
        let (result, _) = self.check_memory_state_contiguous(
            dst_addr,
            size,
            dst_state_mask,
            dst_state,
            dst_test_perm,
            dst_test_perm,
            dst_attr_mask | KMemoryAttribute::UNCACHED,
            dst_attr,
        );
        if result != 0 {
            return result;
        }

        let Some(impl_pt) = self.m_impl.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some((first_entry, mut traversal_context)) = impl_pt.begin_traversal(dst_addr as u64)
        else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        if first_entry.block_size == 0 {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let mut src_addr = src_addr;
        let block_mask = first_entry.block_size - 1;
        let mut cur_addr = first_entry.phys_addr;
        let mut cur_size = first_entry
            .block_size
            .saturating_sub((cur_addr as usize) & block_mask);
        let mut total_size = cur_size;

        let perform_copy = |src_addr: usize, cur_addr: u64, cur_size: usize| -> u32 {
            if !is_linear_mapped_physical_address_for_table(cur_addr) {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            }

            let mut local_src = src_addr;
            let mut local_phys = cur_addr;
            let mut local_size = cur_size;
            if local_size >= std::mem::size_of::<u32>() {
                let copy_size = local_size & !(std::mem::size_of::<u32>() - 1);
                if !memory.lock().unwrap().copy_guest_to_phys(
                    local_phys,
                    local_src as u64,
                    copy_size,
                ) {
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
                local_src += copy_size;
                local_phys += copy_size as u64;
                local_size -= copy_size;
            }

            if local_size > 0
                && !memory.lock().unwrap().copy_guest_to_phys(
                    local_phys,
                    local_src as u64,
                    local_size,
                )
            {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            }

            0
        };

        while total_size < size {
            let Some(next_entry) = impl_pt.continue_traversal(&mut traversal_context) else {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };

            if next_entry.phys_addr != cur_addr + cur_size as u64 {
                let result = perform_copy(src_addr, cur_addr, cur_size);
                if result != 0 {
                    return result;
                }
                src_addr += cur_size;
                cur_addr = next_entry.phys_addr;
                cur_size = next_entry.block_size;
            } else {
                cur_size += next_entry.block_size;
            }

            total_size += next_entry.block_size;
        }

        if total_size > size {
            cur_size -= total_size - size;
        }

        perform_copy(src_addr, cur_addr, cur_size)
    }

    /// Matches upstream `KPageTableBase::CopyMemoryFromKernelToLinear`.
    pub fn copy_memory_from_kernel_to_linear(
        &self,
        dst_addr: usize,
        size: usize,
        dst_state_mask: KMemoryState,
        dst_state: KMemoryState,
        dst_test_perm: KMemoryPermission,
        dst_attr_mask: KMemoryAttribute,
        dst_attr: KMemoryAttribute,
        buffer: usize,
    ) -> u32 {
        if !self.contains_range(dst_addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }
        if size != 0 && buffer == 0 {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let Some(memory) = self.m_memory.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };

        let _lock = KScopedLightLock::new(&self.m_general_lock);
        let (result, _) = self.check_memory_state_contiguous(
            dst_addr,
            size,
            dst_state_mask,
            dst_state,
            dst_test_perm,
            dst_test_perm,
            dst_attr_mask | KMemoryAttribute::UNCACHED,
            dst_attr,
        );
        if result != 0 {
            return result;
        }

        let Some(impl_pt) = self.m_impl.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some((first_entry, mut traversal_context)) = impl_pt.begin_traversal(dst_addr as u64)
        else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        if first_entry.block_size == 0 {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let mut src = buffer;
        let block_mask = first_entry.block_size - 1;
        let mut cur_addr = first_entry.phys_addr;
        let mut cur_size = first_entry
            .block_size
            .saturating_sub((cur_addr as usize) & block_mask);
        let mut total_size = cur_size;

        let perform_copy = |src: usize, cur_addr: u64, cur_size: usize| -> u32 {
            if !is_linear_mapped_physical_address_for_table(cur_addr) {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            }
            let src_slice = unsafe { std::slice::from_raw_parts(src as *const u8, cur_size) };
            if !memory.lock().unwrap().write_phys_block(cur_addr, src_slice) {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            }
            0
        };

        while total_size < size {
            let Some(next_entry) = impl_pt.continue_traversal(&mut traversal_context) else {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };

            if next_entry.phys_addr != cur_addr + cur_size as u64 {
                let result = perform_copy(src, cur_addr, cur_size);
                if result != 0 {
                    return result;
                }
                src += cur_size;
                cur_addr = next_entry.phys_addr;
                cur_size = next_entry.block_size;
            } else {
                cur_size += next_entry.block_size;
            }

            total_size += next_entry.block_size;
        }

        if total_size > size {
            cur_size -= total_size - size;
        }

        perform_copy(src, cur_addr, cur_size)
    }

    /// Matches upstream `KPageTableBase::CopyMemoryFromHeapToHeap`.
    pub fn copy_memory_from_heap_to_heap(
        &self,
        dst_table: &KPageTableBase,
        dst_addr: usize,
        size: usize,
        dst_state_mask: KMemoryState,
        dst_state: KMemoryState,
        dst_test_perm: KMemoryPermission,
        dst_attr_mask: KMemoryAttribute,
        dst_attr: KMemoryAttribute,
        src_addr: usize,
        src_state_mask: KMemoryState,
        src_state: KMemoryState,
        src_test_perm: KMemoryPermission,
        src_attr_mask: KMemoryAttribute,
        src_attr: KMemoryAttribute,
    ) -> u32 {
        self.copy_memory_from_heap_to_heap_impl(
            dst_table,
            dst_addr,
            size,
            dst_state_mask,
            dst_state,
            dst_test_perm,
            dst_attr_mask,
            dst_attr,
            src_addr,
            src_state_mask,
            src_state,
            src_test_perm,
            src_attr_mask,
            src_attr,
            true,
        )
    }

    /// Matches upstream `KPageTableBase::CopyMemoryFromHeapToHeapWithoutCheckDestination`.
    pub fn copy_memory_from_heap_to_heap_without_check_destination(
        &self,
        dst_table: &KPageTableBase,
        dst_addr: usize,
        size: usize,
        dst_state_mask: KMemoryState,
        dst_state: KMemoryState,
        dst_test_perm: KMemoryPermission,
        dst_attr_mask: KMemoryAttribute,
        dst_attr: KMemoryAttribute,
        src_addr: usize,
        src_state_mask: KMemoryState,
        src_state: KMemoryState,
        src_test_perm: KMemoryPermission,
        src_attr_mask: KMemoryAttribute,
        src_attr: KMemoryAttribute,
    ) -> u32 {
        self.copy_memory_from_heap_to_heap_impl(
            dst_table,
            dst_addr,
            size,
            dst_state_mask,
            dst_state,
            dst_test_perm,
            dst_attr_mask,
            dst_attr,
            src_addr,
            src_state_mask,
            src_state,
            src_test_perm,
            src_attr_mask,
            src_attr,
            false,
        )
    }

    fn copy_memory_from_heap_to_heap_impl(
        &self,
        dst_table: &KPageTableBase,
        dst_addr: usize,
        size: usize,
        dst_state_mask: KMemoryState,
        dst_state: KMemoryState,
        dst_test_perm: KMemoryPermission,
        dst_attr_mask: KMemoryAttribute,
        dst_attr: KMemoryAttribute,
        src_addr: usize,
        src_state_mask: KMemoryState,
        src_state: KMemoryState,
        src_test_perm: KMemoryPermission,
        src_attr_mask: KMemoryAttribute,
        src_attr: KMemoryAttribute,
        check_destination: bool,
    ) -> u32 {
        if !self.contains_range(src_addr, size) || !dst_table.contains_range(dst_addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let Some(memory) = self.m_memory.as_ref().or(dst_table.m_memory.as_ref()) else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };

        let _lock_pair = KScopedLightLockPair::new(
            self.m_general_lock.clone(),
            dst_table.m_general_lock.clone(),
        );

        let (result, _) = self.check_memory_state_contiguous(
            src_addr,
            size,
            src_state_mask,
            src_state,
            src_test_perm,
            src_test_perm,
            src_attr_mask | KMemoryAttribute::UNCACHED,
            src_attr,
        );
        if result != 0 {
            return result;
        }
        if check_destination {
            let (result, _) = dst_table.check_memory_state_contiguous(
                dst_addr,
                size,
                dst_state_mask,
                dst_state,
                dst_test_perm,
                dst_test_perm,
                dst_attr_mask | KMemoryAttribute::UNCACHED,
                dst_attr,
            );
            if result != 0 {
                return result;
            }
        }

        let Some(src_impl) = self.m_impl.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some(dst_impl) = dst_table.m_impl.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some((mut src_entry, mut src_context)) = src_impl.begin_traversal(src_addr as u64)
        else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        let Some((mut dst_entry, mut dst_context)) = dst_impl.begin_traversal(dst_addr as u64)
        else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };
        if src_entry.block_size == 0 || dst_entry.block_size == 0 {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let mut cur_src_block_addr = src_entry.phys_addr;
        let mut cur_dst_block_addr = dst_entry.phys_addr;
        let mut cur_src_size =
            src_entry.block_size - ((cur_src_block_addr as usize) & (src_entry.block_size - 1));
        let mut cur_dst_size =
            dst_entry.block_size - ((cur_dst_block_addr as usize) & (dst_entry.block_size - 1));
        src_entry.block_size = cur_src_size;
        dst_entry.block_size = cur_dst_size;

        if size == 0 {
            return 0;
        }

        let mut cur_src_addr = cur_src_block_addr;
        let mut cur_dst_addr = cur_dst_block_addr;
        let mut cur_min_size = cur_src_size.min(cur_dst_size);
        let mut offset = 0usize;

        while offset < size {
            let cur_copy_size = cur_min_size.min(size - offset);
            let mut updated_src = false;
            let mut updated_dst = false;
            let mut skip_copy = false;

            if offset + cur_copy_size != size {
                if cur_src_addr + cur_min_size as u64 == cur_src_block_addr + cur_src_size as u64 {
                    let Some(next_entry) = src_impl.continue_traversal(&mut src_context) else {
                        return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                    };
                    updated_src = cur_src_addr + cur_min_size as u64 != next_entry.phys_addr;
                    src_entry = next_entry;
                }

                if cur_dst_addr + cur_min_size as u64
                    == dst_entry.phys_addr + dst_entry.block_size as u64
                {
                    let Some(next_entry) = dst_impl.continue_traversal(&mut dst_context) else {
                        return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                    };
                    updated_dst = cur_dst_addr + cur_min_size as u64 != next_entry.phys_addr;
                    dst_entry = next_entry;
                }

                if !updated_src && !updated_dst {
                    skip_copy = true;
                    cur_src_block_addr = src_entry.phys_addr;
                }
            }

            if !skip_copy {
                if !is_heap_physical_address_for_table(cur_src_addr)
                    || !is_heap_physical_address_for_table(cur_dst_addr)
                {
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
                if !memory.lock().unwrap().copy_phys_to_phys(
                    cur_dst_addr,
                    cur_src_addr,
                    cur_copy_size,
                ) {
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }

                cur_src_block_addr = src_entry.phys_addr;
                cur_src_addr = if updated_src {
                    cur_src_block_addr
                } else {
                    cur_src_addr + cur_copy_size as u64
                };
                cur_dst_block_addr = dst_entry.phys_addr;
                cur_dst_addr = if updated_dst {
                    cur_dst_block_addr
                } else {
                    cur_dst_addr + cur_copy_size as u64
                };
                offset += cur_copy_size;
            }

            cur_src_size = src_entry.block_size;
            cur_dst_size = dst_entry.block_size;
            let remaining_src =
                cur_src_block_addr.saturating_add(cur_src_size as u64) - cur_src_addr;
            let remaining_dst =
                cur_dst_block_addr.saturating_add(cur_dst_size as u64) - cur_dst_addr;
            cur_min_size = (remaining_src as usize).min(remaining_dst as usize);
        }

        0
    }

    // -- IPC Setup/Cleanup --

    fn get_ipc_test_state_and_attr_mask(
        dst_state: KMemoryState,
    ) -> Option<(KMemoryState, KMemoryAttribute)> {
        match dst_state {
            s if s == KMemoryState::IPC => Some((
                KMemoryState::FLAG_CAN_USE_IPC,
                KMemoryAttribute::UNCACHED
                    | KMemoryAttribute::DEVICE_SHARED
                    | KMemoryAttribute::LOCKED,
            )),
            s if s == KMemoryState::NON_SECURE_IPC => Some((
                KMemoryState::FLAG_CAN_USE_NON_SECURE_IPC,
                KMemoryAttribute::UNCACHED | KMemoryAttribute::LOCKED,
            )),
            s if s == KMemoryState::NON_DEVICE_IPC => Some((
                KMemoryState::FLAG_CAN_USE_NON_DEVICE_IPC,
                KMemoryAttribute::UNCACHED | KMemoryAttribute::LOCKED,
            )),
            _ => None,
        }
    }

    fn get_ipc_source_permission(test_perm: KMemoryPermission) -> KMemoryPermission {
        if test_perm == KMemoryPermission::USER_READ_WRITE {
            KMemoryPermission::KERNEL_READ_WRITE | KMemoryPermission::NOT_MAPPED
        } else {
            KMemoryPermission::USER_READ
        }
    }

    fn get_ipc_aligned_extents(address: usize, size: usize) -> (usize, usize, usize, usize) {
        let aligned_start = address & !(PAGE_SIZE - 1);
        let aligned_end = (address + size).next_multiple_of(PAGE_SIZE);
        let mapping_start = address.next_multiple_of(PAGE_SIZE);
        let mapping_end = (address + size) & !(PAGE_SIZE - 1);
        (aligned_start, aligned_end, mapping_start, mapping_end)
    }

    fn reprotect_ipc_range(&mut self, address: usize, size: usize) -> u32 {
        if size == 0 {
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }

        let end = address + size;
        let mut current = address;
        while current < end {
            let Some(info) = self.query_info(current) else {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };

            let chunk_size = std::cmp::min(info.m_size, end - current);
            debug_assert!(chunk_size % PAGE_SIZE == 0);

            let props = KPageProperties {
                perm: info.m_permission,
                io: false,
                uncached: false,
                disable_merge_attributes: DisableMergeAttribute::NONE,
            };
            let rc = self.operate(
                None,
                current,
                chunk_size / PAGE_SIZE,
                0,
                false,
                props,
                OperationType::ChangePermissions,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }

            current += chunk_size;
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    fn with_memory_page_table<T, F>(
        memory: &Arc<Mutex<Memory>>,
        page_table: *mut common::page_table::PageTable,
        f: F,
    ) -> T
    where
        F: FnOnce(&mut Memory) -> T,
    {
        let mut memory = memory.lock().unwrap();
        let old_page_table = memory.current_page_table_raw();
        memory.set_current_page_table_raw(page_table);
        let result = f(&mut memory);
        memory.set_current_page_table_raw(old_page_table);
        result
    }

    fn read_block_from_page_table(
        memory: &Arc<Mutex<Memory>>,
        page_table: *mut common::page_table::PageTable,
        src_addr: usize,
        size: usize,
    ) -> Option<Vec<u8>> {
        let mut bytes = vec![0u8; size];
        let success = Self::with_memory_page_table(memory, page_table, |memory| {
            memory.read_block(src_addr as u64, &mut bytes)
        });
        success.then_some(bytes)
    }

    fn write_block_to_page_table(
        memory: &Arc<Mutex<Memory>>,
        page_table: *mut common::page_table::PageTable,
        dst_addr: usize,
        bytes: &[u8],
    ) -> bool {
        Self::with_memory_page_table(memory, page_table, |memory| {
            memory.write_block(dst_addr as u64, bytes)
        })
    }

    pub(crate) fn read_block_from_own_page_table(
        &self,
        src_addr: usize,
        size: usize,
    ) -> Option<Vec<u8>> {
        let memory = self.m_memory.as_ref()?;
        let page_table =
            self.m_impl.as_ref()?.as_ref() as *const common::page_table::PageTable as *mut _;
        Self::read_block_from_page_table(memory, page_table, src_addr, size)
    }

    pub(crate) fn has_own_page_table_memory(&self) -> bool {
        self.m_memory.is_some() && self.m_impl.is_some()
    }

    pub(crate) fn write_block_to_own_page_table(&self, dst_addr: usize, bytes: &[u8]) -> bool {
        let Some(memory) = self.m_memory.as_ref() else {
            return false;
        };
        let Some(page_table) = self.m_impl.as_ref() else {
            return false;
        };
        Self::write_block_to_page_table(
            memory,
            page_table.as_ref() as *const common::page_table::PageTable as *mut _,
            dst_addr,
            bytes,
        )
    }

    fn build_ipc_partial_page(
        fill_value: u8,
        send: bool,
        dst_copy_offset: usize,
        src: &[u8],
    ) -> Vec<u8> {
        let mut page = vec![fill_value; PAGE_SIZE];
        if send && !src.is_empty() {
            let copy_end = dst_copy_offset + src.len();
            debug_assert!(copy_end <= PAGE_SIZE);
            page[dst_copy_offset..copy_end].copy_from_slice(src);
        }
        page
    }

    /// Matches upstream `KPageTableBase::SetupForIpcClient`.
    pub fn setup_for_ipc_client(
        &mut self,
        addr: usize,
        size: usize,
        test_perm: KMemoryPermission,
        dst_state: KMemoryState,
    ) -> (u32, usize) {
        if size == 0 {
            return (crate::hle::result::RESULT_SUCCESS.get_inner_value(), 0);
        }
        if !self.contains_range(addr, size) {
            return (
                svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                0,
            );
        }

        let Some((test_state, test_attr_mask)) = Self::get_ipc_test_state_and_attr_mask(dst_state)
        else {
            return (svc_results::RESULT_INVALID_COMBINATION.get_inner_value(), 0);
        };

        let src_perm = Self::get_ipc_source_permission(test_perm);
        let (aligned_start, aligned_end, mapping_start, mapping_end) =
            Self::get_ipc_aligned_extents(addr, size);
        let aligned_last = aligned_end - 1;
        let mapping_last = mapping_end.saturating_sub(1);
        let mut mapped_size = 0usize;
        let mut blocks_needed = 0usize;

        // Snapshot the block infos before mutating the block manager through
        // `operate`, matching upstream's iterator walk while avoiding Rust
        // aliasing between the iterator and mutable page-table updates.
        let infos: Vec<KMemoryInfo> = self
            .m_memory_block_manager
            .find_iterator(aligned_start)
            .map(|block| block.get_memory_info())
            .collect();

        for info in infos {
            let rc = Self::check_memory_state_info(
                &info,
                test_state,
                test_state,
                test_perm,
                test_perm,
                test_attr_mask,
                KMemoryAttribute::NONE,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                if mapped_size > 0 {
                    self.cleanup_for_ipc_client_on_server_setup_failure(
                        mapping_start,
                        mapped_size,
                        src_perm,
                    );
                }
                return (rc, 0);
            }

            if mapping_start < mapping_end
                && mapping_start < info.get_end_address()
                && info.get_address() < mapping_end
            {
                let cur_start = if info.get_address() >= mapping_start {
                    info.get_address()
                } else {
                    mapping_start
                };
                let cur_end = if mapping_last >= info.get_last_address() {
                    info.get_end_address()
                } else {
                    mapping_end
                };
                let cur_size = cur_end - cur_start;

                if info.get_address() < mapping_start {
                    blocks_needed += 1;
                }
                if mapping_last < info.get_last_address() {
                    blocks_needed += 1;
                }

                if (info.get_permission() & KMemoryPermission::IPC_LOCK_CHANGE_MASK) != src_perm {
                    let head_body_attr = if mapping_start >= info.get_address() {
                        DisableMergeAttribute::DISABLE_HEAD_AND_BODY
                    } else {
                        DisableMergeAttribute::NONE
                    };
                    let tail_attr = if cur_end == mapping_end {
                        DisableMergeAttribute::DISABLE_TAIL
                    } else {
                        DisableMergeAttribute::NONE
                    };
                    let properties = KPageProperties {
                        perm: src_perm,
                        io: false,
                        uncached: false,
                        disable_merge_attributes: head_body_attr | tail_attr,
                    };
                    let rc = self.operate(
                        None,
                        cur_start,
                        cur_size / PAGE_SIZE,
                        0,
                        false,
                        properties,
                        OperationType::ChangePermissions,
                    );
                    if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                        if mapped_size > 0 {
                            self.cleanup_for_ipc_client_on_server_setup_failure(
                                mapping_start,
                                mapped_size,
                                src_perm,
                            );
                        }
                        return (rc, 0);
                    }
                }

                mapped_size += cur_size;
            }

            if aligned_last <= info.get_last_address() {
                break;
            }
        }

        (
            crate::hle::result::RESULT_SUCCESS.get_inner_value(),
            blocks_needed,
        )
    }

    /// Matches upstream `KPageTableBase::SetupForIpc`.
    pub fn setup_for_ipc(
        &mut self,
        out_dst_addr: &mut usize,
        size: usize,
        src_addr: usize,
        src_page_table: &mut KPageTableBase,
        test_perm: KMemoryPermission,
        dst_state: KMemoryState,
        send: bool,
    ) -> u32 {
        let _lk = KScopedLightLockPair::new(
            src_page_table.m_general_lock.clone(),
            self.m_general_lock.clone(),
        );

        let (client_rc, num_allocator_blocks) =
            src_page_table.setup_for_ipc_client(src_addr, size, test_perm, dst_state);
        if client_rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            return client_rc;
        }

        let mut allocator = match src_page_table.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };

        let server_rc = self.setup_for_ipc_server(
            out_dst_addr,
            size,
            src_addr,
            test_perm,
            dst_state,
            src_page_table,
            send,
        );
        if server_rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            // Upstream `KPageTableBase::SetupForIpc` uses ON_RESULT_FAILURE:
            // ```
            // src_page_table.CleanupForIpcClientOnServerSetupFailure(
            //     updater.GetPageList(), src_map_start, src_map_size, src_perm);
            // ```
            // where `src_map_start`/`src_map_size` are the mapping-aligned extents
            // and `src_perm` is the post-setup permission derived from `test_perm`.
            // This restores block permissions only; IPC-locking has not happened
            // yet at this point, so `cleanup_for_ipc_client` (which expects
            // `IPC_LOCKED`) would be the wrong rollback path here.
            let (_, _, mapping_src_start, mapping_src_end) =
                Self::get_ipc_aligned_extents(src_addr, size);
            let mapping_src_size = mapping_src_end.saturating_sub(mapping_src_start);
            if mapping_src_size > 0 {
                let src_perm = Self::get_ipc_source_permission(test_perm);
                src_page_table.cleanup_for_ipc_client_on_server_setup_failure(
                    mapping_src_start,
                    mapping_src_size,
                    src_perm,
                );
            }
            return server_rc;
        }

        // The current Rust backend still falls back to the source address when
        // the page-table backend cannot materialize a distinct alias mapping.
        // Keep that adaptation in the page-table owner, not in KServerSession.
        if size > 0 && *out_dst_addr == 0 {
            *out_dst_addr = src_addr;
        }

        let (_, _, mapping_src_start, mapping_src_end) =
            Self::get_ipc_aligned_extents(src_addr, size);
        let mapping_src_size = mapping_src_end.saturating_sub(mapping_src_start);
        if mapping_src_size > 0 {
            let src_perm = Self::get_ipc_source_permission(test_perm);
            src_page_table
                .m_memory_block_manager
                .update_lock_with_allocator(
                    Some(&mut allocator),
                    mapping_src_start,
                    mapping_src_size / PAGE_SIZE,
                    src_perm,
                    |block, new_perm, is_first, is_last| {
                        block.lock_for_ipc(new_perm, is_first, is_last)
                    },
                );
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    /// Matches upstream `KPageTableBase::SetupForIpcServer`.
    pub fn setup_for_ipc_server(
        &mut self,
        out_addr: &mut usize,
        size: usize,
        src_addr: usize,
        test_perm: KMemoryPermission,
        dst_state: KMemoryState,
        src_table: &mut KPageTableBase,
        send: bool,
    ) -> u32 {
        let region_size = self
            .m_alias_region_end
            .saturating_sub(self.m_alias_region_start);
        if size >= region_size {
            return svc_results::RESULT_OUT_OF_ADDRESS_SPACE.get_inner_value();
        }

        let (aligned_src_start, aligned_src_end, mapping_src_start, mapping_src_end) =
            Self::get_ipc_aligned_extents(src_addr, size);
        let aligned_src_size = aligned_src_end.saturating_sub(aligned_src_start);
        let mapping_src_size = mapping_src_end.saturating_sub(mapping_src_start);

        if aligned_src_size == 0 {
            *out_addr = 0;
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }

        let dst_addr = self.find_free_area(
            self.m_alias_region_start,
            region_size / PAGE_SIZE,
            aligned_src_size / PAGE_SIZE,
            PAGE_SIZE,
            aligned_src_start & (PAGE_SIZE - 1),
            self.get_num_guard_pages(),
        );
        if dst_addr == 0 {
            return svc_results::RESULT_OUT_OF_ADDRESS_SPACE.get_inner_value();
        }
        if !self.can_contain_k(dst_addr, aligned_src_size, dst_state) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let mut block_allocator = match self.make_block_update_allocator(
            super::k_dynamic_resource_manager::KMemoryBlockManagerUpdateAllocator::MAX_BLOCKS,
        ) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };

        let unmapped_size = aligned_src_size.saturating_sub(mapping_src_size);
        if unmapped_size > 0 && self.m_resource_limit.is_none() {
            return svc_results::RESULT_LIMIT_REACHED.get_inner_value();
        }
        let mut memory_reservation =
            super::k_scoped_resource_reservation::KScopedResourceReservation::new(
                self.m_resource_limit.clone(),
                LimitableResource::PhysicalMemoryMax,
                unmapped_size as i64,
            );
        if !memory_reservation.succeeded() {
            return svc_results::RESULT_LIMIT_REACHED.get_inner_value();
        }

        fn rollback_ipc_server_mapping(
            page_table: &mut KPageTableBase,
            page_list: Option<&mut PageLinkedList>,
            dst_addr: usize,
            mapped_pages: usize,
        ) {
            if mapped_pages == 0 {
                return;
            }
            let unmap_props = KPageProperties {
                perm: KMemoryPermission::NONE,
                io: false,
                uncached: false,
                disable_merge_attributes: DisableMergeAttribute::NONE,
            };
            let _ = page_table.operate(
                page_list,
                dst_addr,
                mapped_pages,
                0,
                false,
                unmap_props,
                OperationType::Unmap,
            );
        }

        fn close_ipc_partial_page(phys_addr: u64) {
            if phys_addr != 0 {
                if let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut() {
                    kernel.memory_manager_mut().close(phys_addr, 1);
                }
            }
        }

        struct IpcPartialPageGuard {
            phys_addr: u64,
        }
        impl IpcPartialPageGuard {
            fn new(phys_addr: u64) -> Self {
                Self { phys_addr }
            }
        }
        impl Drop for IpcPartialPageGuard {
            fn drop(&mut self) {
                close_ipc_partial_page(self.phys_addr);
            }
        }

        let map_properties = KPageProperties {
            perm: test_perm,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD,
        };

        let memory = self.m_memory.clone().or_else(|| src_table.m_memory.clone());
        let mut updater = KScopedPageTableUpdater::from_mut(self);
        let dst_page_table = self
            .m_impl
            .as_deref_mut()
            .map(|pt| pt as *mut common::page_table::PageTable)
            .unwrap_or(std::ptr::null_mut());
        let src_page_table = src_table
            .m_impl
            .as_deref_mut()
            .map(|pt| pt as *mut common::page_table::PageTable)
            .unwrap_or(std::ptr::null_mut());
        let fill_value = self.m_ipc_fill_value as u8;

        let mut mapped_pages = 0usize;
        let mut cur_mapped_addr = dst_addr;
        let mut start_partial_page = 0u64;
        let mut end_partial_page = 0u64;

        if aligned_src_start < mapping_src_start {
            start_partial_page = if let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut()
            {
                kernel.memory_manager_mut().allocate_and_open_continuous(
                    1,
                    1,
                    self.m_allocate_option,
                )
            } else {
                0
            };
            if start_partial_page == 0 {
                return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
            }
        }

        if mapping_src_end < aligned_src_end
            && (aligned_src_start < mapping_src_end || aligned_src_start == mapping_src_start)
        {
            end_partial_page = if let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut() {
                kernel.memory_manager_mut().allocate_and_open_continuous(
                    1,
                    1,
                    self.m_allocate_option,
                )
            } else {
                0
            };
            if end_partial_page == 0 {
                return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
            }
        }

        let _start_partial_guard = IpcPartialPageGuard::new(start_partial_page);
        let _end_partial_guard = IpcPartialPageGuard::new(end_partial_page);

        if start_partial_page != 0 {
            let rc = self.operate(
                Some(updater.page_list()),
                cur_mapped_addr,
                1,
                start_partial_page,
                true,
                map_properties,
                OperationType::Map,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                rollback_ipc_server_mapping(
                    self,
                    Some(updater.page_list()),
                    dst_addr,
                    mapped_pages,
                );
                return rc;
            }

            if let Some(memory) = &memory {
                let partial_offset = src_addr - aligned_src_start;
                let copy_size = if src_addr + size < mapping_src_start {
                    size
                } else {
                    mapping_src_start - src_addr
                };
                let src_bytes = if send && copy_size > 0 {
                    let Some(bytes) = Self::read_block_from_page_table(
                        memory,
                        src_page_table,
                        src_addr,
                        copy_size,
                    ) else {
                        rollback_ipc_server_mapping(
                            self,
                            Some(updater.page_list()),
                            dst_addr,
                            mapped_pages,
                        );
                        return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                    };
                    bytes
                } else {
                    Vec::new()
                };
                let page_bytes =
                    Self::build_ipc_partial_page(fill_value, send, partial_offset, &src_bytes);
                if !Self::write_block_to_page_table(
                    memory,
                    dst_page_table,
                    cur_mapped_addr,
                    &page_bytes,
                ) {
                    rollback_ipc_server_mapping(
                        self,
                        Some(updater.page_list()),
                        dst_addr,
                        mapped_pages,
                    );
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
            }

            cur_mapped_addr += PAGE_SIZE;
        }

        if mapping_src_start < mapping_src_end {
            let Some(src_impl) = src_table.m_impl.as_ref() else {
                rollback_ipc_server_mapping(
                    self,
                    Some(updater.page_list()),
                    dst_addr,
                    mapped_pages,
                );
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };
            let Some((first_entry, mut traversal_context)) =
                src_impl.begin_traversal(mapping_src_start as u64)
            else {
                rollback_ipc_server_mapping(
                    self,
                    Some(updater.page_list()),
                    dst_addr,
                    mapped_pages,
                );
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };

            let mut run_phys_addr = first_entry.phys_addr;
            let mut run_pages = 1usize;
            let mut next_src_addr = mapping_src_start + PAGE_SIZE;

            while next_src_addr < mapping_src_end {
                let Some(next_entry) = src_impl.continue_traversal(&mut traversal_context) else {
                    rollback_ipc_server_mapping(
                        self,
                        Some(updater.page_list()),
                        dst_addr,
                        mapped_pages,
                    );
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                };

                if next_entry.phys_addr == run_phys_addr + (run_pages * PAGE_SIZE) as u64 {
                    run_pages += 1;
                } else {
                    let direct_map_properties = KPageProperties {
                        perm: test_perm,
                        io: false,
                        uncached: false,
                        disable_merge_attributes: if cur_mapped_addr == dst_addr {
                            DisableMergeAttribute::DISABLE_HEAD
                        } else {
                            DisableMergeAttribute::NONE
                        },
                    };
                    let rc = self.operate(
                        Some(updater.page_list()),
                        cur_mapped_addr,
                        run_pages,
                        run_phys_addr,
                        true,
                        direct_map_properties,
                        OperationType::Map,
                    );
                    if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                        rollback_ipc_server_mapping(
                            self,
                            Some(updater.page_list()),
                            dst_addr,
                            mapped_pages,
                        );
                        return rc;
                    }
                    mapped_pages += run_pages;
                    cur_mapped_addr += run_pages * PAGE_SIZE;
                    run_phys_addr = next_entry.phys_addr;
                    run_pages = 1;
                }

                next_src_addr += PAGE_SIZE;
            }

            let direct_map_properties = KPageProperties {
                perm: test_perm,
                io: false,
                uncached: false,
                disable_merge_attributes: if cur_mapped_addr == dst_addr {
                    DisableMergeAttribute::DISABLE_HEAD
                } else {
                    DisableMergeAttribute::NONE
                },
            };
            let rc = self.operate(
                Some(updater.page_list()),
                cur_mapped_addr,
                run_pages,
                run_phys_addr,
                true,
                direct_map_properties,
                OperationType::Map,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                rollback_ipc_server_mapping(
                    self,
                    Some(updater.page_list()),
                    dst_addr,
                    mapped_pages,
                );
                return rc;
            }
            mapped_pages += run_pages;
            cur_mapped_addr += run_pages * PAGE_SIZE;
        }

        if end_partial_page != 0 {
            let end_map_properties = KPageProperties {
                perm: test_perm,
                io: false,
                uncached: false,
                disable_merge_attributes: if cur_mapped_addr == dst_addr {
                    DisableMergeAttribute::DISABLE_HEAD
                } else {
                    DisableMergeAttribute::NONE
                },
            };
            let rc = self.operate(
                Some(updater.page_list()),
                cur_mapped_addr,
                1,
                end_partial_page,
                true,
                end_map_properties,
                OperationType::Map,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                rollback_ipc_server_mapping(
                    self,
                    Some(updater.page_list()),
                    dst_addr,
                    mapped_pages,
                );
                return rc;
            }
            mapped_pages += 1;

            if let Some(memory) = &memory {
                let copy_size = src_addr + size - mapping_src_end;
                let src_bytes = if send && copy_size > 0 {
                    let Some(bytes) = Self::read_block_from_page_table(
                        memory,
                        src_page_table,
                        mapping_src_end,
                        copy_size,
                    ) else {
                        rollback_ipc_server_mapping(
                            self,
                            Some(updater.page_list()),
                            dst_addr,
                            mapped_pages,
                        );
                        return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                    };
                    bytes
                } else {
                    Vec::new()
                };
                let page_bytes = Self::build_ipc_partial_page(fill_value, send, 0, &src_bytes);
                if !Self::write_block_to_page_table(
                    memory,
                    dst_page_table,
                    cur_mapped_addr,
                    &page_bytes,
                ) {
                    rollback_ipc_server_mapping(
                        self,
                        Some(updater.page_list()),
                        dst_addr,
                        mapped_pages,
                    );
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
            }
        }

        self.update_blocks_with_allocator(
            &mut block_allocator,
            dst_addr,
            aligned_src_size / PAGE_SIZE,
            dst_state,
            test_perm,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        self.m_mapped_ipc_server_memory = self
            .m_mapped_ipc_server_memory
            .saturating_add(unmapped_size);

        *out_addr = dst_addr + (src_addr - aligned_src_start);
        memory_reservation.commit();
        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    /// Matches upstream `KPageTableBase::CleanupForIpcServer`.
    pub fn cleanup_for_ipc_server(
        &mut self,
        addr: usize,
        size: usize,
        dst_state: KMemoryState,
    ) -> u32 {
        if size == 0 {
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let (rc, _, _, _, num_allocator_blocks) = self.check_memory_state_range(
            addr,
            size,
            KMemoryState::ALL,
            dst_state,
            KMemoryPermission::USER_READ,
            KMemoryPermission::USER_READ,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::NONE,
            Self::DEFAULT_MEMORY_IGNORE_ATTR,
        );
        if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            return rc;
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };

        let (aligned_start, aligned_end, mapping_start, mapping_end) =
            Self::get_ipc_aligned_extents(addr, size);
        let aligned_size = aligned_end.saturating_sub(aligned_start);
        let mapping_size = mapping_end.saturating_sub(mapping_start);

        if aligned_size > 0 {
            let unmap_props = KPageProperties {
                perm: KMemoryPermission::NONE,
                io: false,
                uncached: false,
                disable_merge_attributes: DisableMergeAttribute::NONE,
            };
            let rc = self.operate(
                None,
                aligned_start,
                aligned_size / PAGE_SIZE,
                0,
                false,
                unmap_props,
                OperationType::Unmap,
            );
            if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
                return rc;
            }

            self.update_blocks_with_allocator(
                &mut allocator,
                aligned_start,
                aligned_size / PAGE_SIZE,
                KMemoryState::NONE,
                KMemoryPermission::NONE,
                KMemoryAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NONE,
                KMemoryBlockDisableMergeAttribute::NORMAL,
            );
        }

        let unmapped_size = aligned_size.saturating_sub(mapping_size);
        if unmapped_size > 0 {
            if let Some(resource_limit) = &self.m_resource_limit {
                resource_limit
                    .release(LimitableResource::PhysicalMemoryMax, unmapped_size as i64);
            }
            self.m_mapped_ipc_server_memory = self
                .m_mapped_ipc_server_memory
                .saturating_sub(unmapped_size);
        }

        crate::hle::result::RESULT_SUCCESS.get_inner_value()
    }

    /// Matches upstream `KPageTableBase::CleanupForIpcClient`.
    pub fn cleanup_for_ipc_client(
        &mut self,
        addr: usize,
        size: usize,
        dst_state: KMemoryState,
    ) -> u32 {
        if size == 0 {
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let Some((test_state, test_attr_mask)) = Self::get_ipc_test_state_and_attr_mask(dst_state)
        else {
            return svc_results::RESULT_INVALID_COMBINATION.get_inner_value();
        };

        let (_, _, mapping_start, mapping_end) = Self::get_ipc_aligned_extents(addr, size);
        let mapping_size = mapping_end.saturating_sub(mapping_start);
        if mapping_size == 0 {
            return crate::hle::result::RESULT_SUCCESS.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        let rc = self.check_memory_state(
            mapping_start,
            mapping_size,
            test_state,
            test_state,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            test_attr_mask | KMemoryAttribute::IPC_LOCKED,
            KMemoryAttribute::IPC_LOCKED,
        );
        if rc != crate::hle::result::RESULT_SUCCESS.get_inner_value() {
            return rc;
        }

        let mut allocator = match self.make_block_update_allocator(0) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };

        self.m_memory_block_manager.update_lock_with_allocator(
            Some(&mut allocator),
            mapping_start,
            mapping_size / PAGE_SIZE,
            KMemoryPermission::NONE,
            |block, new_perm, is_first, is_last| block.unlock_for_ipc(new_perm, is_first, is_last),
        );

        self.reprotect_ipc_range(mapping_start, mapping_size)
    }

    /// Matches upstream `KPageTableBase::CleanupForIpcClientOnServerSetupFailure`
    /// (k_page_table_base.cpp:5033-5109).
    ///
    /// Rollback helper: when the server side of an IPC setup fails after the
    /// client side has already mapped + permissioned its source pages, this
    /// walks the affected block range and restores each block's protection to
    /// `prot_perm` via `OperationType::ChangePermissions`. Unlike upstream,
    /// the PageLinkedList argument is omitted — Rust's `operate` path does
    /// not thread a PageLinkedList because there are no guest page table
    /// entries to free (see comment on `operate`).
    ///
    /// Caller contract: the page table lock is held, and `address`/`size`
    /// are page-aligned and non-empty.
    pub fn cleanup_for_ipc_client_on_server_setup_failure(
        &mut self,
        address: usize,
        size: usize,
        prot_perm: KMemoryPermission,
    ) {
        debug_assert!(address % PAGE_SIZE == 0);
        debug_assert!(size % PAGE_SIZE == 0);

        let src_map_start = address;
        let src_map_end = address + size;
        let src_map_last = src_map_end - 1;

        debug_assert!(src_map_end > src_map_start);

        // Snapshot the block range so we can both peek ahead (for the tail
        // merge-attribute calculation, upstream `auto next_it = it; ++next_it`)
        // and call `operate` mutably on `self` without holding a borrow on
        // `m_memory_block_manager`.
        let infos: Vec<KMemoryInfo> = self
            .m_memory_block_manager
            .find_iterator(address)
            .map(|block| block.get_memory_info())
            .collect();

        for (i, info) in infos.iter().enumerate() {
            let cur_start = if info.get_address() >= src_map_start {
                info.get_address()
            } else {
                src_map_start
            };
            let cur_end = if src_map_last <= info.get_last_address() {
                src_map_end
            } else {
                info.get_end_address()
            };

            // If we can, fix the protections on the block.
            let needs_fix = (info.get_ipc_lock_count() == 0
                && (info.get_permission() & KMemoryPermission::IPC_LOCK_CHANGE_MASK) != prot_perm)
                || (info.get_ipc_lock_count() != 0
                    && (info.get_original_permission() & KMemoryPermission::IPC_LOCK_CHANGE_MASK)
                        != prot_perm);

            if needs_fix {
                // Check if we actually need to fix the protections on the block.
                let should_operate = cur_end == src_map_end
                    || info.get_address() <= src_map_start
                    || (info.get_permission() & KMemoryPermission::IPC_LOCK_CHANGE_MASK)
                        != prot_perm;

                if should_operate {
                    let start_nc = if info.get_address() == src_map_start {
                        (info.get_disable_merge_attribute()
                            & (KMemoryBlockDisableMergeAttribute::LOCKED
                                | KMemoryBlockDisableMergeAttribute::IPC_LEFT))
                            .is_empty()
                    } else {
                        info.get_address() <= src_map_start
                    };

                    let head_body_attr = if start_nc {
                        DisableMergeAttribute::ENABLE_HEAD_AND_BODY
                    } else {
                        DisableMergeAttribute::NONE
                    };

                    let tail_attr =
                        if cur_end == src_map_end && info.get_end_address() == src_map_end {
                            // Upstream: if (next_it != end) lock_count +=
                            //     next_it->GetIpcDisableMergeCount() - next_it->GetIpcLockCount();
                            let next_contribution = infos.get(i + 1).map_or(0i32, |next| {
                                next.get_ipc_disable_merge_count() as i32
                                    - next.get_ipc_lock_count() as i32
                            });
                            let lock_count = info.get_ipc_lock_count() as i32 + next_contribution;
                            if lock_count == 0 {
                                DisableMergeAttribute::ENABLE_TAIL
                            } else {
                                DisableMergeAttribute::NONE
                            }
                        } else {
                            DisableMergeAttribute::NONE
                        };

                    let properties = KPageProperties {
                        perm: info.get_permission(),
                        io: false,
                        uncached: false,
                        disable_merge_attributes: head_body_attr | tail_attr,
                    };

                    let _ = self.operate(
                        None,
                        cur_start,
                        (cur_end - cur_start) / PAGE_SIZE,
                        0,
                        false,
                        properties,
                        OperationType::ChangePermissions,
                    );
                }
            }

            // If we're past the end of the region, we're done.
            if src_map_last <= info.get_last_address() {
                break;
            }
        }
    }

    // -- Code Locking --

    /// Lock memory for code memory operations.
    /// Matches upstream `KPageTableBase::LockForCodeMemory`.
    pub fn lock_for_code_memory(
        &mut self,
        addr: usize,
        size: usize,
        perm: KMemoryPermission,
    ) -> u32 {
        let mut paddr = 0u64;
        self.lock_memory_and_open(
            None,
            Some(&mut paddr),
            addr,
            size,
            KMemoryState::FLAG_CAN_CODE_ALIAS,
            KMemoryState::FLAG_CAN_CODE_ALIAS,
            KMemoryPermission::from_bits_truncate(0xFF),
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::NONE,
            perm,
            KMemoryAttribute::LOCKED,
        )
    }

    /// Unlock memory for code memory operations.
    /// Matches upstream `KPageTableBase::UnlockForCodeMemory`.
    pub fn unlock_for_code_memory(&mut self, addr: usize, size: usize) -> u32 {
        self.unlock_memory(
            addr,
            size,
            KMemoryState::FLAG_CAN_CODE_ALIAS,
            KMemoryState::FLAG_CAN_CODE_ALIAS,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::LOCKED,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::LOCKED,
        )
    }

    // -- Device Address Space Locking --

    /// Matches upstream `KPageTableBase::LockForMapDeviceAddressSpace`.
    pub fn lock_for_map_device_address_space(
        &mut self,
        addr: usize,
        size: usize,
        perm: KMemoryPermission,
        is_aligned: bool,
        check_heap: bool,
    ) -> (u32, bool) {
        let mut state_flag = if is_aligned {
            KMemoryState::FLAG_CAN_ALIGNED_DEVICE_MAP
        } else {
            KMemoryState::FLAG_CAN_DEVICE_MAP
        };
        if check_heap {
            state_flag |= KMemoryState::FLAG_REFERENCE_COUNTED;
        }
        let num_pages = size / PAGE_SIZE;
        if !self.contains_range(addr, size) {
            return (
                svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value(),
                false,
            );
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        let attr_mask = KMemoryAttribute::from_bits_truncate(
            KMemoryAttribute::IPC_LOCKED.bits() | KMemoryAttribute::LOCKED.bits(),
        );
        let (result, out_old_state, _, _, num_allocator_blocks) = self.check_memory_state_range(
            addr,
            size,
            state_flag,
            state_flag,
            perm,
            perm,
            attr_mask,
            KMemoryAttribute::NONE,
            KMemoryAttribute::DEVICE_SHARED,
        );
        if result != 0 {
            return (result, false);
        }

        let old_state = out_old_state.unwrap_or(KMemoryState::FREE);
        let was_io = Self::k_state_to_svc(
            old_state & KMemoryState::from_bits_truncate(KMemoryState::MASK.bits()),
        ) == SvcMemoryState::Io;

        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return (rc, false),
        };

        self.m_memory_block_manager.update_lock_with_allocator(
            Some(&mut allocator),
            addr,
            num_pages,
            KMemoryPermission::NONE,
            |block, new_perm, is_first, is_last| {
                block.share_to_device(new_perm, is_first, is_last);
            },
        );
        (0, was_io)
    }

    /// Matches upstream `KPageTableBase::LockForUnmapDeviceAddressSpace`.
    pub fn lock_for_unmap_device_address_space(
        &mut self,
        addr: usize,
        size: usize,
        check_heap: bool,
    ) -> u32 {
        let num_pages = size / PAGE_SIZE;
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        let test_state = KMemoryState::FLAG_CAN_DEVICE_MAP
            | if check_heap {
                KMemoryState::FLAG_REFERENCE_COUNTED
            } else {
                KMemoryState::NONE
            };
        let attr_mask = KMemoryAttribute::from_bits_truncate(
            KMemoryAttribute::DEVICE_SHARED.bits() | KMemoryAttribute::LOCKED.bits(),
        );
        let (result, num_allocator_blocks) = self.check_memory_state_contiguous(
            addr,
            size,
            test_state,
            test_state,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            attr_mask,
            KMemoryAttribute::DEVICE_SHARED,
        );
        if result != 0 {
            return result;
        }

        let enable_device_address_space_merge = self.m_enable_device_address_space_merge;
        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };

        self.m_memory_block_manager.update_lock_with_allocator(
            Some(&mut allocator),
            addr,
            num_pages,
            KMemoryPermission::NONE,
            |block, new_perm, is_first, is_last| {
                if enable_device_address_space_merge {
                    block.update_device_disable_merge_state_for_share(new_perm, is_first, is_last);
                } else {
                    block.update_device_disable_merge_state_for_share_right(
                        new_perm, is_first, is_last,
                    );
                }
            },
        );
        0
    }

    /// Matches upstream `KPageTableBase::UnlockForDeviceAddressSpace`.
    pub fn unlock_for_device_address_space(&mut self, addr: usize, size: usize) -> u32 {
        let num_pages = size / PAGE_SIZE;
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        let attr_mask = KMemoryAttribute::from_bits_truncate(
            KMemoryAttribute::DEVICE_SHARED.bits() | KMemoryAttribute::LOCKED.bits(),
        );
        let (result, num_allocator_blocks) = self.check_memory_state_contiguous(
            addr,
            size,
            KMemoryState::FLAG_CAN_DEVICE_MAP,
            KMemoryState::FLAG_CAN_DEVICE_MAP,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            attr_mask,
            KMemoryAttribute::DEVICE_SHARED,
        );
        if result != 0 {
            return result;
        }

        let mut allocator = match self.make_block_update_allocator(num_allocator_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };

        self.m_memory_block_manager.update_lock_with_allocator(
            Some(&mut allocator),
            addr,
            num_pages,
            KMemoryPermission::NONE,
            |block, new_perm, is_first, is_last| {
                block.unshare_to_device(new_perm, is_first, is_last);
            },
        );
        0
    }

    /// Matches upstream `KPageTableBase::UnlockForDeviceAddressSpacePartialMap`.
    pub fn unlock_for_device_address_space_partial_map(&mut self, addr: usize, size: usize) -> u32 {
        let num_pages = size / PAGE_SIZE;
        if !self.contains_range(addr, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = super::k_light_lock::KScopedLightLock::new(general_lock.as_ref());

        let attr_mask = KMemoryAttribute::from_bits_truncate(
            KMemoryAttribute::DEVICE_SHARED.bits() | KMemoryAttribute::LOCKED.bits(),
        );
        let (result, allocator_num_blocks) = self.check_memory_state_contiguous(
            addr,
            size,
            KMemoryState::FLAG_CAN_DEVICE_MAP,
            KMemoryState::FLAG_CAN_DEVICE_MAP,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            attr_mask,
            KMemoryAttribute::DEVICE_SHARED,
        );
        if result != 0 {
            return result;
        }

        let enable_device_address_space_merge = self.m_enable_device_address_space_merge;
        let mut allocator = match self.make_block_update_allocator(allocator_num_blocks) {
            Ok(allocator) => allocator,
            Err(rc) => return rc,
        };

        self.m_memory_block_manager.update_lock_with_allocator(
            Some(&mut allocator),
            addr,
            num_pages,
            KMemoryPermission::NONE,
            |block, new_perm, is_first, is_last| {
                if enable_device_address_space_merge {
                    block
                        .update_device_disable_merge_state_for_unshare(new_perm, is_first, is_last);
                } else {
                    block.update_device_disable_merge_state_for_unshare_right(
                        new_perm, is_first, is_last,
                    );
                }
            },
        );
        0
    }

    // -- IO Region Mapping --

    /// Matches upstream `KPageTableBase::MapIoRegion`.
    pub fn map_io_region(
        &mut self,
        dst: usize,
        phys_addr: u64,
        size: usize,
        perm: KMemoryPermission,
    ) -> u32 {
        let num_pages = size / PAGE_SIZE;
        if !self.contains_range(dst, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        let (result, blocks_needed) = self.check_memory_state_contiguous(
            dst,
            size,
            KMemoryState::all(),
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        let Ok(mut allocator) = self.make_block_update_allocator(blocks_needed) else {
            return svc_results::RESULT_OUT_OF_RESOURCE.get_inner_value();
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        let props = KPageProperties {
            perm,
            io: true,
            uncached: true,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD,
        };
        let op = self.operate(
            Some(updater.page_list()),
            dst,
            num_pages,
            phys_addr,
            true,
            props,
            OperationType::Map,
        );
        if op != 0 {
            return op;
        }

        self.update_blocks_with_allocator(
            &mut allocator,
            dst,
            num_pages,
            KMemoryState::IO_REGISTER,
            perm,
            KMemoryAttribute::LOCKED,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        0
    }

    /// Matches upstream `KPageTableBase::UnmapIoRegion`.
    pub fn unmap_io_region(&mut self, dst: usize, phys_addr: u64, size: usize) -> u32 {
        let num_pages = size / PAGE_SIZE;
        if !self.contains_range(dst, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        let (result, blocks_needed) = self.check_memory_state_contiguous(
            dst,
            size,
            KMemoryState::all(),
            KMemoryState::IO_REGISTER,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::LOCKED,
        );
        if result != 0 {
            return result;
        }

        if let Some(impl_pt) = self.m_impl.as_ref() {
            for page in 0..num_pages {
                let va = dst + page * PAGE_SIZE;
                let expected = phys_addr + (page * PAGE_SIZE) as u64;
                if impl_pt.get_physical_address(va as u64) != Some(expected) {
                    return svc_results::RESULT_INVALID_MEMORY_REGION.get_inner_value();
                }
            }
        }

        let Ok(mut allocator) = self.make_block_update_allocator(blocks_needed) else {
            return svc_results::RESULT_OUT_OF_RESOURCE.get_inner_value();
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        let unmap_props = KPageProperties {
            perm: KMemoryPermission::NONE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        let op = self.operate(
            Some(updater.page_list()),
            dst,
            num_pages,
            0,
            false,
            unmap_props,
            OperationType::Unmap,
        );
        if op != 0 {
            return op;
        }

        self.update_blocks_with_allocator(
            &mut allocator,
            dst,
            num_pages,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
        );
        0
    }

    // =========================================================================
    // MapPageGroup / UnmapPageGroup
    // =========================================================================

    /// Map a KPageGroup's physical pages into the virtual address space.
    ///
    /// Upstream: `KPageTableBase::MapPageGroupImpl` (k_page_table_base.cpp:1623).
    /// Iterates each block in the page group and calls Operate(Map) for each.
    /// On failure, unmaps everything already mapped (rollback).
    fn map_page_group_impl(
        &mut self,
        mut page_list: Option<&mut PageLinkedList>,
        address: usize,
        pg: &super::k_page_group::KPageGroup,
        properties: KPageProperties,
    ) -> u32 {
        let start_address = address;
        let mut cur_address = address;

        for (i, block) in pg.iter().enumerate() {
            // First block uses the full properties (with DisableHead);
            // subsequent blocks use DisableMergeAttribute::None.
            let cur_properties = if i == 0 {
                properties
            } else {
                KPageProperties {
                    perm: properties.perm,
                    io: properties.io,
                    uncached: properties.uncached,
                    disable_merge_attributes: DisableMergeAttribute::NONE,
                }
            };

            let result = self.operate(
                page_list.as_deref_mut(),
                cur_address,
                block.get_num_pages(),
                block.get_address(),
                true,
                cur_properties,
                OperationType::Map,
            );
            if result != 0 {
                // Rollback: unmap everything we already mapped.
                if cur_address != start_address {
                    let unmap_properties = KPageProperties {
                        perm: KMemoryPermission::NONE,
                        io: false,
                        uncached: false,
                        disable_merge_attributes: DisableMergeAttribute::NONE,
                    };
                    let _ = self.operate(
                        page_list.as_deref_mut(),
                        start_address,
                        (cur_address - start_address) / PAGE_SIZE,
                        0,
                        false,
                        unmap_properties,
                        OperationType::Unmap,
                    );
                }
                return result;
            }
            cur_address += block.get_size();
        }

        0
    }

    /// Remap a previously-unmapped virtual range from an existing page group.
    ///
    /// Port of upstream `KPageTableBase::RemapPageGroup`; used as the
    /// `ON_RESULT_FAILURE` rollback path in `UnmapMemory` after destination
    /// pages have been unmapped but source permission restoration fails.
    fn remap_page_group(
        &mut self,
        mut page_list: Option<&mut PageLinkedList>,
        address: usize,
        size: usize,
        pg: &super::k_page_group::KPageGroup,
    ) {
        let start_address = address;
        let last_address = start_address + size - 1;
        let end_address = last_address + 1;

        let mut pg_iter = pg.iter();
        let Some(mut pg_block) = pg_iter.next() else {
            debug_assert!(false, "RemapPageGroup requires a non-empty KPageGroup");
            return;
        };
        let mut pg_phys_addr = pg_block.get_address();
        let mut pg_pages = pg_block.get_num_pages();

        let blocks: Vec<_> = self
            .m_memory_block_manager
            .find_iterator(start_address)
            .map(|block| {
                (
                    block.get_address(),
                    block.get_end_address(),
                    block.get_last_address(),
                    block.get_permission(),
                    block.get_disable_merge_attribute(),
                )
            })
            .collect();

        for (info_address, info_end_address, info_last_address, info_perm, info_disable_merge) in
            blocks
        {
            let mut map_address = info_address.max(start_address);
            let map_end_address = info_end_address.min(end_address);
            debug_assert_ne!(map_end_address, map_address);

            let disable_head_merge = info_address >= start_address
                && info_disable_merge.contains(KMemoryBlockDisableMergeAttribute::NORMAL);
            let map_properties = KPageProperties {
                perm: info_perm,
                io: false,
                uncached: false,
                disable_merge_attributes: if disable_head_merge {
                    DisableMergeAttribute::DISABLE_HEAD
                } else {
                    DisableMergeAttribute::NONE
                },
            };

            let mut map_pages = (map_end_address - map_address) / PAGE_SIZE;
            while map_pages > 0 {
                if pg_pages == 0 {
                    let Some(next_block) = pg_iter.next() else {
                        debug_assert!(false, "RemapPageGroup exhausted KPageGroup early");
                        return;
                    };
                    pg_block = next_block;
                    pg_phys_addr = pg_block.get_address();
                    pg_pages = pg_block.get_num_pages();
                }

                let cur_pages = pg_pages.min(map_pages);
                let result = self.operate(
                    page_list.as_deref_mut(),
                    map_address,
                    cur_pages,
                    pg_phys_addr,
                    true,
                    map_properties,
                    OperationType::Map,
                );
                debug_assert_eq!(result, 0, "RemapPageGroup rollback map failed");

                map_address += cur_pages * PAGE_SIZE;
                map_pages -= cur_pages;
                pg_phys_addr += (cur_pages * PAGE_SIZE) as u64;
                pg_pages -= cur_pages;
            }

            if last_address <= info_last_address {
                break;
            }
        }
    }

    /// Construct a page group from the current virtual mapping.
    ///
    /// Matches upstream `KPageTableBase::MakePageGroup`.
    fn make_page_group(
        &self,
        pg: &mut super::k_page_group::KPageGroup,
        addr: usize,
        num_pages: usize,
    ) -> u32 {
        let size = num_pages * PAGE_SIZE;
        if pg.is_empty() == false {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let Some(impl_pt) = self.m_impl.as_ref() else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };

        let Some((first_entry, mut traversal_context)) = impl_pt.begin_traversal(addr as u64)
        else {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        };

        if first_entry.block_size == 0 {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let block_mask = first_entry.block_size - 1;
        let mut cur_addr = first_entry.phys_addr;
        let mut cur_size = first_entry
            .block_size
            .saturating_sub((cur_addr as usize) & block_mask);
        let mut total_size = cur_size;

        while total_size < size {
            let Some(next_entry) = impl_pt.continue_traversal(&mut traversal_context) else {
                return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
            };

            if next_entry.phys_addr != cur_addr + cur_size as u64 {
                let cur_pages = cur_size / PAGE_SIZE;
                if !is_heap_physical_address_for_table(cur_addr) {
                    return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
                }
                if pg.add_block(cur_addr, cur_pages).is_err() {
                    return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
                }

                cur_addr = next_entry.phys_addr;
                cur_size = next_entry.block_size;
            } else {
                cur_size += next_entry.block_size;
            }

            total_size += next_entry.block_size;
        }

        if total_size > size {
            cur_size -= total_size - size;
        }

        let cur_pages = cur_size / PAGE_SIZE;
        if !is_heap_physical_address_for_table(cur_addr) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }
        if pg.add_block(cur_addr, cur_pages).is_err() {
            return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
        }

        0
    }

    /// Validate that the current mapping at `addr` matches `pg`.
    ///
    /// Upstream `KPageTableBase::IsValidPageGroup` traverses the page table
    /// directly and compares physical blocks. Ruzu's `make_page_group` already
    /// normalizes the current physical mapping into `KPageGroup` blocks, so the
    /// equivalent check is to build the expected group and compare blocks.
    fn is_valid_page_group(
        &self,
        pg: &super::k_page_group::KPageGroup,
        addr: usize,
        num_pages: usize,
    ) -> bool {
        if pg.is_empty() {
            return false;
        }

        let mut expected = super::k_page_group::KPageGroup::new();
        if self.make_page_group(&mut expected, addr, num_pages) != 0 {
            return false;
        }

        if pg.get_num_pages() != expected.get_num_pages() {
            return false;
        }

        let mut actual_blocks = pg.iter();
        let mut expected_blocks = expected.iter();
        loop {
            match (actual_blocks.next(), expected_blocks.next()) {
                (None, None) => return true,
                (Some(actual), Some(expected)) if actual.is_equivalent_to(expected) => {}
                _ => return false,
            }
        }
    }

    /// Create a page group from the current mapping and open a new physical
    /// reference for every page in the group.
    ///
    /// Upstream: `KPageTableBase::MakeAndOpenPageGroup` (k_page_table_base.cpp:2990).
    pub fn make_and_open_page_group(
        &self,
        out: &mut super::k_page_group::KPageGroup,
        address: usize,
        num_pages: usize,
        state_mask: KMemoryState,
        state: KMemoryState,
        perm_mask: KMemoryPermission,
        perm: KMemoryPermission,
        attr_mask: KMemoryAttribute,
        attr: KMemoryAttribute,
    ) -> u32 {
        let size = num_pages * PAGE_SIZE;
        if !self.contains_range(address, size) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        let result = self.check_memory_state(
            address,
            size,
            KMemoryState::from_bits_truncate(
                state_mask.bits() | KMemoryState::FLAG_REFERENCE_COUNTED.bits(),
            ),
            KMemoryState::from_bits_truncate(
                state.bits() | KMemoryState::FLAG_REFERENCE_COUNTED.bits(),
            ),
            perm_mask,
            perm,
            attr_mask,
            attr,
        );
        if result != 0 {
            return result;
        }

        let result = self.make_page_group(out, address, num_pages);
        if result != 0 {
            return result;
        }

        out.open();
        0
    }

    /// Map a page group into the address space with the given state and permission.
    ///
    /// Upstream: `KPageTableBase::MapPageGroup(KProcessAddress addr, const KPageGroup& pg,
    ///     KMemoryState state, KMemoryPermission perm)` (k_page_table_base.cpp:2891).
    pub fn map_page_group(
        &mut self,
        addr: usize,
        pg: &super::k_page_group::KPageGroup,
        state: KMemoryState,
        perm: KMemoryPermission,
    ) -> u32 {
        let num_pages = pg.get_num_pages();
        let size = num_pages * PAGE_SIZE;

        // Validate the address range can contain shared memory.
        if !self.can_contain_k(addr, size, state) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        // Check that the region is currently Free with no permissions.
        let (result, blocks_needed) = self.check_memory_state_contiguous(
            addr,
            size,
            KMemoryState::all(),
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }

        let Ok(mut allocator) = self.make_block_update_allocator(blocks_needed) else {
            return svc_results::RESULT_OUT_OF_RESOURCE.get_inner_value();
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Map the pages.
        let properties = KPageProperties {
            perm,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD,
        };
        let result = self.map_page_group_impl(Some(updater.page_list()), addr, pg, properties);
        if result != 0 {
            return result;
        }

        // Update the memory block manager to track the new state.
        self.update_blocks_with_allocator(
            &mut allocator,
            addr,
            num_pages,
            state,
            perm,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        0
    }

    /// Unmap a page group from the address space.
    ///
    /// Upstream: `KPageTableBase::UnmapPageGroup(KProcessAddress address,
    ///     const KPageGroup& pg, KMemoryState state)` (k_page_table_base.cpp:2932).
    pub fn unmap_page_group(
        &mut self,
        address: usize,
        pg: &super::k_page_group::KPageGroup,
        state: KMemoryState,
    ) -> u32 {
        let num_pages = pg.get_num_pages();
        let size = num_pages * PAGE_SIZE;

        if !self.can_contain_k(address, size, state) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let general_lock = self.m_general_lock.clone();
        let _lock_guard = KScopedLightLock::new(general_lock.as_ref());

        // Check that the region is currently in the expected state.
        let (result, blocks_needed) = self.check_memory_state_contiguous(
            address,
            size,
            KMemoryState::all(),
            state,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::from_bits_truncate(0xFF),
            KMemoryAttribute::NONE,
        );
        if result != 0 {
            return result;
        }
        if !self.is_valid_page_group(pg, address, num_pages) {
            return svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value();
        }

        let Ok(mut allocator) = self.make_block_update_allocator(blocks_needed) else {
            return svc_results::RESULT_OUT_OF_RESOURCE.get_inner_value();
        };
        let mut updater = KScopedPageTableUpdater::from_mut(self);

        // Unmap the pages.
        let properties = KPageProperties {
            perm: KMemoryPermission::NONE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::NONE,
        };
        let result = self.operate(
            Some(updater.page_list()),
            address,
            num_pages,
            0,
            false,
            properties,
            OperationType::Unmap,
        );
        if result != 0 {
            return result;
        }

        // Update block manager back to Free.
        self.update_blocks_with_allocator(
            &mut allocator,
            address,
            num_pages,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NORMAL,
        );

        0
    }
}

impl Default for KPageTableBase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::device_memory::{dram_memory_map, DeviceMemory};
    use crate::hle::kernel::k_memory_manager::{Direction, KMemoryManager, Pool};
    use crate::hle::kernel::k_resource_limit::create_resource_limit_for_process;
    use crate::hle::kernel::kernel::ScopedKernelForTest;
    use crate::hle::kernel::svc::svc_results;
    use crate::hle::result::RESULT_SUCCESS;
    use crate::memory::memory::Memory;

    #[test]
    fn finalize_update_drains_deferred_pages_like_current_eden() {
        let mut table = KPageTableBase::new();
        let mut pages = PageLinkedList::new();
        pages.push(0x1000);
        pages.push(0x2000);

        table.finalize_update(&mut pages);

        assert!(pages.is_empty());
    }

    fn kernel_with_application_pool_for_test(num_pages: usize) -> ScopedKernelForTest {
        let mut kernel = ScopedKernelForTest::new();
        kernel
            .kernel_mut()
            .initialize_memory_block_slab_manager(4096);
        kernel.kernel_mut().initialize_block_info_manager(4096);
        kernel.memory_manager_mut().initialize_pool(
            Pool::Application,
            dram_memory_map::BASE + 0x100000,
            num_pages * PAGE_SIZE,
        );
        kernel
    }

    struct PageTableMemoryForTest {
        _device_memory: Box<DeviceMemory>,
        memory: Arc<Mutex<Memory>>,
    }

    impl PageTableMemoryForTest {
        fn new(backing_size: usize) -> Self {
            let device_memory = Box::new(DeviceMemory::with_size(backing_size));
            let buffer_ptr = &device_memory.buffer as *const common::host_memory::HostMemory;
            let memory = Arc::new(Mutex::new(unsafe {
                Memory::new(
                    SystemRef::null(),
                    device_memory.as_ref() as *const _,
                    buffer_ptr,
                )
            }));

            Self {
                _device_memory: device_memory,
                memory,
            }
        }
    }

    fn attach_page_table_memory_for_test(
        page_table: &mut KPageTableBase,
        memory: Arc<Mutex<Memory>>,
    ) {
        attach_page_table_managers_for_test(page_table);
        page_table.m_address_space_width = 32;
        page_table.m_memory = Some(memory);
        page_table.initialize_impl();
        if let (Some(memory), Some(page_table_impl)) =
            (page_table.m_memory.as_ref(), page_table.m_impl.as_mut())
        {
            memory
                .lock()
                .unwrap()
                .set_current_page_table(page_table_impl.as_mut() as *mut _, true);
        }
    }

    fn attach_page_table_managers_for_test(page_table: &mut KPageTableBase) {
        let kernel = super::super::kernel::get_kernel_ref()
            .expect("page-table tests must install a scoped kernel");
        page_table.m_memory_block_slab_manager = kernel.get_memory_block_slab_manager();
        page_table.m_block_info_manager = kernel.get_block_info_manager();
    }

    #[test]
    fn finalize_large_free_address_space_releases_page_table_directly() {
        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0;
        page_table.m_address_space_end = 1usize << 39;
        page_table.m_address_space_width = 39;
        page_table
            .m_memory_block_manager
            .initialize(0, 1usize << 39, None)
            .unwrap();
        page_table.initialize_impl();

        page_table.finalize();

        assert!(page_table.m_impl.is_none());
        assert_eq!(page_table.m_memory_block_manager.block_count(), 0);
    }

    fn map_source_pages_for_test(
        page_table: &mut KPageTableBase,
        addr: usize,
        num_pages: usize,
        phys_addr: u64,
    ) {
        let Some(memory) = page_table.m_memory.as_ref().cloned() else {
            panic!("test page table memory must be attached before mapping source pages");
        };
        let Some(impl_pt) = page_table.m_impl.as_mut() else {
            panic!("test page table backend must be initialized before mapping source pages");
        };
        memory.lock().unwrap().map_memory_region(
            impl_pt,
            addr as u64,
            (num_pages * PAGE_SIZE) as u64,
            phys_addr,
            KPageTableBase::convert_to_memory_permission(KMemoryPermission::USER_READ_WRITE),
            false,
        );
    }

    #[test]
    fn make_and_open_page_group_requires_reference_counted_state_and_builds_group() {
        let mut kernel = kernel_with_application_pool_for_test(16);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            2,
            2,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_4000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            2,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        let mapped_addr = page_table.m_address_space_start;
        map_source_pages_for_test(&mut page_table, mapped_addr, 2, phys_addr);

        let mut group = crate::hle::kernel::k_page_group::KPageGroup::new();
        let result = page_table.make_and_open_page_group(
            &mut group,
            mapped_addr,
            2,
            KMemoryState::all(),
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::all(),
            KMemoryAttribute::NONE,
        );

        assert_eq!(result, 0);
        assert_eq!(group.get_num_pages(), 2);
        let blocks: Vec<_> = group.iter().collect();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].get_address(), phys_addr);
        assert_eq!(blocks[0].get_num_pages(), 2);

        let mut non_reference_counted_group = crate::hle::kernel::k_page_group::KPageGroup::new();
        let result = page_table.make_and_open_page_group(
            &mut non_reference_counted_group,
            mapped_addr,
            2,
            KMemoryState::MASK,
            KMemoryState::FREE,
            KMemoryPermission::NONE,
            KMemoryPermission::NONE,
            KMemoryAttribute::all(),
            KMemoryAttribute::NONE,
        );
        assert_eq!(
            result,
            svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value()
        );
        assert!(non_reference_counted_group.is_empty());
    }

    #[test]
    fn unmap_process_memory_same_table_validates_shared_code_physical_mapping() {
        let mut kernel = kernel_with_application_pool_for_test(16);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            2,
            2,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1001_0000;
        page_table.m_heap_region_start = 0x1000_8000;
        page_table.m_heap_region_end = 0x1001_0000;
        page_table.m_alias_code_region_start = 0x1000_4000;
        page_table.m_alias_code_region_end = 0x1000_8000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());

        let src_addr = 0x1000_8000;
        let dst_addr = 0x1000_4000;
        page_table.m_memory_block_manager.update(
            src_addr,
            2,
            KMemoryState::CODE_DATA,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        map_source_pages_for_test(&mut page_table, src_addr, 2, phys_addr);

        let mut group = crate::hle::kernel::k_page_group::KPageGroup::new();
        assert_eq!(
            page_table.make_and_open_page_group(
                &mut group,
                src_addr,
                2,
                KMemoryState::FLAG_CAN_MAP_PROCESS,
                KMemoryState::FLAG_CAN_MAP_PROCESS,
                KMemoryPermission::NONE,
                KMemoryPermission::NONE,
                KMemoryAttribute::all(),
                KMemoryAttribute::NONE,
            ),
            0
        );
        assert_eq!(
            page_table.map_page_group(
                dst_addr,
                &group,
                KMemoryState::SHARED_CODE,
                KMemoryPermission::USER_READ_WRITE,
            ),
            0
        );

        assert_eq!(
            page_table.unmap_process_memory_same_table(dst_addr, 2 * PAGE_SIZE, src_addr),
            0
        );

        let dst_info = page_table.query_info(dst_addr).unwrap();
        assert_eq!(dst_info.m_state, KMemoryState::FREE);
        assert_eq!(dst_info.m_permission, KMemoryPermission::NONE);
        let src_info = page_table.query_info(src_addr).unwrap();
        assert_eq!(src_info.m_state, KMemoryState::CODE_DATA);
        assert_eq!(src_info.m_permission, KMemoryPermission::USER_READ_WRITE);
    }

    #[test]
    fn scoped_light_lock_pair_unlocks_both_locks_on_drop() {
        let first = Arc::new(KLightLock::new());
        let second = Arc::new(KLightLock::new());

        {
            let _pair = KScopedLightLockPair::new(second.clone(), first.clone());
            assert!(first.is_locked());
            assert!(second.is_locked());
        }

        assert!(!first.is_locked());
        assert!(!second.is_locked());
    }

    #[test]
    fn scoped_light_lock_pair_try_unlock_half_matches_upstream_helper() {
        let first = Arc::new(KLightLock::new());
        let second = Arc::new(KLightLock::new());

        {
            let mut pair = KScopedLightLockPair::new(first.clone(), second.clone());
            pair.try_unlock_half(&first);
            assert!(!first.is_locked());
            assert!(second.is_locked());
        }

        assert!(!first.is_locked());
        assert!(!second.is_locked());
    }

    #[test]
    fn scoped_light_lock_pair_same_lock_locks_once() {
        let lock = Arc::new(KLightLock::new());

        {
            let _pair = KScopedLightLockPair::new(lock.clone(), lock.clone());
            assert!(lock.is_locked());
        }

        assert!(!lock.is_locked());
    }

    #[test]
    fn query_info_out_of_range_returns_inaccessible_terminal_block() {
        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x0020_0000;
        page_table.m_address_space_end = 0x4000_0000;

        let info = page_table.query_info(0x4004_0000).unwrap();

        assert_eq!(info.m_address, 0x4000_0000);
        assert_eq!(info.m_size, 0usize.wrapping_sub(0x4000_0000));
        assert_eq!(info.m_state, KMemoryState::INACCESSIBLE);
        assert_eq!(info.m_permission, KMemoryPermission::NONE);
        assert_eq!(info.m_attribute, KMemoryAttribute::NONE);
        assert_eq!(info.m_ipc_lock_count, 0);
        assert_eq!(info.m_device_use_count, 0);
    }

    #[test]
    fn set_memory_attribute_permission_locked_requires_permission_lock_capability() {
        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_4000;
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());

        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            1,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let result = page_table.set_memory_attribute(
            page_table.m_address_space_start,
            PAGE_SIZE,
            KMemoryAttribute::PERMISSION_LOCKED.bits().into(),
            KMemoryAttribute::PERMISSION_LOCKED.bits().into(),
        );

        assert_eq!(
            result,
            svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value()
        );

        let info = page_table
            .m_memory_block_manager
            .query_info(page_table.m_address_space_start)
            .unwrap();
        assert_eq!(info.m_attribute, KMemoryAttribute::NONE);
    }

    #[test]
    fn set_memory_permission_updates_block_permission() {
        let _kernel = kernel_with_application_pool_for_test(4);
        let mut page_table = KPageTableBase::new();
        attach_page_table_managers_for_test(&mut page_table);
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_4000;
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());

        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            1,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let result = page_table.set_memory_permission(
            page_table.m_address_space_start,
            PAGE_SIZE,
            KMemoryPermission::USER_READ,
        );

        assert_eq!(result, 0);
        let info = page_table
            .m_memory_block_manager
            .query_info(page_table.m_address_space_start)
            .unwrap();
        assert_eq!(info.m_state, KMemoryState::NORMAL);
        assert_eq!(info.m_permission, KMemoryPermission::USER_READ);
    }

    #[test]
    fn set_process_memory_permission_updates_code_to_code_data() {
        let _kernel = kernel_with_application_pool_for_test(4);
        let mut page_table = KPageTableBase::new();
        attach_page_table_managers_for_test(&mut page_table);
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_4000;
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());

        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            1,
            KMemoryState::CODE,
            KMemoryPermission::USER_READ_EXECUTE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let result = page_table.set_process_memory_permission(
            page_table.m_address_space_start,
            PAGE_SIZE,
            KMemoryPermission::USER_READ_WRITE,
        );

        assert_eq!(result, 0);
        let info = page_table
            .m_memory_block_manager
            .query_info(page_table.m_address_space_start)
            .unwrap();
        assert_eq!(info.m_state, KMemoryState::CODE_DATA);
        assert_eq!(info.m_permission, KMemoryPermission::USER_READ_WRITE);
    }

    #[test]
    fn invalidate_data_cache_validates_range_state_and_mapping() {
        let mut kernel = kernel_with_application_pool_for_test(4);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_4000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            1,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        let cache_addr = page_table.m_address_space_start;
        map_source_pages_for_test(&mut page_table, cache_addr, 1, phys_addr);

        assert_eq!(
            page_table.invalidate_process_data_cache(cache_addr, PAGE_SIZE),
            0
        );
        assert_eq!(
            page_table.invalidate_current_process_data_cache(cache_addr, PAGE_SIZE),
            0
        );
        assert_eq!(
            page_table.invalidate_process_data_cache(page_table.m_address_space_end, PAGE_SIZE),
            svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value()
        );
    }

    #[test]
    fn copy_memory_from_linear_to_kernel_validates_and_reads_physical_backing() {
        let mut kernel = kernel_with_application_pool_for_test(4);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_4000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            1,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let map_addr = page_table.m_address_space_start;
        let src_addr = map_addr + 0x40;
        map_source_pages_for_test(&mut page_table, map_addr, 1, phys_addr);
        let expected = [0x13, 0x37, 0xC0, 0xDE, 0x42, 0x24];
        unsafe {
            std::ptr::copy_nonoverlapping(
                expected.as_ptr(),
                page_table_memory
                    ._device_memory
                    .get_pointer(phys_addr + 0x40),
                expected.len(),
            );
        }

        let mut actual = [0u8; 6];
        assert_eq!(
            page_table.copy_memory_from_linear_to_kernel(
                actual.as_mut_ptr() as usize,
                actual.len(),
                src_addr,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
            ),
            0
        );
        assert_eq!(actual, expected);
        assert_eq!(
            page_table.copy_memory_from_linear_to_kernel(
                actual.as_mut_ptr() as usize,
                actual.len(),
                page_table.m_address_space_end,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
            ),
            svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value()
        );
    }

    #[test]
    fn read_debug_memory_copies_linear_backing_to_guest_memory() {
        let mut kernel = kernel_with_application_pool_for_test(4);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let src_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        let dst_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(src_phys_addr, 0);
        assert_ne!(dst_phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_8000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            2,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let src_map_addr = page_table.m_address_space_start;
        let dst_map_addr = page_table.m_address_space_start + PAGE_SIZE;
        let src_addr = src_map_addr + 0x30;
        let dst_addr = dst_map_addr + 0x70;
        map_source_pages_for_test(&mut page_table, src_map_addr, 1, src_phys_addr);
        map_source_pages_for_test(&mut page_table, dst_map_addr, 1, dst_phys_addr);

        let expected = [0xD1, 0xE2, 0xB3, 0x94, 0x55, 0x26, 0x17];
        unsafe {
            std::ptr::copy_nonoverlapping(
                expected.as_ptr(),
                page_table_memory
                    ._device_memory
                    .get_pointer(src_phys_addr + 0x30),
                expected.len(),
            );
        }

        assert_eq!(
            page_table.read_debug_memory(dst_addr, src_addr, expected.len()),
            0
        );
        let mut actual = [0u8; 7];
        assert!(page_table_memory
            .memory
            .lock()
            .unwrap()
            .read_block(dst_addr as u64, &mut actual));
        assert_eq!(actual, expected);
    }

    #[test]
    fn write_debug_memory_accepts_debuggable_non_writable_memory() {
        let mut kernel = kernel_with_application_pool_for_test(4);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let src_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        let dst_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(src_phys_addr, 0);
        assert_ne!(dst_phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_8000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            1,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start + PAGE_SIZE,
            1,
            KMemoryState::CODE,
            KMemoryPermission::NONE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let src_map_addr = page_table.m_address_space_start;
        let dst_map_addr = page_table.m_address_space_start + PAGE_SIZE;
        let src_addr = src_map_addr + 0x90;
        let dst_addr = dst_map_addr + 0x20;
        map_source_pages_for_test(&mut page_table, src_map_addr, 1, src_phys_addr);
        map_source_pages_for_test(&mut page_table, dst_map_addr, 1, dst_phys_addr);

        let expected = [0xA5, 0x5A, 0xC3, 0x3C, 0x7E, 0xE7];
        assert!(page_table_memory
            .memory
            .lock()
            .unwrap()
            .write_block(src_addr as u64, &expected));

        assert_eq!(
            page_table.write_debug_memory(dst_addr, src_addr, expected.len()),
            0
        );
        let mut actual = [0u8; 6];
        assert!(page_table_memory
            .memory
            .lock()
            .unwrap()
            .read_phys_block(dst_phys_addr + 0x20, &mut actual));
        assert_eq!(actual, expected);
    }

    #[test]
    fn copy_memory_from_linear_to_user_writes_destination_guest_memory() {
        let mut kernel = kernel_with_application_pool_for_test(4);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let src_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        let dst_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(src_phys_addr, 0);
        assert_ne!(dst_phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_8000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            2,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let src_map_addr = page_table.m_address_space_start;
        let dst_map_addr = page_table.m_address_space_start + PAGE_SIZE;
        let dst_addr = page_table.m_address_space_start + PAGE_SIZE + 0x80;
        map_source_pages_for_test(&mut page_table, src_map_addr, 1, src_phys_addr);
        map_source_pages_for_test(&mut page_table, dst_map_addr, 1, dst_phys_addr);

        let src_addr = src_map_addr + 0x40;
        let expected = [0xFE, 0xED, 0xFA, 0xCE, 0x11, 0x22, 0x33];
        unsafe {
            std::ptr::copy_nonoverlapping(
                expected.as_ptr(),
                page_table_memory
                    ._device_memory
                    .get_pointer(src_phys_addr + 0x40),
                expected.len(),
            );
        }

        assert_eq!(
            page_table.copy_memory_from_linear_to_user(
                dst_addr,
                expected.len(),
                src_addr,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
            ),
            0
        );

        let mut actual = [0u8; 7];
        assert!(page_table_memory
            .memory
            .lock()
            .unwrap()
            .read_block(dst_addr as u64, &mut actual));
        assert_eq!(actual, expected);
        assert_eq!(
            page_table.copy_memory_from_linear_to_user(
                dst_addr,
                expected.len(),
                page_table.m_address_space_end,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
            ),
            svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value()
        );
    }

    #[test]
    fn copy_memory_from_user_to_linear_writes_linear_physical_backing() {
        let mut kernel = kernel_with_application_pool_for_test(4);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let src_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        let dst_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(src_phys_addr, 0);
        assert_ne!(dst_phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_8000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            2,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let src_map_addr = page_table.m_address_space_start;
        let dst_map_addr = page_table.m_address_space_start + PAGE_SIZE;
        let src_addr = src_map_addr + 0x60;
        let dst_addr = dst_map_addr + 0x20;
        map_source_pages_for_test(&mut page_table, src_map_addr, 1, src_phys_addr);
        map_source_pages_for_test(&mut page_table, dst_map_addr, 1, dst_phys_addr);

        let expected = [0xAA, 0xBB, 0xCC, 0xDD, 0x99, 0x88, 0x77];
        assert!(page_table_memory
            .memory
            .lock()
            .unwrap()
            .write_block(src_addr as u64, &expected));

        assert_eq!(
            page_table.copy_memory_from_user_to_linear(
                dst_addr,
                expected.len(),
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
                src_addr,
            ),
            0
        );

        let mut actual = [0u8; 7];
        assert!(page_table_memory
            .memory
            .lock()
            .unwrap()
            .read_phys_block(dst_phys_addr + 0x20, &mut actual));
        assert_eq!(actual, expected);
        assert_eq!(
            page_table.copy_memory_from_user_to_linear(
                page_table.m_address_space_end,
                expected.len(),
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
                src_addr,
            ),
            svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value()
        );
    }

    #[test]
    fn copy_memory_from_kernel_to_linear_writes_linear_physical_backing() {
        let mut kernel = kernel_with_application_pool_for_test(4);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_4000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            1,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let dst_addr = page_table.m_address_space_start + 0x30;
        let map_addr = page_table.m_address_space_start;
        map_source_pages_for_test(&mut page_table, map_addr, 1, phys_addr);

        let expected = [0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC];
        assert_eq!(
            page_table.copy_memory_from_kernel_to_linear(
                dst_addr,
                expected.len(),
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
                expected.as_ptr() as usize,
            ),
            0
        );

        let mut actual = [0u8; 7];
        assert!(page_table_memory
            .memory
            .lock()
            .unwrap()
            .read_phys_block(phys_addr + 0x30, &mut actual));
        assert_eq!(actual, expected);
        assert_eq!(
            page_table.copy_memory_from_kernel_to_linear(
                page_table.m_address_space_end,
                expected.len(),
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
                expected.as_ptr() as usize,
            ),
            svc_results::RESULT_INVALID_CURRENT_MEMORY.get_inner_value()
        );
    }

    #[test]
    fn copy_memory_from_heap_to_heap_copies_physical_backing() {
        let mut kernel = kernel_with_application_pool_for_test(4);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let src_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        let dst_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(src_phys_addr, 0);
        assert_ne!(dst_phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_8000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            2,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let src_addr = page_table.m_address_space_start + 0x50;
        let dst_addr = page_table.m_address_space_start + PAGE_SIZE + 0x90;
        let src_map_addr = page_table.m_address_space_start;
        let dst_map_addr = page_table.m_address_space_start + PAGE_SIZE;
        map_source_pages_for_test(&mut page_table, src_map_addr, 1, src_phys_addr);
        map_source_pages_for_test(&mut page_table, dst_map_addr, 1, dst_phys_addr);

        let expected = [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD];
        unsafe {
            std::ptr::copy_nonoverlapping(
                expected.as_ptr(),
                page_table_memory
                    ._device_memory
                    .get_pointer(src_phys_addr + 0x50),
                expected.len(),
            );
        }

        assert_eq!(
            page_table.copy_memory_from_heap_to_heap(
                &page_table,
                dst_addr,
                expected.len(),
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
                src_addr,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
            ),
            0
        );

        let mut actual = [0u8; 7];
        assert!(page_table_memory
            .memory
            .lock()
            .unwrap()
            .read_phys_block(dst_phys_addr + 0x90, &mut actual));
        assert_eq!(actual, expected);
    }

    #[test]
    fn copy_memory_from_heap_to_heap_without_check_destination_skips_dst_state() {
        let mut kernel = kernel_with_application_pool_for_test(4);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let src_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        let dst_phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            1,
            1,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(src_phys_addr, 0);
        assert_ne!(dst_phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1000_8000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start,
            1,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        page_table.m_memory_block_manager.update(
            page_table.m_address_space_start + PAGE_SIZE,
            1,
            KMemoryState::CODE,
            KMemoryPermission::USER_READ_EXECUTE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let src_addr = page_table.m_address_space_start + 0x40;
        let dst_addr = page_table.m_address_space_start + PAGE_SIZE + 0x20;
        let src_map_addr = page_table.m_address_space_start;
        let dst_map_addr = page_table.m_address_space_start + PAGE_SIZE;
        map_source_pages_for_test(&mut page_table, src_map_addr, 1, src_phys_addr);
        map_source_pages_for_test(&mut page_table, dst_map_addr, 1, dst_phys_addr);

        let expected = [0x5A, 0xA5, 0x7E, 0xE7, 0x42];
        unsafe {
            std::ptr::copy_nonoverlapping(
                expected.as_ptr(),
                page_table_memory
                    ._device_memory
                    .get_pointer(src_phys_addr + 0x40),
                expected.len(),
            );
        }

        assert_ne!(
            page_table.copy_memory_from_heap_to_heap(
                &page_table,
                dst_addr,
                expected.len(),
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
                src_addr,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
            ),
            0
        );
        assert_eq!(
            page_table.copy_memory_from_heap_to_heap_without_check_destination(
                &page_table,
                dst_addr,
                expected.len(),
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ_WRITE,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
                src_addr,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryState::FLAG_REFERENCE_COUNTED,
                KMemoryPermission::USER_READ,
                KMemoryAttribute::NONE,
                KMemoryAttribute::NONE,
            ),
            0
        );

        let mut actual = [0u8; 5];
        assert!(page_table_memory
            .memory
            .lock()
            .unwrap()
            .read_phys_block(dst_phys_addr + 0x20, &mut actual));
        assert_eq!(actual, expected);
    }

    #[test]
    fn map_memory_updates_destination_block_state_to_stack() {
        let mut kernel = kernel_with_application_pool_for_test(16);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            2,
            2,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1001_0000;
        page_table.m_alias_region_start = 0x1000_0000;
        page_table.m_alias_region_end = 0x1001_0000;
        page_table.m_stack_region_start = 0x1000_0000;
        page_table.m_stack_region_end = 0x1001_0000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            0x1000_8000,
            2,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        map_source_pages_for_test(&mut page_table, 0x1000_8000, 2, phys_addr);

        let result = page_table.map_memory(0x1000_4000, 0x1000_8000, 0x2000);
        assert_eq!(result, 0);

        let info = page_table.query_info(0x1000_5ff0).unwrap();
        assert_eq!(info.m_state, KMemoryState::STACK);
        assert_eq!(info.m_permission, KMemoryPermission::USER_READ_WRITE);
    }

    #[test]
    fn unmap_memory_restores_original_source_state() {
        let mut kernel = kernel_with_application_pool_for_test(16);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            2,
            2,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1001_0000;
        page_table.m_alias_region_start = 0x1000_0000;
        page_table.m_alias_region_end = 0x1001_0000;
        page_table.m_stack_region_start = 0x1000_0000;
        page_table.m_stack_region_end = 0x1001_0000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            0x1000_8000,
            2,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        map_source_pages_for_test(&mut page_table, 0x1000_8000, 2, phys_addr);

        assert_eq!(page_table.map_memory(0x1000_4000, 0x1000_8000, 0x2000), 0);
        assert_eq!(page_table.unmap_memory(0x1000_4000, 0x1000_8000, 0x2000), 0);

        let src_info = page_table.query_info(0x1000_8ff0).unwrap();
        let dst_info = page_table.query_info(0x1000_4ff0).unwrap();
        assert_eq!(src_info.m_state, KMemoryState::NORMAL);
        assert_eq!(src_info.m_permission, KMemoryPermission::USER_READ_WRITE);
        assert_eq!(src_info.m_attribute, KMemoryAttribute::NONE);
        assert_eq!(dst_info.m_state, KMemoryState::FREE);
        assert_eq!(dst_info.m_permission, KMemoryPermission::NONE);
    }

    #[test]
    fn map_and_unmap_code_memory_round_trips_block_state() {
        let mut kernel = kernel_with_application_pool_for_test(16);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            2,
            2,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(phys_addr, 0);

        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1001_0000;
        page_table.m_alias_code_region_start = 0x1000_0000;
        page_table.m_alias_code_region_end = 0x1001_0000;
        attach_page_table_memory_for_test(&mut page_table, page_table_memory.memory.clone());
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            0x1000_8000,
            2,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        map_source_pages_for_test(&mut page_table, 0x1000_8000, 2, phys_addr);

        assert_eq!(
            page_table.map_code_memory(0x1000_4000, 0x1000_8000, 0x2000),
            0
        );
        let locked_src = page_table.query_info(0x1000_8ff0).unwrap();
        let alias_dst = page_table.query_info(0x1000_4ff0).unwrap();
        assert_eq!(locked_src.m_state, KMemoryState::NORMAL);
        assert_eq!(
            locked_src.m_permission,
            KMemoryPermission::KERNEL_READ | KMemoryPermission::NOT_MAPPED
        );
        assert_eq!(locked_src.m_attribute, KMemoryAttribute::LOCKED);
        assert_eq!(alias_dst.m_state, KMemoryState::ALIAS_CODE);

        assert_eq!(
            page_table.unmap_code_memory(0x1000_4000, 0x1000_8000, 0x2000),
            0
        );
        let restored_src = page_table.query_info(0x1000_8ff0).unwrap();
        let freed_dst = page_table.query_info(0x1000_4ff0).unwrap();
        assert_eq!(restored_src.m_state, KMemoryState::NORMAL);
        assert_eq!(
            restored_src.m_permission,
            KMemoryPermission::USER_READ_WRITE
        );
        assert_eq!(restored_src.m_attribute, KMemoryAttribute::NONE);
        assert_eq!(freed_dst.m_state, KMemoryState::FREE);
        assert_eq!(freed_dst.m_permission, KMemoryPermission::NONE);
    }

    #[test]
    fn setup_for_ipc_client_changes_permissions_without_locking_until_server_success() {
        let mut page_table = KPageTableBase::new();
        page_table.m_address_space_start = 0x1000_0000;
        page_table.m_address_space_end = 0x1001_0000;
        assert!(page_table
            .m_memory_block_manager
            .initialize(
                page_table.m_address_space_start,
                page_table.m_address_space_end,
                None,
            )
            .is_ok());
        page_table.m_memory_block_manager.update(
            0x1000_0000,
            4,
            KMemoryState::NORMAL,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );

        let addr = 0x1000_0080;
        let size = PAGE_SIZE * 2 + 0x100;

        let (rc, blocks_needed) = page_table.setup_for_ipc_client(
            addr,
            size,
            KMemoryPermission::USER_READ_WRITE,
            KMemoryState::IPC,
        );
        assert_eq!(rc, 0);
        assert_eq!(blocks_needed, 2);
        let setup = page_table.query_info(0x1000_1000).unwrap();
        assert!(!setup.m_attribute.contains(KMemoryAttribute::IPC_LOCKED));
        assert_eq!(setup.m_permission, KMemoryPermission::USER_READ_WRITE,);

        page_table.cleanup_for_ipc_client_on_server_setup_failure(
            addr.next_multiple_of(PAGE_SIZE),
            PAGE_SIZE,
            KMemoryPermission::KERNEL_READ_WRITE | KMemoryPermission::NOT_MAPPED,
        );

        let restored = page_table.query_info(0x1000_1000).unwrap();
        assert!(!restored.m_attribute.contains(KMemoryAttribute::IPC_LOCKED));
        assert_eq!(restored.m_permission, KMemoryPermission::USER_READ_WRITE);
    }

    #[test]
    fn setup_and_cleanup_for_ipc_server_account_partial_page_backend_cost() {
        let _kernel = kernel_with_application_pool_for_test(16);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let mut dst = KPageTableBase::new();
        attach_page_table_managers_for_test(&mut dst);
        dst.m_address_space_width = 32;
        dst.m_address_space_start = 0x1000_0000;
        dst.m_address_space_end = 0x1002_0000;
        dst.m_alias_region_start = 0x1000_0000;
        dst.m_alias_region_end = 0x1002_0000;
        dst.m_resource_limit = Some(Arc::new(create_resource_limit_for_process(
            0x10_0000,
        )));
        dst.m_memory = Some(page_table_memory.memory.clone());
        dst.initialize_impl();
        assert!(dst
            .m_memory_block_manager
            .initialize(dst.m_address_space_start, dst.m_address_space_end, None)
            .is_ok());

        let mut src = KPageTableBase::new();
        attach_page_table_managers_for_test(&mut src);
        src.m_address_space_start = 0x2000_0000;
        src.m_address_space_end = 0x2002_0000;
        assert!(src
            .m_memory_block_manager
            .initialize(src.m_address_space_start, src.m_address_space_end, None)
            .is_ok());

        let mut out_addr = 0usize;
        let src_addr = 0x2000_0080usize;
        let size = PAGE_SIZE + 0x100;
        let (aligned_src_start, aligned_src_end, mapping_src_start, mapping_src_end) =
            KPageTableBase::get_ipc_aligned_extents(src_addr, size);
        let aligned_src_size = aligned_src_end - aligned_src_start;
        let mapping_src_size = if mapping_src_start < mapping_src_end {
            mapping_src_end - mapping_src_start
        } else {
            0
        };
        let partial_backend_cost = aligned_src_size - mapping_src_size;

        assert_eq!(
            dst.setup_for_ipc_server(
                &mut out_addr,
                size,
                src_addr,
                KMemoryPermission::USER_READ,
                KMemoryState::IPC,
                &mut src,
                false,
            ),
            0
        );
        assert_ne!(out_addr, 0);
        assert_eq!(dst.m_mapped_ipc_server_memory, partial_backend_cost);
        assert_eq!(
            dst.m_resource_limit
                .as_ref()
                .unwrap()
                .get_current_value(LimitableResource::PhysicalMemoryMax),
            partial_backend_cost as i64
        );

        assert_eq!(
            dst.cleanup_for_ipc_server(out_addr, size, KMemoryState::IPC),
            0
        );
        assert_eq!(dst.m_mapped_ipc_server_memory, 0);
        assert_eq!(
            dst.m_resource_limit
                .as_ref()
                .unwrap()
                .get_current_value(LimitableResource::PhysicalMemoryMax),
            0
        );
    }

    #[test]
    fn setup_for_ipc_server_direct_map_preserves_contiguous_physical_run() {
        let mut kernel = kernel_with_application_pool_for_test(16);
        let page_table_memory = PageTableMemoryForTest::new(0x400000);
        let phys_addr = kernel.memory_manager_mut().allocate_and_open_continuous(
            2,
            2,
            KMemoryManager::encode_option(Pool::Application, Direction::FromFront),
        );
        assert_ne!(phys_addr, 0);

        let mut src = KPageTableBase::new();
        attach_page_table_managers_for_test(&mut src);
        src.m_address_space_width = 32;
        src.m_address_space_start = 0x2000_0000;
        src.m_address_space_end = 0x2002_0000;
        src.m_memory = Some(page_table_memory.memory.clone());
        src.initialize_impl();
        assert!(src
            .m_memory_block_manager
            .initialize(src.m_address_space_start, src.m_address_space_end, None)
            .is_ok());
        let source_props = KPageProperties {
            perm: KMemoryPermission::USER_READ_WRITE,
            io: false,
            uncached: false,
            disable_merge_attributes: DisableMergeAttribute::DISABLE_HEAD,
        };
        assert_eq!(
            src.operate(
                None,
                0x2000_0000,
                2,
                phys_addr,
                true,
                source_props,
                OperationType::Map,
            ),
            0
        );

        let mut dst = KPageTableBase::new();
        attach_page_table_managers_for_test(&mut dst);
        dst.m_address_space_width = 32;
        dst.m_address_space_start = 0x1000_0000;
        dst.m_address_space_end = 0x1002_0000;
        dst.m_alias_region_start = 0x1000_0000;
        dst.m_alias_region_end = 0x1002_0000;
        dst.m_resource_limit = Some(Arc::new(create_resource_limit_for_process(
            0x10_0000,
        )));
        dst.m_memory = Some(page_table_memory.memory.clone());
        dst.initialize_impl();
        assert!(dst
            .m_memory_block_manager
            .initialize(dst.m_address_space_start, dst.m_address_space_end, None)
            .is_ok());

        let mut out_addr = 0usize;
        assert_eq!(
            dst.setup_for_ipc_server(
                &mut out_addr,
                2 * PAGE_SIZE,
                0x2000_0000,
                KMemoryPermission::USER_READ,
                KMemoryState::IPC,
                &mut src,
                false,
            ),
            0
        );

        let dst_impl = dst.m_impl.as_ref().unwrap();
        assert_eq!(
            dst_impl.get_physical_address(out_addr as u64),
            Some(phys_addr)
        );
        assert_eq!(
            dst_impl.get_physical_address((out_addr + PAGE_SIZE) as u64),
            Some(phys_addr + PAGE_SIZE as u64)
        );
        assert_eq!(dst.m_mapped_ipc_server_memory, 0);

        assert_eq!(
            dst.cleanup_for_ipc_server(out_addr, 2 * PAGE_SIZE, KMemoryState::IPC),
            0
        );
        if let Some(kernel) = crate::hle::kernel::kernel::get_kernel_mut() {
            kernel.memory_manager_mut().close(phys_addr, 2);
        }
    }

    #[test]
    fn build_ipc_partial_page_fills_and_overlays_start_fragment() {
        let page = KPageTableBase::build_ipc_partial_page(b'Y', true, 0x80, &[1, 2, 3, 4]);
        assert_eq!(page.len(), PAGE_SIZE);
        assert!(page[..0x80].iter().all(|b| *b == b'Y'));
        assert_eq!(&page[0x80..0x84], &[1, 2, 3, 4]);
        assert!(page[0x84..].iter().all(|b| *b == b'Y'));
    }

    #[test]
    fn build_ipc_partial_page_non_send_fills_entire_page() {
        let page = KPageTableBase::build_ipc_partial_page(b'Y', false, 0, &[1, 2, 3, 4]);
        assert_eq!(page.len(), PAGE_SIZE);
        assert!(page.iter().all(|b| *b == b'Y'));
    }

    /// Set up an AArch32-shaped page table for `can_contain` parity tests.
    ///   address_space   = [0x0000_0000, 0x1_0000_0000)
    ///   code            = [0x0020_0000, 0x4000_0000)        ← MapSmall
    ///   alias_code      = [0x0020_0000, 0x1_0000_0000)      ← MapSmall start..MapLarge end
    ///   heap            = [0x4000_0000, 0xB800_0000)        ← 2 GiB
    ///   alias           = [0xB800_0000, 0xF800_0000)        ← 1 GiB above heap
    fn aarch32_layout() -> KPageTableBase {
        let mut pt = KPageTableBase::new();
        pt.m_address_space_start = 0x0000_0000;
        pt.m_address_space_end = 0x1_0000_0000;
        pt.m_code_region_start = 0x0020_0000;
        pt.m_code_region_end = 0x4000_0000;
        pt.m_alias_code_region_start = 0x0020_0000;
        pt.m_alias_code_region_end = 0x1_0000_0000;
        pt.m_heap_region_start = 0x4000_0000;
        pt.m_heap_region_end = 0xB800_0000;
        pt.m_alias_region_start = 0xB800_0000;
        pt.m_alias_region_end = 0xF800_0000;
        pt.m_stack_region_start = pt.m_code_region_start;
        pt.m_stack_region_end = pt.m_code_region_end;
        pt.m_kernel_map_region_start = pt.m_code_region_start;
        pt.m_kernel_map_region_end = pt.m_code_region_end;
        pt
    }

    /// Upstream `CanContain(Shared)` excludes addresses inside heap or alias —
    #[test]
    fn can_contain_shared_rejects_address_in_alias_region() {
        let pt = aarch32_layout();
        assert!(
            !pt.can_contain(0xB940_4000, 0x40000, SvcMemoryState::Shared),
            "Shared inside alias region must be rejected (matches upstream)"
        );
    }

    #[test]
    fn can_contain_shared_rejects_address_in_heap_region() {
        let pt = aarch32_layout();
        // 0x60000000 is firmly inside heap [0x40000000, 0xB8000000).
        assert!(!pt.can_contain(0x6000_0000, 0x4000, SvcMemoryState::Shared));
    }

    #[test]
    fn can_contain_shared_accepts_alias_code_below_heap() {
        let pt = aarch32_layout();
        // Inside MapSmall (alias_code region) and below heap.
        assert!(pt.can_contain(0x0040_0000, 0x4000, SvcMemoryState::Shared));
    }

    #[test]
    fn initialize_32bit_without_alias_places_shared_outside_folded_heap() {
        let mut pt = KPageTableBase::new();
        let result = pt.initialize_for_process(
            CreateProcessFlag::ADDRESS_SPACE_32_BIT_WITHOUT_ALIAS.bits(),
            false,
            true,
            true,
            0,
            0x0020_0000,
            0x03E0_0000,
            None,
            None,
            None,
            0,
        );

        assert_eq!(result, RESULT_SUCCESS.get_inner_value());
        assert_eq!(pt.m_heap_region_start, 0x4000_0000);
        assert_eq!(pt.m_heap_region_end, 0xC000_0000);
        assert_eq!(pt.m_alias_region_start, pt.m_alias_region_end);

        // inside folded heap, so upstream CanContain(Shared) rejects it.
        assert!(!pt.can_contain(0xB900_4000, 0x0110_0000, SvcMemoryState::Shared));

        // The following guest retry sits in alias_code but outside folded heap,
        // so it is a valid shared-memory placement for 32-bit-no-map.
        assert!(pt.can_contain(0xD5C0_4000, 0x0110_0000, SvcMemoryState::Shared));
    }

    #[test]
    fn can_contain_normal_excludes_alias_but_not_heap() {
        let mut pt = aarch32_layout();
        // Upstream asserts that Normal candidates are in the heap.
        assert!(pt.can_contain(0x6000_0000, 0x4000, SvcMemoryState::Normal));
        // Exercise the alias exclusion without violating that precondition.
        pt.m_alias_region_start = 0x6000_0000;
        pt.m_alias_region_end = 0x6000_4000;
        assert!(!pt.can_contain(0x6000_0000, 0x4000, SvcMemoryState::Normal));
    }

    #[test]
    fn can_contain_ipc_requires_alias_excludes_heap() {
        let mut pt = aarch32_layout();
        // Upstream asserts that IPC candidates are in the alias region.
        assert!(pt.can_contain(0xC000_0000, 0x4000, SvcMemoryState::Ipc));
        // Exercise the heap exclusion without violating that precondition.
        pt.m_heap_region_start = 0xC000_0000;
        pt.m_heap_region_end = 0xC000_4000;
        assert!(!pt.can_contain(0xC000_0000, 0x4000, SvcMemoryState::Ipc));
    }

    #[test]
    fn can_contain_free_only_checks_region() {
        let pt = aarch32_layout();
        // Free should accept anything in the address space, even inside heap.
        assert!(pt.can_contain(0x6000_0000, 0x4000, SvcMemoryState::Free));
        // Outside the address space rejects.
        assert!(!pt.can_contain(0xFFFF_FFFF, 0x4000, SvcMemoryState::Free));
    }
}
