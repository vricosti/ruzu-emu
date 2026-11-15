//! Port of Lichii `video_core/query_dma.h` (absent from Eden): macro DMA reads of memory the
//! query cache still holds reports for.
//!
//! Only `FlushPending` is ported. `HasPendingQueries`, `HoldsReports`, `Trace` and
//! `TraceEnabled` are diagnostics (the `EDEN_NX2_QUERY_DMA_TRACE` log) or serve paths ruzu does
//! not have.

use parking_lot::Mutex;

use crate::memory_manager::MemoryManager;

/// Device page size `FlushPending` walks the read by.
const PAGE_SIZE: u64 = 0x1000;

/// The device ranges of `[source, source + size)`, one per GPU page that is mapped.
pub fn device_pages(
    source: u64,
    size: u64,
    mut gpu_to_cpu_address: impl FnMut(u64) -> Option<u64>,
) -> Vec<(u64, u64)> {
    let mut pages = Vec::new();
    let end = source.wrapping_add(size);
    let mut begin = source;
    while begin < end {
        let page_end = ((begin & !(PAGE_SIZE - 1)) + PAGE_SIZE).min(end);
        if let Some(device) = gpu_to_cpu_address(begin) {
            pages.push((device, page_end - begin));
        }
        begin = page_end;
    }
    pages
}

/// Writes the reports in `[source, source + size)` to guest memory, waiting for pending host
/// results. Runs where the rasterizer's caches are owned.
///
/// Lichii `QueryDma::FlushPending` (query_dma.h:63-76). The memory manager lock is released
/// before the rasterizer waits: the report writes that wait releases may lock it.
pub fn flush_pending(memory: &Mutex<MemoryManager>, source: u64, size: u64) {
    let (rasterizer, pages) = {
        let memory = memory.lock();
        let Some(rasterizer) = memory.rasterizer_handle() else {
            return;
        };
        (rasterizer, device_pages(source, size, |address| memory.gpu_to_cpu_address(address)))
    };
    for (device, length) in pages {
        // SAFETY: the rasterizer bound to the memory manager outlives the engines that read
        // through it, as for the memory manager's own rasterizer calls.
        unsafe { rasterizer.with_mut(|rasterizer| rasterizer.flush_query_results(device, length)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_pages_split_the_read_at_page_boundaries_and_skip_unmapped_pages() {
        let pages = device_pages(0x1ff0, 0x2020, |address| {
            (address < 0x3000).then_some(address + 0x10_0000)
        });
        assert_eq!(pages, vec![(0x10_1ff0, 0x10), (0x10_2000, 0x1000)]);
        assert!(device_pages(0x1000, 0, Some).is_empty());
    }
}
