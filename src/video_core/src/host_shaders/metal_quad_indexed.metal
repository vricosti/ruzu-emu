// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Native counterpart of Eden's vulkan_quad_indexed.comp.
#include <metal_stdlib>
using namespace metal;

struct QuadIndexedParams {
    uint base_vertex;
    uint index_shift;
    uint is_strip;
    uint num_primitives;
};

kernel void assemble_quad_indexed(const device uchar* input_indexes [[buffer(0)]],
                                  device uint* output_indexes [[buffer(1)]],
                                  constant QuadIndexedParams& p [[buffer(2)]],
                                  uint primitive [[thread_position_in_grid]]) {
    if (primitive >= p.num_primitives) return;
    constexpr uint quads_swizzle[6] = {0, 1, 2, 0, 2, 3};
    constexpr uint quad_strip_swizzle[6] = {0, 3, 1, 0, 2, 3};
    for (uint vertex_index = 0; vertex_index < 6; ++vertex_index) {
        uint offset = p.is_strip == 0 ? primitive*4 + quads_swizzle[vertex_index]
                                     : primitive*2 + quad_strip_swizzle[vertex_index];
        ulong byte_offset = ulong(offset) << p.index_shift;
        uint index = 0;
        // Byte loads support native buffer offsets without requiring a u32
        // alignment or fetching beyond the last complete source element.
        for (uint byte_index = 0; byte_index < (1u << p.index_shift); ++byte_index) {
            index |= uint(input_indexes[byte_offset + byte_index]) << (8u*byte_index);
        }
        output_indexes[ulong(primitive)*6 + vertex_index] = index + p.base_vertex;
    }
}
