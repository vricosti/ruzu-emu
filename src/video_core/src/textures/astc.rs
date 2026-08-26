// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: Apache-2.0

//! Port of `video_core/textures/astc.h` and `astc.cpp`.
//!
//! ASTC (Adaptive Scalable Texture Compression) block decoder.
//! The original C++ code is derived from the FasTC reference implementation
//! (University of North Carolina at Chapel Hill).
//!
//! This module decodes ASTC-encoded texture data into RGBA8 output.

use super::workers::get_thread_workers;
use common::alignment::divide_up;
use smallvec::SmallVec;

// ── Bit stream helpers ───────────────────────────────────────────────────────

/// Input bit stream that reads bits from a byte slice in LSB-first order.
///
/// Port of the anonymous `InputBitStream` class in `astc.cpp`.
struct InputBitStream<'a> {
    data: &'a [u8],
    byte_pos: usize,
    next_bit: usize,
    bits_read: usize,
}

impl<'a> InputBitStream<'a> {
    fn new(data: &'a [u8], start_offset: usize) -> Self {
        Self {
            data,
            byte_pos: 0,
            next_bit: start_offset % 8,
            bits_read: 0,
        }
    }

    fn bits_read(&self) -> usize {
        self.bits_read
    }

    fn read_bit(&mut self) -> bool {
        if self.bits_read >= self.data.len() * 8 {
            return false;
        }
        let bit = (self.data[self.byte_pos] >> self.next_bit) & 1 != 0;
        self.next_bit += 1;
        while self.next_bit >= 8 {
            self.next_bit -= 8;
            self.byte_pos += 1;
        }
        self.bits_read += 1;
        bit
    }

    fn read_bits(&mut self, n_bits: usize) -> u32 {
        let mut ret = 0u32;
        for i in 0..n_bits {
            ret |= (self.read_bit() as u32) << i;
        }
        ret
    }
}

/// Output bit stream that writes bits to a byte slice in LSB-first order.
///
/// Port of the anonymous `OutputBitStream` class in `astc.cpp`.
struct OutputBitStream<'a> {
    data: &'a mut [u8],
    byte_pos: usize,
    num_bits: usize,
    bits_written: usize,
    next_bit: usize,
}

impl<'a> OutputBitStream<'a> {
    fn new(data: &'a mut [u8], num_bits: usize, start_offset: usize) -> Self {
        Self {
            data,
            byte_pos: 0,
            num_bits,
            bits_written: 0,
            next_bit: start_offset % 8,
        }
    }

    #[allow(dead_code)]
    fn bits_written(&self) -> usize {
        self.bits_written
    }

    fn write_bit(&mut self, b: bool) {
        if self.bits_written >= self.num_bits {
            return;
        }
        let mask = 1u8 << self.next_bit;
        // Clear the bit
        self.data[self.byte_pos] &= !mask;
        // Write the bit if set
        if b {
            self.data[self.byte_pos] |= mask;
        }
        self.next_bit += 1;
        if self.next_bit >= 8 {
            self.byte_pos += 1;
            self.next_bit = 0;
        }
        self.bits_written += 1;
    }

    fn write_bits(&mut self, val: u32, n_bits: u32) {
        for i in 0..n_bits {
            self.write_bit(((val >> i) & 1) != 0);
        }
    }

    #[allow(dead_code)]
    fn write_bits_r(&mut self, val: u32, n_bits: u32) {
        for i in 0..n_bits {
            self.write_bit(((val >> (n_bits - i - 1)) & 1) != 0);
        }
    }
}

// ── Bits helper ──────────────────────────────────────────────────────────────

/// Port of `Bits<IntType>` class from `astc.cpp`.
/// Provides bit extraction: single bit via `bit(pos)` and range via `bits(start, end)`.
struct Bits {
    val: u32,
}

impl Bits {
    fn new(val: u32) -> Self {
        Self { val }
    }

    /// Extract a single bit at `bit_pos`.
    fn bit(&self, bit_pos: u32) -> u32 {
        (self.val >> bit_pos) & 1
    }

    /// Extract bits in range [start, end] (inclusive). Handles start > end by swapping.
    fn bits(&self, start: u32, end: u32) -> u32 {
        let (s, e) = if start > end {
            (end, start)
        } else {
            (start, end)
        };
        if s == e {
            return self.bit(s);
        }
        let mask = (1u64 << (e - s + 1)) - 1;
        (self.val >> s) & (mask as u32)
    }
}

// ── Integer encoding types ───────────────────────────────────────────────────

/// Encoding type for bounded integer sequences in ASTC.
///
/// Port of `IntegerEncoding` enum from `astc.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegerEncoding {
    JustBits,
    Quint,
    Trit,
}

/// A single integer value decoded from a bounded integer sequence.
///
/// Port of `IntegerEncodedValue` struct from `astc.cpp`.
#[derive(Debug, Clone, Copy)]
struct IntegerEncodedValue {
    encoding: IntegerEncoding,
    num_bits: u32,
    bit_value: u32,
    /// Holds either quint_value or trit_value (union in C++).
    quint_trit_value: u32,
}

impl IntegerEncodedValue {
    const fn new(encoding: IntegerEncoding, num_bits: u32) -> Self {
        Self {
            encoding,
            num_bits,
            bit_value: 0,
            quint_trit_value: 0,
        }
    }

    const fn matches_encoding(&self, other: &IntegerEncodedValue) -> bool {
        self.encoding as u32 == other.encoding as u32 && self.num_bits == other.num_bits
    }

    /// Returns the number of bits required to encode `num_vals` values.
    fn get_bit_length(&self, num_vals: u32) -> u32 {
        let mut total_bits = self.num_bits * num_vals;
        match self.encoding {
            IntegerEncoding::Trit => {
                total_bits += (num_vals * 8 + 4) / 5;
            }
            IntegerEncoding::Quint => {
                total_bits += (num_vals * 7 + 2) / 3;
            }
            IntegerEncoding::JustBits => {}
        }
        total_bits
    }
}

/// Port of `CreateEncoding(u32)` from `astc.cpp`.
const fn create_encoding(mut max_value: u32) -> IntegerEncodedValue {
    while max_value > 0 {
        let check = max_value + 1;
        // Is max_value a power of two minus one?
        if (check & (check - 1)) == 0 {
            return IntegerEncodedValue::new(IntegerEncoding::JustBits, max_value.count_ones());
        }
        // Is max_value of the type 3*2^n - 1?
        if check % 3 == 0 && ((check / 3) & ((check / 3) - 1)) == 0 {
            return IntegerEncodedValue::new(IntegerEncoding::Trit, (check / 3 - 1).count_ones());
        }
        // Is max_value of the type 5*2^n - 1?
        if check % 5 == 0 && ((check / 5) & ((check / 5) - 1)) == 0 {
            return IntegerEncodedValue::new(IntegerEncoding::Quint, (check / 5 - 1).count_ones());
        }
        max_value -= 1;
    }
    IntegerEncodedValue::new(IntegerEncoding::JustBits, 0)
}

/// Pre-computed encoding table for max values 0..255.
///
/// Port of `ASTC_ENCODINGS_VALUES` from `astc.cpp`.
const ASTC_ENCODINGS_VALUES: [IntegerEncodedValue; 256] = {
    let mut arr = [IntegerEncodedValue::new(IntegerEncoding::JustBits, 0); 256];
    let mut i = 0usize;
    while i < 256 {
        arr[i] = create_encoding(i as u32);
        i += 1;
    }
    arr
};

// ── Integer sequence decoding ────────────────────────────────────────────────

/// Inline storage matching upstream's `boost::static_vector<..., 256>`.
type IntegerEncodedVector = SmallVec<[IntegerEncodedValue; 256]>;

/// Port of `DecodeTritBlock` from `astc.cpp`.
/// Implements the algorithm in ASTC spec section C.2.12.
fn decode_trit_block(
    bits: &mut InputBitStream,
    result: &mut IntegerEncodedVector,
    n_bits_per_value: u32,
) {
    let mut m = [0u32; 5];
    let mut t = [0u32; 5];

    // Read the trit encoded block according to table C.2.14
    m[0] = bits.read_bits(n_bits_per_value as usize);
    let mut big_t = bits.read_bits(2);
    m[1] = bits.read_bits(n_bits_per_value as usize);
    big_t |= bits.read_bits(2) << 2;
    m[2] = bits.read_bits(n_bits_per_value as usize);
    big_t |= (bits.read_bit() as u32) << 4;
    m[3] = bits.read_bits(n_bits_per_value as usize);
    big_t |= bits.read_bits(2) << 5;
    m[4] = bits.read_bits(n_bits_per_value as usize);
    big_t |= (bits.read_bit() as u32) << 7;

    let tb = Bits::new(big_t);
    let c = if tb.bits(2, 4) == 7 {
        t[4] = 2;
        t[3] = 2;
        (tb.bits(5, 7) << 2) | tb.bits(0, 1)
    } else {
        if tb.bits(5, 6) == 3 {
            t[4] = 2;
            t[3] = tb.bit(7);
        } else {
            t[4] = tb.bit(7);
            t[3] = tb.bits(5, 6);
        }
        tb.bits(0, 4)
    };

    let cb = Bits::new(c);
    if cb.bits(0, 1) == 3 {
        t[2] = 2;
        t[1] = cb.bit(4);
        t[0] = (cb.bit(3) << 1) | (cb.bit(2) & !cb.bit(3));
    } else if cb.bits(2, 3) == 3 {
        t[2] = 2;
        t[1] = 2;
        t[0] = cb.bits(0, 1);
    } else {
        t[2] = cb.bit(4);
        t[1] = cb.bits(2, 3);
        t[0] = (cb.bit(1) << 1) | (cb.bit(0) & !cb.bit(1));
    }

    for i in 0..5 {
        let mut val = IntegerEncodedValue::new(IntegerEncoding::Trit, n_bits_per_value);
        val.bit_value = m[i];
        val.quint_trit_value = t[i];
        result.push(val);
    }
}

/// Port of `DecodeQuintBlock` from `astc.cpp`.
/// Implements the algorithm in ASTC spec section C.2.12.
fn decode_quint_block(
    bits: &mut InputBitStream,
    result: &mut IntegerEncodedVector,
    n_bits_per_value: u32,
) {
    let mut m = [0u32; 3];
    let mut q = [0u32; 3];

    // Read the quint encoded block according to table C.2.15
    m[0] = bits.read_bits(n_bits_per_value as usize);
    let mut big_q = bits.read_bits(3);
    m[1] = bits.read_bits(n_bits_per_value as usize);
    big_q |= bits.read_bits(2) << 3;
    m[2] = bits.read_bits(n_bits_per_value as usize);
    big_q |= bits.read_bits(2) << 5;

    let qb = Bits::new(big_q);
    if qb.bits(1, 2) == 3 && qb.bits(5, 6) == 0 {
        q[0] = 4;
        q[1] = 4;
        q[2] = (qb.bit(0) << 2) | ((qb.bit(4) & !qb.bit(0)) << 1) | (qb.bit(3) & !qb.bit(0));
    } else {
        let c;
        if qb.bits(1, 2) == 3 {
            q[2] = 4;
            c = (qb.bits(3, 4) << 3) | ((!qb.bits(5, 6) & 3) << 1) | qb.bit(0);
        } else {
            q[2] = qb.bits(5, 6);
            c = qb.bits(0, 4);
        }

        let cb = Bits::new(c);
        if cb.bits(0, 2) == 5 {
            q[1] = 4;
            q[0] = cb.bits(3, 4);
        } else {
            q[1] = cb.bits(3, 4);
            q[0] = cb.bits(0, 2);
        }
    }

    for i in 0..3 {
        let mut val = IntegerEncodedValue::new(IntegerEncoding::Quint, n_bits_per_value);
        val.bit_value = m[i];
        val.quint_trit_value = q[i];
        result.push(val);
    }
}

