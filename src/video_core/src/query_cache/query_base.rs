// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's `src/video_core/query_cache/query_base.h`.
//!
//! Defines the base query types: `QueryBase`, `GuestQuery`, and `HostQueryBase`,
//! along with the `QueryFlagBits` bitflags used to track query state.

use bitflags::bitflags;

/// Type alias matching C++ `DAddr` (device address).
pub type DAddr = u64;

/// Type alias matching C++ `VAddr` (virtual address).
pub type VAddr = u64;

bitflags! {
    /// Flags tracking the state of a query.
    ///
    /// Maps to C++ `QueryFlagBits`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct QueryFlagBits: u32 {
        /// Indicates if this query has a timestamp.
        const HAS_TIMESTAMP        = 1 << 0;
        /// Indicates if the query has been synced in the host.
        const IS_FINAL_VALUE_SYNCED = 1 << 1;
        /// Indicates if the query has been synced in the host.
        const IS_HOST_SYNCED       = 1 << 2;
        /// Indicates if the query has been synced with the guest.
        const IS_GUEST_SYNCED      = 1 << 3;
        /// Indicates if this query points to a host query.
        const IS_HOST_MANAGED      = 1 << 4;
        /// Indicates if this query was rewritten by another query.
        const IS_REWRITTEN         = 1 << 5;
        /// Indicates the value of the query has been nullified.
        const IS_INVALIDATED       = 1 << 6;
        /// Indicates the query has not been set by a guest query.
        const IS_ORPHAN            = 1 << 7;
        /// Indicates the query is a fence.
        const IS_FENCE             = 1 << 8;
    }
}

/// Base query data shared by guest and host queries.
///
/// Maps to C++ `QueryBase`.
#[derive(Debug, Clone)]
pub struct QueryBase {
    pub guest_address: DAddr,
    pub flags: QueryFlagBits,
    pub value: u64,
}

impl QueryBase {
    /// Default constructor (zero-initialized).
    pub fn new() -> Self {
        Self {
            guest_address: 0,
            flags: QueryFlagBits::empty(),
            value: 0,
        }
    }

    /// Parameterized constructor.
    pub fn with_params(address: DAddr, flags: QueryFlagBits, value: u64) -> Self {
        Self {
            guest_address: address,
            flags,
            value,
        }
    }
}

impl Default for QueryBase {
    fn default() -> Self {
        Self::new()
    }
}

/// A query written by the guest.
///
/// Maps to C++ `GuestQuery`.
#[derive(Debug, Clone)]
pub struct GuestQuery {
    pub base: QueryBase,
}

impl GuestQuery {
    /// Create a new guest query.
    ///
    /// Maps to C++ `GuestQuery::GuestQuery(bool isLong, VAddr address, u64 queryValue)`.
    pub fn new(is_long: bool, address: VAddr, query_value: u64) -> Self {
        let mut flags = QueryFlagBits::IS_FINAL_VALUE_SYNCED;
        if is_long {
            flags |= QueryFlagBits::HAS_TIMESTAMP;
        }
        Self {
            base: QueryBase::with_params(address, flags, query_value),
        }
    }
}

/// A query managed by the host backend.
///
/// Maps to C++ `HostQueryBase`.
#[derive(Debug, Clone)]
pub struct HostQueryBase {
    pub base: QueryBase,
    pub start_bank_id: u32,
    pub size_banks: u32,
    pub start_slot: usize,
    pub size_slots: usize,
}

impl HostQueryBase {
    /// Default constructor — creates an orphan host-managed query.
    pub fn new() -> Self {
        Self {
            base: QueryBase::with_params(
                0,
                QueryFlagBits::IS_HOST_MANAGED | QueryFlagBits::IS_ORPHAN,
                0,
            ),
            start_bank_id: 0,
            size_banks: 0,
            start_slot: 0,
            size_slots: 0,
        }
    }

    /// Parameterized constructor.
    pub fn with_params(has_timestamp: bool, address: VAddr) -> Self {
        let mut flags = QueryFlagBits::IS_HOST_MANAGED;
        if has_timestamp {
            flags |= QueryFlagBits::HAS_TIMESTAMP;
        }
        Self {
            base: QueryBase::with_params(address, flags, 0),
            start_bank_id: 0,
            size_banks: 0,
            start_slot: 0,
            size_slots: 0,
        }
    }
}

impl Default for HostQueryBase {
    fn default() -> Self {
        Self::new()
    }
}
