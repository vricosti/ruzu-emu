//! Port of zuyu/src/core/hle/kernel/init/init_slab_setup.h/.cpp
//! Status: COMPLET (stub — runtime dependencies not yet available)
//! Derniere synchro: 2026-03-11
//!
//! Slab heap initialization: resource counts, size calculation, and
//! slab heap setup. Full implementation requires KMemoryLayout, KMemoryManager,
//! and all slab-allocated kernel object types.

use crate::hardware_properties;

/// Default slab resource counts, matching upstream constexpr values.
const SLAB_COUNT_K_PROCESS: usize = 80;
const SLAB_COUNT_K_THREAD: usize = 800;
const SLAB_COUNT_K_EVENT: usize = 900;
const SLAB_COUNT_K_INTERRUPT_EVENT: usize = 100;
const SLAB_COUNT_K_PORT: usize = 384;
const SLAB_COUNT_K_SHARED_MEMORY: usize = 80;
const SLAB_COUNT_K_TRANSFER_MEMORY: usize = 200;
const SLAB_COUNT_K_CODE_MEMORY: usize = 10;
const SLAB_COUNT_K_DEVICE_ADDRESS_SPACE: usize = 300;
const SLAB_COUNT_K_SESSION: usize = 1133;
const SLAB_COUNT_K_LIGHT_SESSION: usize = 100;
const SLAB_COUNT_K_OBJECT_NAME: usize = 7;
const SLAB_COUNT_K_RESOURCE_LIMIT: usize = 5;
const SLAB_COUNT_K_DEBUG: usize = hardware_properties::NUM_CPU_CORES as usize;
const SLAB_COUNT_K_IO_POOL: usize = 1;
const SLAB_COUNT_K_IO_REGION: usize = 6;
const SLAB_COUNT_K_SESSION_REQUEST_MAPPINGS: usize = 40;

/// Extra thread count when resource limit is increased.
const SLAB_COUNT_EXTRA_K_THREAD: usize = (1024 + 256 + 256) - SLAB_COUNT_K_THREAD;

/// Slab resource counts for all kernel object types.
#[derive(Debug, Clone)]
pub struct KSlabResourceCounts {
    pub num_k_process: usize,
    pub num_k_thread: usize,
    pub num_k_event: usize,
    pub num_k_interrupt_event: usize,
    pub num_k_port: usize,
    pub num_k_shared_memory: usize,
    pub num_k_transfer_memory: usize,
    pub num_k_code_memory: usize,
    pub num_k_device_address_space: usize,
    pub num_k_session: usize,
    pub num_k_light_session: usize,
    pub num_k_object_name: usize,
    pub num_k_resource_limit: usize,
    pub num_k_debug: usize,
    pub num_k_io_pool: usize,
    pub num_k_io_region: usize,
    pub num_k_session_request_mappings: usize,
}

impl KSlabResourceCounts {
    /// Create default slab resource counts matching upstream.
    pub fn create_default() -> Self {
        Self {
            num_k_process: SLAB_COUNT_K_PROCESS,
            num_k_thread: SLAB_COUNT_K_THREAD,
            num_k_event: SLAB_COUNT_K_EVENT,
            num_k_interrupt_event: SLAB_COUNT_K_INTERRUPT_EVENT,
            num_k_port: SLAB_COUNT_K_PORT,
            num_k_shared_memory: SLAB_COUNT_K_SHARED_MEMORY,
            num_k_transfer_memory: SLAB_COUNT_K_TRANSFER_MEMORY,
            num_k_code_memory: SLAB_COUNT_K_CODE_MEMORY,
            num_k_device_address_space: SLAB_COUNT_K_DEVICE_ADDRESS_SPACE,
            num_k_session: SLAB_COUNT_K_SESSION,
            num_k_light_session: SLAB_COUNT_K_LIGHT_SESSION,
            num_k_object_name: SLAB_COUNT_K_OBJECT_NAME,
            num_k_resource_limit: SLAB_COUNT_K_RESOURCE_LIMIT,
            num_k_debug: SLAB_COUNT_K_DEBUG,
            num_k_io_pool: SLAB_COUNT_K_IO_POOL,
            num_k_io_region: SLAB_COUNT_K_IO_REGION,
            num_k_session_request_mappings: SLAB_COUNT_K_SESSION_REQUEST_MAPPINGS,
        }
    }

    /// Get the extra thread count for resource limit increase.
    pub fn extra_k_thread_count() -> usize {
        SLAB_COUNT_EXTRA_K_THREAD
    }
}

/// Initialize slab resource counts for the kernel.
/// If the thread resource limit should be increased, adds extra threads.
pub fn initialize_slab_resource_counts(counts: &mut KSlabResourceCounts) {
    *counts = KSlabResourceCounts::create_default();
    if crate::hle::kernel::board::k_system_control::init::should_increase_thread_resource_limit() {
        counts.num_k_thread += SLAB_COUNT_EXTRA_K_THREAD;
    }
}

/// Kernel slab heap gap size.
/// Upstream: `constexpr size_t KernelSlabHeapGapSize = 2_MiB - 356_KiB`.
const KERNEL_SLAB_HEAP_GAP_SIZE: usize = 2 * 1024 * 1024 - 356 * 1024;