/// Port of `DecodeIntegerSequence` from `astc.cpp`.
///
/// Fills result with the values that are encoded in the given
/// bitstream. We must know beforehand what the maximum possible
/// value is, and how many values we're decoding.
fn decode_integer_sequence(
    result: &mut IntegerEncodedVector,
    bits: &mut InputBitStream,
    max_range: u32,
    n_values: u32,
) {
    let val = ASTC_ENCODINGS_VALUES[max_range as usize];
    let mut n_vals_decoded = 0u32;
    while n_vals_decoded < n_values {
        match val.encoding {
            IntegerEncoding::Quint => {
                decode_quint_block(bits, result, val.num_bits);
                n_vals_decoded += 3;
            }
            IntegerEncoding::Trit => {
                decode_trit_block(bits, result, val.num_bits);
                n_vals_decoded += 5;
            }
            IntegerEncoding::JustBits => {
                let mut v = val;
                v.bit_value = bits.read_bits(val.num_bits as usize);
                result.push(v);
                n_vals_decoded += 1;
            }
        }
    }
}

// ── Replicate helper ─────────────────────────────────────────────────────────

/// Port of `Replicate` template from `astc.cpp`.
///
/// Replicates low `num_bits` such that [(to_bit - 1):(to_bit - 1 - from_bit)]
/// is the same as [(num_bits - 1):0] and repeats all the way down.
fn replicate(val: u32, mut num_bits: u32, to_bit: u32) -> u32 {
    if num_bits == 0 || to_bit == 0 {
        return 0;
    }
    let v = val & ((1 << num_bits) - 1);
    let mut res = v;
    let mut reslen = num_bits;
    while reslen < to_bit {
        let mut comp = 0u32;
        if num_bits > to_bit - reslen {
            let newshift = to_bit - reslen;
            comp = num_bits - newshift;
            num_bits = newshift;
        }
        res <<= num_bits;
        res |= v >> comp;
        reslen += num_bits;
    }
    res
}

// Pre-computed replicate tables matching upstream.
// Port of REPLICATE_*_BIT_TO_*_TABLE arrays.

const fn make_replicate_table<const N: usize>(num_bits: u32, to_bit: u32) -> [u32; N] {
    let mut table = [0u32; N];
    let mut i = 0u32;
    while (i as usize) < N {
        // Inline replicate for const context
        table[i as usize] = const_replicate(i, num_bits, to_bit);
        i += 1;
    }
    table
}

const fn const_replicate(val: u32, mut num_bits: u32, to_bit: u32) -> u32 {
    if num_bits == 0 || to_bit == 0 {
        return 0;
    }
    let v = val & ((1 << num_bits) - 1);
    let mut res = v;
    let mut reslen = num_bits;
    while reslen < to_bit {
        let mut comp = 0u32;
        if num_bits > to_bit - reslen {
            let newshift = to_bit - reslen;
            comp = num_bits - newshift;
            num_bits = newshift;
        }
        res <<= num_bits;
        res |= v >> comp;
        reslen += num_bits;
    }
    res
}

static REPLICATE_BYTE_TO_16_TABLE: [u32; 256] = make_replicate_table::<256>(8, 16);
static REPLICATE_BIT_TO_7_TABLE: [u32; 2] = make_replicate_table::<2>(1, 7);
static REPLICATE_BIT_TO_9_TABLE: [u32; 2] = make_replicate_table::<2>(1, 9);

static REPLICATE_1_BIT_TO_8_TABLE: [u32; 2] = make_replicate_table::<2>(1, 8);
static REPLICATE_2_BIT_TO_8_TABLE: [u32; 4] = make_replicate_table::<4>(2, 8);
static REPLICATE_3_BIT_TO_8_TABLE: [u32; 8] = make_replicate_table::<8>(3, 8);
static REPLICATE_4_BIT_TO_8_TABLE: [u32; 16] = make_replicate_table::<16>(4, 8);
static REPLICATE_5_BIT_TO_8_TABLE: [u32; 32] = make_replicate_table::<32>(5, 8);
static REPLICATE_6_BIT_TO_8_TABLE: [u32; 64] = make_replicate_table::<64>(6, 8);
static REPLICATE_7_BIT_TO_8_TABLE: [u32; 128] = make_replicate_table::<128>(7, 8);
static REPLICATE_8_BIT_TO_8_TABLE: [u32; 256] = make_replicate_table::<256>(8, 8);

static REPLICATE_1_BIT_TO_6_TABLE: [u32; 2] = make_replicate_table::<2>(1, 6);
static REPLICATE_2_BIT_TO_6_TABLE: [u32; 4] = make_replicate_table::<4>(2, 6);
static REPLICATE_3_BIT_TO_6_TABLE: [u32; 8] = make_replicate_table::<8>(3, 6);
static REPLICATE_4_BIT_TO_6_TABLE: [u32; 16] = make_replicate_table::<16>(4, 6);
static REPLICATE_5_BIT_TO_6_TABLE: [u32; 32] = make_replicate_table::<32>(5, 6);

fn replicate_byte_to_16(value: usize) -> u32 {
    REPLICATE_BYTE_TO_16_TABLE[value]
}

fn replicate_bit_to_7(value: usize) -> u32 {
    REPLICATE_BIT_TO_7_TABLE[value]
}

fn replicate_bit_to_9(value: usize) -> u32 {
    REPLICATE_BIT_TO_9_TABLE[value]
}

/// Port of `FastReplicateTo8`.
fn fast_replicate_to_8(value: u32, num_bits: u32) -> u32 {
    match num_bits {
        1 => REPLICATE_1_BIT_TO_8_TABLE[value as usize],
        2 => REPLICATE_2_BIT_TO_8_TABLE[value as usize],
        3 => REPLICATE_3_BIT_TO_8_TABLE[value as usize],
        4 => REPLICATE_4_BIT_TO_8_TABLE[value as usize],
        5 => REPLICATE_5_BIT_TO_8_TABLE[value as usize],
        6 => REPLICATE_6_BIT_TO_8_TABLE[value as usize],
        7 => REPLICATE_7_BIT_TO_8_TABLE[value as usize],
        8 => REPLICATE_8_BIT_TO_8_TABLE[value as usize],
        _ => replicate(value, num_bits, 8),
    }
}

/// Port of `FastReplicateTo6`.
fn fast_replicate_to_6(value: u32, num_bits: u32) -> u32 {
    match num_bits {
        1 => REPLICATE_1_BIT_TO_6_TABLE[value as usize],
        2 => REPLICATE_2_BIT_TO_6_TABLE[value as usize],
        3 => REPLICATE_3_BIT_TO_6_TABLE[value as usize],
        4 => REPLICATE_4_BIT_TO_6_TABLE[value as usize],
        5 => REPLICATE_5_BIT_TO_6_TABLE[value as usize],
        _ => replicate(value, num_bits, 6),
    }
}

// ── Pixel type ───────────────────────────────────────────────────────────────

/// Port of `Pixel` class from `astc.cpp`.
///
/// Color channels are stored as [A, R, G, B] with per-channel bit depth.
#[derive(Debug, Clone, Copy)]
struct Pixel {
    bit_depth: [u8; 4],
    color: [i16; 4],
}

impl Pixel {
    fn new_default() -> Self {
        Pixel {
            bit_depth: [8, 8, 8, 8],
            color: [0, 0, 0, 0],
        }
    }

    fn new(a: i32, r: i32, g: i32, b: i32) -> Self {
        Self::new_with_depth(a, r, g, b, 8)
    }

    fn new_with_depth(a: i32, r: i32, g: i32, b: i32, bit_depth: u8) -> Self {
        Pixel {
            bit_depth: [bit_depth, bit_depth, bit_depth, bit_depth],
            color: [a as i16, r as i16, g as i16, b as i16],
        }
    }

    /// Port of `Pixel::ConvertChannelToFloat` from `astc.cpp`.
    #[allow(dead_code)]
    fn convert_channel_to_float(channel: i16, bit_depth: u8) -> f32 {
        let denominator = ((1u32 << bit_depth) - 1) as f32;
        f32::from(channel) / denominator
    }

    fn a(&self) -> i16 {
        self.color[0]
    }
    fn r(&self) -> i16 {
        self.color[1]
    }
    fn g(&self) -> i16 {
        self.color[2]
    }
    fn b(&self) -> i16 {
        self.color[3]
    }
    fn set_a(&mut self, v: i16) {
        self.color[0] = v;
    }
    #[allow(dead_code)]
    fn set_r(&mut self, v: i16) {
        self.color[1] = v;
    }
    #[allow(dead_code)]
    fn set_g(&mut self, v: i16) {
        self.color[2] = v;
    }
    #[allow(dead_code)]
    fn set_b(&mut self, v: i16) {
        self.color[3] = v;
    }
    fn component(&self, idx: usize) -> i16 {
        self.color[idx]
    }
    fn set_component(&mut self, idx: usize, v: i16) {
        self.color[idx] = v;
    }

    /// Port of `Pixel::ChangeBitDepth()`.
    fn change_bit_depth(&mut self) {
        for i in 0..4 {
            self.color[i] = Self::change_bit_depth_single(self.color[i], self.bit_depth[i]);
            self.bit_depth[i] = 8;
        }
    }

    /// Port of `Pixel::ChangeBitDepth(ChannelType, u8)`.
    fn change_bit_depth_single(val: i16, old_depth: u8) -> i16 {
        debug_assert!(old_depth <= 8);
        if old_depth == 8 {
            val
        } else if old_depth == 0 {
            (1i16 << 8) - 1
        } else if 8 > old_depth {
            fast_replicate_to_8(val as u32, old_depth as u32) as i16
        } else {
            let bits_wasted = old_depth - 8;
            let value = ((val as u16) + (1 << (bits_wasted - 1))) >> bits_wasted;
            value.clamp(0, (1 << 8) - 1) as i16
        }
    }

    /// Port of `Pixel::GetBitDepth` from `astc.cpp`.
    #[allow(dead_code)]
    fn get_bit_depth(&self) -> [u8; 4] {
        self.bit_depth
    }

    /// Port of `Pixel::Pack()`.
    ///
    /// Takes all components, transforms them to 8-bit, then packs into
    /// R8G8B8A8 (little-endian: R in LSB, A in MSB).
    fn pack(&self) -> u32 {
        let mut eight_bit = *self;
        eight_bit.change_bit_depth();

        let mut r = 0u32;
        r |= eight_bit.a() as u32 & 0xFF;
        r <<= 8;
        r |= eight_bit.b() as u32 & 0xFF;
        r <<= 8;
        r |= eight_bit.g() as u32 & 0xFF;
        r <<= 8;
        r |= eight_bit.r() as u32 & 0xFF;
        r
    }

    /// Port of `Pixel::ClampByte()`.
    fn clamp_byte(&mut self) {
        for i in 0..4 {
            self.color[i] = self.color[i].max(0).min(255);
        }
    }

    #[allow(dead_code)]
    fn make_opaque(&mut self) {
        self.set_a(255);
    }
}

// ── TexelWeightParams ────────────────────────────────────────────────────────

/// Port of `TexelWeightParams` struct from `astc.cpp`.
#[derive(Debug, Clone, Copy, Default)]
struct TexelWeightParams {
    width: u32,
    height: u32,
    dual_plane: bool,
    max_weight: u32,
    error: bool,
    void_extent_ldr: bool,
    void_extent_hdr: bool,
}

impl TexelWeightParams {
    /// Port of `TexelWeightParams::GetPackedBitSize()`.
    fn get_packed_bit_size(&self) -> u32 {
        let mut n_idxs = self.height * self.width;
        if self.dual_plane {
            n_idxs *= 2;
        }
        ASTC_ENCODINGS_VALUES[self.max_weight as usize].get_bit_length(n_idxs)
    }

