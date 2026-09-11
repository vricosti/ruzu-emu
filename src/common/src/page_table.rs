//! Port of eden/src/common/page_table.h and eden/src/common/page_table.cpp
//! (Eden 5f142c7926: 8-byte packed page entries backed by a
//! `SparseLargeVector`).
//! Status: COMPLET
//! Derniere synchro: 2026-09-11
//!
//! Upstream nests `PageEntryData` (and its `Data` bitfield) inside
//! `PageTable`; Rust has no nested types so they live at module level.
//! `TraversalEntry`/`TraversalContext` stay here as upstream, while the
//! traversal itself is owned by `KPageTableBase::begin_traversal` /
//! `continue_traversal` (moved there upstream by the same commit).

use crate::sparse_large_vector::SparseLargeVector;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageType {
    /// Page is unmapped and should cause an access error.
    Unmapped = 0b00,
    /// Page is mapped to regular memory. This is the only type you can get pointers to.
    Memory = 0b01,
    /// Page is mapped to regular memory, but inaccessible from CPU fastmem and must use
    /// the callbacks.
    DebugMemory = 0b10,
    /// Page is mapped to regular memory, but also needs to check for rasterizer cache flushing and
    /// invalidation
    RasterizerCachedMemory = 0b11,
}

impl PageType {
    /// `static_cast<PageType>(bits)` for the two-bit `type` field.
    pub const fn from_bits(bits: u64) -> Self {
        match bits & 0b11 {
            0b00 => PageType::Unmapped,
            0b01 => PageType::Memory,
            0b10 => PageType::DebugMemory,
            _ => PageType::RasterizerCachedMemory,
        }
    }
}

/// Upstream `PageTable::TraversalEntry`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TraversalEntry {
    pub phys_addr: u64,
    pub block_size: usize,
}

/// Upstream `PageTable::TraversalContext`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TraversalContext {
    pub next_page: u64,
    pub next_offset: u64,
}

/// Upstream `PageEntryData::Data`: the packed 64-bit layout
/// `marked:1 | type:2 | block:9 | page:45 | block2:7` (LSB first).
///
/// `page` holds bits 12..56 of the host pointer in place, so the JIT can mask
/// the attributes away with [`PageTable::ATTRIBUTE_MASK`] and add the guest
/// address directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Data(u64);

impl Data {
    const MARKED_SHIFT: u32 = 0;
    const TYPE_SHIFT: u32 = 1;
    const BLOCK_SHIFT: u32 = 3;
    const PAGE_SHIFT: u32 = 12;
    const BLOCK2_SHIFT: u32 = 57;

    /// Upstream `Data(bool marked_, PageType type_, u16 block_, u64 page_)`.
    /// `page_` is the (unshifted) host pointer.
    pub const fn new(marked: bool, page_type: PageType, block: u16, page: u64) -> Self {
        let marked = (marked as u64) & 0b1;
        let type_ = (page_type as u64) & ((1u64 << 2) - 1);
        let block_lo = (block as u64) & ((1u64 << 9) - 1);
        let page = (page >> 12) & ((1u64 << 45) - 1);
        let block2 = ((block as u64) >> 9) & ((1u64 << 7) - 1);
        Self(
            (marked << Self::MARKED_SHIFT)
                | (type_ << Self::TYPE_SHIFT)
                | (block_lo << Self::BLOCK_SHIFT)
                | (page << Self::PAGE_SHIFT)
                | (block2 << Self::BLOCK2_SHIFT),
        )
    }

    /// `std::bit_cast<Data>(u64)`.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// `std::bit_cast<u64>(Data)`.
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    /// `u64 marked : 1`
    pub const fn marked(self) -> bool {
        (self.0 >> Self::MARKED_SHIFT) & 0b1 != 0
    }

    /// `u64 type : 2`
    pub const fn type_bits(self) -> u64 {
        (self.0 >> Self::TYPE_SHIFT) & 0b11
    }

    /// `u64 block : 9`
    pub const fn block(self) -> u64 {
        (self.0 >> Self::BLOCK_SHIFT) & ((1u64 << 9) - 1)
    }

