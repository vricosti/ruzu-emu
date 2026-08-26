// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/dirty_flags.h and video_core/dirty_flags.cpp
//!
//! Dirty flag indices and setup for Maxwell3D state tracking.

use crate::engines::maxwell_3d::{
    NUM_REGS, PIPELINE_BASE, RT_BASE, RT_CONTROL, RT_STRIDE, SURFACE_CLIP_BASE,
    TEX_HEADER_POOL_BASE, TEX_SAMPLER_POOL_BASE, VERTEX_STREAM_BASE, VERTEX_STREAM_LIMIT_BASE,
    ZETA_BASE, ZETA_ENABLE, ZETA_SIZE_BASE,
};

/// Dirty flag indices matching upstream `VideoCommon::Dirty` enum.
pub mod flags {
    pub const NULL_ENTRY: u8 = 0;
    pub const DESCRIPTORS: u8 = 1;
    pub const RENDER_TARGETS: u8 = 2;
    pub const RENDER_TARGET_CONTROL: u8 = 3;
    pub const COLOR_BUFFER0: u8 = 4;
    pub const COLOR_BUFFER1: u8 = 5;
    pub const COLOR_BUFFER2: u8 = 6;
    pub const COLOR_BUFFER3: u8 = 7;
    pub const COLOR_BUFFER4: u8 = 8;
    pub const COLOR_BUFFER5: u8 = 9;
    pub const COLOR_BUFFER6: u8 = 10;
    pub const COLOR_BUFFER7: u8 = 11;
    pub const ZETA_BUFFER: u8 = 12;
    pub const RESCALE_VIEWPORTS: u8 = 13;
    pub const RESCALE_SCISSORS: u8 = 14;
    pub const VERTEX_BUFFERS: u8 = 15;
    pub const VERTEX_BUFFER0: u8 = 16;
    // VertexBuffer31 = VertexBuffer0 + 31 = 47
    pub const VERTEX_BUFFER31: u8 = VERTEX_BUFFER0 + 31;
    pub const INDEX_BUFFER: u8 = 48;
    pub const SHADERS: u8 = 49;
    // Special entries
    pub const DEPTH_BIAS_GLOBAL: u8 = 50;
    pub const LAST_COMMON_ENTRY: u8 = 51;
}

/// A dirty state table: one flag index per Maxwell3D register.
pub type DirtyTable = [u8; NUM_REGS];

/// Two-table set used by Maxwell3D dirty state tracking.
/// tables[0] = per-entry flag, tables[1] = group flag.
pub type DirtyTables = [DirtyTable; 2];

/// Port of upstream `VideoCommon::Dirty::GetDirtyFlagsForMethod`.
pub const fn get_dirty_flags_for_method(method: u32) -> (u8, u8) {
    const OFF_VERTEX_STREAMS: u32 = 0x2C0;
    const OFF_VERTEX_STREAM_LIMITS: u32 = 0x2F8;
    const OFF_INDEX_BUFFER: u32 = 0x460;
    const OFF_TEX_HEADER: u32 = 0x800;
    const OFF_TEX_SAMPLER: u32 = 0xA00;
    const OFF_RT: u32 = 0xE00;
    const OFF_SURFACE_CLIP: u32 = 0xE38;
    const OFF_RT_CONTROL: u32 = 0xE40;
    const OFF_ZETA_ENABLE: u32 = 0xE4C;
    const OFF_ZETA_SIZE_WIDTH: u32 = 0xE50;
    const OFF_ZETA_SIZE_HEIGHT: u32 = 0xE54;
    const OFF_ZETA: u32 = 0xE60;
    const OFF_PIPELINES: u32 = 0x1D00;

    if method >= OFF_VERTEX_STREAMS && method < OFF_VERTEX_STREAMS + 96 {
        let buffer_idx = (method - OFF_VERTEX_STREAMS) / 3;
        return (
            flags::VERTEX_BUFFER0.wrapping_add(buffer_idx as u8),
            flags::VERTEX_BUFFERS,
        );
    }

    if method >= OFF_VERTEX_STREAM_LIMITS && method < OFF_VERTEX_STREAM_LIMITS + 32 {
        let buffer_idx = method - OFF_VERTEX_STREAM_LIMITS;
        return (
            flags::VERTEX_BUFFER0.wrapping_add(buffer_idx as u8),
            flags::VERTEX_BUFFERS,
        );
    }

    if method == OFF_INDEX_BUFFER || (method > OFF_INDEX_BUFFER && method < OFF_INDEX_BUFFER + 3) {
        return (flags::INDEX_BUFFER, flags::NULL_ENTRY);
    }

    if method >= OFF_TEX_HEADER && method < OFF_TEX_HEADER + 256 {
        return (flags::DESCRIPTORS, flags::NULL_ENTRY);
    }

    if method >= OFF_TEX_SAMPLER && method < OFF_TEX_SAMPLER + 256 {
        return (flags::DESCRIPTORS, flags::NULL_ENTRY);
    }

    if method >= OFF_RT && method < OFF_RT + 64 {
        let rt_idx = (method - OFF_RT) / 8;
        return (
            flags::COLOR_BUFFER0.wrapping_add(rt_idx as u8),
            flags::RENDER_TARGETS,
        );
    }

    if method == OFF_SURFACE_CLIP || (method > OFF_SURFACE_CLIP && method < OFF_SURFACE_CLIP + 4) {
        return (flags::RENDER_TARGETS, flags::NULL_ENTRY);
    }

    if method == OFF_RT_CONTROL {
        return (flags::RENDER_TARGETS, flags::RENDER_TARGET_CONTROL);
    }

    if method == OFF_ZETA_ENABLE || method == OFF_ZETA_SIZE_WIDTH || method == OFF_ZETA_SIZE_HEIGHT
    {
        return (flags::ZETA_BUFFER, flags::RENDER_TARGETS);
    }

    if method >= OFF_ZETA && method < OFF_ZETA + 8 {
        return (flags::ZETA_BUFFER, flags::RENDER_TARGETS);
    }

    if method >= OFF_PIPELINES && method < OFF_PIPELINES + 1024 {
        return (flags::SHADERS, flags::NULL_ENTRY);
    }

    (flags::NULL_ENTRY, flags::NULL_ENTRY)
}

