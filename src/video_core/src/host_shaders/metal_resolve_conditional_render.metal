// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

#include <metal_stdlib>
using namespace metal;

// Native counterpart of resolve_conditional_render.comp. The two halves of
// the conditional value must BOTH be nonzero; this is not a uint64 != 0 test.
kernel void resolve_conditional_render(
    device const uint* data [[buffer(0)]],
    device uint* result [[buffer(1)]],
    constant uint& compare_to_zero [[buffer(2)]]) {
    if (compare_to_zero != 0u) {
        *result = (data[0] != 0u && data[1] != 0u) ? 1u : 0u;
    } else {
        *result = (data[0] == data[4] && data[1] == data[5]) ? 1u : 0u;
    }
}
