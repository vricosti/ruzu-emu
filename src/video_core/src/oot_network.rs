// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Partial port of Lichii `src/video_core/oot_network.h` (`VideoCommon::OotNetwork`).
//!
//! The Legend of Zelda: Ocarina of Time (Switch 2) runs tensor networks on its images. Only the
//! title's program id is ported here: `KeplerCompute::dispatch_native_compute` gates the movie
//! network replacement on it (Lichii aae0b063f7, #721). The rest of the upstream header (the
//! gameplay anti-aliasing bypass: native program keys, tonemap shader hashes, `BypassState`,
//! `BypassEnabled` and its `oot_anti_aliasing` setting) belongs to a separate feature that ruzu
//! does not have; see DIFF.md.

/// Port of `OotNetwork::ProgramId`.
pub const PROGRAM_ID: u64 = 0x0400486024DEC000;
