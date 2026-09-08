// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ns/ns_types.h

/// nn::ns::detail::ApplicationEvent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ApplicationEvent {
    Installing = 2,
    Installed = 3,
    GameCardNotInserted = 5,
    Archived = 11,
    GameCard = 16,
}

/// nn::ns::detail::ApplicationControlSource
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ApplicationControlSource {
    CacheOnly = 0,
    Storage = 1,
    StorageOnly = 2,
}

/// nn::ns::detail::BackgroundNetworkUpdateState
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BackgroundNetworkUpdateState {
    None = 0,
    InProgress = 1,
    Ready = 2,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ApplicationRecord {
    pub application_id: u64,
    pub last_event: ApplicationEvent,
    pub attributes: u8,
    pub _padding0: [u8; 0x6],
    pub last_updated: i64,
}
const _: () = assert!(core::mem::size_of::<ApplicationRecord>() == 0x18);

/// ApplicationView
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ApplicationView {
    /// ApplicationId.
    pub application_id: u64,
    /// Unknown.
    pub unk: u32,
    /// Flags.
    pub flags: u32,
    /// Unknown.
    pub unk_x10: [u8; 0x10],
    /// Unknown.
    pub unk_x20: u32,
    /// Unknown.
    pub unk_x24: u16,
    /// Unknown.
    pub unk_x26: [u8; 0x2],
    /// Unknown.
    pub unk_x28: [u8; 0x8],
    /// Unknown.
    pub unk_x30: [u8; 0x10],
    /// Unknown.
    pub unk_x40: u32,
    /// Unknown.
    pub unk_x44: u8,
    /// Unknown.
    pub unk_x45: [u8; 0xb],
}
const _: () = assert!(core::mem::size_of::<ApplicationView>() == 0x50);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ApplicationRightsOnClient {
    pub application_id: u64,
    pub uid: [u8; 0x10], // Common::UUID
    pub flags: u8,
    pub flags2: u8,
    pub _padding: [u8; 0x6],
}
const _: () = assert!(core::mem::size_of::<ApplicationRightsOnClient>() == 0x20);

/// NsPromotionInfo
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct PromotionInfo {
    /// POSIX timestamp for the promotion start.
    pub start_timestamp: u64,
    /// POSIX timestamp for the promotion end.
    pub end_timestamp: u64,
    /// Remaining time until the promotion ends, in nanoseconds.
    pub remaining_time: i64,
    pub _padding0: [u8; 0x4],
    /// Flags.
    pub flags: u8,
    pub _padding1: [u8; 0x3],
}
const _: () = assert!(core::mem::size_of::<PromotionInfo>() == 0x20);

/// NsApplicationViewWithPromotionInfo
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ApplicationViewWithPromotionInfo {
    pub view: ApplicationView,
    pub promotion: PromotionInfo,
}
const _: () = assert!(core::mem::size_of::<ApplicationViewWithPromotionInfo>() == 0x70);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ApplicationOccupiedSizeEntity {
    pub storage_id: u8,
    pub _padding: [u8; 7],
    pub app_size: u64,
    pub patch_size: u64,
    pub aoc_size: u64,
}
const _: () = assert!(core::mem::size_of::<ApplicationOccupiedSizeEntity>() == 0x20);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ApplicationOccupiedSize {
    pub entities: [ApplicationOccupiedSizeEntity; 4],
}
const _: () = assert!(core::mem::size_of::<ApplicationOccupiedSize>() == 0x80);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ContentPath {
    pub file_system_proxy_type: u8,
    pub _padding: [u8; 7],
    pub program_id: u64,
}
const _: () = assert!(core::mem::size_of::<ContentPath>() == 0x10);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C, align(8))]
pub struct Uid {
    pub uuid: [u8; 0x10], // Common::UUID
}
const _: () = assert!(core::mem::size_of::<Uid>() == 0x10);

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct PlayStatistics {
    pub application_id: u64,
    pub first_entry_index: u32,
    pub first_timestamp_user: u32,
    pub first_timestamp_network: u32,
    pub last_entry_index: u32,
    pub last_timestamp_user: u32,
    pub last_timestamp_network: u32,
    pub play_time_in_minutes: u32,
    pub total_launches: u32,
}
const _: () = assert!(core::mem::size_of::<PlayStatistics>() == 0x28);
