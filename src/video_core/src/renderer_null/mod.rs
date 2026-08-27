// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's video_core/renderer_null/renderer_null.h and renderer_null.cpp
//! Status: COMPLET
//!
//! Null rendering backend — all draw/render calls are silently ignored.
//! Used for headless mode and testing without GPU output.

pub mod null_rasterizer;
pub mod renderer_null;