    /// Port of `TexelWeightParams::GetNumWeightValues()`.
    fn get_num_weight_values(&self) -> u32 {
        let mut ret = self.width * self.height;
        if self.dual_plane {
            ret *= 2;
        }
        ret
    }
}

/// Port of `DecodeBlockInfo` from `astc.cpp`.
fn decode_block_info(strm: &mut InputBitStream) -> TexelWeightParams {
    let mut params = TexelWeightParams::default();

    // Read the entire block mode all at once
    let mode_bits = strm.read_bits(11) as u16;

    // Does this match the void extent block mode?
    if (mode_bits & 0x01FF) == 0x1FC {
        if mode_bits & 0x200 != 0 {
            params.void_extent_hdr = true;
        } else {
            params.void_extent_ldr = true;
        }
        // Next two bits must be one.
        if (mode_bits & 0x400) == 0 || !strm.read_bit() {
            params.error = true;
        }
        return params;
    }

    // First check if the last four bits are zero
    if (mode_bits & 0xF) == 0 {
        params.error = true;
        return params;
    }

    // If the last two bits are zero, then if bits
    // [6-8] are all ones, this is also reserved.
    if (mode_bits & 0x3) == 0 && (mode_bits & 0x1C0) == 0x1C0 {
        params.error = true;
        return params;
    }

    // Determine layout (0-9) corresponding to table C.2.8
    let layout;
    if (mode_bits & 0x1) != 0 || (mode_bits & 0x2) != 0 {
        if mode_bits & 0x8 != 0 {
            if mode_bits & 0x4 != 0 {
                if mode_bits & 0x100 != 0 {
                    layout = 4;
                } else {
                    layout = 3;
                }
            } else {
                layout = 2;
            }
        } else if mode_bits & 0x4 != 0 {
            layout = 1;
        } else {
            layout = 0;
        }
    } else {
        if mode_bits & 0x100 != 0 {
            if mode_bits & 0x80 != 0 {
                debug_assert!((mode_bits & 0x40) == 0);
                if mode_bits & 0x20 != 0 {
                    layout = 8;
                } else {
                    layout = 7;
                }
            } else {
                layout = 9;
            }
        } else if mode_bits & 0x80 != 0 {
            layout = 6;
        } else {
            layout = 5;
        }
    }

    debug_assert!(layout < 10);

    // Determine R
    let mut big_r: u32 = if mode_bits & 0x10 != 0 { 1 } else { 0 };
    if layout < 5 {
        big_r |= ((mode_bits & 0x3) as u32) << 1;
    } else {
        big_r |= ((mode_bits & 0xC) >> 1) as u32;
    }
    debug_assert!((2..=7).contains(&big_r));

    // Determine width & height
    match layout {
        0 => {
            let a = ((mode_bits >> 5) & 0x3) as u32;
            let b = ((mode_bits >> 7) & 0x3) as u32;
            params.width = b + 4;
            params.height = a + 2;
        }
        1 => {
            let a = ((mode_bits >> 5) & 0x3) as u32;
            let b = ((mode_bits >> 7) & 0x3) as u32;
            params.width = b + 8;
            params.height = a + 2;
        }
        2 => {
            let a = ((mode_bits >> 5) & 0x3) as u32;
            let b = ((mode_bits >> 7) & 0x3) as u32;
            params.width = a + 2;
            params.height = b + 8;
        }
        3 => {
            let a = ((mode_bits >> 5) & 0x3) as u32;
            let b = ((mode_bits >> 7) & 0x1) as u32;
            params.width = a + 2;
            params.height = b + 6;
        }
        4 => {
            let a = ((mode_bits >> 5) & 0x3) as u32;
            let b = ((mode_bits >> 7) & 0x1) as u32;
            params.width = b + 2;
            params.height = a + 2;
        }
        5 => {
            let a = ((mode_bits >> 5) & 0x3) as u32;
            params.width = 12;
            params.height = a + 2;
        }
        6 => {
            let a = ((mode_bits >> 5) & 0x3) as u32;
            params.width = a + 2;
            params.height = 12;
        }
        7 => {
            params.width = 6;
            params.height = 10;
        }
        8 => {
            params.width = 10;
            params.height = 6;
        }
        9 => {
            let a = ((mode_bits >> 5) & 0x3) as u32;
            let b = ((mode_bits >> 9) & 0x3) as u32;
            params.width = a + 6;
            params.height = b + 6;
        }
        _ => {
            debug_assert!(false, "Don't know this layout...");
            params.error = true;
        }
    }

    // Determine whether or not we're using dual planes and/or high precision layouts.
    let d = layout != 9 && (mode_bits & 0x400) != 0;
    let h = layout != 9 && (mode_bits & 0x200) != 0;

    if h {
        const MAX_WEIGHTS_H: [u32; 6] = [9, 11, 15, 19, 23, 31];
        params.max_weight = MAX_WEIGHTS_H[(big_r - 2) as usize];
    } else {
        const MAX_WEIGHTS: [u32; 6] = [1, 2, 3, 4, 5, 7];
        params.max_weight = MAX_WEIGHTS[(big_r - 2) as usize];
    }

    params.dual_plane = d;

    params
}

// ── Color value decoding ─────────────────────────────────────────────────────

/// Port of `DecodeColorValues` from `astc.cpp`.
fn decode_color_values(
    out: &mut [u32],
    data: &[u8],
    modes: &[u32],
    n_partitions: u32,
    n_bits_for_color_data: u32,
) {
    // First figure out how many color values we have
    let mut n_values = 0u32;
    for i in 0..n_partitions as usize {
        n_values += ((modes[i] >> 2) + 1) << 1;
    }

    // Then based on the number of values and the remaining number of bits,
    // figure out the max value for each of them...
    let mut range = 256u32;
    while range > 0 {
        range -= 1;
        let val = ASTC_ENCODINGS_VALUES[range as usize];
        let bit_length = val.get_bit_length(n_values);
        if bit_length <= n_bits_for_color_data {
            // Find the smallest possible range that matches the given encoding
            while range > 0 {
                range -= 1;
                let newval = ASTC_ENCODINGS_VALUES[range as usize];
                if !newval.matches_encoding(&val) {
                    break;
                }
            }
            // Return to last matching range.
            range += 1;
            break;
        }
    }

    // We now have enough to decode our integer sequence.
    let mut decoded_color_values = IntegerEncodedVector::new();
    let mut color_stream = InputBitStream::new(data, 0);
    decode_integer_sequence(
        &mut decoded_color_values,
        &mut color_stream,
        range,
        n_values,
    );

    // Once we have the decoded values, we need to dequantize them to the 0-255 range
    // This procedure is outlined in ASTC spec C.2.13
    let mut out_idx = 0usize;
    for val in &decoded_color_values {
        if out_idx >= n_values as usize {
            break;
        }

        let bitlen = val.num_bits;
        let bitval = val.bit_value;

        debug_assert!(bitlen >= 1);

        // A is just the lsb replicated 9 times.
        let a = replicate_bit_to_9((bitval & 1) as usize);

        match val.encoding {
            IntegerEncoding::JustBits => {
                out[out_idx] = fast_replicate_to_8(bitval, bitlen);
                out_idx += 1;
            }
            IntegerEncoding::Trit => {
                let d = val.quint_trit_value;
                let (b, c) = match bitlen {
                    1 => (0u32, 204u32),
                    2 => {
                        let b_bit = (bitval >> 1) & 1;
                        (
                            (b_bit << 8) | (b_bit << 4) | (b_bit << 2) | (b_bit << 1),
                            93,
                        )
                    }
                    3 => {
                        let cb = (bitval >> 1) & 3;
                        ((cb << 7) | (cb << 2) | cb, 44)
                    }
                    4 => {
                        let dcb = (bitval >> 1) & 7;
                        ((dcb << 6) | dcb, 22)
                    }
                    5 => {
                        let edcb = (bitval >> 1) & 0xF;
                        ((edcb << 5) | (edcb >> 2), 11)
                    }
                    6 => {
                        let fedcb = (bitval >> 1) & 0x1F;
                        ((fedcb << 4) | (fedcb >> 4), 5)
                    }
                    _ => {
                        debug_assert!(false, "Unsupported trit encoding for color values!");
                        (0, 0)
                    }
                };
                let mut t = d * c + b;
                t ^= a;
                t = (a & 0x80) | (t >> 2);
                out[out_idx] = t;
                out_idx += 1;
            }
            IntegerEncoding::Quint => {
                let d = val.quint_trit_value;
                let (b, c) = match bitlen {
                    1 => (0u32, 113u32),
                    2 => {
                        let b_bit = (bitval >> 1) & 1;
                        ((b_bit << 8) | (b_bit << 3) | (b_bit << 2), 54)
                    }
                    3 => {
                        let cb = (bitval >> 1) & 3;
                        ((cb << 7) | (cb << 1) | (cb >> 1), 26)
                    }
                    4 => {
                        let dcb = (bitval >> 1) & 7;
                        ((dcb << 6) | (dcb >> 1), 13)
                    }
                    5 => {
                        let edcb = (bitval >> 1) & 0xF;
                        ((edcb << 5) | (edcb >> 3), 6)
                    }
                    _ => {
                        debug_assert!(false, "Unsupported quint encoding for color values!");
                        (0, 0)
                    }
                };
                let mut t = d * c + b;
                t ^= a;
                t = (a & 0x80) | (t >> 2);
                out[out_idx] = t;
                out_idx += 1;
            }
        }
    }

    // Make sure that each of our values is in the proper range...
    for i in 0..n_values as usize {
        debug_assert!(out[i] <= 255);
    }
}

// ── Texel weight unquantization ──────────────────────────────────────────────

/// Port of `UnquantizeTexelWeight` from `astc.cpp`.
fn unquantize_texel_weight(val: &IntegerEncodedValue) -> u32 {
    let bitval = val.bit_value;
    let bitlen = val.num_bits;

    let a = replicate_bit_to_7((bitval & 1) as usize);

    let mut result = 0u32;
    match val.encoding {
        IntegerEncoding::JustBits => {
            result = fast_replicate_to_6(bitval, bitlen);
        }
        IntegerEncoding::Trit => {
            let d = val.quint_trit_value;
            debug_assert!(d < 3);

            match bitlen {
                0 => {
                    let results = [0u32, 32, 63];
                    result = results[d as usize];
                }
                1 => {
                    let c = 50u32;
                    result = d * c;
                    result ^= a;
                    result = (a & 0x20) | (result >> 2);
                }
                2 => {
                    let c = 23u32;
                    let b_bit = (bitval >> 1) & 1;
                    let b = (b_bit << 6) | (b_bit << 2) | b_bit;
                    result = d * c + b;
                    result ^= a;
                    result = (a & 0x20) | (result >> 2);
                }
                3 => {
                    let c = 11u32;
                    let cb = (bitval >> 1) & 3;
                    let b = (cb << 5) | cb;
                    result = d * c + b;
                    result ^= a;
                    result = (a & 0x20) | (result >> 2);
                }
                _ => {
                    debug_assert!(false, "Invalid trit encoding for texel weight");
                }
            }
        }
        IntegerEncoding::Quint => {
            let d = val.quint_trit_value;
            debug_assert!(d < 5);

            match bitlen {
                0 => {
                    let results = [0u32, 16, 32, 47, 63];
                    result = results[d as usize];
                }
                1 => {
                    let c = 28u32;
                    result = d * c;
                    result ^= a;
                    result = (a & 0x20) | (result >> 2);
                }
                2 => {
                    let c = 13u32;
                    let b_bit = (bitval >> 1) & 1;
                    let b = (b_bit << 6) | (b_bit << 1);
                    result = d * c + b;
                    result ^= a;
                    result = (a & 0x20) | (result >> 2);
                }
                _ => {
                    debug_assert!(false, "Invalid quint encoding for texel weight");
                }
            }
        }
    }

    debug_assert!(result < 64);

    // Change from [0,63] to [0,64]
    if result > 32 {
        result += 1;
    }

    result
}

