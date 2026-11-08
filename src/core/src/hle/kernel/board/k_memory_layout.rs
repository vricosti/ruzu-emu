//! Port of zuyu/src/core/hle/kernel/board/nintendo/nx/k_memory_layout.h/.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-21
//!
//! Board-level memory layout constants and initialization for the
//! Nintendo NX (Switch). Sets up device physical memory regions,
//! DRAM regions, and pool partition regions.

use crate::hle::kernel::k_memory_layout::KMemoryLayout;
use crate::hle::kernel::k_memory_manager::KMemoryManager;
use crate::hle::kernel::k_memory_region_type::*;
use crate::hle::kernel::k_trace::{IS_K_TRACE_ENABLED, K_TRACE_BUFFER_SIZE};

/// Main physical memory address (DRAM base).
/// Maps to upstream `MainMemoryAddress` in the Kernel namespace.
pub const MAIN_MEMORY_ADDRESS: u64 = 0x8000_0000;

/// DRAM physical base address used for memory layout initialization.
/// Maps to upstream `DramPhysicalAddress`.
pub const DRAM_PHYSICAL_ADDRESS: u64 = 0x8000_0000;

/// Size of the early-reserved DRAM region.
/// Maps to upstream `ReservedEarlyDramSize`.
pub const RESERVED_EARLY_DRAM_SIZE: usize = 0x6_0000; // 384 KiB

/// Carveout alignment for pool partitions.
const CARVEOUT_ALIGNMENT: usize = 0x2_0000; // 128 KiB

/// Maximum carveout size.
const CARVEOUT_SIZE_MAX: usize = 512 * 1024 * 1024 - CARVEOUT_ALIGNMENT;

// ---------------------------------------------------------------------------
// Helper: SetupPowerManagementControllerMemoryRegion
// Port of upstream anonymous `SetupPowerManagementControllerMemoryRegion`.
// ---------------------------------------------------------------------------

fn setup_power_management_controller_memory_region(layout: &mut KMemoryLayout) -> bool {
    // Above firmware 2.0.0, the PMC is not mappable.
    let phys_tree = layout.get_physical_memory_region_tree_mut();
    if !phys_tree.insert(
        0x7000_E000,
        0x400,
        K_MEMORY_REGION_TYPE_NONE.get_value() | K_MEMORY_REGION_ATTR_NO_USER_MAP,
        0,
        0,
    ) {
        return false;
    }
    phys_tree.insert(
        0x7000_E400,
        0xC00,
        K_MEMORY_REGION_TYPE_POWER_MANAGEMENT_CONTROLLER.get_value()
            | K_MEMORY_REGION_ATTR_NO_USER_MAP,
        0,
        0,
    )
}

// ---------------------------------------------------------------------------
// Helper: InsertPoolPartitionRegionIntoBothTrees
// Port of upstream `InsertPoolPartitionRegionIntoBothTrees`.
// ---------------------------------------------------------------------------

fn insert_pool_partition_region_into_both_trees(
    layout: &mut KMemoryLayout,
    start: u64,
    size: usize,
    phys_type: KMemoryRegionTypeValue,
    virt_type: KMemoryRegionTypeValue,
    cur_attr: &mut u32,
) {
    let attr = *cur_attr;
    *cur_attr += 1;

    // Insert into physical tree.
    assert!(
        layout.get_physical_memory_region_tree_mut().insert(
            start,
            size,
            phys_type.get_value(),
            attr,
            0,
        ),
        "Failed to insert pool partition into physical tree"
    );

    // Find the inserted region to get its pair_address.
    let pair_address = {
        let phys = layout
            .get_physical_memory_region_tree()
            .find_by_type_and_attribute(phys_type.get_value(), attr);
        let phys = phys.expect("Failed to find inserted pool partition region");
        assert!(phys.get_end_address() != 0);
        phys.get_pair_address()
    };

    // Insert into virtual tree.
    assert!(
        layout.get_virtual_memory_region_tree_mut().insert(
            pair_address,
            size,
            virt_type.get_value(),
            attr,
            0,
        ),
        "Failed to insert pool partition into virtual tree"
    );
}

// ---------------------------------------------------------------------------
// SetupDevicePhysicalMemoryRegions
// Port of upstream Init::SetupDevicePhysicalMemoryRegions (k_memory_layout.cpp:47-75).
// ---------------------------------------------------------------------------

