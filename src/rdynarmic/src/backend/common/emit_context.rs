/// Memory emission options consumed by host backends.
///
/// Mirrors the corresponding fields of upstream `Dynarmic::A64::UserConfig`
/// that the memory-emission helpers read from the backend emit context. The
/// x64 backend currently consumes every field; the arm64 backend will share the
/// same frontend-facing configuration instead of adding a second user config.
#[derive(Clone, Debug)]
pub struct MemoryEmitConfig {
    /// Number of bits in the guest VA space reachable via the fastmem
    /// region. `64` means no masking. `< 64` means either silently mirror or
    /// abort out-of-range accesses to the fallback path.
    pub fastmem_address_space_bits: usize,
    /// If true, vaddrs outside the fastmem AS are masked into range.
    /// If false, out-of-range vaddrs jump to the abort handler.
    pub silently_mirror_fastmem: bool,
    /// Whether exclusive (LDXR/STXR) memory accesses are emitted inline
    /// using fastmem. When false, exclusive accesses always use callbacks.
    pub fastmem_exclusive_access: bool,
    /// Whether a faulting exclusive fastmem access recompiles its block with
    /// that instruction marked do-not-fastmem.
    pub recompile_on_exclusive_fastmem_failure: bool,
    /// Whether a fastmem inst that faulted should cause its block to be
    /// recompiled with the inst marked do-not-fastmem.
    pub recompile_on_fastmem_failure: bool,
    /// Whether the JIT has a page table to emit lookups into.
    pub page_table_present: bool,
    /// Number of bits in the guest VA space the page table covers.
    pub page_table_address_space_bits: usize,
    /// If true, vaddrs outside the page-table AS are silently mirrored.
    /// If false, out-of-range vaddrs abort to the fallback path.
    pub silently_mirror_page_table: bool,
    /// If true, page-table entries are stored as `host_ptr - vaddr` so the
    /// lookup result is `page + vaddr` directly.
    pub absolute_offset_page_table: bool,
    /// Number of low bits in page-table entries that are attribute flags.
    pub page_table_pointer_mask_bits: u32,
    /// Bitmask of access widths to detect misalignment for via the page-table
    /// path. `16 | 32 | 64 | 128` matches upstream zuyu.
    pub detect_misaligned_access_via_page_table: u32,
    /// If true, only detect misalignment when the access actually crosses a
    /// page boundary.
    pub only_detect_misalignment_via_page_table_on_page_boundary: bool,
    /// If true, every memory access checks `JitState.halt_reason` for
    /// `MemoryAbort` and returns from the run-loop if set.
    pub check_halt_on_memory_access: bool,
    /// Logical CPU id for global-monitor coordination on exclusives.
    pub processor_id: usize,
}

impl Default for MemoryEmitConfig {
    fn default() -> Self {
        Self {
            fastmem_address_space_bits: 64,
            silently_mirror_fastmem: true,
            fastmem_exclusive_access: false,
            recompile_on_exclusive_fastmem_failure: true,
            recompile_on_fastmem_failure: true,
            page_table_present: false,
            page_table_address_space_bits: 64,
            silently_mirror_page_table: true,
            absolute_offset_page_table: false,
            page_table_pointer_mask_bits: 0,
            detect_misaligned_access_via_page_table: 0,
            only_detect_misalignment_via_page_table_on_page_boundary: false,
            check_halt_on_memory_access: false,
            processor_id: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MemoryEmitConfig;

    #[test]
    fn fastmem_failure_recompilation_is_enabled_by_default() {
        let config = MemoryEmitConfig::default();
        assert!(config.recompile_on_fastmem_failure);
        assert!(config.recompile_on_exclusive_fastmem_failure);
    }
}