/// Port of `UnquantizeTexelWeights` from `astc.cpp`.
fn unquantize_texel_weights(
    out: &mut [[u32; 144]; 2],
    weights: &IntegerEncodedVector,
    params: &TexelWeightParams,
    block_width: u32,
    block_height: u32,
) {
    let mut weight_idx = 0usize;
    let mut unquantized = [[0u32; 144]; 2];

    let mut itr = weights.iter();
    loop {
        let w = match itr.next() {
            Some(w) => w,
            None => break,
        };
        unquantized[0][weight_idx] = unquantize_texel_weight(w);

        if params.dual_plane {
            match itr.next() {
                Some(w2) => {
                    unquantized[1][weight_idx] = unquantize_texel_weight(w2);
                }
                None => break,
            }
        }

        weight_idx += 1;
        if weight_idx >= (params.width * params.height) as usize {
            break;
        }
    }

    // Do infill if necessary (Section C.2.18) ...
    let ds = (1024 + (block_width / 2)) / (block_width - 1);
    let dt = (1024 + (block_height / 2)) / (block_height - 1);

    let plane_scale = if params.dual_plane { 2u32 } else { 1u32 };
    for plane in 0..plane_scale as usize {
        for t in 0..block_height {
            for s in 0..block_width {
                let cs = ds * s;
                let ct = dt * t;

                let gs = (cs * (params.width - 1) + 32) >> 6;
                let gt = (ct * (params.height - 1) + 32) >> 6;

                let js = gs >> 4;
                let fs = gs & 0xF;

                let jt = gt >> 4;
                let ft = gt & 0x0F;

                let w11 = (fs * ft + 8) >> 4;
                let w10 = ft - w11;
                let w01 = fs - w11;
                let w00 = 16 - fs - ft + w11;

                let v0 = (js + jt * params.width) as usize;

                let find_texel = |tidx: usize| -> u32 {
                    if tidx < (params.width * params.height) as usize {
                        unquantized[plane][tidx]
                    } else {
                        0
                    }
                };

                let p00 = find_texel(v0);
                let p01 = find_texel(v0 + 1);
                let p10 = find_texel(v0 + params.width as usize);
                let p11 = find_texel(v0 + params.width as usize + 1);

                out[plane][(t * block_width + s) as usize] =
                    (p00 * w00 + p01 * w01 + p10 * w10 + p11 * w11 + 8) >> 4;
            }
        }
    }
}

// ── Helper functions ─────────────────────────────────────────────────────────

/// Port of `BitTransferSigned` from `astc.cpp` (C.2.14).
/// Returns (new_a, new_b) to avoid mutable aliasing issues with Rust's borrow checker.
fn bit_transfer_signed(a: i32, b: i32) -> (i32, i32) {
    let mut new_b = b >> 1;
    new_b |= a & 0x80;
    let mut new_a = a >> 1;
    new_a &= 0x3F;
    if new_a & 0x20 != 0 {
        new_a -= 0x40;
    }
    (new_a, new_b)
}

/// Port of `BlueContract` from `astc.cpp` (C.2.14).
fn blue_contract(a: i32, r: i32, g: i32, b: i32) -> Pixel {
    Pixel::new(
        (a) as i32,
        ((r + b) >> 1) as i32,
        ((g + b) >> 1) as i32,
        b as i32,
    )
}

/// Port of `hash52` from `astc.cpp` (C.2.21).
fn hash52(mut p: u32) -> u32 {
    p ^= p >> 15;
    p = p.wrapping_sub(p << 17);
    p = p.wrapping_add(p << 7);
    p = p.wrapping_add(p << 4);
    p ^= p >> 5;
    p = p.wrapping_add(p << 16);
    p ^= p >> 7;
    p ^= p >> 3;
    p ^= p << 6;
    p ^= p >> 17;
    p
}

/// Port of `SelectPartition` from `astc.cpp` (C.2.21).
fn select_partition(
    seed: i32,
    mut x: i32,
    mut y: i32,
    mut z: i32,
    partition_count: i32,
    small_block: bool,
) -> u32 {
    if partition_count == 1 {
        return 0;
    }

    if small_block {
        x <<= 1;
        y <<= 1;
        z <<= 1;
    }

    let seed = seed + (partition_count - 1) * 1024;
    let rnum = hash52(seed as u32);

    let mut seed1 = (rnum & 0xF) as u8;
    let mut seed2 = ((rnum >> 4) & 0xF) as u8;
    let mut seed3 = ((rnum >> 8) & 0xF) as u8;
    let mut seed4 = ((rnum >> 12) & 0xF) as u8;
    let mut seed5 = ((rnum >> 16) & 0xF) as u8;
    let mut seed6 = ((rnum >> 20) & 0xF) as u8;
    let mut seed7 = ((rnum >> 24) & 0xF) as u8;
    let mut seed8 = ((rnum >> 28) & 0xF) as u8;
    let mut seed9 = ((rnum >> 18) & 0xF) as u8;
    let mut seed10 = ((rnum >> 22) & 0xF) as u8;
    let mut seed11 = ((rnum >> 26) & 0xF) as u8;
    let mut seed12 = (((rnum >> 30) | (rnum << 2)) & 0xF) as u8;

    seed1 = seed1.wrapping_mul(seed1);
    seed2 = seed2.wrapping_mul(seed2);
    seed3 = seed3.wrapping_mul(seed3);
    seed4 = seed4.wrapping_mul(seed4);
    seed5 = seed5.wrapping_mul(seed5);
    seed6 = seed6.wrapping_mul(seed6);
    seed7 = seed7.wrapping_mul(seed7);
    seed8 = seed8.wrapping_mul(seed8);
    seed9 = seed9.wrapping_mul(seed9);
    seed10 = seed10.wrapping_mul(seed10);
    seed11 = seed11.wrapping_mul(seed11);
    seed12 = seed12.wrapping_mul(seed12);

    let (sh1, sh2): (i32, i32);
    if seed & 1 != 0 {
        sh1 = if seed & 2 != 0 { 4 } else { 5 };
        sh2 = if partition_count == 3 { 6 } else { 5 };
    } else {
        sh1 = if partition_count == 3 { 6 } else { 5 };
        sh2 = if seed & 2 != 0 { 4 } else { 5 };
    }
    let sh3 = if seed & 0x10 != 0 { sh1 } else { sh2 };

    seed1 >>= sh1 as u32;
    seed2 >>= sh2 as u32;
    seed3 >>= sh1 as u32;
    seed4 >>= sh2 as u32;
    seed5 >>= sh1 as u32;
    seed6 >>= sh2 as u32;
    seed7 >>= sh1 as u32;
    seed8 >>= sh2 as u32;
    seed9 >>= sh3 as u32;
    seed10 >>= sh3 as u32;
    seed11 >>= sh3 as u32;
    seed12 >>= sh3 as u32;

    let a = (seed1 as i32) * x + (seed2 as i32) * y + (seed11 as i32) * z + ((rnum >> 14) as i32);
    let b = (seed3 as i32) * x + (seed4 as i32) * y + (seed12 as i32) * z + ((rnum >> 10) as i32);
    let c = (seed5 as i32) * x + (seed6 as i32) * y + (seed9 as i32) * z + ((rnum >> 6) as i32);
    let d = (seed7 as i32) * x + (seed8 as i32) * y + (seed10 as i32) * z + ((rnum >> 2) as i32);

    let a = a & 0x3F;
    let b = b & 0x3F;
    let mut c = c & 0x3F;
    let mut d = d & 0x3F;

    if partition_count < 4 {
        d = 0;
    }
    if partition_count < 3 {
        c = 0;
    }

    if a >= b && a >= c && a >= d {
        0
    } else if b >= c && b >= d {
        1
    } else if c >= d {
        2
    } else {
        3
    }
}

/// Port of `Select2DPartition` from `astc.cpp`.
fn select_2d_partition(seed: i32, x: i32, y: i32, partition_count: i32, small_block: bool) -> u32 {
    select_partition(seed, x, y, 0, partition_count, small_block)
}

// ── Endpoint computation ─────────────────────────────────────────────────────

/// Port of `IsHDRColorEndpointMode` from `astc.cpp`.
const fn is_hdr_color_endpoint_mode(cem: u32) -> bool {
    matches!(cem, 2 | 3 | 7 | 11 | 14 | 15)
}

/// Port of `SignExtend` from `astc.cpp`.
const fn sign_extend(value: i32, nbits: u32) -> i32 {
    let sign_bit = 1 << (nbits - 1);
    (value ^ sign_bit) - sign_bit
}

/// Port of `HDREndpointRGB` from `astc.cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HdrEndpointRgb {
    r0: i32,
    g0: i32,
    b0: i32,
    r1: i32,
    g1: i32,
    b1: i32,
}

/// Port of `DecodeHDREndpointMode7` from `astc.cpp`.
fn decode_hdr_endpoint_mode_7(v0: u32, v1: u32, v2: u32, v3: u32) -> HdrEndpointRgb {
    let modeval = ((v0 & 0xC0) >> 6) | ((v1 & 0x80) >> 5) | ((v2 & 0x80) >> 4);

    let (majcomp, mode) = if (modeval & 0xC) != 0xC {
        (modeval >> 2, modeval & 3)
    } else if modeval != 0xF {
        (modeval & 3, 4)
    } else {
        (0, 5)
    };

    let mut red = (v0 & 0x3F) as i32;
    let mut green = (v1 & 0x1F) as i32;
    let mut blue = (v2 & 0x1F) as i32;
    let mut scale = (v3 & 0x1F) as i32;

    let x0 = (v1 >> 6) & 1;
    let x1 = (v1 >> 5) & 1;
    let x2 = (v2 >> 6) & 1;
    let x3 = (v2 >> 5) & 1;
    let x4 = (v3 >> 7) & 1;
    let x5 = (v3 >> 6) & 1;
    let x6 = (v3 >> 5) & 1;

    let ohm = 1u32 << mode;
    if ohm & 0x30 != 0 {
        green |= (x0 << 6) as i32;
    }
    if ohm & 0x3A != 0 {
        green |= (x1 << 5) as i32;
    }
    if ohm & 0x30 != 0 {
        blue |= (x2 << 6) as i32;
    }
    if ohm & 0x3A != 0 {
        blue |= (x3 << 5) as i32;
    }
    if ohm & 0x3D != 0 {
        scale |= (x6 << 5) as i32;
    }
    if ohm & 0x2D != 0 {
        scale |= (x5 << 6) as i32;
    }
    if ohm & 0x04 != 0 {
        scale |= (x4 << 7) as i32;
    }
    if ohm & 0x3B != 0 {
        red |= (x4 << 6) as i32;
    }
    if ohm & 0x04 != 0 {
        red |= (x3 << 6) as i32;
    }
    if ohm & 0x10 != 0 {
        red |= (x5 << 7) as i32;
    }
    if ohm & 0x0F != 0 {
        red |= (x2 << 7) as i32;
    }
    if ohm & 0x05 != 0 {
        red |= (x1 << 8) as i32;
    }
    if ohm & 0x0A != 0 {
        red |= (x0 << 8) as i32;
    }
    if ohm & 0x05 != 0 {
        red |= (x0 << 9) as i32;
    }
    if ohm & 0x02 != 0 {
        red |= (x6 << 9) as i32;
    }
    if ohm & 0x01 != 0 {
        red |= (x3 << 10) as i32;
    }
    if ohm & 0x02 != 0 {
        red |= (x5 << 10) as i32;
    }

    const SHAMTS: [u32; 6] = [1, 1, 2, 3, 4, 5];
    let shamt = SHAMTS[mode as usize];
    red <<= shamt;
    green <<= shamt;
    blue <<= shamt;
    scale <<= shamt;

    if mode != 5 {
        green = red - green;
        blue = red - blue;
    }

    if majcomp == 1 {
        std::mem::swap(&mut red, &mut green);
    }
    if majcomp == 2 {
        std::mem::swap(&mut red, &mut blue);
    }

    HdrEndpointRgb {
        r0: (red - scale).clamp(0, 0xFFF),
        g0: (green - scale).clamp(0, 0xFFF),
        b0: (blue - scale).clamp(0, 0xFFF),
        r1: red.clamp(0, 0xFFF),
        g1: green.clamp(0, 0xFFF),
        b1: blue.clamp(0, 0xFFF),
    }
}

