// SPDX-FileCopyrightText: Copyright 2019 yuzu Emulator Project
// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Native counterpart of Eden's vulkan_uint8.comp.
#include <metal_stdlib>
using namespace metal;

kernel void assemble_uint8(const device uchar* input_indexes [[buffer(0)]],
                          device ushort* output_indexes [[buffer(1)]],
                          constant uint& count [[buffer(2)]],
                          uint id [[thread_position_in_grid]]) {
    if (id >= count) return;
    uint index = uint(input_indexes[id]);
    // Preserve Eden's restart remapping, including its fixed 0xff token.
    output_indexes[id] = ushort(index == 0xffu ? 0xffffu : index);
}