/// Fill a block of entries in a single table with a given dirty index.
pub fn fill_block(table: &mut DirtyTable, begin: usize, num: usize, dirty_index: u8) {
    table[begin..begin + num].fill(dirty_index);
}

/// Fill a block of entries in both tables with respective dirty indices.
pub fn fill_block_both(
    tables: &mut DirtyTables,
    begin: usize,
    num: usize,
    index_a: u8,
    index_b: u8,
) {
    fill_block(&mut tables[0], begin, num, index_a);
    fill_block(&mut tables[1], begin, num, index_b);
}

/// Sets up dirty flags for all Maxwell3D register ranges.
///
/// This mirrors the upstream `SetupDirtyFlags` function. The actual register offsets
/// depend on the Maxwell3D register layout which is defined in the engines module.
pub fn setup_dirty_flags(tables: &mut DirtyTables) {
    setup_dirty_vertex_buffers(tables);
    setup_index_buffer(tables);
    setup_dirty_descriptors(tables);
    setup_dirty_render_targets(tables);
    setup_dirty_shaders(tables);
}

fn setup_dirty_vertex_buffers(tables: &mut DirtyTables) {
    const NUM_VERTEX_ARRAYS: usize = 32;
    const NUM_VERTEX_STREAM_WORDS: usize = 4;
    const NUM_VERTEX_STREAM_LIMIT_WORDS: usize = 2;
    const NUM_VERTEX_STREAM_LIMITS_WORDS: usize = NUM_VERTEX_ARRAYS * NUM_VERTEX_STREAM_LIMIT_WORDS;
    const NUM_ARRAY_WORDS_DIRTY: usize = 3;

    for i in 0..NUM_VERTEX_ARRAYS {
        let array_offset = VERTEX_STREAM_BASE as usize + i * NUM_VERTEX_STREAM_WORDS;
        let limit_offset = VERTEX_STREAM_LIMIT_BASE as usize + i * NUM_VERTEX_STREAM_LIMIT_WORDS;
        let per_buffer = flags::VERTEX_BUFFER0 + i as u8;
        fill_block_both(
            tables,
            array_offset,
            NUM_ARRAY_WORDS_DIRTY,
            per_buffer,
            flags::VERTEX_BUFFERS,
        );
        fill_block_both(
            tables,
            limit_offset,
            NUM_VERTEX_STREAM_LIMITS_WORDS,
            per_buffer,
            flags::VERTEX_BUFFERS,
        );
    }
}