/// Set up device physical memory regions (MMIO, interrupt controllers, etc.).
/// Port of upstream `Init::SetupDevicePhysicalMemoryRegions`.
pub fn setup_device_physical_memory_regions(layout: &mut KMemoryLayout) {
    // PMC (Power Management Controller) — split into non-mappable + mappable.
    assert!(setup_power_management_controller_memory_region(layout));

    let phys_tree = layout.get_physical_memory_region_tree_mut();

    // Memory Controller.
    assert!(phys_tree.insert(
        0x7001_9000,
        0x1000,
        K_MEMORY_REGION_TYPE_MEMORY_CONTROLLER.get_value() | K_MEMORY_REGION_ATTR_NO_USER_MAP,
        0,
        0,
    ));

    // Memory Controller 0.
    assert!(phys_tree.insert(
        0x7001_C000,
        0x1000,
        K_MEMORY_REGION_TYPE_MEMORY_CONTROLLER0.get_value() | K_MEMORY_REGION_ATTR_NO_USER_MAP,
        0,
        0,
    ));

    // Memory Controller 1.
    assert!(phys_tree.insert(
        0x7001_D000,
        0x1000,
        K_MEMORY_REGION_TYPE_MEMORY_CONTROLLER1.get_value() | K_MEMORY_REGION_ATTR_NO_USER_MAP,
        0,
        0,
    ));

    // GIC region: surrounding.
    assert!(phys_tree.insert(
        0x5004_0000,
        0x1000,
        K_MEMORY_REGION_TYPE_NONE.get_value() | K_MEMORY_REGION_ATTR_NO_USER_MAP,
        0,
        0,
    ));

    // GIC Distributor — ShouldKernelMap.
    assert!(phys_tree.insert(
        0x5004_1000,
        0x1000,
        K_MEMORY_REGION_TYPE_INTERRUPT_DISTRIBUTOR.get_value()
            | K_MEMORY_REGION_ATTR_SHOULD_KERNEL_MAP,
        0,
        0,
    ));

    // GIC CPU Interface — ShouldKernelMap.
    assert!(phys_tree.insert(
        0x5004_2000,
        0x1000,
        K_MEMORY_REGION_TYPE_INTERRUPT_CPU_INTERFACE.get_value()
            | K_MEMORY_REGION_ATTR_SHOULD_KERNEL_MAP,
        0,
        0,
    ));

    // GIC region: surrounding (upper).
    assert!(phys_tree.insert(
        0x5004_3000,
        0x1_D000,
        K_MEMORY_REGION_TYPE_NONE.get_value() | K_MEMORY_REGION_ATTR_NO_USER_MAP,
        0,
        0,
    ));

    // IRAM — ShouldKernelMap (debug-logging-to-iram support).
    assert!(phys_tree.insert(
        0x4000_0000,
        0x4_0000,
        K_MEMORY_REGION_TYPE_LEGACY_LPS_IRAM.get_value() | K_MEMORY_REGION_ATTR_SHOULD_KERNEL_MAP,
        0,
        0,
    ));

    // Above firmware 2.0.0: prevent mapping BPMP exception vectors.
    assert!(phys_tree.insert(
        0x6000_F000,
        0x1000,
        K_MEMORY_REGION_TYPE_NONE.get_value() | K_MEMORY_REGION_ATTR_NO_USER_MAP,
        0,
        0,
    ));

    // Above firmware 2.0.0: prevent mapping the iPatch region.
    assert!(phys_tree.insert(
        0x6001_DC00,
        0x400,
        K_MEMORY_REGION_TYPE_NONE.get_value() | K_MEMORY_REGION_ATTR_NO_USER_MAP,
        0,
        0,
    ));
}

// ---------------------------------------------------------------------------
// SetupDramPhysicalMemoryRegions
// Port of upstream Init::SetupDramPhysicalMemoryRegions (k_memory_layout.cpp:77-98).
// ---------------------------------------------------------------------------

