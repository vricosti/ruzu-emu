// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

#version 450
#extension GL_ARB_shader_stencil_export : require

layout(binding = 0) uniform usampler2D color_tex;

layout(location = 0) in vec2 texcoord;

void main() {
    uvec2 packed = textureLod(color_tex, texcoord, 0).rg;
    gl_FragDepth = uintBitsToFloat(packed.r);
    gl_FragStencilRefARB = int(packed.g & 0xffu);
}
