// SPDX-FileCopyrightText: Copyright 2021 yuzu Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of zuyu/src/core/hle/service/nvnflinger/window.h

/// Attributes queryable with Query
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeWindow {
    Width = 0,
    Height = 1,
    Format = 2,
    MinUndequeedBuffers = 3,
    QueuesToWindowComposer = 4,
    ConcreteType = 5,
    DefaultWidth = 6,
    DefaultHeight = 7,
    TransformHint = 8,
    ConsumerRunningBehind = 9,
    ConsumerUsageBits = 10,
    StickyTransform = 11,
    DefaultDataSpace = 12,
    BufferAge = 13,
}

/// Parameter for Connect/Disconnect
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeWindowApi {
    NoConnectedApi = 0,
    Egl = 1,
    Cpu = 2,
    Media = 3,
    Camera = 4,
}

impl Default for NativeWindowApi {
    fn default() -> Self {
        NativeWindowApi::NoConnectedApi
    }
}

/// Scaling mode parameter for QueueBuffer
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeWindowScalingMode {
    Freeze = 0,
    ScaleToWindow = 1,
    ScaleCrop = 2,
    NoScaleCrop = 3,
    PreserveAspectRatio = 4,
}

impl Default for NativeWindowScalingMode {
    fn default() -> Self {
        NativeWindowScalingMode::Freeze
    }
}

bitflags::bitflags! {
    /// Transform parameter for QueueBuffer.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct NativeWindowTransform: u32 {
        const NONE = 0x0;
        const INVERSE_DISPLAY = 0x08;
    }
}
