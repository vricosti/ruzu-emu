//! Port of zuyu/src/core/hle/kernel/k_process_page_table.h
//! Status: EN COURS
//! Derniere synchro: 2026-03-17
//!
//! KProcessPageTable: thin wrapper around KPageTableBase matching upstream.
//! All methods delegate to the inner KPageTableBase.

use std::sync::{Arc, Mutex};

use super::k_memory_block::{
    convert_to_k_memory_permission, KMemoryAttribute, KMemoryInfo, KMemoryPermission, KMemoryState,
    SvcMemoryPermission,
};
use super::k_page_table_base::KPageTableBase;
use super::k_resource_limit::KResourceLimit;
use super::k_typed_address::{KPhysicalAddress, KProcessAddress};
use crate::memory::memory::Memory;

fn svc_perm_to_k_memory_permission(perm: u32) -> KMemoryPermission {
    convert_to_k_memory_permission(SvcMemoryPermission::from_bits_truncate(perm as u8))
}

/// The process page table.
/// Matches upstream `KProcessPageTable` (k_process_page_table.h).
/// Thin wrapper around KPageTableBase.
pub struct KProcessPageTable {
    base: KPageTableBase,
}

impl KProcessPageTable {
    pub fn new() -> Self {
        Self {
            base: KPageTableBase::new(),
        }
    }