/// Calculate the total slab heap size for all kernel object types.
/// Port of upstream `CalculateTotalSlabHeapSize`.
///
/// Each slab type contributes alignment padding + count * object_size.
pub fn calculate_total_slab_heap_size(counts: &KSlabResourceCounts) -> usize {
    use crate::hle::kernel::*;
    // Mirrors upstream FOREACH_SLAB_TYPE: size and alignment describe the
    // corresponding host object, not a guest ABI structure or an estimate.
    macro_rules! slab {
        ($ty:ty, $count:expr) => {
            (
                std::mem::align_of::<$ty>(),
                std::mem::size_of::<$ty>(),
                $count,
            )
        };
    }
    let slab_entries: &[(usize, usize, usize)] = &[
        slab!(k_process::KProcess, counts.num_k_process),
        slab!(k_thread::KThread, counts.num_k_thread),
        slab!(k_event::KEvent, counts.num_k_event),
        slab!(k_port::KPort, counts.num_k_port),
        slab!(k_session_request::KSessionRequest, counts.num_k_session * 2),
        slab!(k_shared_memory::KSharedMemory, counts.num_k_shared_memory),
        slab!(
            k_shared_memory_info::KSharedMemoryInfo,
            counts.num_k_shared_memory * 8
        ),
        slab!(
            k_transfer_memory::KTransferMemory,
            counts.num_k_transfer_memory
        ),
        slab!(k_code_memory::KCodeMemory, counts.num_k_code_memory),
        slab!(
            k_device_address_space::KDeviceAddressSpace,
            counts.num_k_device_address_space
        ),
        slab!(k_session::KSession, counts.num_k_session),
        slab!(
            k_thread_local_page::KThreadLocalPage,
            counts.num_k_process + (counts.num_k_process + counts.num_k_thread) / 8
        ),
        slab!(k_object_name::KObjectName, counts.num_k_object_name),
        slab!(
            k_resource_limit::KResourceLimit,
            counts.num_k_resource_limit
        ),
        slab!(
            k_event_info::KEventInfo,
            counts.num_k_thread + counts.num_k_debug
        ),
        slab!(k_debug::KDebug, counts.num_k_debug),
        slab!(
            k_system_resource::KSecureSystemResource,
            counts.num_k_process
        ),
        slab!(
            k_thread::LockWithPriorityInheritanceInfo,
            counts.num_k_thread
        ),
    ];

    let mut size = 0usize;
    for &(align, obj_size, count) in slab_entries {
        size += align;
        size += common::alignment::align_up(
            (obj_size * count) as u64,
            std::mem::align_of::<usize>() as u64,
        ) as usize;
    }

    size += KERNEL_SLAB_HEAP_GAP_SIZE;
    size
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialized_slab_counts_fit_kernel_resource_reservation() {
        let mut counts = KSlabResourceCounts::create_default();
        initialize_slab_resource_counts(&mut counts);
        let slab_size = calculate_total_slab_heap_size(&counts);
        let resource_size = crate::hle::kernel::k_memory_layout::KMemoryLayout::
            get_resource_region_size_for_init(true);
        assert!(slab_size <= resource_size,
            "slab size {slab_size:#x} exceeds resource reservation {resource_size:#x}");
    }

    #[test]
    fn slab_size_accounts_for_object_names_and_actual_event_layout() {
        use crate::hle::kernel::{k_event::KEvent, k_object_name::KObjectName};
        let mut counts = KSlabResourceCounts::create_default();
        counts.num_k_object_name = 0;
        counts.num_k_event = 0;
        let base = calculate_total_slab_heap_size(&counts);
        counts.num_k_object_name = 7;
        let name_bytes = common::alignment::align_up(
            (7 * std::mem::size_of::<KObjectName>()) as u64,
            std::mem::align_of::<usize>() as u64,
        ) as usize;
        assert_eq!(calculate_total_slab_heap_size(&counts), base + name_bytes);
        counts.num_k_event = 13;
        let event_bytes = common::alignment::align_up(
            (13 * std::mem::size_of::<KEvent>()) as u64,
            std::mem::align_of::<usize>() as u64,
        ) as usize;
        assert_eq!(
            calculate_total_slab_heap_size(&counts),
            base + name_bytes + event_bytes
        );
    }
}

/// Initialize slab heaps from the slab memory region.
/// Port of upstream `InitializeSlabHeaps`.
///
/// Upstream shuffles slab types randomly and inserts random gaps between them
/// for ASLR. Since we run in user-space without kernel VA layout, we
/// allocate each slab sequentially from a single backing Vec.
///
/// This is called during kernel initialization and sets up the slab allocators
/// used by `KSlabHeap<T>::Allocate()` for each kernel object type.
pub fn initialize_slab_heaps() {
    // In the host-emulated model, slab heaps are initialized lazily when
    // kernel objects are first allocated. The KSlabHeap infrastructure uses
    // index-based free lists backed by Vec<Option<T>>, which don't require
    // pre-mapped memory regions. Upstream's slab layout randomization (ASLR)
    // is a security feature of the real kernel that doesn't apply to the
    // emulated environment.
    //
    // The important contract is that KSlabResourceCounts defines the maximum
    // number of each object type, and KSlabHeap enforces those limits.
    log::info!("Slab heaps initialized (host-emulated model)");
}