/// Port of `DecodeHDREndpointMode11` from `astc.cpp`.
fn decode_hdr_endpoint_mode_11(
    v0: u32,
    v1: u32,
    v2: u32,
    v3: u32,
    v4: u32,
    v5: u32,
) -> HdrEndpointRgb {
    let majcomp = ((v4 & 0x80) >> 7) | ((v5 & 0x80) >> 6);
    if majcomp == 3 {
        return HdrEndpointRgb {
            r0: (v0 << 4) as i32,
            g0: (v2 << 4) as i32,
            b0: ((v4 & 0x7F) << 5) as i32,
            r1: (v1 << 4) as i32,
            g1: (v3 << 4) as i32,
            b1: ((v5 & 0x7F) << 5) as i32,
        };
    }

    let mode = ((v1 & 0x80) >> 7) | ((v2 & 0x80) >> 6) | ((v3 & 0x80) >> 5);
    let mut va = (v0 | ((v1 & 0x40) << 2)) as i32;
    let mut vb0 = (v2 & 0x3F) as i32;
    let mut vb1 = (v3 & 0x3F) as i32;
    let mut vc = (v1 & 0x3F) as i32;
    let mut vd0 = (v4 & 0x7F) as i32;
    let mut vd1 = (v5 & 0x7F) as i32;

    const DBITSTAB: [u32; 8] = [7, 6, 7, 6, 5, 6, 5, 6];
    vd0 = sign_extend(vd0, DBITSTAB[mode as usize]);
    vd1 = sign_extend(vd1, DBITSTAB[mode as usize]);

    let x0 = (v2 >> 6) & 1;
    let x1 = (v3 >> 6) & 1;
    let x2 = (v4 >> 6) & 1;
    let x3 = (v5 >> 6) & 1;
    let x4 = (v4 >> 5) & 1;
    let x5 = (v5 >> 5) & 1;

    let ohm = 1u32 << mode;
    if ohm & 0xA4 != 0 {
        va |= (x0 << 9) as i32;
    }
    if ohm & 0x08 != 0 {
        va |= (x2 << 9) as i32;
    }
    if ohm & 0x50 != 0 {
        va |= (x4 << 9) as i32;
    }
    if ohm & 0x50 != 0 {
        va |= (x5 << 10) as i32;
    }
    if ohm & 0xA0 != 0 {
        va |= (x1 << 10) as i32;
    }
    if ohm & 0xC0 != 0 {
        va |= (x2 << 11) as i32;
    }
    if ohm & 0x04 != 0 {
        vc |= (x1 << 6) as i32;
    }
    if ohm & 0xE8 != 0 {
        vc |= (x3 << 6) as i32;
    }
    if ohm & 0x20 != 0 {
        vc |= (x2 << 7) as i32;
    }
    if ohm & 0x5B != 0 {
        vb0 |= (x0 << 6) as i32;
        vb1 |= (x1 << 6) as i32;
    }
    if ohm & 0x12 != 0 {
        vb0 |= (x2 << 7) as i32;
        vb1 |= (x3 << 7) as i32;
    }

    let shamt = (mode as i32 >> 1) ^ 3;
    va <<= shamt;
    vb0 <<= shamt;
    vb1 <<= shamt;
    vc <<= shamt;
    vd0 <<= shamt;
    vd1 <<= shamt;

    let mut result = HdrEndpointRgb {
        r0: (va - vc).clamp(0, 0xFFF),
        g0: (va - vb0 - vc - vd0).clamp(0, 0xFFF),
        b0: (va - vb1 - vc - vd1).clamp(0, 0xFFF),
        r1: va.clamp(0, 0xFFF),
        g1: (va - vb0).clamp(0, 0xFFF),
        b1: (va - vb1).clamp(0, 0xFFF),
    };

    if majcomp == 1 {
        std::mem::swap(&mut result.r0, &mut result.g0);
        std::mem::swap(&mut result.r1, &mut result.g1);
    } else if majcomp == 2 {
        std::mem::swap(&mut result.r0, &mut result.b0);
        std::mem::swap(&mut result.r1, &mut result.b1);
    }
    result
}

/// Port of `ComputeEndpoints` from `astc.cpp` (C.2.14).
/// Returns (ep1, ep2) to avoid double-mutable-borrow issues with Rust's borrow checker.
fn compute_endpoints(
    color_values: &[u32],
    idx: &mut usize,
    color_endpoint_mode: u32,
) -> (Pixel, Pixel) {
    let mut ep1 = Pixel::new_default();
    let mut ep2 = Pixel::new_default();
    // Helper: read N u32 values from color_values starting at *idx, advance idx
    macro_rules! read_uint_values {
        ($n:expr) => {{
            let mut v = [0u32; $n];
            for i in 0..$n {
                v[i] = color_values[*idx];
                *idx += 1;
            }
            v
        }};
    }

    macro_rules! read_int_values {
        ($n:expr) => {{
            let mut v = [0i32; $n];
            for i in 0..$n {
                v[i] = color_values[*idx] as i32;
                *idx += 1;
            }
            v
        }};
    }

    match color_endpoint_mode {
        0 => {
            let v = read_uint_values!(2);
            ep1 = Pixel::new(0xFF, v[0] as i32, v[0] as i32, v[0] as i32);
            ep2 = Pixel::new(0xFF, v[1] as i32, v[1] as i32, v[1] as i32);
        }
        1 => {
            let v = read_uint_values!(2);
            let l0 = (v[0] >> 2) | (v[1] & 0xC0);
            let l1 = (l0 + (v[1] & 0x3F)).min(0xFF);
            ep1 = Pixel::new(0xFF, l0 as i32, l0 as i32, l0 as i32);
            ep2 = Pixel::new(0xFF, l1 as i32, l1 as i32, l1 as i32);
        }
        4 => {
            let v = read_uint_values!(4);
            ep1 = Pixel::new(v[2] as i32, v[0] as i32, v[0] as i32, v[0] as i32);
            ep2 = Pixel::new(v[3] as i32, v[1] as i32, v[1] as i32, v[1] as i32);
        }
        5 => {
            let mut v = read_int_values!(4);
            let (a, b) = bit_transfer_signed(v[1], v[0]);
            v[1] = a;
            v[0] = b;
            let (a, b) = bit_transfer_signed(v[3], v[2]);
            v[3] = a;
            v[2] = b;
            ep1 = Pixel::new(v[2], v[0], v[0], v[0]);
            ep2 = Pixel::new(v[2] + v[3], v[0] + v[1], v[0] + v[1], v[0] + v[1]);
            ep1.clamp_byte();
            ep2.clamp_byte();
        }
        6 => {
            let v = read_uint_values!(4);
            ep1 = Pixel::new(
                0xFF,
                (v[0] * v[3] >> 8) as i32,
                (v[1] * v[3] >> 8) as i32,
                (v[2] * v[3] >> 8) as i32,
            );
            ep2 = Pixel::new(0xFF, v[0] as i32, v[1] as i32, v[2] as i32);
        }
        8 => {
            let v = read_uint_values!(6);
            if v[1] + v[3] + v[5] >= v[0] + v[2] + v[4] {
                ep1 = Pixel::new(0xFF, v[0] as i32, v[2] as i32, v[4] as i32);
                ep2 = Pixel::new(0xFF, v[1] as i32, v[3] as i32, v[5] as i32);
            } else {
                ep1 = blue_contract(0xFF, v[1] as i32, v[3] as i32, v[5] as i32);
                ep2 = blue_contract(0xFF, v[0] as i32, v[2] as i32, v[4] as i32);
            }
        }
        9 => {
            let mut v = read_int_values!(6);
            let (a, b) = bit_transfer_signed(v[1], v[0]);
            v[1] = a;
            v[0] = b;
            let (a, b) = bit_transfer_signed(v[3], v[2]);
            v[3] = a;
            v[2] = b;
            let (a, b) = bit_transfer_signed(v[5], v[4]);
            v[5] = a;
            v[4] = b;
            if v[1] + v[3] + v[5] >= 0 {
                ep1 = Pixel::new(0xFF, v[0], v[2], v[4]);
                ep2 = Pixel::new(0xFF, v[0] + v[1], v[2] + v[3], v[4] + v[5]);
            } else {
                ep1 = blue_contract(0xFF, v[0] + v[1], v[2] + v[3], v[4] + v[5]);
                ep2 = blue_contract(0xFF, v[0], v[2], v[4]);
            }
            ep1.clamp_byte();
            ep2.clamp_byte();
        }
        10 => {
            let v = read_uint_values!(6);
            ep1 = Pixel::new(
                v[4] as i32,
                (v[0] * v[3] >> 8) as i32,
                (v[1] * v[3] >> 8) as i32,
                (v[2] * v[3] >> 8) as i32,
            );
            ep2 = Pixel::new(v[5] as i32, v[0] as i32, v[1] as i32, v[2] as i32);
        }
        12 => {
            let v = read_uint_values!(8);
            if v[1] + v[3] + v[5] >= v[0] + v[2] + v[4] {
                ep1 = Pixel::new(v[6] as i32, v[0] as i32, v[2] as i32, v[4] as i32);
                ep2 = Pixel::new(v[7] as i32, v[1] as i32, v[3] as i32, v[5] as i32);
            } else {
                ep1 = blue_contract(v[7] as i32, v[1] as i32, v[3] as i32, v[5] as i32);
                ep2 = blue_contract(v[6] as i32, v[0] as i32, v[2] as i32, v[4] as i32);
            }
        }
        13 => {
            let mut v = read_int_values!(8);
            let (a, b) = bit_transfer_signed(v[1], v[0]);
            v[1] = a;
            v[0] = b;
            let (a, b) = bit_transfer_signed(v[3], v[2]);
            v[3] = a;
            v[2] = b;
            let (a, b) = bit_transfer_signed(v[5], v[4]);
            v[5] = a;
            v[4] = b;
            let (a, b) = bit_transfer_signed(v[7], v[6]);
            v[7] = a;
            v[6] = b;
            if v[1] + v[3] + v[5] >= 0 {
                ep1 = Pixel::new(v[6], v[0], v[2], v[4]);
                ep2 = Pixel::new(v[7] + v[6], v[0] + v[1], v[2] + v[3], v[4] + v[5]);
            } else {
                ep1 = blue_contract(v[6] + v[7], v[0] + v[1], v[2] + v[3], v[4] + v[5]);
                ep2 = blue_contract(v[6], v[0], v[2], v[4]);
            }
            ep1.clamp_byte();
            ep2.clamp_byte();
        }
        2 => {
            let v = read_uint_values!(2);
            let (y0, y1) = if v[1] >= v[0] {
                (v[0] << 4, v[1] << 4)
            } else {
                ((v[1] << 4) + 8, (v[0] << 4) - 8)
            };
            ep1 = Pixel::new(0x780, y0 as i32, y0 as i32, y0 as i32);
            ep2 = Pixel::new(0x780, y1 as i32, y1 as i32, y1 as i32);
        }
        3 => {
            let v = read_uint_values!(2);
            let (y0, d) = if v[0] & 0x80 != 0 {
                (
                    ((v[1] & 0xE0) << 4) | ((v[0] & 0x7F) << 2),
                    (v[1] & 0x1F) << 2,
                )
            } else {
                (
                    ((v[1] & 0xF0) << 4) | ((v[0] & 0x7F) << 1),
                    (v[1] & 0x0F) << 1,
                )
            };
            let y1 = (y0 + d).min(0xFFF);
            ep1 = Pixel::new(0x780, y0 as i32, y0 as i32, y0 as i32);
            ep2 = Pixel::new(0x780, y1 as i32, y1 as i32, y1 as i32);
        }
        7 => {
            let v = read_uint_values!(4);
            let rgb = decode_hdr_endpoint_mode_7(v[0], v[1], v[2], v[3]);
            ep1 = Pixel::new(0x780, rgb.r0, rgb.g0, rgb.b0);
            ep2 = Pixel::new(0x780, rgb.r1, rgb.g1, rgb.b1);
        }
        11 => {
            let v = read_uint_values!(6);
            let rgb = decode_hdr_endpoint_mode_11(v[0], v[1], v[2], v[3], v[4], v[5]);
            ep1 = Pixel::new(0x780, rgb.r0, rgb.g0, rgb.b0);
            ep2 = Pixel::new(0x780, rgb.r1, rgb.g1, rgb.b1);
        }
        14 => {
            let v = read_uint_values!(8);
            let rgb = decode_hdr_endpoint_mode_11(v[0], v[1], v[2], v[3], v[4], v[5]);
            ep1 = Pixel::new(v[6] as i32, rgb.r0, rgb.g0, rgb.b0);
            ep2 = Pixel::new(v[7] as i32, rgb.r1, rgb.g1, rgb.b1);
        }
        15 => {
            let v = read_uint_values!(8);
            let rgb = decode_hdr_endpoint_mode_11(v[0], v[1], v[2], v[3], v[4], v[5]);
            let mode = ((v[6] >> 7) & 1) | ((v[7] >> 6) & 2);
            let mut a6 = (v[6] & 0x7F) as i32;
            let mut a7 = (v[7] & 0x7F) as i32;
            let (alpha0, alpha1) = if mode == 3 {
                (a6 << 5, a7 << 5)
            } else {
                a6 |= (a7 << (mode + 1)) & 0x780;
                a7 &= 0x3F >> mode;
                a7 ^= 0x20 >> mode;
                a7 -= 0x20 >> mode;
                a6 <<= 4 - mode;
                a7 <<= 4 - mode;
                a7 += a6;
                (a6, a7.clamp(0, 0xFFF))
            };
            ep1 = Pixel::new(alpha0, rgb.r0, rgb.g0, rgb.b0);
            ep2 = Pixel::new(alpha1, rgb.r1, rgb.g1, rgb.b1);
        }
        _ => {
            debug_assert!(false, "Unsupported color endpoint mode");
        }
    }

    (ep1, ep2)
}