fn setup_index_buffer(tables: &mut DirtyTables) {
    const INDEX_BUFFER_WORDS: usize = 7;
    fill_block(
        &mut tables[0],
        crate::engines::maxwell_3d::IB_BASE as usize,
        INDEX_BUFFER_WORDS,
        flags::INDEX_BUFFER,
    );
}

fn setup_dirty_descriptors(tables: &mut DirtyTables) {
    const DESCRIPTOR_POOL_WORDS: usize = 3;
    fill_block(
        &mut tables[0],
        TEX_HEADER_POOL_BASE as usize,
        DESCRIPTOR_POOL_WORDS,
        flags::DESCRIPTORS,
    );
    fill_block(
        &mut tables[0],
        TEX_SAMPLER_POOL_BASE as usize,
        DESCRIPTOR_POOL_WORDS,
        flags::DESCRIPTORS,
    );
}

fn setup_dirty_render_targets(tables: &mut DirtyTables) {
    const NUM_RENDER_TARGETS: usize = 8;
    const RENDER_TARGET_WORDS: usize = 0x40 / 4;
    const SURFACE_CLIP_WORDS: usize = 0x8 / 4;
    const ZETA_WORDS: usize = 0x14 / 4;

    let rt_begin = RT_BASE as usize;
    let rt_num = RENDER_TARGET_WORDS * NUM_RENDER_TARGETS;
    for rt in 0..NUM_RENDER_TARGETS {
        fill_block(
            &mut tables[0],
            rt_begin + rt * RT_STRIDE as usize,
            RENDER_TARGET_WORDS,
            flags::COLOR_BUFFER0 + rt as u8,
        );
    }
    fill_block(&mut tables[1], rt_begin, rt_num, flags::RENDER_TARGETS);
    fill_block(
        &mut tables[0],
        SURFACE_CLIP_BASE as usize,
        SURFACE_CLIP_WORDS,
        flags::RENDER_TARGETS,
    );

    tables[0][RT_CONTROL as usize] = flags::RENDER_TARGETS;
    tables[1][RT_CONTROL as usize] = flags::RENDER_TARGET_CONTROL;

    tables[0][ZETA_ENABLE as usize] = flags::ZETA_BUFFER;
    tables[1][ZETA_ENABLE as usize] = flags::RENDER_TARGETS;
    tables[0][ZETA_SIZE_BASE as usize] = flags::ZETA_BUFFER;
    tables[1][ZETA_SIZE_BASE as usize] = flags::RENDER_TARGETS;
    tables[0][ZETA_SIZE_BASE as usize + 1] = flags::ZETA_BUFFER;
    tables[1][ZETA_SIZE_BASE as usize + 1] = flags::RENDER_TARGETS;
    fill_block(
        &mut tables[0],
        ZETA_BASE as usize,
        ZETA_WORDS,
        flags::ZETA_BUFFER,
    );
    fill_block(
        &mut tables[1],
        ZETA_BASE as usize,
        ZETA_WORDS,
        flags::RENDER_TARGETS,
    );
}