    /// Initialize for a user process.
    /// Upstream: `m_page_table.InitializeForProcess(as_type, enable_aslr, enable_das_merge,
    ///     from_back, pool, code_address, code_size, system_resource, resource_limit,
    ///     memory, aslr_space_start)`
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
        self.base.initialize_for_process(
            as_flags,
            enable_aslr,
            enable_das_merge,
            from_back,
            pool,
            code_address,
            code_size,
            system_resource,
            resource_limit,
            memory,
            aslr_space_start,
        )
    }

    pub fn finalize(&mut self) {
        self.base.finalize();
    }

    // -- Region getters delegating to base --

    pub fn get_address_space_start(&self) -> KProcessAddress {
        KProcessAddress::new(self.base.get_address_space_start() as u64)
    }

    pub fn get_address_space_size(&self) -> usize {
        self.base.get_address_space_size()
    }

    pub fn get_heap_region_start(&self) -> KProcessAddress {
        KProcessAddress::new(self.base.get_heap_region_start() as u64)
    }

    pub fn get_heap_region_size(&self) -> usize {
        self.base.get_heap_region_size()
    }

    pub fn get_alias_region_start(&self) -> KProcessAddress {
        KProcessAddress::new(self.base.get_alias_region_start() as u64)
    }

    pub fn get_alias_region_size(&self) -> usize {
        self.base.get_alias_region_size()
    }

    pub fn is_in_alias_region(&self, addr: KProcessAddress, size: usize) -> bool {
        self.base.is_in_alias_region(addr.get() as usize, size)
    }

    pub fn get_stack_region_start(&self) -> KProcessAddress {
        KProcessAddress::new(self.base.get_stack_region_start() as u64)
    }

    pub fn get_stack_region_size(&self) -> usize {
        self.base.get_stack_region_size()
    }

    pub fn get_kernel_map_region_start(&self) -> KProcessAddress {
        KProcessAddress::new(self.base.get_kernel_map_region_start() as u64)
    }

    pub fn get_kernel_map_region_size(&self) -> usize {
        self.base.get_kernel_map_region_size()
    }

    pub fn get_code_region_start(&self) -> KProcessAddress {
        KProcessAddress::new(self.base.get_code_region_start() as u64)
    }

    pub fn get_code_region_size(&self) -> usize {
        self.base.get_code_region_size()
    }

    pub fn get_alias_code_region_start(&self) -> KProcessAddress {
        KProcessAddress::new(self.base.get_alias_code_region_start() as u64)
    }

    pub fn get_alias_code_region_size(&self) -> usize {
        self.base.get_alias_code_region_size()
    }

    pub fn get_address_space_width(&self) -> u32 {
        self.base.get_address_space_width()
    }

    pub fn get_num_guard_pages(&self) -> usize {
        self.base.get_num_guard_pages()
    }

    pub fn get_allocate_option(&self) -> u32 {
        self.base.get_allocate_option()
    }

    pub fn get_current_heap_size(&self) -> usize {
        self.base.get_current_heap_size()
    }

    // -- Mapping operations delegating to base --

    pub fn set_heap_size(&mut self, size: usize) -> (u32, KProcessAddress) {
        let (result, addr) = self.base.set_heap_size(size);
        (result, KProcessAddress::new(addr as u64))
    }

    pub fn set_max_heap_size(&mut self, size: usize) -> u32 {
        self.base.set_max_heap_size(size)
    }

    pub fn set_memory_permission(&mut self, addr: KProcessAddress, size: usize, perm: u32) -> u32 {
        self.base.set_memory_permission(
            addr.get() as usize,
            size,
            svc_perm_to_k_memory_permission(perm),
        )
    }

    pub fn set_memory_attribute(
        &mut self,
        addr: KProcessAddress,
        size: usize,
        mask: u32,
        attr: u32,
    ) -> u32 {
        self.base
            .set_memory_attribute(addr.get() as usize, size, mask, attr)
    }

    pub fn map_memory(&mut self, dst: KProcessAddress, src: KProcessAddress, size: usize) -> u32 {
        self.base
            .map_memory(dst.get() as usize, src.get() as usize, size)
    }

    pub fn unmap_memory(&mut self, dst: KProcessAddress, src: KProcessAddress, size: usize) -> u32 {
        self.base
            .unmap_memory(dst.get() as usize, src.get() as usize, size)
    }

    /// Upstream: `KProcessPageTable::UnmapProcessMemory`.
    pub fn unmap_process_memory(
        &mut self,
        dst_address: KProcessAddress,
        size: usize,
        src_page_table: &KProcessPageTable,
        src_address: KProcessAddress,
    ) -> u32 {
        self.base.unmap_process_memory(
            dst_address.get() as usize,
            size,
            &src_page_table.base,
            src_address.get() as usize,
        )
    }

    /// Same-table helper used by the current Rust SVC process-handle owner graph.
    pub fn unmap_process_memory_same_table(
        &mut self,
        dst_address: KProcessAddress,
        size: usize,
        src_address: KProcessAddress,
    ) -> u32 {
        self.base.unmap_process_memory_same_table(
            dst_address.get() as usize,
            size,
            src_address.get() as usize,
        )
    }

    pub fn map_physical_memory(&mut self, addr: KProcessAddress, size: usize) -> u32 {
        self.base.map_physical_memory(addr.get() as usize, size)
    }

    pub fn unmap_physical_memory(&mut self, addr: KProcessAddress, size: usize) -> u32 {
        self.base.unmap_physical_memory(addr.get() as usize, size)
    }

    pub fn map_static(&mut self, phys_addr: u64, size: usize, perm: KMemoryPermission) -> u32 {
        self.base.map_static(phys_addr, size, perm)
    }

    pub fn map_io(&mut self, phys_addr: u64, size: usize, perm: KMemoryPermission) -> u32 {
        self.base.map_io(phys_addr, size, perm)
    }

    pub fn map_region(&mut self, region_type: u32, perm: KMemoryPermission) -> u32 {
        self.base.map_region(region_type, perm)
    }

    pub fn set_process_memory_permission(
        &mut self,
        addr: KProcessAddress,
        size: usize,
        perm: KMemoryPermission,
    ) -> u32 {
        self.base
            .set_process_memory_permission(addr.get() as usize, size, perm)
    }

    // -- Query --

    pub fn query_info(&self, addr: usize) -> Option<KMemoryInfo> {
        self.base.query_info(addr)
    }

    pub fn query_info_with_page_info(
        &self,
        addr: usize,
    ) -> Option<(KMemoryInfo, super::svc::svc_types::PageInfo)> {
        self.base.query_info_with_page_info(addr)
    }

    pub fn dump_memory_blocks(&self) {
        self.base.m_memory_block_manager.dump_blocks();
    }

    /// Iterate over all memory blocks (for diagnostics / snapshotting).
    pub fn iter_blocks(&self) -> impl Iterator<Item = &super::k_memory_block::KMemoryBlock> {
        self.base.m_memory_block_manager.iter()
    }

    pub fn contains(&self, addr: KProcessAddress, size: usize) -> bool {
        self.base.contains_range(addr.get() as usize, size)
    }

    pub fn get_physical_address(&self, address: KProcessAddress) -> Option<KPhysicalAddress> {
        // Query the page table implementation for the physical address.
        // Upstream: m_impl->GetPhysicalAddress(virt_addr)
        if let Some(ref impl_) = self.base.m_impl {
            impl_
                .get_physical_address(address.get())
                .map(KPhysicalAddress::new)
        } else {
            None
        }
    }

    /// Upstream: `KProcessPageTable::InvalidateProcessDataCache`.
    pub fn invalidate_process_data_cache(&self, addr: KProcessAddress, size: usize) -> u32 {
        self.base
            .invalidate_process_data_cache(addr.get() as usize, size)
    }

    /// Upstream: `KProcessPageTable::ReadDebugMemory`.
    pub fn read_debug_memory(
        &self,
        dst_address: KProcessAddress,
        src_address: KProcessAddress,
        size: usize,
    ) -> u32 {
        self.base
            .read_debug_memory(dst_address.get() as usize, src_address.get() as usize, size)
    }

    /// Upstream: `KProcessPageTable::WriteDebugMemory`.
    pub fn write_debug_memory(
        &self,
        dst_address: KProcessAddress,
        src_address: KProcessAddress,
        size: usize,
    ) -> u32 {
        self.base
            .write_debug_memory(dst_address.get() as usize, src_address.get() as usize, size)
    }

    /// Upstream: `KProcessPageTable::CopyMemoryFromLinearToUser`.
    pub fn copy_memory_from_linear_to_user(
        &self,
        dst_addr: KProcessAddress,
        size: usize,
        src_addr: KProcessAddress,
        src_state_mask: KMemoryState,
        src_state: KMemoryState,
        src_test_perm: KMemoryPermission,
        src_attr_mask: KMemoryAttribute,
        src_attr: KMemoryAttribute,
    ) -> u32 {
        self.base.copy_memory_from_linear_to_user(
            dst_addr.get() as usize,
            size,
            src_addr.get() as usize,
            src_state_mask,
            src_state,
            src_test_perm,
            src_attr_mask,
            src_attr,
        )
    }

    /// Upstream: `KProcessPageTable::CopyMemoryFromLinearToKernel`.
    pub fn copy_memory_from_linear_to_kernel(
        &self,
        dst_addr: usize,
        size: usize,
        src_addr: KProcessAddress,
        src_state_mask: KMemoryState,
        src_state: KMemoryState,
        src_test_perm: KMemoryPermission,
        src_attr_mask: KMemoryAttribute,
        src_attr: KMemoryAttribute,
    ) -> u32 {
        self.base.copy_memory_from_linear_to_kernel(
            dst_addr,
            size,
            src_addr.get() as usize,
            src_state_mask,
            src_state,
            src_test_perm,
            src_attr_mask,
            src_attr,
        )
    }

    /// Upstream: `KProcessPageTable::CopyMemoryFromUserToLinear`.
    pub fn copy_memory_from_user_to_linear(
        &self,
        dst_addr: KProcessAddress,
        size: usize,
        dst_state_mask: KMemoryState,
        dst_state: KMemoryState,
        dst_test_perm: KMemoryPermission,
        dst_attr_mask: KMemoryAttribute,
        dst_attr: KMemoryAttribute,
        src_addr: KProcessAddress,
    ) -> u32 {
        self.base.copy_memory_from_user_to_linear(
            dst_addr.get() as usize,
            size,
            dst_state_mask,
            dst_state,
            dst_test_perm,
            dst_attr_mask,
            dst_attr,
            src_addr.get() as usize,
        )
    }

    /// Upstream: `KProcessPageTable::CopyMemoryFromKernelToLinear`.
    pub fn copy_memory_from_kernel_to_linear(
        &self,
        dst_addr: KProcessAddress,
        size: usize,
        dst_state_mask: KMemoryState,
        dst_state: KMemoryState,
        dst_test_perm: KMemoryPermission,
        dst_attr_mask: KMemoryAttribute,
        dst_attr: KMemoryAttribute,
        src_addr: usize,
    ) -> u32 {
        self.base.copy_memory_from_kernel_to_linear(
            dst_addr.get() as usize,
            size,
            dst_state_mask,
            dst_state,
            dst_test_perm,
            dst_attr_mask,
            dst_attr,
            src_addr,
        )
    }

    /// Upstream: `KProcessPageTable::CopyMemoryFromHeapToHeap`.
    pub fn copy_memory_from_heap_to_heap(
        &self,
        dst_page_table: &KProcessPageTable,
        dst_addr: KProcessAddress,
        size: usize,
        dst_state_mask: KMemoryState,
        dst_state: KMemoryState,
        dst_test_perm: KMemoryPermission,
        dst_attr_mask: KMemoryAttribute,
        dst_attr: KMemoryAttribute,
        src_addr: KProcessAddress,
        src_state_mask: KMemoryState,
        src_state: KMemoryState,
        src_test_perm: KMemoryPermission,
        src_attr_mask: KMemoryAttribute,
        src_attr: KMemoryAttribute,
    ) -> u32 {
        self.base.copy_memory_from_heap_to_heap(
            &dst_page_table.base,
            dst_addr.get() as usize,
            size,
            dst_state_mask,
            dst_state,
            dst_test_perm,
            dst_attr_mask,
            dst_attr,
            src_addr.get() as usize,
            src_state_mask,
            src_state,
            src_test_perm,
            src_attr_mask,
            src_attr,
        )
    }

    /// Upstream: `KProcessPageTable::CopyMemoryFromHeapToHeapWithoutCheckDestination`.
    pub fn copy_memory_from_heap_to_heap_without_check_destination(
        &self,
        dst_page_table: &KProcessPageTable,
        dst_addr: KProcessAddress,
        size: usize,
        dst_state_mask: KMemoryState,
        dst_state: KMemoryState,
        dst_test_perm: KMemoryPermission,
        dst_attr_mask: KMemoryAttribute,
        dst_attr: KMemoryAttribute,
        src_addr: KProcessAddress,
        src_state_mask: KMemoryState,
        src_state: KMemoryState,
        src_test_perm: KMemoryPermission,
        src_attr_mask: KMemoryAttribute,
        src_attr: KMemoryAttribute,
    ) -> u32 {
        self.base
            .copy_memory_from_heap_to_heap_without_check_destination(
                &dst_page_table.base,
                dst_addr.get() as usize,
                size,
                dst_state_mask,
                dst_state,
                dst_test_perm,
                dst_attr_mask,
                dst_attr,
                src_addr.get() as usize,
                src_state_mask,
                src_state,
                src_test_perm,
                src_attr_mask,
                src_attr,
            )
    }

    /// Upstream: `bool CanContain(KProcessAddress addr, size_t size, KMemoryState state) const`.
    pub fn can_contain(
        &self,
        addr: KProcessAddress,
        size: usize,
        state: super::k_memory_block::KMemoryState,
    ) -> bool {
        self.base.can_contain_k(addr.get() as usize, size, state)
    }

    // -- Page mapping --

    pub fn map_pages_find_free(
        &mut self,
        num_pages: usize,
        alignment: usize,
        phys_addr: u64,
        is_pa_valid: bool,
        region_start: KProcessAddress,
        region_num_pages: usize,
        state: super::k_memory_block::KMemoryState,
        perm: KMemoryPermission,
    ) -> (u32, KProcessAddress) {
        let (result, addr) = self.base.map_pages_find_free(
            num_pages,
            alignment,
            phys_addr,
            is_pa_valid,
            region_start.get() as usize,
            region_num_pages,
            state,
            perm,
        );
        (result, KProcessAddress::new(addr as u64))
    }

    pub fn map_pages_at_address(
        &mut self,
        addr: KProcessAddress,
        num_pages: usize,
        state: super::k_memory_block::KMemoryState,
        perm: KMemoryPermission,
    ) -> u32 {
        self.base
            .map_pages_at_address(addr.get() as usize, num_pages, state, perm)
    }

    pub fn unmap_pages(
        &mut self,
        addr: KProcessAddress,
        num_pages: usize,
        state: super::k_memory_block::KMemoryState,
    ) -> u32 {
        self.base.unmap_pages(addr.get() as usize, num_pages, state)
    }

    pub fn lock_for_transfer_memory(
        &mut self,
        out_pg: &mut super::k_page_group::KPageGroup,
        addr: KProcessAddress,
        size: usize,
        perm: KMemoryPermission,
    ) -> u32 {
        self.base
            .lock_for_transfer_memory(out_pg, addr.get() as usize, size, perm)
    }

    pub fn unlock_for_transfer_memory(
        &mut self,
        addr: KProcessAddress,
        size: usize,
        pg: &super::k_page_group::KPageGroup,
    ) -> u32 {
        self.base
            .unlock_for_transfer_memory(addr.get() as usize, size, pg)
    }

    pub fn lock_for_code_memory(
        &mut self,
        out_pg: &mut super::k_page_group::KPageGroup,
        addr: KProcessAddress,
        size: usize,
    ) -> u32 {
        self.base
            .lock_for_code_memory(out_pg, addr.get() as usize, size)
    }

    pub fn unlock_for_code_memory(
        &mut self,
        addr: KProcessAddress,
        size: usize,
        pg: &super::k_page_group::KPageGroup,
    ) -> u32 {
        self.base
            .unlock_for_code_memory(addr.get() as usize, size, pg)
    }

    // -- IPC memory locking --

    pub fn lock_for_ipc_user_buffer(
        &mut self,
        out_paddr: &mut u64,
        addr: KProcessAddress,
        size: usize,
    ) -> u32 {
        self.base
            .lock_for_ipc_user_buffer(out_paddr, addr.get() as usize, size)
    }

    pub fn unlock_for_ipc_user_buffer(&mut self, addr: KProcessAddress, size: usize) -> u32 {
        self.base
            .unlock_for_ipc_user_buffer(addr.get() as usize, size)
    }

    pub fn setup_for_ipc_client(
        &mut self,
        addr: KProcessAddress,
        size: usize,
        test_perm: KMemoryPermission,
        dst_state: super::k_memory_block::KMemoryState,
    ) -> u32 {
        self.base
            .setup_for_ipc_client(addr.get() as usize, size, test_perm, dst_state)
            .0
    }

    pub fn setup_for_ipc(
        &mut self,
        out_addr: &mut KProcessAddress,
        size: usize,
        src_addr: KProcessAddress,
        src_page_table: &mut KProcessPageTable,
        test_perm: KMemoryPermission,
        dst_state: super::k_memory_block::KMemoryState,
        send: bool,
    ) -> u32 {
        let mut out = out_addr.get() as usize;
        let rc = self.base.setup_for_ipc(
            &mut out,
            size,
            src_addr.get() as usize,
            &mut src_page_table.base,
            test_perm,
            dst_state,
            send,
        );
        *out_addr = KProcessAddress::new(out as u64);
        rc
    }

    pub fn setup_for_ipc_server(
        &mut self,
        out_addr: &mut KProcessAddress,
        size: usize,
        src_addr: KProcessAddress,
        test_perm: KMemoryPermission,
        dst_state: super::k_memory_block::KMemoryState,
        src_table: &mut KProcessPageTable,
        send: bool,
    ) -> u32 {
        let mut out = out_addr.get() as usize;
        let rc = self.base.setup_for_ipc_server(
            &mut out,
            size,
            src_addr.get() as usize,
            test_perm,
            dst_state,
            &mut src_table.base,
            send,
        );
        *out_addr = KProcessAddress::new(out as u64);
        rc
    }

    pub fn cleanup_for_ipc_server(
        &mut self,
        addr: KProcessAddress,
        size: usize,
        dst_state: super::k_memory_block::KMemoryState,
    ) -> u32 {
        self.base
            .cleanup_for_ipc_server(addr.get() as usize, size, dst_state)
    }

    pub fn cleanup_for_ipc_client(
        &mut self,
        addr: KProcessAddress,
        size: usize,
        dst_state: super::k_memory_block::KMemoryState,
    ) -> u32 {
        self.base
            .cleanup_for_ipc_client(addr.get() as usize, size, dst_state)
    }

    // -- Code memory mapping --

    /// Map code memory: copies src pages to dst, reprotects src.
    /// Upstream: `KProcessPageTable::MapCodeMemory`.
    pub fn map_code_memory(
        &mut self,
        dst: KProcessAddress,
        src: KProcessAddress,
        size: usize,
    ) -> u32 {
        self.base
            .map_code_memory(dst.get() as usize, src.get() as usize, size)
    }

    /// Unmap code memory: unmaps dst, restores src permissions.
    /// Upstream: `KProcessPageTable::UnmapCodeMemory`.
    pub fn unmap_code_memory(
        &mut self,
        dst: KProcessAddress,
        src: KProcessAddress,
        size: usize,
    ) -> u32 {
        self.base
            .unmap_code_memory(dst.get() as usize, src.get() as usize, size)
    }

    // -- Memory bridge --

    /// Set the Memory bridge on the underlying KPageTableBase.
    /// Must be called after the address space is configured.
    pub fn set_memory(&mut self, memory: Arc<Mutex<Memory>>) {
        self.base.set_memory(memory);
    }

    // -- Page group mapping --

    /// Map a KPageGroup into the process address space.
    /// Upstream: `KProcessPageTable::MapPageGroup`.
    pub fn map_page_group(
        &mut self,
        addr: super::k_typed_address::KProcessAddress,
        pg: &super::k_page_group::KPageGroup,
        state: super::k_memory_block::KMemoryState,
        perm: KMemoryPermission,
    ) -> u32 {
        self.base
            .map_page_group(addr.get() as usize, pg, state, perm)
    }

    /// Unmap a KPageGroup from the process address space.
    /// Upstream: `KProcessPageTable::UnmapPageGroup`.
    pub fn unmap_page_group(
        &mut self,
        addr: super::k_typed_address::KProcessAddress,
        pg: &super::k_page_group::KPageGroup,
        state: super::k_memory_block::KMemoryState,
    ) -> u32 {
        self.base.unmap_page_group(addr.get() as usize, pg, state)
    }

    /// Create a KPageGroup from the current mapping and open references.
    /// Upstream: `KProcessPageTable::MakeAndOpenPageGroup`.
    pub fn make_and_open_page_group(
        &self,
        out: &mut super::k_page_group::KPageGroup,
        address: super::k_typed_address::KProcessAddress,
        num_pages: usize,
        state_mask: super::k_memory_block::KMemoryState,
        state: super::k_memory_block::KMemoryState,
        perm_mask: KMemoryPermission,
        perm: KMemoryPermission,
        attr_mask: super::k_memory_block::KMemoryAttribute,
        attr: super::k_memory_block::KMemoryAttribute,
    ) -> u32 {
        self.base.make_and_open_page_group(
            out,
            address.get() as usize,
            num_pages,
            state_mask,
            state,
            perm_mask,
            perm,
            attr_mask,
            attr,
        )
    }

    // -- Direct base access --

    pub fn get_base(&self) -> &KPageTableBase {
        &self.base
    }

    pub fn get_base_mut(&mut self) -> &mut KPageTableBase {
        &mut self.base
    }

    pub fn get_block_info_manager(
        &self,
    ) -> Option<Arc<super::k_dynamic_resource_manager::KBlockInfoManager>> {
        self.base.get_block_info_manager()
    }

    // -- Compatibility setters (used during process load before InitializeForProcess) --
    // These directly modify the base fields. Will be removed once
    // KProcess::load_from_metadata calls initialize_for_process instead.

    pub fn configure_address_space(&mut self, start: KProcessAddress, size: usize, width: u32) {
        let base = self.get_base_mut();
        base.m_address_space_start = start.get() as usize;
        base.m_address_space_end = start.get() as usize + size;
        base.m_address_space_width = width;
        // Initialize the block manager for this address space. Pull the
        // kernel-wide slab manager so the sentinel block comes from the
        // shared slab — matches upstream's `Initialize(start, end, slab)`
        // signature.
        let slab = crate::hle::kernel::kernel::get_kernel_ref()
            .and_then(|k| k.get_memory_block_slab_manager());
        base.m_memory_block_slab_manager = slab.clone();
        base.m_block_info_manager =
            crate::hle::kernel::kernel::get_kernel_ref().and_then(|k| k.get_block_info_manager());
        let slab_ref = slab.as_deref();
        let _ = base.m_memory_block_manager.initialize(
            base.m_address_space_start,
            base.m_address_space_end,
            slab_ref,
        );
        // Initialize the page table implementation (Common::PageTable).
        // Upstream does this in InitializeForProcess; here we do it in the
        // legacy path as well so that Operate() can write page table entries.
        base.initialize_impl();
    }

    pub fn set_code_region(&mut self, start: KProcessAddress, size: usize) {
        let base = self.get_base_mut();
        base.m_code_region_start = start.get() as usize;
        base.m_code_region_end = start.get() as usize + size;
        // For 32-bit, alias_code_region covers the full address space.
        if base.m_address_space_width <= 32 {
            base.m_alias_code_region_start = base.m_code_region_start;
            base.m_alias_code_region_end = base.m_address_space_end;
            base.m_stack_region_start = base.m_code_region_start;
            base.m_stack_region_end = base.m_code_region_end;
            base.m_kernel_map_region_start = base.m_code_region_start;
            base.m_kernel_map_region_end = base.m_code_region_end;
        }
    }

    pub fn set_stack_region(&mut self, start: KProcessAddress, size: usize) {
        // Direct set for legacy compatibility.
        let base = self.get_base_mut();
        base.m_stack_region_start = start.get() as usize;
        base.m_stack_region_end = start.get() as usize + size;
    }

    pub fn set_heap_region(&mut self, start: KProcessAddress, size: usize) {
        let base = self.get_base_mut();
        base.m_heap_region_start = start.get() as usize;
        base.m_heap_region_end = start.get() as usize + size;
        base.m_max_heap_size = size;
        if base.m_current_heap_end < base.m_heap_region_start {
            base.m_current_heap_end = base.m_heap_region_start;
        }
        if base.m_current_heap_end > base.m_heap_region_end {
            base.m_current_heap_end = base.m_heap_region_end;
        }
    }
}

impl Default for KProcessPageTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_memory_permission_converts_svc_permission_bits() {
        assert_eq!(
            svc_perm_to_k_memory_permission(
                crate::hle::kernel::svc::svc_types::MemoryPermission::Read as u32
            ),
            KMemoryPermission::USER_READ
        );
        assert_eq!(
            svc_perm_to_k_memory_permission(
                crate::hle::kernel::svc::svc_types::MemoryPermission::ReadWrite as u32
            ),
            KMemoryPermission::USER_READ_WRITE
        );
        assert_eq!(
            svc_perm_to_k_memory_permission(
                crate::hle::kernel::svc::svc_types::MemoryPermission::None as u32
            ),
            KMemoryPermission::KERNEL_READ | KMemoryPermission::NOT_MAPPED
        );
    }
}