// ── Block decompression ──────────────────────────────────────────────────────

/// Port of `FillVoidExtentLDR` from `astc.cpp`.
fn fill_void_extent_ldr(
    strm: &mut InputBitStream,
    out_buf: &mut [u32],
    block_width: u32,
    block_height: u32,
) {
    // Don't actually care about the void extent, just read the bits...
    for _ in 0..4 {
        strm.read_bits(13);
    }

    // Decode the RGBA components and renormalize them to the range [0, 255]
    let r = strm.read_bits(16) as u16;
    let g = strm.read_bits(16) as u16;
    let b = strm.read_bits(16) as u16;
    let a = strm.read_bits(16) as u16;

    let rgba = (r >> 8) as u32
        | ((g & 0xFF00) as u32)
        | (((b & 0xFF00) as u32) << 8)
        | (((a & 0xFF00) as u32) << 16);

    for j in 0..block_height {
        for i in 0..block_width {
            out_buf[(j * block_width + i) as usize] = rgba;
        }
    }
}

/// Port of `HalfToFloat` from `astc.cpp`.
fn half_to_float(h: u16) -> f32 {
    let sign = u32::from(h & 0x8000) << 16;
    let exp = (h & 0x7C00) >> 10;
    let mut mant = u32::from(h & 0x03FF);
    let bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            let mut e = 127 - 15 + 1;
            while mant & 0x400 == 0 {
                mant <<= 1;
                e -= 1;
            }
            mant &= 0x3FF;
            sign | ((e as u32) << 23) | (mant << 13)
        }
    } else if exp == 0x1F {
        sign | 0x7F80_0000 | (mant << 13)
    } else {
        sign | ((u32::from(exp) + (127 - 15)) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}

/// Port of `HalfToClampedByte` from `astc.cpp`.
fn half_to_clamped_byte(half_bits: u16) -> u16 {
    let value = half_to_float(half_bits);
    let clamped = value.clamp(0.0, 1.0);
    (clamped * 255.0 + 0.5) as u16
}

/// Port of `FillError` from `astc.cpp`.
fn fill_error(out_buf: &mut [u32], block_width: u32, block_height: u32) {
    for j in 0..block_height {
        for i in 0..block_width {
            out_buf[(j * block_width + i) as usize] = 0x00000000;
        }
    }
}

/// Reverse a byte's bits.
/// Port of the `REVERSE_BYTE` macro from `astc.cpp`.
fn reverse_byte(b: u8) -> u8 {
    let b = b as u64;
    ((b.wrapping_mul(0x80200802) & 0x0884422110).wrapping_mul(0x0101010101) >> 32) as u8
}

/// Port of `DecompressBlock` from `astc.cpp`.
fn decompress_block(
    in_buf: &[u8; 16],
    block_width: u32,
    block_height: u32,
    out_buf: &mut [u32; 144],
) {
    let mut strm = InputBitStream::new(in_buf, 0);
    let weight_params = decode_block_info(&mut strm);

    if weight_params.error {
        debug_assert!(false, "Invalid block mode");
        fill_error(out_buf, block_width, block_height);
        return;
    }

    if weight_params.void_extent_ldr {
        fill_void_extent_ldr(&mut strm, out_buf, block_width, block_height);
        return;
    }

    if weight_params.void_extent_hdr {
        debug_assert!(false, "HDR void extent blocks are unsupported!");
        fill_error(out_buf, block_width, block_height);
        return;
    }

    if weight_params.width > block_width {
        debug_assert!(
            false,
            "Texel weight grid width should be smaller than block width"
        );
        fill_error(out_buf, block_width, block_height);
        return;
    }

    if weight_params.height > block_height {
        debug_assert!(
            false,
            "Texel weight grid height should be smaller than block height"
        );
        fill_error(out_buf, block_width, block_height);
        return;
    }

    if weight_params.get_num_weight_values() > 64 {
        debug_assert!(false, "Too many weights in the weight grid");
        fill_error(out_buf, block_width, block_height);
        return;
    }

    // Read num partitions
    let n_partitions = strm.read_bits(2) + 1;
    debug_assert!(n_partitions <= 4);

    if n_partitions == 4 && weight_params.dual_plane {
        debug_assert!(
            false,
            "Dual plane mode is incompatible with four partition blocks"
        );
        fill_error(out_buf, block_width, block_height);
        return;
    }

    // Determine partitions, partition index, and color endpoint modes
    let mut color_endpoint_mode = [0u32; 4];

    // Define color data.
    let mut color_endpoint_data = [0u8; 16];
    let mut color_endpoint_stream = OutputBitStream::new(&mut color_endpoint_data, 16 * 8, 0);

    // Read extra config data...
    let (partition_index, base_cem) = if n_partitions == 1 {
        color_endpoint_mode[0] = strm.read_bits(4);
        (0, 0)
    } else {
        (strm.read_bits(10), strm.read_bits(6))
    };
    let base_mode = base_cem & 3;

    // Remaining bits are color endpoint data...
    let n_weight_bits = weight_params.get_packed_bit_size();
    if !(24..=96).contains(&n_weight_bits) {
        debug_assert!(false, "Invalid weight bit count");
        fill_error(out_buf, block_width, block_height);
        return;
    }
    let mut remaining_bits = 128i32 - n_weight_bits as i32 - strm.bits_read() as i32;

    // Consider extra bits prior to texel data...
    let mut extra_cem_bits = 0u32;
    if base_mode != 0 {
        match n_partitions {
            2 => extra_cem_bits += 2,
            3 => extra_cem_bits += 5,
            4 => extra_cem_bits += 8,
            _ => debug_assert!(false),
        }
    }
    remaining_bits -= extra_cem_bits as i32;

    // Do we have a dual plane situation?
    let plane_selector_bits = if weight_params.dual_plane { 2u32 } else { 0u32 };
    remaining_bits -= plane_selector_bits as i32;

    // Read color data...
    let color_data_bits = remaining_bits as u32;
    while remaining_bits > 0 {
        let nb = remaining_bits.min(8) as usize;
        let b = strm.read_bits(nb);
        color_endpoint_stream.write_bits(b, nb as u32);
        remaining_bits -= 8;
    }

    // Read the plane selection bits
    let plane_idx = strm.read_bits(plane_selector_bits as usize);

    // Read the rest of the CEM
    if base_mode != 0 {
        let extra_cem = strm.read_bits(extra_cem_bits as usize);
        let mut cem = (extra_cem << 6) | base_cem;
        cem >>= 2;

        let mut c_flags = [false; 4];
        for i in 0..n_partitions as usize {
            c_flags[i] = (cem & 1) != 0;
            cem >>= 1;
        }

        let mut m_vals = [0u8; 4];
        for i in 0..n_partitions as usize {
            m_vals[i] = (cem & 3) as u8;
            cem >>= 2;
            debug_assert!(m_vals[i] <= 3);
        }

        for i in 0..n_partitions as usize {
            color_endpoint_mode[i] = base_mode;
            if !c_flags[i] {
                color_endpoint_mode[i] -= 1;
            }
            color_endpoint_mode[i] <<= 2;
            color_endpoint_mode[i] |= m_vals[i] as u32;
        }
    } else if n_partitions > 1 {
        let cem = base_cem >> 2;
        for i in 0..n_partitions as usize {
            color_endpoint_mode[i] = cem;
        }
    }

    // Make sure everything up till here is sane.
    for i in 0..n_partitions as usize {
        debug_assert!(color_endpoint_mode[i] < 16);
    }
    debug_assert_eq!(
        strm.bits_read() as u32 + weight_params.get_packed_bit_size(),
        128
    );

    // Decode both color data and texel weight data
    let mut color_values = [0u32; 32]; // Four values, two endpoints, four maximum partitions
    decode_color_values(
        &mut color_values,
        &color_endpoint_data,
        &color_endpoint_mode,
        n_partitions,
        color_data_bits,
    );

    let mut endpoints = [[Pixel::new_default(); 2]; 4];
    let mut color_values_idx = 0usize;
    for i in 0..n_partitions as usize {
        let (ep1, ep2) =
            compute_endpoints(&color_values, &mut color_values_idx, color_endpoint_mode[i]);
        endpoints[i][0] = ep1;
        endpoints[i][1] = ep2;
    }

    // Read the texel weight data..
    let mut texel_weight_data = *in_buf;

    // Reverse everything
    for i in 0..8 {
        let a = reverse_byte(texel_weight_data[i]);
        let b = reverse_byte(texel_weight_data[15 - i]);
        texel_weight_data[i] = b;
        texel_weight_data[15 - i] = a;
    }

    // Make sure that higher non-texel bits are set to zero
    let clear_byte_start = (weight_params.get_packed_bit_size() >> 3) + 1;
    if clear_byte_start > 0 && (clear_byte_start as usize) <= texel_weight_data.len() {
        texel_weight_data[(clear_byte_start - 1) as usize] &=
            ((1u32 << (weight_params.get_packed_bit_size() % 8)) - 1) as u8;
        let start = clear_byte_start as usize;
        let end = texel_weight_data.len().min(start + (16 - start));
        for byte in &mut texel_weight_data[start..end] {
            *byte = 0;
        }
    }

    let mut texel_weight_values = IntegerEncodedVector::new();
    let mut weight_stream = InputBitStream::new(&texel_weight_data, 0);
    decode_integer_sequence(
        &mut texel_weight_values,
        &mut weight_stream,
        weight_params.max_weight,
        weight_params.get_num_weight_values(),
    );

    // Blocks can be at most 12x12, so we can have as many as 144 weights
    let mut weights = [[0u32; 144]; 2];
    unquantize_texel_weights(
        &mut weights,
        &texel_weight_values,
        &weight_params,
        block_width,
        block_height,
    );

    // Now that we have endpoints and weights, we can interpolate and generate
    // the proper decoding...
    for j in 0..block_height {
        for i in 0..block_width {
            let partition = select_2d_partition(
                partition_index as i32,
                i as i32,
                j as i32,
                n_partitions as i32,
                (block_height * block_width) < 32,
            );
            debug_assert!(partition < n_partitions);

            let mut p = Pixel::new_default();
            for c in 0..4usize {
                let mut c0 = endpoints[partition as usize][0].component(c) as u32;
                let mut c1 = endpoints[partition as usize][1].component(c) as u32;

                let mut plane = 0usize;
                if weight_params.dual_plane && (((plane_idx + 1) & 3) == c as u32) {
                    plane = 1;
                }

                let weight = weights[plane][(j * block_width + i) as usize];

                let is_hdr = is_hdr_color_endpoint_mode(color_endpoint_mode[partition as usize])
                    && !(color_endpoint_mode[partition as usize] == 14 && c == 0);

                if is_hdr {
                    c0 <<= 4;
                    c1 <<= 4;
                    let value = (c0 * (64 - weight) + c1 * weight + 32) / 64;
                    let exponent = (value & 0xF800) >> 11;
                    let mantissa = value & 0x7FF;
                    let transformed_mantissa = if mantissa < 512 {
                        3 * mantissa
                    } else if mantissa >= 1536 {
                        5 * mantissa - 2048
                    } else {
                        4 * mantissa - 512
                    };
                    let converted = (exponent << 10) + (transformed_mantissa >> 3);
                    let half_bits = if converted >= 0x7C00 {
                        0x7BFF
                    } else {
                        converted as u16
                    };
                    p.set_component(c, half_to_clamped_byte(half_bits) as i16);
                } else {
                    c0 = replicate_byte_to_16(c0 as usize);
                    c1 = replicate_byte_to_16(c1 as usize);
                    let value = (c0 * (64 - weight) + c1 * weight + 32) / 64;
                    if value == 65535 {
                        p.set_component(c, 255);
                    } else {
                        let converted = value as f64;
                        p.set_component(c, (255.0 * (converted / 65536.0) + 0.5) as i16);
                    }
                }
            }

            out_buf[(j * block_width + i) as usize] = p.pack();
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Decompress ASTC-encoded texture data into RGBA8 output.
///
/// Port of `Tegra::Texture::ASTC::Decompress`.
///
/// # Parameters
/// - `data`: ASTC compressed input data
/// - `width`, `height`, `depth`: texture dimensions in pixels
/// - `block_width`, `block_height`: ASTC block dimensions
/// - `output`: pre-allocated RGBA8 output buffer
pub fn decompress(
    data: &[u8],
    width: u32,
    height: u32,
    depth: u32,
    block_width: u32,
    block_height: u32,
    output: &mut [u8],
) {
    let rows = divide_up(u64::from(height), u64::from(block_height)) as u32;
    let cols = divide_up(u64::from(width), u64::from(block_width)) as u32;

    let workers = get_thread_workers();

    // The queued work is completed before this function returns, matching the
    // spans captured by value in Eden's worker closures.
    #[derive(Clone, Copy)]
    struct SendConstPtr(*const u8);
    unsafe impl Send for SendConstPtr {}
    unsafe impl Sync for SendConstPtr {}
    impl SendConstPtr {
        unsafe fn as_slice<'a>(self, len: usize) -> &'a [u8] {
            std::slice::from_raw_parts(self.0, len)
        }
    }
    #[derive(Clone, Copy)]
    struct SendPtr(*mut u8);
    unsafe impl Send for SendPtr {}
    unsafe impl Sync for SendPtr {}
    impl SendPtr {
        unsafe fn add(self, offset: usize) -> *mut u8 {
            self.0.add(offset)
        }
    }

    let send_data = SendConstPtr(data.as_ptr());
    let data_len = data.len();
    let send_output = SendPtr(output.as_mut_ptr());
    let output_len = output.len();

    for z in 0..depth {
        let depth_offset = z * height * width * 4;
        for y_index in 0..rows {
            workers.queue_stateless_work(move || {
                // SAFETY: `decompress` waits for every queued request before its
                // borrowed input slice can cease to be valid.
                let data = unsafe { send_data.as_slice(data_len) };
                let y = y_index * block_height;
                for x_index in 0..cols {
                    let block_index = (z * rows * cols) + (y_index * cols) + x_index;
                    let x = x_index * block_width;

                    let block_offset = (block_index * 16) as usize;
                    if block_offset + 16 > data.len() {
                        continue;
                    }
                    let block_ptr: &[u8; 16] =
                        data[block_offset..block_offset + 16].try_into().unwrap();

                    // Blocks can be at most 12x12
                    let mut uncomp_data = [0u32; 144];
                    decompress_block(block_ptr, block_width, block_height, &mut uncomp_data);

                    let decomp_width = block_width.min(width - x);
                    let decomp_height = block_height.min(height - y);

                    // Write the decompressed data to the output buffer
                    let out_base = (depth_offset + (y * width + x) * 4) as usize;
                    for h in 0..decomp_height {
                        let out_offset = out_base + (h * width * 4) as usize;
                        let src_offset = (h * block_width) as usize;
                        let copy_bytes = (decomp_width * 4) as usize;

                        if out_offset + copy_bytes <= output_len {
                            // SAFETY: non-overlapping writes, synchronized via wait_for_requests
                            unsafe {
                                let src = uncomp_data[src_offset..].as_ptr() as *const u8;
                                let dst = send_output.add(out_offset);
                                std::ptr::copy_nonoverlapping(src, dst, copy_bytes);
                            }
                        }
                    }
                }
            });
        }
        workers.wait_for_requests();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_bit_stream_basic() {
        let data = [0b10110100u8, 0b11001010u8];
        let mut stream = InputBitStream::new(&data, 0);
        // LSB first: bit0 = 0, bit1 = 0, bit2 = 1, bit3 = 0, bit4 = 1, bit5 = 1, bit6 = 0, bit7 = 1
        assert!(!stream.read_bit()); // bit 0
        assert!(!stream.read_bit()); // bit 1
        assert!(stream.read_bit()); // bit 2
        assert_eq!(stream.bits_read(), 3);
    }

    #[test]
    fn bit_stream_start_offset_matches_upstream_first_byte_semantics() {
        let data = [0b1011_0100u8, 0b1100_1010u8];
        let mut input = InputBitStream::new(&data, 9);
        assert!(!input.read_bit());
        assert!(input.read_bit());

        let mut output_data = [0u8; 2];
        let mut output = OutputBitStream::new(&mut output_data, 16, 9);
        output.write_bits(0b11, 2);
        assert_eq!(output_data, [0b0000_0110, 0]);
    }

    #[test]
    fn create_encoding_power_of_two() {
        let enc = create_encoding(255);
        assert_eq!(enc.encoding, IntegerEncoding::JustBits);
        assert_eq!(enc.num_bits, 8);
    }

    #[test]
    fn create_encoding_trit() {
        // 2 = 3*1 - 1, so trit with 0 extra bits
        let enc = create_encoding(2);
        assert_eq!(enc.encoding, IntegerEncoding::Trit);
        assert_eq!(enc.num_bits, 0);
    }

    #[test]
    fn create_encoding_quint() {
        // 4 = 5*1 - 1, so quint with 0 extra bits
        let enc = create_encoding(4);
        assert_eq!(enc.encoding, IntegerEncoding::Quint);
        assert_eq!(enc.num_bits, 0);
    }

    #[test]
    fn encodings_table_populated() {
        assert_eq!(ASTC_ENCODINGS_VALUES.len(), 256);
        // Entry 0 should be JustBits with 0 bits
        assert_eq!(ASTC_ENCODINGS_VALUES[0].encoding, IntegerEncoding::JustBits);
        assert_eq!(ASTC_ENCODINGS_VALUES[0].num_bits, 0);
    }

    #[test]
    fn integer_encoded_value_bit_length() {
        // JustBits with 8 bits, 10 values = 80 bits
        let enc = IntegerEncodedValue::new(IntegerEncoding::JustBits, 8);
        assert_eq!(enc.get_bit_length(10), 80);

        // Trit with 4 bits, 5 values = 5*4 + (5*8+4)/5 = 20 + 8 = 28
        let enc_trit = IntegerEncodedValue::new(IntegerEncoding::Trit, 4);
        assert_eq!(enc_trit.get_bit_length(5), 28);
    }

    #[test]
    fn replicate_basic() {
        // Replicate 1 bit (value=1) to 8 bits should give 0xFF
        assert_eq!(replicate(1, 1, 8), 0xFF);
        // Replicate 1 bit (value=0) to 8 bits should give 0x00
        assert_eq!(replicate(0, 1, 8), 0x00);
        // Replicate 4 bits (value=0xA) to 8 bits
        assert_eq!(replicate(0xA, 4, 8), 0xAA);
    }

    #[test]
    fn fast_replicate_to_8_matches_replicate() {
        for num_bits in 1..=8u32 {
            for value in 0..(1u32 << num_bits) {
                assert_eq!(
                    fast_replicate_to_8(value, num_bits),
                    replicate(value, num_bits, 8),
                    "mismatch for value={value}, num_bits={num_bits}"
                );
            }
        }
    }

    #[test]
    fn fast_replicate_to_6_matches_replicate() {
        for num_bits in 1..=5u32 {
            for value in 0..(1u32 << num_bits) {
                assert_eq!(
                    fast_replicate_to_6(value, num_bits),
                    replicate(value, num_bits, 6),
                    "mismatch for value={value}, num_bits={num_bits}"
                );
            }
        }
    }

    #[test]
    fn bits_helper_extraction() {
        let b = Bits::new(0b11010110);
        assert_eq!(b.bit(0), 0);
        assert_eq!(b.bit(1), 1);
        assert_eq!(b.bit(2), 1);
        assert_eq!(b.bits(0, 3), 0b0110);
        assert_eq!(b.bits(4, 7), 0b1101);
    }

    #[test]
    fn decode_integer_sequence_just_bits() {
        // Encode 4 values with 3 bits each (max_range = 7)
        // Values: 5, 3, 7, 1
        // LSB-first bit layout:
        //   val0=5: bits 0,1,2 = 1,0,1
        //   val1=3: bits 3,4,5 = 1,1,0
        //   val2=7: bits 6,7,8 = 1,1,1
        //   val3=1: bits 9,10,11 = 1,0,0
        // byte0 (bits 0-7): 11011101 = 0xDD
        // byte1 (bits 8-15): 00000_100_11 => bit8=1, bit9=1, bit10=0, bit11=0 => 0x03
        let data = [0xDDu8, 0x03u8];
        let mut stream = InputBitStream::new(&data, 0);
        let mut result = IntegerEncodedVector::new();
        decode_integer_sequence(&mut result, &mut stream, 7, 4);
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].bit_value, 5);
        assert_eq!(result[1].bit_value, 3);
        assert_eq!(result[2].bit_value, 7);
        assert_eq!(result[3].bit_value, 1);
        assert!(!result.spilled());
    }

    #[test]
    fn hdr_endpoint_helpers_match_eden_oracle() {
        let mode_7_cases = [
            ([0x12, 0x34, 0x56, 0x78], [564, 524, 520, 804, 764, 760]),
            ([0xC3, 0xA5, 0xE7, 0x69], [0, 0, 0, 96, 1184, 3296]),
            (
                [0x80, 0x40, 0x20, 0x10],
                [2240, 2240, 2240, 2304, 2304, 2304],
            ),
            ([0xFF, 0xFF, 0xFF, 0xFF], [0, 0, 0, 4064, 4064, 4064]),
        ];
        for (input, expected) in mode_7_cases {
            let result = decode_hdr_endpoint_mode_7(input[0], input[1], input[2], input[3]);
            assert_eq!(
                [result.r0, result.g0, result.b0, result.r1, result.g1, result.b1],
                expected
            );
        }

        let mode_11_cases = [
            (
                [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC],
                [288, 1376, 832, 832, 1920, 1920],
            ),
            (
                [0xFF, 0x80, 0xC0, 0xA0, 0x7F, 0x01],
                [2815, 2752, 2782, 2815, 2815, 2783],
            ),
            (
                [0x20, 0x40, 0x60, 0x80, 0xA0, 0xC0],
                [512, 1536, 1024, 1024, 2048, 2048],
            ),
            (
                [0x11, 0x22, 0x33, 0x44, 0x80, 0x80],
                [272, 816, 0, 544, 1088, 0],
            ),
        ];
        for (input, expected) in mode_11_cases {
            let result = decode_hdr_endpoint_mode_11(
                input[0], input[1], input[2], input[3], input[4], input[5],
            );
            assert_eq!(
                [result.r0, result.g0, result.b0, result.r1, result.g1, result.b1],
                expected
            );
        }
    }

    #[test]
    fn hdr_compute_endpoints_match_eden_oracle() {
        let values = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
        let cases = [
            (2, [1920, 288, 288, 288, 1920, 832, 832, 832]),
            (3, [1920, 804, 804, 804, 1920, 812, 812, 812]),
            (7, [1920, 564, 524, 520, 1920, 804, 764, 760]),
            (11, [1920, 288, 1376, 832, 1920, 832, 1920, 1920]),
            (14, [222, 288, 1376, 832, 240, 832, 1920, 1920]),
            (15, [3008, 288, 1376, 832, 3584, 832, 1920, 1920]),
        ];
        for (mode, expected) in cases {
            let mut index = 0;
            let (ep1, ep2) = compute_endpoints(&values, &mut index, mode);
            assert_eq!(
                [
                    ep1.a(),
                    ep1.r(),
                    ep1.g(),
                    ep1.b(),
                    ep2.a(),
                    ep2.r(),
                    ep2.g(),
                    ep2.b()
                ],
                expected
            );
        }
    }

    #[test]
    fn half_conversion_matches_eden_oracle() {
        let cases = [
            (0x0000, 0.0, 0),
            (0x0001, 5.960_464_5e-8, 0),
            (0x03FF, 6.097_555e-5, 0),
            (0x0400, 6.103_515_6e-5, 0),
            (0x3800, 0.5, 128),
            (0x3C00, 1.0, 255),
            (0x4000, 2.0, 255),
            (0x7BFF, 65_504.0, 255),
            (0x8000, -0.0, 0),
            (0xBC00, -1.0, 0),
        ];
        for (bits, expected_float, expected_byte) in cases {
            assert_eq!(half_to_float(bits), expected_float);
            assert_eq!(half_to_clamped_byte(bits), expected_byte);
        }
    }

    #[test]
    fn hdr_decoder_corpus_matches_eden_oracle() {
        fn mix(hash: u64, value: u32) -> u64 {
            (hash ^ u64::from(value)).wrapping_mul(1_099_511_628_211)
        }

        let mut state = 0xC001_D00Du32;
        let mut mode_7_hash = 1_469_598_103_934_665_603u64;
        let mut mode_11_hash = 1_469_598_103_934_665_603u64;
        for _ in 0..4096 {
            let mut next = || {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                state >> 24
            };
            let (v0, v1, v2, v3, v4, v5) = (next(), next(), next(), next(), next(), next());
            let rgb_7 = decode_hdr_endpoint_mode_7(v0, v1, v2, v3);
            for value in [rgb_7.r0, rgb_7.g0, rgb_7.b0, rgb_7.r1, rgb_7.g1, rgb_7.b1] {
                mode_7_hash = mix(mode_7_hash, value as u32);
            }
            let rgb_11 = decode_hdr_endpoint_mode_11(v0, v1, v2, v3, v4, v5);
            for value in [
                rgb_11.r0, rgb_11.g0, rgb_11.b0, rgb_11.r1, rgb_11.g1, rgb_11.b1,
            ] {
                mode_11_hash = mix(mode_11_hash, value as u32);
            }
        }
        assert_eq!(mode_7_hash, 0x56E5_2F08_EA64_D705);
        assert_eq!(mode_11_hash, 0xF531_20EC_B67D_A1D9);

        let mut half_hash = 1_469_598_103_934_665_603u64;
        for bits in 0u16..=0x7BFF {
            half_hash = mix(half_hash, half_to_float(bits).to_bits());
            half_hash = mix(half_hash, u32::from(half_to_clamped_byte(bits)));
        }
        assert_eq!(half_hash, 0xCEF4_5129_7017_B6FC);
    }

    #[test]
    fn pixel_pack_white() {
        let p = Pixel::new(255, 255, 255, 255);
        assert_eq!(p.pack(), 0xFFFFFFFF);
    }

    #[test]
    fn pixel_pack_red() {
        // R=255, G=0, B=0, A=255
        // Pack order: R in LSB, A in MSB => 0xFF0000FF
        let p = Pixel::new(255, 255, 0, 0);
        assert_eq!(p.pack(), 0xFF0000FF);
    }

    #[test]
    fn pixel_clamp_byte() {
        let mut p = Pixel::new_default();
        p.set_component(0, 300);
        p.set_component(1, -50);
        p.set_component(2, 128);
        p.set_component(3, 0);
        p.clamp_byte();
        assert_eq!(p.component(0), 255);
        assert_eq!(p.component(1), 0);
        assert_eq!(p.component(2), 128);
        assert_eq!(p.component(3), 0);
    }

    #[test]
    fn reverse_byte_works() {
        assert_eq!(reverse_byte(0b10000000), 0b00000001);
        assert_eq!(reverse_byte(0b11110000), 0b00001111);
        assert_eq!(reverse_byte(0b10101010), 0b01010101);
        assert_eq!(reverse_byte(0xFF), 0xFF);
        assert_eq!(reverse_byte(0x00), 0x00);
    }

    #[test]
    fn hash52_deterministic() {
        // Just verify it's deterministic and gives non-trivial results
        let h1 = hash52(12345);
        let h2 = hash52(12345);
        assert_eq!(h1, h2);
        assert_ne!(hash52(0), hash52(1));
    }

    #[test]
    fn decode_block_info_void_extent_ldr() {
        // Construct a block mode that matches void extent LDR: (modeBits & 0x01FF) == 0x1FC
        // 0x1FC = 0b111111100, need bit 9 clear for LDR, bit 10 set, bit 11 set
        // modeBits = 0b11_0_11111100 = 0x5FC (bit 10=1, bit 9=0, bits 0-8=0x1FC)
        // But bit 11 is read separately via strm.read_bit() -- we need 12 bits total.
        // Bits 0-10 from read_bits(11), then bit 11 from read_bit().
        // modeBits = 0x5FC = 0b10111111100 (11 bits)
        // Then next bit must be 1.
        // LSB-first: byte0 has bits 0-7 = 0b11111100 = 0xFC
        // byte1 has bits 8-15: bits 8-10 = 0b101 = need bit8=1, bit9=0, bit10=1
        // Then bit11 = 1 (for the strm.ReadBit() check)
        // So bits 8-11 = 0b1101 => byte1 = 0x0D
        let data = [0xFCu8, 0x0D, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut strm = InputBitStream::new(&data, 0);
        let params = decode_block_info(&mut strm);
        assert!(params.void_extent_ldr);
        assert!(!params.void_extent_hdr);
        assert!(!params.error);
    }

    #[test]
    fn decompress_void_extent_uses_borrowed_worker_buffers() {
        let mut block = [0u8; 16];
        let mut stream = OutputBitStream::new(&mut block, 128, 0);
        stream.write_bits(0x5FC, 11);
        stream.write_bits(1, 1);
        for _ in 0..4 {
            stream.write_bits(0, 13);
        }
        stream.write_bits(0x1234, 16);
        stream.write_bits(0x5678, 16);
        stream.write_bits(0x9ABC, 16);
        stream.write_bits(0xDEF0, 16);
        assert_eq!(stream.bits_written(), 128);

        let mut data = [0u8; 32];
        data[..16].copy_from_slice(&block);
        data[16..].copy_from_slice(&block);
        let mut output = [0u8; 4 * 4 * 4 * 2];
        decompress(&data, 4, 4, 2, 4, 4, &mut output);
        for pixel in output.chunks_exact(4) {
            assert_eq!(pixel, [0x12, 0x56, 0x9A, 0xDE]);
        }
    }
    // Reden decodes ASTC through two independent implementations: this CPU
    // decoder, and `host_shaders/astc_decoder.comp` on the GPU (the path
    // OpenGL takes, since `accelerate_astc` defaults to `Gpu`). The GPU copy
    // wrote its dequantised values at `color_values[++out_index]` while this
    // one writes at `out[out_idx]` and then increments, so for identical input
    // the two disagreed by one slot.
    //
    // `compute_endpoints` hands slot i to a fixed colour channel, so that
    // one-slot shift is a channel rotation. This reproduces it by execution:
    // a neutral grey texel decodes neutral under the correct layout and picks
    // up a colour cast under the shifted one.
    #[test]
    fn shifting_color_values_by_one_slot_rotates_the_endpoint_channels() {
        // Colour endpoint mode 8: LDR RGB direct, six values ordered
        // R0, R1, G0, G1, B0, B1.
        const NEUTRAL: [u32; 6] = [120, 140, 120, 140, 120, 140];

        let mut idx = 0usize;
        let (ep1, ep2) = compute_endpoints(&NEUTRAL, &mut idx, 8);
        // R == G == B on both endpoints: genuinely neutral grey.
        assert_eq!(ep1.color[1], ep1.color[2]);
        assert_eq!(ep1.color[2], ep1.color[3]);
        assert_eq!(ep2.color[1], ep2.color[2]);
        assert_eq!(ep2.color[2], ep2.color[3]);

        // What the GPU shader produced: slot 0 holds a leftover value from the
        // previous block and every real value lands one slot later.
        const STALE: u32 = 30;
        let shifted: [u32; 6] = [
            STALE, NEUTRAL[0], NEUTRAL[1], NEUTRAL[2], NEUTRAL[3], NEUTRAL[4],
        ];
        let mut idx = 0usize;
        let (bad1, bad2) = compute_endpoints(&shifted, &mut idx, 8);

        // The same neutral texel is no longer neutral.
        assert_ne!(
            (bad1.color[1], bad1.color[2], bad1.color[3]),
            (ep1.color[1], ep1.color[2], ep1.color[3]),
            "shifted endpoints must differ from the correct ones"
        );
        let neutral_after_shift = bad1.color[1] == bad1.color[2]
            && bad1.color[2] == bad1.color[3]
            && bad2.color[1] == bad2.color[2]
            && bad2.color[2] == bad2.color[3];
        assert!(
            !neutral_after_shift,
            "a one-slot shift must tint a neutral texel: ep1={:?} ep2={:?}",
            bad1.color, bad2.color
        );
    }
}