/// Set up DRAM physical memory regions.
/// Port of upstream `Init::SetupDramPhysicalMemoryRegions`.
pub fn setup_dram_physical_memory_regions(layout: &mut KMemoryLayout, mode: common::settings::MemoryLayout) {
    let intended_memory_size = super::k_system_control::init::get_intended_memory_size(mode);
    let physical_memory_base_address =
        super::k_system_control::init::get_kernel_physical_base_address(DRAM_PHYSICAL_ADDRESS, mode);

    let phys_tree = layout.get_physical_memory_region_tree_mut();

    // Insert the full DRAM region.
    assert!(phys_tree.insert(
        physical_memory_base_address,
        intended_memory_size,
        K_MEMORY_REGION_TYPE_DRAM.get_value(),
        0,
        0,
    ));

    // Insert the DramReservedEarly sub-region.
    assert!(phys_tree.insert(
        physical_memory_base_address,
        RESERVED_EARLY_DRAM_SIZE,
        K_MEMORY_REGION_TYPE_DRAM_RESERVED_EARLY.get_value(),
        0,
        0,
    ));

    // Insert the KTrace block at the end of DRAM, if KTrace is enabled.
    if IS_K_TRACE_ENABLED && K_TRACE_BUFFER_SIZE > 0 {
        let ktrace_buffer_phys_addr =
            physical_memory_base_address + intended_memory_size as u64 - K_TRACE_BUFFER_SIZE as u64;
        assert!(phys_tree.insert(
            ktrace_buffer_phys_addr,
            K_TRACE_BUFFER_SIZE,
            K_MEMORY_REGION_TYPE_KERNEL_TRACE_BUFFER.get_value(),
            0,
            0,
        ));
    }
}

// ---------------------------------------------------------------------------
// SetupPoolPartitionMemoryRegions
// Port of upstream Init::SetupPoolPartitionMemoryRegions (k_memory_layout.cpp:100-199).
// ---------------------------------------------------------------------------

