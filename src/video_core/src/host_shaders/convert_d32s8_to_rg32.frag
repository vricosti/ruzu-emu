// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

#version 450

layout(binding = 0) uniform sampler2D depth_tex;
layout(binding = 1) uniform usampler2D stencil_tex;

layout(location = 0) in vec2 texcoord;
layout(location = 0) out uvec2 color;

void main() {
    float depth = textureLod(depth_tex, texcoord, 0).r;
    uint stencil = textureLod(stencil_tex, texcoord, 0).r;
    color = uvec2(floatBitsToUint(depth), stencil);
}
