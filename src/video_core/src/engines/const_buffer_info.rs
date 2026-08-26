// SPDX-FileCopyrightText: Copyright 2019 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/engines/const_buffer_info.h`.

pub type GPUVAddr = u64;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConstBufferInfo {
    pub address: GPUVAddr,
    pub size: u32,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_buffer_info_matches_upstream_layout() {
        assert_eq!(std::mem::size_of::<ConstBufferInfo>(), 16);
        assert_eq!(std::mem::align_of::<ConstBufferInfo>(), 8);
        assert_eq!(std::mem::offset_of!(ConstBufferInfo, address), 0);
        assert_eq!(std::mem::offset_of!(ConstBufferInfo, size), 8);
        assert_eq!(std::mem::offset_of!(ConstBufferInfo, enabled), 12);
    }

    #[test]
    fn default_value_matches_value_initialized_upstream_state() {
        assert_eq!(ConstBufferInfo::default().address, 0);
        assert_eq!(ConstBufferInfo::default().size, 0);
        assert!(!ConstBufferInfo::default().enabled);
    }
}