/// Set up pool partition memory regions (application, applet, system, etc.).
/// Port of upstream `Init::SetupPoolPartitionMemoryRegions`.
///
/// This is the most complex initialization function, splitting DRAM into
/// application pool, applet pool, unsafe system pool, pool management,
/// and secure system pool regions.
pub fn setup_pool_partition_memory_regions(layout: &mut KMemoryLayout, mode: common::settings::MemoryLayout) {
    // Start by identifying the extents of the DRAM memory region.
    let dram_extents = layout.get_main_memory_physical_extents();
    assert!(dram_extents.get_end_address() != 0);

    // Determine the end of the pool region.
    let pool_end = dram_extents.get_end_address() - K_TRACE_BUFFER_SIZE as u64;

    // Find the start of the kernel DRAM region.
    let kernel_dram_start = {
        let kernel_dram_region = layout
            .get_physical_memory_region_tree()
            .find_first_derived(K_MEMORY_REGION_TYPE_DRAM_KERNEL_BASE);
        let kernel_dram_region = kernel_dram_region.expect("Failed to find DramKernelBase region");
        let addr = kernel_dram_region.get_address();
        assert!(
            addr % CARVEOUT_ALIGNMENT as u64 == 0,
            "kernel_dram_start not aligned to CarveoutAlignment"
        );
        addr
    };

    // Find the start of the pool partitions region.
    let pool_partitions_start = {
        let pool_partitions_region = layout
            .get_physical_memory_region_tree()
            .find_by_type_and_attribute(K_MEMORY_REGION_TYPE_DRAM_POOL_PARTITION.get_value(), 0);
        let pool_partitions_region =
            pool_partitions_region.expect("Failed to find DramPoolPartition region");
        pool_partitions_region.get_address()
    };

    // Get Application and Applet pool sizes.
    let application_pool_size = super::k_system_control::init::get_application_pool_size(mode);
    let applet_pool_size = super::k_system_control::init::get_applet_pool_size(mode);
    let unsafe_system_pool_min_size =
        super::k_system_control::init::get_minimum_non_secure_system_pool_size();

    // Decide on starting addresses for our pools.
    let application_pool_start = pool_end - application_pool_size as u64;
    let applet_pool_start = application_pool_start - applet_pool_size as u64;
    let unsafe_system_pool_start = std::cmp::min(
        kernel_dram_start + CARVEOUT_SIZE_MAX as u64,
        common::alignment::align_down(
            applet_pool_start - unsafe_system_pool_min_size as u64,
            CARVEOUT_ALIGNMENT as u64,
        ),
    );
    let unsafe_system_pool_size = (applet_pool_start - unsafe_system_pool_start) as usize;

    // We want to arrange application pool depending on where the middle of DRAM is.
    let dram_midpoint = (dram_extents.get_address() + dram_extents.get_end_address()) / 2;
    let mut cur_pool_attr: u32 = 0;
    let mut total_overhead_size: usize = 0;

    if dram_extents.get_end_address() <= dram_midpoint || dram_midpoint <= application_pool_start {
        // Single application pool region.
        insert_pool_partition_region_into_both_trees(
            layout,
            application_pool_start,
            application_pool_size,
            K_MEMORY_REGION_TYPE_DRAM_APPLICATION_POOL,
            K_MEMORY_REGION_TYPE_VIRTUAL_DRAM_APPLICATION_POOL,
            &mut cur_pool_attr,
        );
        total_overhead_size +=
            KMemoryManager::calculate_management_overhead_size(application_pool_size);
    } else {
        // Split application pool across DRAM midpoint.
        let first_application_pool_size = (dram_midpoint - application_pool_start) as usize;
        let second_application_pool_size =
            (application_pool_start + application_pool_size as u64 - dram_midpoint) as usize;

        insert_pool_partition_region_into_both_trees(
            layout,
            application_pool_start,
            first_application_pool_size,
            K_MEMORY_REGION_TYPE_DRAM_APPLICATION_POOL,
            K_MEMORY_REGION_TYPE_VIRTUAL_DRAM_APPLICATION_POOL,
            &mut cur_pool_attr,
        );
        insert_pool_partition_region_into_both_trees(
            layout,
            dram_midpoint,
            second_application_pool_size,
            K_MEMORY_REGION_TYPE_DRAM_APPLICATION_POOL,
            K_MEMORY_REGION_TYPE_VIRTUAL_DRAM_APPLICATION_POOL,
            &mut cur_pool_attr,
        );
        total_overhead_size +=
            KMemoryManager::calculate_management_overhead_size(first_application_pool_size);
        total_overhead_size +=
            KMemoryManager::calculate_management_overhead_size(second_application_pool_size);
    }

    // Insert the applet pool.
    insert_pool_partition_region_into_both_trees(
        layout,
        applet_pool_start,
        applet_pool_size,
        K_MEMORY_REGION_TYPE_DRAM_APPLET_POOL,
        K_MEMORY_REGION_TYPE_VIRTUAL_DRAM_APPLET_POOL,
        &mut cur_pool_attr,
    );
    total_overhead_size += KMemoryManager::calculate_management_overhead_size(applet_pool_size);

    // Insert the nonsecure system pool.
    insert_pool_partition_region_into_both_trees(
        layout,
        unsafe_system_pool_start,
        unsafe_system_pool_size,
        K_MEMORY_REGION_TYPE_DRAM_SYSTEM_NON_SECURE_POOL,
        K_MEMORY_REGION_TYPE_VIRTUAL_DRAM_SYSTEM_NON_SECURE_POOL,
        &mut cur_pool_attr,
    );
    total_overhead_size +=
        KMemoryManager::calculate_management_overhead_size(unsafe_system_pool_size);

    // Insert the pool management region.
    total_overhead_size += KMemoryManager::calculate_management_overhead_size(
        ((unsafe_system_pool_start - pool_partitions_start) as usize) - total_overhead_size,
    );
    let pool_management_start = unsafe_system_pool_start - total_overhead_size as u64;
    let pool_management_size = total_overhead_size;
    let mut pool_management_attr: u32 = 0;
    insert_pool_partition_region_into_both_trees(
        layout,
        pool_management_start,
        pool_management_size,
        K_MEMORY_REGION_TYPE_DRAM_POOL_MANAGEMENT,
        K_MEMORY_REGION_TYPE_VIRTUAL_DRAM_POOL_MANAGEMENT,
        &mut pool_management_attr,
    );

    // Insert the system pool.
    let system_pool_size = (pool_management_start - pool_partitions_start) as usize;
    insert_pool_partition_region_into_both_trees(
        layout,
        pool_partitions_start,
        system_pool_size,
        K_MEMORY_REGION_TYPE_DRAM_SYSTEM_POOL,
        K_MEMORY_REGION_TYPE_VIRTUAL_DRAM_SYSTEM_POOL,
        &mut cur_pool_attr,
    );
}