fn setup_dirty_shaders(tables: &mut DirtyTables) {
    const MAX_SHADER_PROGRAMS: usize = 6;
    const PIPELINE_WORDS: usize = 0x40 / 4;
    fill_block(
        &mut tables[0],
        PIPELINE_BASE as usize,
        PIPELINE_WORDS * MAX_SHADER_PROGRAMS,
        flags::SHADERS,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_dirty_flags_for_method_matches_upstream_boundaries() {
        assert_eq!(
            get_dirty_flags_for_method(0x2C0),
            (flags::VERTEX_BUFFER0, flags::VERTEX_BUFFERS)
        );
        assert_eq!(
            get_dirty_flags_for_method(0x2C0 + 95),
            (flags::VERTEX_BUFFER31, flags::VERTEX_BUFFERS)
        );
        assert_eq!(
            get_dirty_flags_for_method(0x2C0 + 96),
            (flags::NULL_ENTRY, flags::NULL_ENTRY)
        );
        // Preserve Eden's branch ordering even where the hard-coded ranges overlap.
        assert_eq!(
            get_dirty_flags_for_method(0x2F8 + 31),
            (flags::VERTEX_BUFFER0 + 29, flags::VERTEX_BUFFERS)
        );
        assert_eq!(
            get_dirty_flags_for_method(0x460 + 2),
            (flags::INDEX_BUFFER, flags::NULL_ENTRY)
        );
        assert_eq!(
            get_dirty_flags_for_method(0x800 + 255),
            (flags::DESCRIPTORS, flags::NULL_ENTRY)
        );
        assert_eq!(
            get_dirty_flags_for_method(0xA00 + 255),
            (flags::DESCRIPTORS, flags::NULL_ENTRY)
        );
        assert_eq!(
            get_dirty_flags_for_method(0xE00 + 63),
            (flags::COLOR_BUFFER7, flags::RENDER_TARGETS)
        );
        assert_eq!(
            get_dirty_flags_for_method(0xE38),
            (flags::COLOR_BUFFER7, flags::RENDER_TARGETS)
        );
        assert_eq!(
            get_dirty_flags_for_method(0xE40),
            (flags::RENDER_TARGETS, flags::RENDER_TARGET_CONTROL)
        );
        assert_eq!(
            get_dirty_flags_for_method(0xE50),
            (flags::ZETA_BUFFER, flags::RENDER_TARGETS)
        );
        assert_eq!(
            get_dirty_flags_for_method(0x1D00 + 1023),
            (flags::SHADERS, flags::NULL_ENTRY)
        );
        assert_eq!(
            get_dirty_flags_for_method(0x1D00 + 1024),
            (flags::NULL_ENTRY, flags::NULL_ENTRY)
        );
    }

    #[test]
    fn vertex_limit_setup_preserves_upstream_full_array_fill() {
        let mut tables = [[flags::NULL_ENTRY; NUM_REGS]; 2];
        setup_dirty_flags(&mut tables);

        for index in 0..32usize {
            let begin = VERTEX_STREAM_LIMIT_BASE as usize + index * 2;
            assert_eq!(tables[0][begin], flags::VERTEX_BUFFER0 + index as u8);
            assert_eq!(tables[0][begin + 1], flags::VERTEX_BUFFER0 + index as u8);
            assert_eq!(tables[1][begin], flags::VERTEX_BUFFERS);
            assert_eq!(tables[1][begin + 1], flags::VERTEX_BUFFERS);
        }

        let first_word_after_limits = VERTEX_STREAM_LIMIT_BASE as usize + 64;
        assert_eq!(tables[0][first_word_after_limits], flags::SHADERS);
        assert_eq!(tables[1][first_word_after_limits], flags::VERTEX_BUFFERS);
        assert_eq!(
            tables[1][first_word_after_limits + 61],
            flags::VERTEX_BUFFERS
        );
        assert_eq!(tables[1][first_word_after_limits + 62], flags::NULL_ENTRY);
    }

    #[test]
    fn zeta_size_setup_marks_only_width_and_height() {
        let mut tables = [[flags::NULL_ENTRY; NUM_REGS]; 2];
        setup_dirty_flags(&mut tables);

        for (table, expected) in tables
            .iter()
            .zip([flags::ZETA_BUFFER, flags::RENDER_TARGETS])
        {
            assert_eq!(table[ZETA_SIZE_BASE as usize], expected);
            assert_eq!(table[ZETA_SIZE_BASE as usize + 1], expected);
            assert_eq!(table[ZETA_SIZE_BASE as usize + 2], flags::NULL_ENTRY);
        }
    }

    #[test]
    fn dirty_table_has_upstream_fixed_register_count() {
        assert_eq!(std::mem::size_of::<DirtyTable>(), NUM_REGS);
    }

    #[test]
    fn setup_dirty_flags_marks_core_upstream_ranges() {
        let mut tables = [[flags::NULL_ENTRY; NUM_REGS]; 2];
        setup_dirty_flags(&mut tables);

        assert_eq!(
            tables[0][crate::engines::maxwell_3d::IB_BASE as usize],
            flags::INDEX_BUFFER
        );
        assert_eq!(tables[0][TEX_HEADER_POOL_BASE as usize], flags::DESCRIPTORS);
        assert_eq!(tables[0][RT_BASE as usize], flags::COLOR_BUFFER0);
        assert_eq!(tables[1][RT_BASE as usize], flags::RENDER_TARGETS);
        assert_eq!(tables[0][RT_CONTROL as usize], flags::RENDER_TARGETS);
        assert_eq!(tables[1][RT_CONTROL as usize], flags::RENDER_TARGET_CONTROL);
        assert_eq!(tables[0][ZETA_BASE as usize], flags::ZETA_BUFFER);
        assert_eq!(tables[1][ZETA_BASE as usize], flags::RENDER_TARGETS);
        assert_eq!(tables[0][PIPELINE_BASE as usize], flags::SHADERS);
    }
}
