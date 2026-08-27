// SPDX-FileCopyrightText: Copyright 2026 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <cstdint>
#include <cstddef>

#include <bc_decoder.h>
#include <stb_dxt.h>

extern "C" {

void ruzu_stb_compress_bc1_block(std::uint8_t* dest, const std::uint8_t* src, int alpha) {
    stb_compress_bc1_block(dest, src, alpha, STB_DXT_NORMAL);
}

void ruzu_stb_compress_bc3_block(std::uint8_t* dest, const std::uint8_t* src) {
    stb_compress_bc3_block(dest, src, STB_DXT_NORMAL);
}

void ruzu_decode_bc1_block(const std::uint8_t* src,
                           std::uint8_t* dst,
                           std::uintptr_t x,
                           std::uintptr_t y,
                           std::uintptr_t width,
                           std::uintptr_t height) {
    bcn::DecodeBc1(src, dst, x, y, width, height);
}

void ruzu_decode_bc2_block(const std::uint8_t* src,
                           std::uint8_t* dst,
                           std::uintptr_t x,
                           std::uintptr_t y,
                           std::uintptr_t width,
                           std::uintptr_t height) {
    bcn::DecodeBc2(src, dst, x, y, width, height);
}

void ruzu_decode_bc3_block(const std::uint8_t* src,
                           std::uint8_t* dst,
                           std::uintptr_t x,
                           std::uintptr_t y,
                           std::uintptr_t width,
                           std::uintptr_t height) {
    bcn::DecodeBc3(src, dst, x, y, width, height);
}

void ruzu_decode_bc4_block(const std::uint8_t* src,
                           std::uint8_t* dst,
                           std::uintptr_t x,
                           std::uintptr_t y,
                           std::uintptr_t width,
                           std::uintptr_t height,
                           bool is_signed) {
    bcn::DecodeBc4(src, dst, x, y, width, height, is_signed);
}

void ruzu_decode_bc5_block(const std::uint8_t* src,
                           std::uint8_t* dst,
                           std::uintptr_t x,
                           std::uintptr_t y,
                           std::uintptr_t width,
                           std::uintptr_t height,
                           bool is_signed) {
    bcn::DecodeBc5(src, dst, x, y, width, height, is_signed);
}

void ruzu_decode_bc6_block(const std::uint8_t* src,
                           std::uint8_t* dst,
                           std::uintptr_t x,
                           std::uintptr_t y,
                           std::uintptr_t width,
                           std::uintptr_t height,
                           bool is_signed) {
    bcn::DecodeBc6(src, dst, x, y, width, height, is_signed);
}

void ruzu_decode_bc7_block(const std::uint8_t* src,
                           std::uint8_t* dst,
                           std::uintptr_t x,
                           std::uintptr_t y,
                           std::uintptr_t width,
                           std::uintptr_t height) {
    bcn::DecodeBc7(src, dst, x, y, width, height);
}

}
