// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

#include <metal_stdlib>
using namespace metal;

// Native counterpart of query prefix-scan accumulation: Metal gives each draw
// a distinct visibility slot, and a report needs the last sum, not all prefixes.
kernel void visibility_resolve(const device ulong* input [[buffer(0)]],
                               device ulong* output [[buffer(1)]],
                               const device ulong& previous [[buffer(2)]],
                               constant uint3& params [[buffer(3)]],
                               uint id [[thread_position_in_threadgroup]],
                               uint group [[threadgroup_position_in_grid]]) {
    threadgroup ulong sums[256];
    uint index = group * 256u + id;
    sums[id] = index < params.y ? input[params.x + index] : 0ul;
    threadgroup_barrier(mem_flags::mem_threadgroup);
    for (uint stride = 128u; stride != 0u; stride >>= 1u) {
        if (id < stride) sums[id] += sums[id + stride];
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (id == 0u) output[group] = sums[0] + (params.z != 0u ? previous : 0ul);
}