    /// `u64 page : 45` — 44 bits of actual data (64 - page offset (12) - reserved (8)) + a sign bit
    pub const fn page(self) -> u64 {
        (self.0 >> Self::PAGE_SHIFT) & ((1u64 << 45) - 1)
    }

    /// `u64 block2 : 7`
    pub const fn block2(self) -> u64 {
        (self.0 >> Self::BLOCK2_SHIFT) & ((1u64 << 7) - 1)
    }
}

/// Atomic tuple of host pointer, page type, and block id.
/// This uses the lower bits of a given pointer to store the attributes.
/// Writing and reading the pointer attribute pair is guaranteed to be atomic for the same method
/// call. In other words, they are guaranteed to be synchronized at all times.
///
/// Upstream `PageTable::PageEntryData`.
#[repr(transparent)]
pub struct PageEntryData {
    data_raw: AtomicU64,
}

const _: () = assert!(std::mem::size_of::<Data>() == std::mem::size_of::<AtomicU64>());
const _: () = assert!(std::mem::size_of::<PageEntryData>() == 8);

impl PageEntryData {
    pub const fn new() -> Self {
        Self {
            data_raw: AtomicU64::new(0),
        }
    }

    /// Upstream `Raw()`.
    pub fn raw(&self) -> Data {
        Data::from_bits(self.data_raw.load(Ordering::Relaxed))
    }

    /// Returns the page pointer.
    ///
    /// Upstream `Pointer(bool ignored_marked = false)`.
    pub fn pointer(&self, ignored_marked: bool) -> usize {
        Self::extract_pointer(
            Data::from_bits(self.data_raw.load(Ordering::Relaxed)),
            ignored_marked,
        )
    }

    /// Returns the page type attribute.
    ///
    /// Upstream `Type()`.
    pub fn page_type(&self) -> PageType {
        PageType::from_bits(Data::from_bits(self.data_raw.load(Ordering::Relaxed)).type_bits())
    }

    /// Returns the block identifier.
    ///
    /// Upstream `Block()`.
    pub fn block(&self) -> u16 {
        Self::extract_block(Data::from_bits(self.data_raw.load(Ordering::Relaxed)))
    }

    /// Returns the page pointer and attribute pair, extracted from the same atomic read.
    ///
    /// Upstream `PointerTypeBlock(bool ignore_marked = false)`.
    pub fn pointer_type_block(&self, ignore_marked: bool) -> (usize, PageType, u16) {
        let non_atomic_raw = Data::from_bits(self.data_raw.load(Ordering::Relaxed));
        (
            Self::extract_pointer(non_atomic_raw, ignore_marked),
            PageType::from_bits(non_atomic_raw.type_bits()),
            Self::extract_block(non_atomic_raw),
        )
    }

    /// Write page info atomically.
    ///
    /// Upstream `Store(bool marked, PageType type, u16 block, uintptr_t pointer)`.
    pub fn store(&self, marked: bool, page_type: PageType, block: u16, pointer: usize) {
        self.data_raw.store(
            Data::new(marked, page_type, block, pointer as u64).to_bits(),
            Ordering::SeqCst,
        );
    }

    /// Upstream `MarkRasterizerCached()`: sets `marked` and
    /// `type = RasterizerCachedMemory` while keeping pointer and block.
    pub fn mark_rasterizer_cached(&self) {
        self.data_raw.fetch_or(0b111, Ordering::SeqCst);
    }

    /// Upstream `MarkDebug(u64 ptr, u16 block)`.
    pub fn mark_debug(&self, ptr: usize, block: u16) {
        self.store(true, PageType::DebugMemory, block, ptr);
    }

    /// Unpack a pointer from a page info raw representation.
    ///
    /// Upstream `ExtractPointer(Data raw, bool ignore_marked = false)`.
    pub const fn extract_pointer(raw: Data, ignore_marked: bool) -> usize {
        if raw.marked() && !ignore_marked {
            0
        } else {
            // shift raw.page's fake sign bit to the actual sign bit, then sign extend
            (((raw.page() << (64 - 44)) as i64) >> (64 - 44 - 12)) as usize
        }
    }

