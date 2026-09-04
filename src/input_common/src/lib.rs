// SPDX-FileCopyrightText: Copyright 2017 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `input_common/` from the C++ codebase.
//!
//! This crate provides the input subsystem, input engine abstraction,
//! input mapping, input polling, and various input drivers and helpers.

pub mod drivers;
pub mod helpers;
pub mod input_engine;
pub mod input_mapping;
pub mod input_poller;
pub mod main_common;

// Re-export key types for convenience
pub use input_engine::{
    BasicMotion, EngineInputType, InputEngine, InputIdentifier, MappingCallback, MappingData,
    PadIdentifier, UpdateCallback, VibrationRequest,
};
pub use main_common::{polling, InputSubsystem};
