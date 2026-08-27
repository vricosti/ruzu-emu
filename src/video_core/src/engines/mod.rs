// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU engine module declarations.
//!
//! The Tegra X1 GPU has several engines, each responsible for a class of
//! operations. Engine identifiers and pushbuffer dispatch remain owned by
//! `puller`, matching Eden.

pub mod const_buffer_info;
pub mod draw_manager;
pub mod engine_interface;
pub mod engine_upload;
pub mod fermi_2d;
pub mod kepler_compute;
pub mod kepler_memory;
pub mod maxwell_3d;
pub mod maxwell_dma;
pub mod nv01_timer;
pub mod puller;
pub mod sw_blitter;