    /// Upstream `ExtractBlock(Data raw)`.
    pub const fn extract_block(raw: Data) -> u16 {
        (raw.block() | (raw.block2() << 9)) as u16
    }
}

impl Default for PageEntryData {
    fn default() -> Self {
        Self::new()
    }
}

/// A (reasonably) fast way of allowing switchable and remappable process address spaces. It loosely
/// mimics the way a real CPU page table works.
pub struct PageTable {
    /// Vector of memory pointers backing each page. An entry can only be non-null if the
    /// corresponding attribute element is of type `Memory`.
    pub entries: SparseLargeVector<PageEntryData>,

    pub fastmem_arena: *mut u8,
    pub current_address_space_width_in_bits: usize,
    pub current_page_bits: usize,
}

impl PageTable {
    /// Masks out bits reserved for attribute tagging.
    pub const ATTRIBUTE_MASK: u64 = ((1u64 << 44) - 1) << 12;

    /// Specifies sign bit for page table entries.
    pub const SIGN_BIT: u64 = 45 + 12; // 44 bits of data + page offset

    pub fn new() -> Self {
        Self {
            entries: SparseLargeVector::new(),
            fastmem_arena: std::ptr::null_mut(),
            current_address_space_width_in_bits: 0,
            current_page_bits: 0,
        }
    }

    /// Resizes the page table to be able to accommodate enough pages within
    /// a given address space.
    ///
    /// * `address_space_width_in_bits` - The address size width in bits.
    /// * `page_bits` - The page size in bits.
    pub fn resize(&mut self, address_space_width_in_bits: usize, page_bits: usize) {
        let num_page_table_entries = 1usize << (address_space_width_in_bits - page_bits);
        self.entries.resize_and_clear(num_page_table_entries);
        self.current_address_space_width_in_bits = address_space_width_in_bits;
        self.current_page_bits = page_bits;
    }

    pub fn get_address_space_bits(&self) -> usize {
        self.current_address_space_width_in_bits
    }
}

impl Default for PageTable {
    fn default() -> Self {
        Self::new()
    }
}

