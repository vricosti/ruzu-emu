// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

#include <metal_stdlib>
using namespace metal;

struct ConditionalArgumentsParams {
    uint word_count;
    uint disabled_word;
    uint source_stride_words;
    uint command_count;
    uint inverted;
};

// Metal counterpart of conditional command execution: generate independent
// indirect records without changing the original draw/dispatch arguments.
kernel void conditional_arguments(
    device const uint* predicate [[buffer(0)]],
    device const uint* source [[buffer(1)]],
    device uint* destination [[buffer(2)]],
    constant ConditionalArgumentsParams& p [[buffer(3)]],
    uint command [[thread_position_in_grid]]) {
    if (command >= p.command_count) return;
    bool enabled = (*predicate != 0u) != (p.inverted != 0u);
    for (uint word = 0u; word < p.word_count; ++word) {
        uint value = source[ulong(command) * p.source_stride_words + word];
        destination[ulong(command) * p.word_count + word] =
            !enabled && word == p.disabled_word ? 0u : value;
    }
}
