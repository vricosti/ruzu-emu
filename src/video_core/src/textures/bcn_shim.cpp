// SPDX-FileCopyrightText: Copyright 2026 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <cstdint>
#include <cstddef>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <utility>

#include <bc_decoder.h>
#include <bc7enc.h>
#include <stb_dxt.h>

extern "C" {

void ruzu_stb_compress_bc1_block(std::uint8_t* dest, const std::uint8_t* src, int alpha) {
    stb_compress_bc1_block(dest, src, alpha, STB_DXT_NORMAL);
}

void ruzu_stb_compress_bc3_block(std::uint8_t* dest, const std::uint8_t* src) {
    stb_compress_bc3_block(dest, src, STB_DXT_NORMAL);
}

namespace {

// Analytical real-time BC7 mode 6 encoder, in the spirit of Basis Universal's
// transcoder-internal real-time encoders: principal-axis projection and one
// closed-form least-squares endpoint refinement, no mode or partition search.
// Mode 6 is a single-subset RGBA mode (7777 endpoints + per-endpoint P bit,
// 4-bit indices), so every block, opaque or not, is encoded the same way.

// BC7 4-bit index interpolation weights from the specification.
constexpr int kWeights4[16] = {0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64};

struct Bits128 {
    std::uint64_t lo = 0;
    std::uint64_t hi = 0;
    unsigned cursor = 0;
    void put(std::uint32_t value, unsigned count) {
        for (unsigned i = 0; i < count; ++i, ++cursor) {
            const std::uint64_t bit = (value >> i) & 1u;
            if (cursor < 64) {
                lo |= bit << cursor;
            } else {
                hi |= bit << (cursor - 64);
            }
        }
    }
};

inline int clamp255(float v) {
    return v < 0.0f ? 0 : (v > 255.0f ? 255 : static_cast<int>(v + 0.5f));
}

// Quantize an 8-bit endpoint channel to mode 6's 7 bits + shared P bit.
inline std::uint32_t quant7(int v, int pbit) {
    // Effective dequantized value is (e << 1) | p; choose e for this p.
    int e = (v - pbit) >> 1;
    if (e < 0) {
        e = 0;
    } else if (e > 127) {
        e = 127;
    }
    return static_cast<std::uint32_t>(e);
}

inline int dequant7(std::uint32_t e, int pbit) {
    return static_cast<int>((e << 1) | static_cast<std::uint32_t>(pbit));
}

void encode_bc7_mode6_analytical(std::uint8_t* dest, const std::uint8_t* src) {
    float px[16][4];
    float mean[4] = {0, 0, 0, 0};
    for (int i = 0; i < 16; ++i) {
        for (int c = 0; c < 4; ++c) {
            px[i][c] = static_cast<float>(src[i * 4 + c]);
            mean[c] += px[i][c];
        }
    }
    for (float& m : mean) {
        m *= 1.0f / 16.0f;
    }

    // Principal axis by three power iterations over the covariance action.
    // Seed with the pixel farthest from the mean: unlike a fixed vector, it
    // can never be orthogonal to the dominant direction of the block.
    float axis[4] = {0.0f, 0.0f, 0.0f, 0.0f};
    float best = 0.0f;
    for (int i = 0; i < 16; ++i) {
        float len = 0.0f;
        float d[4];
        for (int c = 0; c < 4; ++c) {
            d[c] = px[i][c] - mean[c];
            len += d[c] * d[c];
        }
        if (len > best) {
            best = len;
            for (int c = 0; c < 4; ++c) {
                axis[c] = d[c];
            }
        }
    }
    if (best < 1e-10f) {
        axis[0] = 1.0f; // Constant block: any axis works.
    }
    for (int iter = 0; iter < 3; ++iter) {
        float next[4] = {0, 0, 0, 0};
        for (int i = 0; i < 16; ++i) {
            float d[4];
            float dot = 0.0f;
            for (int c = 0; c < 4; ++c) {
                d[c] = px[i][c] - mean[c];
                dot += d[c] * axis[c];
            }
            for (int c = 0; c < 4; ++c) {
                next[c] += dot * d[c];
            }
        }
        float len = 0.0f;
        for (float c : next) {
            len += c * c;
        }
        if (len < 1e-10f) {
            break; // Constant block: axis is irrelevant.
        }
        len = 1.0f / std::sqrt(len);
        for (int c = 0; c < 4; ++c) {
            axis[c] = next[c] * len;
        }
    }

    // Project all pixels on the axis; the extremes become the endpoints.
    float tmin = 1e30f, tmax = -1e30f;
    float t[16];
    for (int i = 0; i < 16; ++i) {
        float dot = 0.0f;
        for (int c = 0; c < 4; ++c) {
            dot += (px[i][c] - mean[c]) * axis[c];
        }
        t[i] = dot;
        tmin = tmin < dot ? tmin : dot;
        tmax = tmax > dot ? tmax : dot;
    }
    float e0[4], e1[4];
    for (int c = 0; c < 4; ++c) {
        e0[c] = mean[c] + tmin * axis[c];
        e1[c] = mean[c] + tmax * axis[c];
    }

    // Analytical 4-bit indices from the scalar projection.
    int idx[16];
    const float span = tmax - tmin;
    const float scale = span > 1e-6f ? 15.0f / span : 0.0f;
    for (int i = 0; i < 16; ++i) {
        int q = static_cast<int>((t[i] - tmin) * scale + 0.5f);
        idx[i] = q < 0 ? 0 : (q > 15 ? 15 : q);
    }

    // One closed-form least-squares refinement of the endpoints given the
    // indices (the cheap half of bc7enc's refinement).
    float aa = 0.0f, ab = 0.0f, bb = 0.0f;
    float ax[4] = {0, 0, 0, 0}, bx[4] = {0, 0, 0, 0};
    for (int i = 0; i < 16; ++i) {
        const float w = static_cast<float>(kWeights4[idx[i]]) * (1.0f / 64.0f);
        const float omw = 1.0f - w;
        aa += omw * omw;
        ab += omw * w;
        bb += w * w;
        for (int c = 0; c < 4; ++c) {
            ax[c] += omw * px[i][c];
            bx[c] += w * px[i][c];
        }
    }
    const float det = aa * bb - ab * ab;
    if (det > 1e-6f) {
        const float inv = 1.0f / det;
        for (int c = 0; c < 4; ++c) {
            const float n0 = (ax[c] * bb - bx[c] * ab) * inv;
            const float n1 = (bx[c] * aa - ax[c] * ab) * inv;
            e0[c] = n0;
            e1[c] = n1;
        }
    }

    int p0 = 0, p1 = 0;
    int q0[4], q1[4];
    // Choose each endpoint's shared P bit by majority of channel LSBs.
    int lsb0 = 0, lsb1 = 0;
    for (int c = 0; c < 4; ++c) {
        lsb0 += clamp255(e0[c]) & 1;
        lsb1 += clamp255(e1[c]) & 1;
    }
    p0 = lsb0 >= 2 ? 1 : 0;
    p1 = lsb1 >= 2 ? 1 : 0;
    for (int c = 0; c < 4; ++c) {
        q0[c] = static_cast<int>(quant7(clamp255(e0[c]), p0));
        q1[c] = static_cast<int>(quant7(clamp255(e1[c]), p1));
    }

    // Recompute indices against the quantized endpoints so decode error stays
    // centered, still by scalar projection (no 16-way search).
    float d0[4], d1[4];
    for (int c = 0; c < 4; ++c) {
        d0[c] = static_cast<float>(dequant7(static_cast<std::uint32_t>(q0[c]), p0));
        d1[c] = static_cast<float>(dequant7(static_cast<std::uint32_t>(q1[c]), p1));
    }
    float axis2[4];
    float len2 = 0.0f;
    for (int c = 0; c < 4; ++c) {
        axis2[c] = d1[c] - d0[c];
        len2 += axis2[c] * axis2[c];
    }
    if (len2 > 1e-6f) {
        const float inv = 15.0f / len2;
        for (int i = 0; i < 16; ++i) {
            float dot = 0.0f;
            for (int c = 0; c < 4; ++c) {
                dot += (px[i][c] - d0[c]) * axis2[c];
            }
            int q = static_cast<int>(dot * inv + 0.5f);
            idx[i] = q < 0 ? 0 : (q > 15 ? 15 : q);
        }
    } else {
        for (int& i : idx) {
            i = 0;
        }
    }

    // The anchor index (pixel 0) must have its top bit clear; otherwise swap.
    if (idx[0] > 7) {
        for (int c = 0; c < 4; ++c) {
            std::swap(q0[c], q1[c]);
        }
        std::swap(p0, p1);
        for (int& i : idx) {
            i = 15 - i;
        }
    }

    Bits128 bits;
    bits.put(1u << 6, 7); // Mode 6: six zero bits, then a one.
    for (int c = 0; c < 4; ++c) {
        bits.put(static_cast<std::uint32_t>(q0[c]), 7);
        bits.put(static_cast<std::uint32_t>(q1[c]), 7);
    }
    bits.put(static_cast<std::uint32_t>(p0), 1);
    bits.put(static_cast<std::uint32_t>(p1), 1);
    bits.put(static_cast<std::uint32_t>(idx[0]), 3);
    for (int i = 1; i < 16; ++i) {
        bits.put(static_cast<std::uint32_t>(idx[i]), 4);
    }
    std::memcpy(dest, &bits.lo, 8);
    std::memcpy(dest + 8, &bits.hi, 8);
}

} // Anonymous namespace

void ruzu_bc7enc_compress_block(std::uint8_t* dest, const std::uint8_t* src) {
    static const bool fast = [] {
        const char* value = std::getenv("RUZU_BC7_FAST");
        // The analytical encoder is the runtime default; RUZU_BC7_FAST=0
        // selects the slower searching encoder (bc7enc, 16 partitions).
        return value == nullptr || value[0] != '0';
    }();
    if (fast) {
        encode_bc7_mode6_analytical(dest, src);
        return;
    }
    // Thread-safe one-time global table and parameter initialization
    // (C++11 magic static); bc7enc_compress_block is thread-safe afterwards.
    static const bc7enc_compress_block_params params = [] {
        bc7enc_compress_block_init();
        bc7enc_compress_block_params p;
        bc7enc_compress_block_params_init(&p);
        // Offline-grade profile: fewer mode-1 partitions than the exhaustive
        // default, still far above the analytical mode-6 path.
        p.m_max_partitions = 16;
        return p;
    }();
    bc7enc_compress_block(dest, src, &params);
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
