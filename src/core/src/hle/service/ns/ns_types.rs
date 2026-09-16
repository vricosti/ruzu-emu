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

/// ApplicationDownloadState from ns_types.h.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ApplicationDownloadState {
    pub downloaded_size: u64,
    pub total_size: u64,
    pub unk_x10: u32,
    pub state: u8,
    pub unk_x15: u8,
    pub unk_x16: [u8; 2],
    pub unk_x18: u64,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ApplicationViewV19 {
    pub application_id: u64,
    pub version: u32,
    pub flags: u32,
    pub download_state: ApplicationDownloadState,
    pub download_progress: ApplicationDownloadState,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct ApplicationViewV20 {
    pub application_id: u64,
    pub version: u32,
    pub flags: u32,
    pub unk: u32,
    pub _padding: [u8; 4],
    pub download_state: ApplicationDownloadState,
    pub download_progress: ApplicationDownloadState,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ApplicationViewData {
    pub application_id: u64,
    pub version: u32,
    pub flags: u32,
    pub unk: u32,
    pub download_state: ApplicationDownloadState,
    pub download_progress: ApplicationDownloadState,
}

const _: () = assert!(core::mem::size_of::<ApplicationDownloadState>() == 0x20);
const _: () = assert!(core::mem::size_of::<ApplicationViewV19>() == 0x50);
const _: () = assert!(core::mem::size_of::<ApplicationViewV20>() == 0x58);

/// WriteApplicationView: explicit little-endian serialization also initializes
/// the C++ V20 alignment gap, which must not expose host padding bytes.
pub fn write_application_view(dst: &mut [u8], data: &ApplicationViewData, is_fw20: bool) -> usize {
    let size = if is_fw20 { 0x58 } else { 0x50 };
    if dst.len() < size { return 0; }
    dst[..size].fill(0);
    dst[..8].copy_from_slice(&data.application_id.to_le_bytes());
    dst[8..12].copy_from_slice(&data.version.to_le_bytes());
    dst[12..16].copy_from_slice(&data.flags.to_le_bytes());
    if is_fw20 { dst[16..20].copy_from_slice(&data.unk.to_le_bytes()); }
    let start = if is_fw20 { 24 } else { 16 };
    for (i, state) in [data.download_state, data.download_progress].iter().enumerate() {
        let out = &mut dst[start + i * 32..start + (i + 1) * 32];
        out[..8].copy_from_slice(&state.downloaded_size.to_le_bytes());
        out[8..16].copy_from_slice(&state.total_size.to_le_bytes());
        out[16..20].copy_from_slice(&state.unk_x10.to_le_bytes());
        out[20] = state.state;
        out[21] = state.unk_x15;
        out[22..24].copy_from_slice(&state.unk_x16);
        out[24..32].copy_from_slice(&state.unk_x18.to_le_bytes());
    }
    size
}

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

/// ApplicationViewWithPromotionData from ns_types.h.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplicationViewWithPromotionData {
    pub view: ApplicationViewData,
    pub promotion: PromotionInfo,
}

pub fn write_application_view_with_promotion(dst: &mut [u8], data: &ApplicationViewWithPromotionData, is_fw20: bool) -> usize {
    let written = write_application_view(dst, &data.view, is_fw20);
    if written == 0 || dst.len() - written < 0x20 { return 0; }
    let out = &mut dst[written..written + 0x20];
    let p = &data.promotion;
    out[..8].copy_from_slice(&p.start_timestamp.to_le_bytes());
    out[8..16].copy_from_slice(&p.end_timestamp.to_le_bytes());
    out[16..24].copy_from_slice(&p.remaining_time.to_le_bytes());
    out[24..28].copy_from_slice(&p._padding0);
    out[28] = p.flags;
    out[29..32].copy_from_slice(&p._padding1);
    written + 0x20
}

#[cfg(test)]
mod application_view_tests {
    use super::*;

    #[test]
    fn view_versions_preserve_offsets_and_zero_padding() {
        assert_eq!(std::mem::offset_of!(ApplicationViewV19, download_state), 0x10);
        assert_eq!(std::mem::offset_of!(ApplicationViewV20, download_state), 0x18);
        assert_eq!(std::mem::offset_of!(ApplicationViewV20, download_progress), 0x38);
        let data = ApplicationViewWithPromotionData {
            view: ApplicationViewData {
                application_id: 0x1122_3344_5566_7788, version: 0x70000,
                flags: 0x401f17, unk: 0xaabbccdd,
                download_state: ApplicationDownloadState { downloaded_size: 123, state: 7, ..Default::default() },
                download_progress: ApplicationDownloadState { total_size: 456, ..Default::default() },
            },
            promotion: PromotionInfo { remaining_time: -123, flags: 3, ..Default::default() },
        };
        for modern in [false, true] {
            let size = if modern { 0x58 } else { 0x50 };
            let start = if modern { 24 } else { 16 };
            let mut out = [0xcc; 0x80];
            assert_eq!(write_application_view_with_promotion(&mut out, &data, modern), size + 32);
            assert_eq!(&out[..8], &data.view.application_id.to_le_bytes());
            assert_eq!(&out[8..12], &0x70000u32.to_le_bytes());
            assert_eq!(&out[12..16], &0x401f17u32.to_le_bytes());
            assert_eq!(&out[start..start + 8], &123u64.to_le_bytes());
            assert_eq!(out[start + 20], 7);
            assert_eq!(&out[start + 40..start + 48], &456u64.to_le_bytes());
            assert_eq!(&out[size + 16..size + 24], &(-123i64).to_le_bytes());
            assert_eq!(out[size + 28], 3);
            assert_eq!(&out[size + 29..size + 32], &[0; 3]);
            assert!(out[size + 32..].iter().all(|b| *b == 0xcc));
            if modern { assert_eq!(&out[20..24], &[0; 4]); }
            let mut short = vec![0xcc; size - 1];
            assert_eq!(write_application_view(&mut short, &data.view, modern), 0);
            assert!(short.iter().all(|b| *b == 0xcc));
            let mut partial = vec![0xcc; size + 31];
            assert_eq!(write_application_view_with_promotion(&mut partial, &data, modern), 0);
            assert_eq!(&partial[..size], &out[..size]);
            assert!(partial[size..].iter().all(|b| *b == 0xcc));
        }
    }
}

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