// Safety: the entries are atomics inside a SparseLargeVector; `fastmem_arena`
// is a raw pointer owned by HostMemory.
unsafe impl Send for PageTable {}
unsafe impl Sync for PageTable {}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST_PTR: usize = 0x7f12_3456_7000;

    #[test]
    fn constants_match_upstream() {
        assert_eq!(PageTable::ATTRIBUTE_MASK, 0x00FF_FFFF_FFFF_F000);
        assert_eq!(PageTable::SIGN_BIT, 57);
        assert_eq!(std::mem::size_of::<PageEntryData>(), 8);
    }

    #[test]
    fn data_packs_fields_in_upstream_bit_positions() {
        let data = Data::new(true, PageType::DebugMemory, 0x1ABC, HOST_PTR as u64);
        let expected = 0b1
            | (0b10u64 << 1)
            | ((0x1ABCu64 & 0x1FF) << 3)
            | ((((HOST_PTR as u64) >> 12) & ((1 << 45) - 1)) << 12)
            | (((0x1ABCu64 >> 9) & 0x7F) << 57);
        assert_eq!(data.to_bits(), expected);
        assert!(data.marked());
        assert_eq!(data.type_bits(), 0b10);
        assert_eq!(data.block(), 0x1ABC & 0x1FF);
        assert_eq!(data.block2(), 0x1ABC >> 9);
        assert_eq!(data.page(), (HOST_PTR as u64) >> 12);
    }

    #[test]
    fn store_and_extract_round_trip() {
        let entry = PageEntryData::new();
        entry.store(false, PageType::Memory, 0x1ABC, HOST_PTR);
        assert_eq!(entry.pointer(false), HOST_PTR);
        assert_eq!(entry.page_type(), PageType::Memory);
        assert_eq!(entry.block(), 0x1ABC);
        assert_eq!(
            entry.pointer_type_block(false),
            (HOST_PTR, PageType::Memory, 0x1ABC)
        );
        // The JIT contract: masking the raw entry yields the pointer for
        // canonical (< 2^47) host pointers.
        assert_eq!(
            entry.raw().to_bits() & PageTable::ATTRIBUTE_MASK,
            HOST_PTR as u64
        );
    }

    #[test]
    fn pointer_low_bits_are_dropped_by_the_page_field() {
        let entry = PageEntryData::new();
        entry.store(false, PageType::Memory, 1, HOST_PTR | 0xABC);
        assert_eq!(entry.pointer(false), HOST_PTR);
    }

    #[test]
    fn extract_pointer_sign_extends_from_bit_55() {
        let raw = Data::new(false, PageType::Memory, 0, 0x0080_0000_0000_0000);
        assert_eq!(
            PageEntryData::extract_pointer(raw, false) as u64,
            0xFF80_0000_0000_0000
        );
        // Bit 56 of the pointer lands in the unused 45th page bit and is
        // dropped, exactly like upstream's `(page << 20) >> 8`.
        let raw = Data::new(false, PageType::Memory, 0, 0x0100_0000_0000_0000);
        assert_eq!(PageEntryData::extract_pointer(raw, false), 0);
    }

    #[test]
    fn mark_rasterizer_cached_hides_pointer_but_keeps_it_for_ignore_marked() {
        let entry = PageEntryData::new();
        entry.store(false, PageType::Memory, 0x77, HOST_PTR);
        entry.mark_rasterizer_cached();
        assert!(entry.raw().marked());
        assert_eq!(entry.page_type(), PageType::RasterizerCachedMemory);
        assert_eq!(entry.pointer(false), 0);
        assert_eq!(entry.pointer(true), HOST_PTR);
        assert_eq!(entry.block(), 0x77);
        assert_eq!(
            entry.pointer_type_block(true),
            (HOST_PTR, PageType::RasterizerCachedMemory, 0x77)
        );
        // Upstream `fetch_or(0b111)` on a marked entry is idempotent.
        let before = entry.raw();
        entry.mark_rasterizer_cached();
        assert_eq!(entry.raw(), before);
    }

    #[test]
    fn mark_debug_sets_marked_and_debug_type() {
        let entry = PageEntryData::new();
        entry.store(false, PageType::Memory, 5, HOST_PTR);
        entry.mark_debug(HOST_PTR, 5);
        assert_eq!(entry.page_type(), PageType::DebugMemory);
        assert!(entry.raw().marked());
        assert_eq!(entry.pointer(false), 0);
        assert_eq!(entry.pointer(true), HOST_PTR);
        assert_eq!(entry.block(), 5);
    }

    #[test]
    fn unmapped_entry_is_all_zero() {
        let entry = PageEntryData::new();
        entry.store(false, PageType::Unmapped, 0, 0);
        assert_eq!(entry.raw().to_bits(), 0);
        assert_eq!(entry.pointer_type_block(false), (0, PageType::Unmapped, 0));
    }

    #[test]
    fn page_type_from_bits_matches_explicit_values() {
        assert_eq!(PageType::from_bits(0b00), PageType::Unmapped);
        assert_eq!(PageType::from_bits(0b01), PageType::Memory);
        assert_eq!(PageType::from_bits(0b10), PageType::DebugMemory);
        assert_eq!(PageType::from_bits(0b11), PageType::RasterizerCachedMemory);
    }

    #[test]
    fn resize_allocates_entries_for_the_address_space() {
        let mut pt = PageTable::new();
        pt.resize(36, 12); // 36-bit address space, 4KB pages
        assert_eq!(pt.get_address_space_bits(), 36);
        assert_eq!(pt.current_page_bits, 12);
        assert_eq!(pt.entries.size(), 1 << 24);
        // Fresh entries read as unmapped without committing anything.
        assert_eq!(pt.entries[123].pointer_type_block(false), (0, PageType::Unmapped, 0));
    }
}
