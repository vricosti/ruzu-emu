// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of video_core/engines/sw_blitter/converter.h and converter.cpp
//!
//! Pixel format conversion for the software blit engine. Converts between
//! native GPU render target formats and an intermediate f32x4 (RGBA)
//! representation for filtering/scaling.
//!
//! The C++ implementation uses heavily templated `ConverterImpl<Traits>` classes
//! with compile-time component layout. In Rust we use a runtime-dispatched
//! approach via trait objects, preserving the same per-format behavior.

use std::collections::HashMap;

// ── Enums matching upstream ─────────────────────────────────────────────────

/// Channel swizzle — which RGBA channel a component maps to.
///
/// Corresponds to the C++ `Swizzle` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swizzle {
    R = 0,
    G = 1,
    B = 2,
    A = 3,
    None = 4,
}

/// Component data type.
///
/// Corresponds to the C++ `ComponentType` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ComponentType {
    Snorm = 1,
    Unorm = 2,
    Sint = 3,
    Uint = 4,
    SnormForceFp16 = 5,
    UnormForceFp16 = 6,
    Float = 7,
    Srgb = 8,
}

// ── sRGB lookup tables ──────────────────────────────────────────────────────

/// sRGB to linear RGB lookup table (256 entries for 8-bit sRGB input).
///
/// Corresponds to `SRGB_TO_RGB_LUT` in the C++ source.
#[rustfmt::skip]
pub const SRGB_TO_RGB_LUT: [f32; 256] = [
    0.000000e+00, 3.035270e-04, 6.070540e-04, 9.105810e-04, 1.214108e-03, 1.517635e-03,
    1.821162e-03, 2.124689e-03, 2.428216e-03, 2.731743e-03, 3.035270e-03, 3.346536e-03,
    3.676507e-03, 4.024717e-03, 4.391442e-03, 4.776953e-03, 5.181517e-03, 5.605392e-03,
    6.048833e-03, 6.512091e-03, 6.995410e-03, 7.499032e-03, 8.023193e-03, 8.568126e-03,
    9.134059e-03, 9.721218e-03, 1.032982e-02, 1.096009e-02, 1.161224e-02, 1.228649e-02,
    1.298303e-02, 1.370208e-02, 1.444384e-02, 1.520851e-02, 1.599629e-02, 1.680738e-02,
    1.764195e-02, 1.850022e-02, 1.938236e-02, 2.028856e-02, 2.121901e-02, 2.217389e-02,
    2.315337e-02, 2.415763e-02, 2.518686e-02, 2.624122e-02, 2.732089e-02, 2.842604e-02,
    2.955684e-02, 3.071344e-02, 3.189603e-02, 3.310477e-02, 3.433981e-02, 3.560131e-02,
    3.688945e-02, 3.820437e-02, 3.954624e-02, 4.091520e-02, 4.231141e-02, 4.373503e-02,
    4.518620e-02, 4.666509e-02, 4.817183e-02, 4.970657e-02, 5.126946e-02, 5.286065e-02,
    5.448028e-02, 5.612849e-02, 5.780543e-02, 5.951124e-02, 6.124605e-02, 6.301001e-02,
    6.480327e-02, 6.662594e-02, 6.847817e-02, 7.036009e-02, 7.227185e-02, 7.421357e-02,
    7.618538e-02, 7.818742e-02, 8.021982e-02, 8.228271e-02, 8.437621e-02, 8.650046e-02,
    8.865558e-02, 9.084171e-02, 9.305897e-02, 9.530747e-02, 9.758735e-02, 9.989873e-02,
    1.022417e-01, 1.046165e-01, 1.070231e-01, 1.094617e-01, 1.119324e-01, 1.144354e-01,
    1.169707e-01, 1.195384e-01, 1.221388e-01, 1.247718e-01, 1.274377e-01, 1.301365e-01,
    1.328683e-01, 1.356333e-01, 1.384316e-01, 1.412633e-01, 1.441285e-01, 1.470273e-01,
    1.499598e-01, 1.529261e-01, 1.559265e-01, 1.589608e-01, 1.620294e-01, 1.651322e-01,
    1.682694e-01, 1.714411e-01, 1.746474e-01, 1.778884e-01, 1.811642e-01, 1.844750e-01,
    1.878208e-01, 1.912017e-01, 1.946178e-01, 1.980693e-01, 2.015563e-01, 2.050787e-01,
    2.086369e-01, 2.122308e-01, 2.158605e-01, 2.195262e-01, 2.232280e-01, 2.269659e-01,
    2.307401e-01, 2.345506e-01, 2.383976e-01, 2.422811e-01, 2.462013e-01, 2.501583e-01,
    2.541521e-01, 2.581829e-01, 2.622507e-01, 2.663556e-01, 2.704978e-01, 2.746773e-01,
    2.788943e-01, 2.831487e-01, 2.874408e-01, 2.917706e-01, 2.961383e-01, 3.005438e-01,
    3.049873e-01, 3.094689e-01, 3.139887e-01, 3.185468e-01, 3.231432e-01, 3.277781e-01,
    3.324515e-01, 3.371636e-01, 3.419144e-01, 3.467041e-01, 3.515326e-01, 3.564001e-01,
    3.613068e-01, 3.662526e-01, 3.712377e-01, 3.762621e-01, 3.813260e-01, 3.864294e-01,
    3.915725e-01, 3.967552e-01, 4.019778e-01, 4.072402e-01, 4.125426e-01, 4.178851e-01,
    4.232677e-01, 4.286905e-01, 4.341536e-01, 4.396572e-01, 4.452012e-01, 4.507858e-01,
    4.564110e-01, 4.620770e-01, 4.677838e-01, 4.735315e-01, 4.793202e-01, 4.851499e-01,
    4.910209e-01, 4.969330e-01, 5.028865e-01, 5.088813e-01, 5.149177e-01, 5.209956e-01,
    5.271151e-01, 5.332764e-01, 5.394795e-01, 5.457245e-01, 5.520114e-01, 5.583404e-01,
    5.647115e-01, 5.711249e-01, 5.775805e-01, 5.840784e-01, 5.906188e-01, 5.972018e-01,
    6.038274e-01, 6.104956e-01, 6.172066e-01, 6.239604e-01, 6.307572e-01, 6.375968e-01,
    6.444797e-01, 6.514056e-01, 6.583748e-01, 6.653873e-01, 6.724432e-01, 6.795425e-01,
    6.866853e-01, 6.938717e-01, 7.011019e-01, 7.083758e-01, 7.156935e-01, 7.230551e-01,
    7.304608e-01, 7.379104e-01, 7.454042e-01, 7.529422e-01, 7.605245e-01, 7.681512e-01,
    7.758222e-01, 7.835378e-01, 7.912979e-01, 7.991027e-01, 8.069522e-01, 8.148466e-01,
    8.227857e-01, 8.307699e-01, 8.387990e-01, 8.468732e-01, 8.549926e-01, 8.631572e-01,
    8.713671e-01, 8.796224e-01, 8.879231e-01, 8.962694e-01, 9.046612e-01, 9.130986e-01,
    9.215819e-01, 9.301109e-01, 9.386857e-01, 9.473065e-01, 9.559733e-01, 9.646863e-01,
    9.734453e-01, 9.822506e-01, 9.911021e-01, 1.000000e+00,
];

/// Linear RGB to sRGB lookup table (256 entries for 8-bit linear input).
///
/// Corresponds to `RGB_TO_SRGB_LUT` in the C++ source.
#[rustfmt::skip]
pub const RGB_TO_SRGB_LUT: [f32; 256] = [
    0.000000e+00, 4.984009e-02, 8.494473e-02, 1.107021e-01, 1.318038e-01, 1.500052e-01,
    1.661857e-01, 1.808585e-01, 1.943532e-01, 2.068957e-01, 2.186491e-01, 2.297351e-01,
    2.402475e-01, 2.502604e-01, 2.598334e-01, 2.690152e-01, 2.778465e-01, 2.863614e-01,
    2.945889e-01, 3.025538e-01, 3.102778e-01, 3.177796e-01, 3.250757e-01, 3.321809e-01,
    3.391081e-01, 3.458689e-01, 3.524737e-01, 3.589320e-01, 3.652521e-01, 3.714419e-01,
    3.775084e-01, 3.834581e-01, 3.892968e-01, 3.950301e-01, 4.006628e-01, 4.061998e-01,
    4.116451e-01, 4.170030e-01, 4.222770e-01, 4.274707e-01, 4.325873e-01, 4.376298e-01,
    4.426010e-01, 4.475037e-01, 4.523403e-01, 4.571131e-01, 4.618246e-01, 4.664766e-01,
    4.710712e-01, 4.756104e-01, 4.800958e-01, 4.845292e-01, 4.889122e-01, 4.932462e-01,
    4.975329e-01, 5.017734e-01, 5.059693e-01, 5.101216e-01, 5.142317e-01, 5.183006e-01,
    5.223295e-01, 5.263194e-01, 5.302714e-01, 5.341862e-01, 5.380651e-01, 5.419087e-01,
    5.457181e-01, 5.494938e-01, 5.532369e-01, 5.569480e-01, 5.606278e-01, 5.642771e-01,
    5.678965e-01, 5.714868e-01, 5.750484e-01, 5.785821e-01, 5.820884e-01, 5.855680e-01,
    5.890211e-01, 5.924487e-01, 5.958509e-01, 5.992285e-01, 6.025819e-01, 6.059114e-01,
    6.092176e-01, 6.125010e-01, 6.157619e-01, 6.190008e-01, 6.222180e-01, 6.254140e-01,
    6.285890e-01, 6.317436e-01, 6.348780e-01, 6.379926e-01, 6.410878e-01, 6.441637e-01,
    6.472208e-01, 6.502595e-01, 6.532799e-01, 6.562824e-01, 6.592672e-01, 6.622347e-01,
    6.651851e-01, 6.681187e-01, 6.710356e-01, 6.739363e-01, 6.768209e-01, 6.796897e-01,
    6.825429e-01, 6.853807e-01, 6.882034e-01, 6.910111e-01, 6.938041e-01, 6.965826e-01,
    6.993468e-01, 7.020969e-01, 7.048331e-01, 7.075556e-01, 7.102645e-01, 7.129600e-01,
    7.156424e-01, 7.183118e-01, 7.209683e-01, 7.236121e-01, 7.262435e-01, 7.288625e-01,
    7.314693e-01, 7.340640e-01, 7.366470e-01, 7.392181e-01, 7.417776e-01, 7.443256e-01,
    7.468624e-01, 7.493880e-01, 7.519025e-01, 7.544061e-01, 7.568989e-01, 7.593810e-01,
    7.618526e-01, 7.643137e-01, 7.667645e-01, 7.692052e-01, 7.716358e-01, 7.740564e-01,
    7.764671e-01, 7.788681e-01, 7.812595e-01, 7.836413e-01, 7.860138e-01, 7.883768e-01,
    7.907307e-01, 7.930754e-01, 7.954110e-01, 7.977377e-01, 8.000556e-01, 8.023647e-01,
    8.046651e-01, 8.069569e-01, 8.092403e-01, 8.115152e-01, 8.137818e-01, 8.160402e-01,
    8.182903e-01, 8.205324e-01, 8.227665e-01, 8.249926e-01, 8.272109e-01, 8.294214e-01,
    8.316242e-01, 8.338194e-01, 8.360070e-01, 8.381871e-01, 8.403597e-01, 8.425251e-01,
    8.446831e-01, 8.468339e-01, 8.489776e-01, 8.511142e-01, 8.532437e-01, 8.553662e-01,
    8.574819e-01, 8.595907e-01, 8.616927e-01, 8.637881e-01, 8.658767e-01, 8.679587e-01,
    8.700342e-01, 8.721032e-01, 8.741657e-01, 8.762218e-01, 8.782716e-01, 8.803151e-01,
    8.823524e-01, 8.843835e-01, 8.864085e-01, 8.884274e-01, 8.904402e-01, 8.924471e-01,
    8.944480e-01, 8.964431e-01, 8.984324e-01, 9.004158e-01, 9.023935e-01, 9.043654e-01,
    9.063318e-01, 9.082925e-01, 9.102476e-01, 9.121972e-01, 9.141413e-01, 9.160800e-01,
    9.180133e-01, 9.199412e-01, 9.218637e-01, 9.237810e-01, 9.256931e-01, 9.276000e-01,
    9.295017e-01, 9.313982e-01, 9.332896e-01, 9.351761e-01, 9.370575e-01, 9.389339e-01,
    9.408054e-01, 9.426719e-01, 9.445336e-01, 9.463905e-01, 9.482424e-01, 9.500897e-01,
    9.519322e-01, 9.537700e-01, 9.556032e-01, 9.574316e-01, 9.592555e-01, 9.610748e-01,
    9.628896e-01, 9.646998e-01, 9.665055e-01, 9.683068e-01, 9.701037e-01, 9.718961e-01,
    9.736842e-01, 9.754679e-01, 9.772474e-01, 9.790225e-01, 9.807934e-01, 9.825601e-01,
    9.843225e-01, 9.860808e-01, 9.878350e-01, 9.895850e-01, 9.913309e-01, 9.930727e-01,
    9.948106e-01, 9.965444e-01, 9.982741e-01, 1.000000e+00,
];

// ── Converter trait ─────────────────────────────────────────────────────────

/// Trait for pixel format converters.
///
/// Corresponds to the C++ `Converter` base class.
pub trait Converter: Send {
    /// Convert native-format pixel data to f32x4 intermediate representation.
    fn convert_to(&self, input: &[u8], output: &mut [f32]);

    /// Convert f32x4 intermediate representation back to native-format pixel data.
    fn convert_from(&self, input: &[f32], output: &mut [u8]);
}

// ── Null converter ──────────────────────────────────────────────────────────

/// Null converter — fills output with zeros for unsupported formats.
///
/// Corresponds to the C++ `NullConverter` class.
struct NullConverter;

impl Converter for NullConverter {
    fn convert_to(&self, _input: &[u8], output: &mut [f32]) {
        output.fill(0.0);
    }

    fn convert_from(&self, _input: &[f32], output: &mut [u8]) {
        output.fill(0);
    }
}

// ── Format traits ───────────────────────────────────────────────────────────

/// Describes the component layout of a render target format.
///
/// This replaces the C++ per-format `*Traits` structs. In Rust we store
/// these as runtime data rather than compile-time template parameters.
#[derive(Debug, Clone)]
struct FormatTraits {
    num_components: usize,
    component_types: Vec<ComponentType>,
    component_sizes: Vec<usize>,
    component_swizzle: Vec<Swizzle>,
}

// ── Generic converter ───────────────────────────────────────────────────────

/// Runtime-dispatched format converter.
///
/// Replaces the C++ `ConverterImpl<Traits>` template with a data-driven
/// approach. Behavior is identical; component extraction and insertion
/// use the same bit-manipulation logic.
struct GenericConverter {
    traits: FormatTraits,
    total_bytes_per_pixel: usize,
    /// Which u32 word each component lives in.
    bound_words: Vec<usize>,
    /// Bit offset within that word for each component.
    bound_offsets: Vec<usize>,
    /// Bitmask for each component (shifted to position).
    component_mask: Vec<u32>,
}

impl GenericConverter {
    fn new(traits: FormatTraits) -> Self {
        let total_bits: usize = traits.component_sizes.iter().sum();
        // Round up to next power of two, then divide by 8.
        // Port of ConverterImpl::CalculateByteSize().
        let total_bytes = if total_bits == 0 {
            0
        } else {
            let power = (usize::BITS - total_bits.leading_zeros() - 1) as usize;
            let base_size = 1usize << power;
            let mask = base_size - 1;
            if (total_bits & mask) != 0 {
                (base_size << 1) / 8
            } else {
                base_size / 8
            }
        };

        // Upstream's stack array is sized per format; all supported formats fit in four words.
        assert!(total_bytes <= 16);

        // Pre-compute bound_words and bound_offsets.
        // Port of ConverterImpl::GetBoundWordsOffsets<>.
        let num_components = traits.num_components;
        let mut bound_words = vec![0usize; num_components];
        let mut bound_offsets = vec![0usize; num_components];
        {
            let total_bits_per_word: usize = 32;
            let mut accumulated_size: usize = 0;
            let mut word_index: usize = 0;
            for i in 0..num_components {
                bound_offsets[i] = accumulated_size;
                bound_words[i] = word_index;
                accumulated_size += traits.component_sizes[i];
                if accumulated_size > total_bits_per_word {
                    // Component spans word boundary; move to next word.
                    bound_offsets[i] = 0;
                    word_index += 1;
                    bound_words[i] = word_index;
                    accumulated_size = traits.component_sizes[i];
                }
            }
        }

        // Pre-compute component masks.
        let mut component_mask = vec![0u32; num_components];
        for i in 0..num_components {
            let size = traits.component_sizes[i];
            if size >= 32 {
                component_mask[i] = u32::MAX;
            } else {
                component_mask[i] = ((1u64 << size) - 1) as u32;
            }
            component_mask[i] <<= bound_offsets[i];
        }

        Self {
            traits,
            total_bytes_per_pixel: total_bytes,
            bound_words,
            bound_offsets,
            component_mask,
        }
    }

    /// Port of ConverterImpl::ConvertToComponent<which_component>.
    ///
    /// Extracts a component from packed word data and converts to f32.
    #[inline]
    fn convert_to_component(&self, which_component: usize, which_word: u32) -> f32 {
        let size = self.traits.component_sizes[which_component];
        let offset = self.bound_offsets[which_component];
        let comp_type = self.traits.component_types[which_component];
        let swizzle = self.traits.component_swizzle[which_component];

        // Extract raw bits.
        let value = if size >= 32 {
            which_word
        } else {
            (which_word >> offset) & ((1u64 << size) - 1) as u32
        };

        let sign_extend = |base_value: u32, bits: usize| -> i32 {
            let shift_amount = 32 - bits;
            ((base_value << shift_amount) as i32) >> shift_amount
        };

        let force_to_fp16 = |base_value: f32| -> f32 {
            let tmp = base_value.to_bits();
            let mantissa_mask: u32 = !((1u32 << (23 - 10)) - 1);
            f32::from_bits(tmp & mantissa_mask)
        };

        let from_fp_n = |base_value: u32, bits: usize, mantissa: usize| -> f32 {
            let shift_towards = 23 - mantissa;
            let new_value =
                ((sign_extend(base_value, bits) << shift_towards) as u32) & !(1u32 << 31);
            f32::from_bits(new_value)
        };

        let calculate_snorm = || -> f32 {
            let signed_val = sign_extend(value, size);
            let max_val = ((1u64 << (size - 1)) - 1) as f32;
            signed_val as f32 / max_val
        };

        let calculate_unorm = || -> f32 {
            let max_val = ((1u64 << size) - 1) as f32;
            value as f32 / max_val
        };

        match comp_type {
            ComponentType::Snorm => calculate_snorm(),
            ComponentType::Unorm => calculate_unorm(),
            ComponentType::Sint => sign_extend(value, size) as f32,
            ComponentType::Uint => sign_extend(value, size) as f32,
            ComponentType::SnormForceFp16 => force_to_fp16(calculate_snorm()),
            ComponentType::UnormForceFp16 => force_to_fp16(calculate_unorm()),
            ComponentType::Float => {
                if size == 32 {
                    f32::from_bits(value)
                } else if size == 16 {
                    // FP16 to FP32 conversion.
                    let sign_mask: u32 = 0x8000;
                    let mantissa_mask: u32 = 0x03FF;
                    f32::from_bits(
                        ((value & sign_mask) << 16)
                            | (((value & 0x7C00).wrapping_add(0x1C000)) << 13)
                            | ((value & mantissa_mask) << 13),
                    )
                } else {
                    from_fp_n(value, size, size.saturating_sub(5))
                }
            }
            ComponentType::Srgb => {
                if swizzle == Swizzle::A {
                    calculate_unorm()
                } else if size == 8 {
                    SRGB_TO_RGB_LUT[value as usize]
                } else {
                    // Fallback for non-8-bit sRGB (upstream logs UNIMPLEMENTED).
                    calculate_unorm()
                }
            }
        }
    }

    /// Port of ConverterImpl::ConvertFromComponent<which_component>.
    ///
    /// Converts an f32 component value and inserts it into packed word data.
    #[inline]
    fn convert_from_component(
        &self,
        which_component: usize,
        which_word: &mut u32,
        in_component: f32,
    ) {
        let size = self.traits.component_sizes[which_component];
        let offset = self.bound_offsets[which_component];
        let comp_type = self.traits.component_types[which_component];
        let swizzle = self.traits.component_swizzle[which_component];
        let mask = self.component_mask[which_component];

        let insert_to_word = |word: &mut u32, new_val: u32| {
            *word |= (new_val << offset) & mask;
        };

        let to_fp_n = |base_value: f32, _bits: usize, mantissa: usize| -> u32 {
            let tmp_value = base_value.max(0.0).to_bits();
            let shift_towards = 23 - mantissa;
            tmp_value >> shift_towards
        };

        let calculate_unorm = || -> u32 {
            let max_val = ((1u64 << size) - 1) as f32;
            (in_component * max_val) as u32
        };

        match comp_type {
            ComponentType::Snorm | ComponentType::SnormForceFp16 => {
                let max_val = ((1u64 << (size - 1)) - 1) as f32;
                let tmp_word = (in_component * max_val) as i32;
                insert_to_word(which_word, tmp_word as u32);
            }
            ComponentType::Unorm | ComponentType::UnormForceFp16 => {
                let tmp_word = calculate_unorm();
                insert_to_word(which_word, tmp_word);
            }
            ComponentType::Sint => {
                let tmp_word = in_component as i32;
                insert_to_word(which_word, tmp_word as u32);
            }
            ComponentType::Uint => {
                let tmp_word = in_component as u32;
                insert_to_word(which_word, tmp_word);
            }
            ComponentType::Float => {
                if size == 32 {
                    insert_to_word(which_word, in_component.to_bits());
                } else if size == 16 {
                    // FP32 to FP16 conversion.
                    let sign_mask: u32 = 0x8000;
                    let mantissa_mask_16: u32 = 0x03FF;
                    let exponent_mask_16: u32 = 0x7C00;
                    let tmp_word = in_component.to_bits();
                    let half = ((tmp_word >> 16) & sign_mask)
                        | ((((tmp_word & 0x7F80_0000).wrapping_sub(0x3800_0000)) >> 13)
                            & exponent_mask_16)
                        | ((tmp_word >> 13) & mantissa_mask_16);
                    insert_to_word(which_word, half);
                } else {
                    insert_to_word(
                        which_word,
                        to_fp_n(in_component, size, size.saturating_sub(5)),
                    );
                }
            }
            ComponentType::Srgb => {
                let mut comp = in_component;
                if swizzle != Swizzle::A && size == 8 {
                    let index = calculate_unorm() as usize;
                    if index < RGB_TO_SRGB_LUT.len() {
                        comp = RGB_TO_SRGB_LUT[index];
                    }
                }
                let max_val = ((1u64 << size) - 1) as f32;
                let tmp_word = (comp * max_val) as u32;
                insert_to_word(which_word, tmp_word);
            }
        }
    }
}

impl Converter for GenericConverter {
    fn convert_to(&self, input: &[u8], output: &mut [f32]) {
        let components_per_ir = 4usize;
        let num_pixels = output.len() / components_per_ir;
        let t = &self.traits;

        for pixel in 0..num_pixels {
            let src_start = pixel * self.total_bytes_per_pixel;
            let dst_start = pixel * components_per_ir;

            // Read raw words from input.
            let mut words = [0u32; 4];
            let copy_len = self
                .total_bytes_per_pixel
                .min(input.len().saturating_sub(src_start));
            let src_bytes = &input[src_start..src_start + copy_len];
            let words_bytes: &mut [u8] = unsafe {
                std::slice::from_raw_parts_mut(words.as_mut_ptr() as *mut u8, words.len() * 4)
            };
            words_bytes[..copy_len].copy_from_slice(src_bytes);

            // Initialize output components to zero.
            for i in 0..components_per_ir {
                output[dst_start + i] = 0.0;
            }

            // Extract each component and place in the correct IR slot.
            for comp in 0..t.num_components {
                let swizzle = t.component_swizzle[comp];
                if swizzle == Swizzle::None {
                    continue;
                }
                let ir_index = swizzle as usize;
                if ir_index >= components_per_ir {
                    continue;
                }
                let word_idx = self.bound_words[comp];
                let word = if word_idx < words.len() {
                    words[word_idx]
                } else {
                    0
                };
                output[dst_start + ir_index] = self.convert_to_component(comp, word);
            }
        }
    }

    fn convert_from(&self, input: &[f32], output: &mut [u8]) {
        let components_per_ir = 4usize;
        let num_pixels = output.len() / self.total_bytes_per_pixel;
        let t = &self.traits;

        for pixel in 0..num_pixels {
            let src_start = pixel * components_per_ir;
            let dst_start = pixel * self.total_bytes_per_pixel;

            let old_components = &input[src_start..src_start + components_per_ir];
            let mut words = [0u32; 4];

            // Insert each component from the IR slot into packed words.
            for comp in 0..t.num_components {
                let swizzle = t.component_swizzle[comp];
                if swizzle == Swizzle::None {
                    continue;
                }
                let ir_index = swizzle as usize;
                if ir_index >= components_per_ir {
                    continue;
                }
                let word_idx = self.bound_words[comp];
                if word_idx < words.len() {
                    self.convert_from_component(
                        comp,
                        &mut words[word_idx],
                        old_components[ir_index],
                    );
                }
            }

            // Write words back to output bytes.
            let words_bytes: &[u8] =
                unsafe { std::slice::from_raw_parts(words.as_ptr() as *const u8, words.len() * 4) };
            let end = (dst_start + self.total_bytes_per_pixel).min(output.len());
            let copy_len = end - dst_start;
            output[dst_start..end].copy_from_slice(&words_bytes[..copy_len]);
        }
    }
}

// ── ConverterFactory ────────────────────────────────────────────────────────

/// Factory for creating and caching format converters.
///
/// Corresponds to the C++ `ConverterFactory` class.
pub struct ConverterFactory {
    cache: HashMap<u32, Box<dyn Converter + Send>>,
}

impl ConverterFactory {
    /// Create a new converter factory.
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Get a converter for the given render target format.
    ///
    /// Corresponds to `ConverterFactory::GetFormatConverter`.
    pub fn get_format_converter(&mut self, format: u32) -> &dyn Converter {
        if !self.cache.contains_key(&format) {
            self.build_converter(format);
        }
        self.cache.get(&format).unwrap().as_ref()
    }

    /// Build and cache a converter for the given format.
    ///
    /// Corresponds to `ConverterFactory::BuildConverter`.
    /// Each match arm creates a `GenericConverter` with the format traits
    /// matching the upstream `*Traits` structs.
    fn build_converter(&mut self, format: u32) {
        use ComponentType::*;
        use Swizzle::*;

        let traits = match format {
            // R32G32B32A32_FLOAT = 0xC0
            0xC0 => FormatTraits {
                num_components: 4,
                component_types: vec![Float, Float, Float, Float],
                component_sizes: vec![32, 32, 32, 32],
                component_swizzle: vec![R, G, B, A],
            },
            // R32G32B32A32_SINT = 0xC1
            0xC1 => FormatTraits {
                num_components: 4,
                component_types: vec![Sint, Sint, Sint, Sint],
                component_sizes: vec![32, 32, 32, 32],
                component_swizzle: vec![R, G, B, A],
            },
            // R32G32B32A32_UINT = 0xC2
            0xC2 => FormatTraits {
                num_components: 4,
                component_types: vec![Uint, Uint, Uint, Uint],
                component_sizes: vec![32, 32, 32, 32],
                component_swizzle: vec![R, G, B, A],
            },
            // R32G32B32X32_FLOAT = 0xC3
            0xC3 => FormatTraits {
                num_components: 4,
                component_types: vec![Float, Float, Float, Float],
                component_sizes: vec![32, 32, 32, 32],
                component_swizzle: vec![R, G, B, None],
            },
            // R32G32B32X32_SINT = 0xC4
            0xC4 => FormatTraits {
                num_components: 4,
                component_types: vec![Sint, Sint, Sint, Sint],
                component_sizes: vec![32, 32, 32, 32],
                component_swizzle: vec![R, G, B, None],
            },
            // R32G32B32X32_UINT = 0xC5
            0xC5 => FormatTraits {
                num_components: 4,
                component_types: vec![Uint, Uint, Uint, Uint],
                component_sizes: vec![32, 32, 32, 32],
                component_swizzle: vec![R, G, B, None],
            },
            // R16G16B16A16_UNORM = 0xC6
            0xC6 => FormatTraits {
                num_components: 4,
                component_types: vec![Unorm, Unorm, Unorm, Unorm],
                component_sizes: vec![16, 16, 16, 16],
                component_swizzle: vec![R, G, B, A],
            },
            // R16G16B16A16_SNORM = 0xC7
            0xC7 => FormatTraits {
                num_components: 4,
                component_types: vec![Snorm, Snorm, Snorm, Snorm],
                component_sizes: vec![16, 16, 16, 16],
                component_swizzle: vec![R, G, B, A],
            },
            // R16G16B16A16_SINT = 0xC8
            0xC8 => FormatTraits {
                num_components: 4,
                component_types: vec![Sint, Sint, Sint, Sint],
                component_sizes: vec![16, 16, 16, 16],
                component_swizzle: vec![R, G, B, A],
            },
            // R16G16B16A16_UINT = 0xC9
            0xC9 => FormatTraits {
                num_components: 4,
                component_types: vec![Uint, Uint, Uint, Uint],
                component_sizes: vec![16, 16, 16, 16],
                component_swizzle: vec![R, G, B, A],
            },
            // R16G16B16A16_FLOAT = 0xCA
            0xCA => FormatTraits {
                num_components: 4,
                component_types: vec![Float, Float, Float, Float],
                component_sizes: vec![16, 16, 16, 16],
                component_swizzle: vec![R, G, B, A],
            },
            // R32G32_FLOAT = 0xCB
            0xCB => FormatTraits {
                num_components: 2,
                component_types: vec![Float, Float],
                component_sizes: vec![32, 32],
                component_swizzle: vec![R, G],
            },
            // R32G32_SINT = 0xCC
            0xCC => FormatTraits {
                num_components: 2,
                component_types: vec![Sint, Sint],
                component_sizes: vec![32, 32],
                component_swizzle: vec![R, G],
            },
            // R32G32_UINT = 0xCD
            0xCD => FormatTraits {
                num_components: 2,
                component_types: vec![Uint, Uint],
                component_sizes: vec![32, 32],
                component_swizzle: vec![R, G],
            },
            // R16G16B16X16_FLOAT = 0xCE
            0xCE => FormatTraits {
                num_components: 4,
                component_types: vec![Float, Float, Float, Float],
                component_sizes: vec![16, 16, 16, 16],
                component_swizzle: vec![R, G, B, None],
            },
            // A8R8G8B8_UNORM = 0xCF
            0xCF => FormatTraits {
                num_components: 4,
                component_types: vec![Unorm, Unorm, Unorm, Unorm],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![A, R, G, B],
            },
            // A8R8G8B8_SRGB = 0xD0
            0xD0 => FormatTraits {
                num_components: 4,
                component_types: vec![Srgb, Srgb, Srgb, Srgb],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![A, R, G, B],
            },
            // A2B10G10R10_UNORM = 0xD1
            0xD1 => FormatTraits {
                num_components: 4,
                component_types: vec![Unorm, Unorm, Unorm, Unorm],
                component_sizes: vec![2, 10, 10, 10],
                component_swizzle: vec![A, B, G, R],
            },
            // A2B10G10R10_UINT = 0xD2
            0xD2 => FormatTraits {
                num_components: 4,
                component_types: vec![Uint, Uint, Uint, Uint],
                component_sizes: vec![2, 10, 10, 10],
                component_swizzle: vec![A, B, G, R],
            },
            // A8B8G8R8_UNORM = 0xD5
            0xD5 => FormatTraits {
                num_components: 4,
                component_types: vec![Unorm, Unorm, Unorm, Unorm],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![A, B, G, R],
            },
            // A8B8G8R8_SRGB = 0xD6
            0xD6 => FormatTraits {
                num_components: 4,
                component_types: vec![Srgb, Srgb, Srgb, Srgb],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![A, B, G, R],
            },
            // A8B8G8R8_SNORM = 0xD7
            0xD7 => FormatTraits {
                num_components: 4,
                component_types: vec![Snorm, Snorm, Snorm, Snorm],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![A, B, G, R],
            },
            // A8B8G8R8_SINT = 0xD8
            0xD8 => FormatTraits {
                num_components: 4,
                component_types: vec![Sint, Sint, Sint, Sint],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![A, B, G, R],
            },
            // A8B8G8R8_UINT = 0xD9
            0xD9 => FormatTraits {
                num_components: 4,
                component_types: vec![Uint, Uint, Uint, Uint],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![A, B, G, R],
            },
            // R16G16_UNORM = 0xDA
            0xDA => FormatTraits {
                num_components: 2,
                component_types: vec![Unorm, Unorm],
                component_sizes: vec![16, 16],
                component_swizzle: vec![R, G],
            },
            // R16G16_SNORM = 0xDB
            0xDB => FormatTraits {
                num_components: 2,
                component_types: vec![Snorm, Snorm],
                component_sizes: vec![16, 16],
                component_swizzle: vec![R, G],
            },
            // R16G16_SINT = 0xDC
            0xDC => FormatTraits {
                num_components: 2,
                component_types: vec![Sint, Sint],
                component_sizes: vec![16, 16],
                component_swizzle: vec![R, G],
            },
            // R16G16_UINT = 0xDD
            0xDD => FormatTraits {
                num_components: 2,
                component_types: vec![Uint, Uint],
                component_sizes: vec![16, 16],
                component_swizzle: vec![R, G],
            },
            // R16G16_FLOAT = 0xDE
            0xDE => FormatTraits {
                num_components: 2,
                component_types: vec![Float, Float],
                component_sizes: vec![16, 16],
                component_swizzle: vec![R, G],
            },
            // A2R10G10B10_UNORM = 0xDF
            0xDF => FormatTraits {
                num_components: 4,
                component_types: vec![Unorm, Unorm, Unorm, Unorm],
                component_sizes: vec![2, 10, 10, 10],
                component_swizzle: vec![A, R, G, B],
            },
            // B10G11R11_FLOAT = 0xE0
            0xE0 => FormatTraits {
                num_components: 3,
                component_types: vec![Float, Float, Float],
                component_sizes: vec![10, 11, 11],
                component_swizzle: vec![B, G, R],
            },
            // R32_SINT = 0xE3
            0xE3 => FormatTraits {
                num_components: 1,
                component_types: vec![Sint],
                component_sizes: vec![32],
                component_swizzle: vec![R],
            },
            // R32_UINT = 0xE4
            0xE4 => FormatTraits {
                num_components: 1,
                component_types: vec![Uint],
                component_sizes: vec![32],
                component_swizzle: vec![R],
            },
            // R32_FLOAT = 0xE5
            0xE5 => FormatTraits {
                num_components: 1,
                component_types: vec![Float],
                component_sizes: vec![32],
                component_swizzle: vec![R],
            },
            // X8R8G8B8_UNORM = 0xE6
            0xE6 => FormatTraits {
                num_components: 4,
                component_types: vec![Unorm, Unorm, Unorm, Unorm],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![None, R, G, B],
            },
            // X8R8G8B8_SRGB = 0xE7
            0xE7 => FormatTraits {
                num_components: 4,
                component_types: vec![Srgb, Srgb, Srgb, Srgb],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![None, R, G, B],
            },
            // R5G6B5_UNORM = 0xE8
            0xE8 => FormatTraits {
                num_components: 3,
                component_types: vec![Unorm, Unorm, Unorm],
                component_sizes: vec![5, 6, 5],
                component_swizzle: vec![R, G, B],
            },
            // A1R5G5B5_UNORM = 0xE9
            0xE9 => FormatTraits {
                num_components: 4,
                component_types: vec![Unorm, Unorm, Unorm, Unorm],
                component_sizes: vec![1, 5, 5, 5],
                component_swizzle: vec![A, R, G, B],
            },
            // R8G8_UNORM = 0xEA
            0xEA => FormatTraits {
                num_components: 2,
                component_types: vec![Unorm, Unorm],
                component_sizes: vec![8, 8],
                component_swizzle: vec![R, G],
            },
            // R8G8_SNORM = 0xEB
            0xEB => FormatTraits {
                num_components: 2,
                component_types: vec![Snorm, Snorm],
                component_sizes: vec![8, 8],
                component_swizzle: vec![R, G],
            },
            // R8G8_SINT = 0xEC
            0xEC => FormatTraits {
                num_components: 2,
                component_types: vec![Sint, Sint],
                component_sizes: vec![8, 8],
                component_swizzle: vec![R, G],
            },
            // R8G8_UINT = 0xED
            0xED => FormatTraits {
                num_components: 2,
                component_types: vec![Uint, Uint],
                component_sizes: vec![8, 8],
                component_swizzle: vec![R, G],
            },
            // R16_UNORM = 0xEE
            0xEE => FormatTraits {
                num_components: 1,
                component_types: vec![Unorm],
                component_sizes: vec![16],
                component_swizzle: vec![R],
            },
            // R16_SNORM = 0xEF
            0xEF => FormatTraits {
                num_components: 1,
                component_types: vec![Snorm],
                component_sizes: vec![16],
                component_swizzle: vec![R],
            },
            // R16_SINT = 0xF0
            0xF0 => FormatTraits {
                num_components: 1,
                component_types: vec![Sint],
                component_sizes: vec![16],
                component_swizzle: vec![R],
            },
            // R16_UINT = 0xF1
            0xF1 => FormatTraits {
                num_components: 1,
                component_types: vec![Uint],
                component_sizes: vec![16],
                component_swizzle: vec![R],
            },
            // R16_FLOAT = 0xF2
            0xF2 => FormatTraits {
                num_components: 1,
                component_types: vec![Float],
                component_sizes: vec![16],
                component_swizzle: vec![R],
            },
            // R8_UNORM = 0xF3
            0xF3 => FormatTraits {
                num_components: 1,
                component_types: vec![Unorm],
                component_sizes: vec![8],
                component_swizzle: vec![R],
            },
            // R8_SNORM = 0xF4
            0xF4 => FormatTraits {
                num_components: 1,
                component_types: vec![Snorm],
                component_sizes: vec![8],
                component_swizzle: vec![R],
            },
            // R8_SINT = 0xF5
            0xF5 => FormatTraits {
                num_components: 1,
                component_types: vec![Sint],
                component_sizes: vec![8],
                component_swizzle: vec![R],
            },
            // R8_UINT = 0xF6
            0xF6 => FormatTraits {
                num_components: 1,
                component_types: vec![Uint],
                component_sizes: vec![8],
                component_swizzle: vec![R],
            },
            // X1R5G5B5_UNORM = 0xF8
            0xF8 => FormatTraits {
                num_components: 4,
                component_types: vec![Unorm, Unorm, Unorm, Unorm],
                component_sizes: vec![1, 5, 5, 5],
                component_swizzle: vec![None, R, G, B],
            },
            // X8B8G8R8_UNORM = 0xF9
            0xF9 => FormatTraits {
                num_components: 4,
                component_types: vec![Unorm, Unorm, Unorm, Unorm],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![None, B, G, R],
            },
            // X8B8G8R8_SRGB = 0xFA
            0xFA => FormatTraits {
                num_components: 4,
                component_types: vec![Srgb, Srgb, Srgb, Srgb],
                component_sizes: vec![8, 8, 8, 8],
                component_swizzle: vec![None, B, G, R],
            },
            // Unknown format — use NullConverter
            _ => {
                log::warn!("Unimplemented format converter for format 0x{:X}", format);
                self.cache.insert(format, Box::new(NullConverter));
                return;
            }
        };

        self.cache
            .insert(format, Box::new(GenericConverter::new(traits)));
    }
}

impl Default for ConverterFactory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_srgb_lut_bounds() {
        assert_eq!(SRGB_TO_RGB_LUT.len(), 256);
        assert_eq!(RGB_TO_SRGB_LUT.len(), 256);
        // SRGB_TO_RGB_LUT: maps sRGB [0..255] to linear [0.0..1.0]
        assert!((SRGB_TO_RGB_LUT[0] - 0.0).abs() < 1e-7);
        assert!((SRGB_TO_RGB_LUT[255] - 1.0).abs() < 1e-7);
        // Eden's linear-to-sRGB table is a monotonic encoding, not a scale factor.
        assert!((RGB_TO_SRGB_LUT[0] - 0.0).abs() < 1e-7);
        assert_eq!(RGB_TO_SRGB_LUT[128], 7.366470e-01);
        assert_eq!(RGB_TO_SRGB_LUT[255], 1.0);
        assert!(RGB_TO_SRGB_LUT.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn linear_to_srgb_encodes_color_but_not_alpha() {
        let converter = GenericConverter::new(FormatTraits {
            num_components: 4,
            component_types: vec![ComponentType::Srgb; 4],
            component_sizes: vec![8; 4],
            component_swizzle: vec![Swizzle::R, Swizzle::G, Swizzle::B, Swizzle::A],
        });
        let mut output = [0u8; 8];
        converter.convert_from(
            &[0.0, 128.0 / 255.0, 1.0, 128.0 / 255.0, 1.0, 1.0, 1.0, 1.0],
            &mut output,
        );
        assert_eq!(output, [0, 187, 255, 128, 255, 255, 255, 255]);
    }

    #[test]
    fn four_word_pixels_preserve_components_without_cross_pixel_state() {
        let converter = GenericConverter::new(FormatTraits {
            num_components: 4,
            component_types: vec![ComponentType::Float; 4],
            component_sizes: vec![32; 4],
            component_swizzle: vec![Swizzle::R, Swizzle::G, Swizzle::B, Swizzle::A],
        });
        let values = [1.0f32, -2.0, 0.5, 16.0, 0.0, 0.0, 0.0, 0.0];
        let mut bytes = [0xff; 32];
        converter.convert_from(&values, &mut bytes);
        let mut restored = [99.0; 8];
        converter.convert_to(&bytes, &mut restored);
        assert_eq!(restored, values);
        assert_eq!(&bytes[16..], &[0; 16]);
    }

    #[test]
    fn test_converter_factory_creates_converters() {
        let mut factory = ConverterFactory::new();
        // A8B8G8R8_UNORM = 0xD5
        let converter = factory.get_format_converter(0xD5);
        // Input: A=0xFF, B=0x00, G=0x80, R=0x40  (byte order in memory)
        let input = [0xFF, 0x00, 0x80, 0x40];
        let mut output = [0.0f32; 4];
        converter.convert_to(&input, &mut output);
        // Swizzle is A,B,G,R so: component 0 -> A slot, component 1 -> B slot, etc.
        // R (index 0) = component with swizzle R = 0x40/255
        // G (index 1) = component with swizzle G = 0x80/255
        // B (index 2) = component with swizzle B = 0x00/255
        // A (index 3) = component with swizzle A = 0xFF/255
        assert!(
            (output[0] - (0x40 as f32 / 255.0)).abs() < 0.01,
            "R mismatch: {}",
            output[0]
        );
        assert!(
            (output[1] - (0x80 as f32 / 255.0)).abs() < 0.01,
            "G mismatch: {}",
            output[1]
        );
        assert!(
            (output[2] - (0x00 as f32 / 255.0)).abs() < 0.01,
            "B mismatch: {}",
            output[2]
        );
        assert!(
            (output[3] - (0xFF as f32 / 255.0)).abs() < 0.01,
            "A mismatch: {}",
            output[3]
        );
    }

    #[test]
    fn test_null_converter_for_unknown_format() {
        let mut factory = ConverterFactory::new();
        let converter = factory.get_format_converter(0x01); // Unknown
        let input = [0xFF; 4];
        let mut output = [1.0f32; 4];
        converter.convert_to(&input, &mut output);
        assert!(output.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn test_r32_float_roundtrip() {
        let mut factory = ConverterFactory::new();
        // R32_FLOAT = 0xE5
        let converter = factory.get_format_converter(0xE5);
        let val: f32 = 3.14159;
        let input = val.to_le_bytes();
        let mut ir = [0.0f32; 4];
        converter.convert_to(&input, &mut ir);
        assert!(
            (ir[0] - val).abs() < 1e-6,
            "R mismatch: {} vs {}",
            ir[0],
            val
        );
        assert_eq!(ir[1], 0.0);
        assert_eq!(ir[2], 0.0);
        assert_eq!(ir[3], 0.0);

        // Roundtrip back.
        let mut output = [0u8; 4];
        converter.convert_from(&ir, &mut output);
        let result = f32::from_le_bytes(output);
        assert!(
            (result - val).abs() < 1e-6,
            "Roundtrip mismatch: {} vs {}",
            result,
            val
        );
    }

    #[test]
    fn test_r8_unorm_roundtrip() {
        let mut factory = ConverterFactory::new();
        // R8_UNORM = 0xF3
        let converter = factory.get_format_converter(0xF3);
        let input = [128u8];
        let mut ir = [0.0f32; 4];
        converter.convert_to(&input, &mut ir);
        assert!((ir[0] - (128.0 / 255.0)).abs() < 0.01);

        let mut output = [0u8; 1];
        converter.convert_from(&ir, &mut output);
        assert_eq!(output[0], 128);
    }

    #[test]
    fn test_r16g16_float_convert() {
        let mut factory = ConverterFactory::new();
        // R16G16_FLOAT = 0xDE
        let converter = factory.get_format_converter(0xDE);
        // FP16 for 1.0 = 0x3C00
        let input: [u8; 4] = [0x00, 0x3C, 0x00, 0x40]; // R=1.0, G=2.0 in FP16
        let mut ir = [0.0f32; 4];
        converter.convert_to(&input, &mut ir);
        assert!((ir[0] - 1.0).abs() < 0.01, "R mismatch: {}", ir[0]);
        assert!((ir[1] - 2.0).abs() < 0.01, "G mismatch: {}", ir[1]);
    }

    #[test]
    fn r16_float_unpack_preserves_fractional_mantissa() {
        let mut factory = ConverterFactory::new();
        let converter = factory.get_format_converter(0xF2); // R16_FLOAT
        let input = 0x3E00u16.to_le_bytes(); // 1.5 in IEEE 754 binary16
        let mut ir = [0.0f32; 4];

        converter.convert_to(&input, &mut ir);

        assert_eq!(ir[0], 1.5);
    }

    #[test]
    fn test_r32g32_uint_convert() {
        let mut factory = ConverterFactory::new();
        // R32G32_UINT = 0xCD
        let converter = factory.get_format_converter(0xCD);
        let mut input = [0u8; 8];
        input[0..4].copy_from_slice(&42u32.to_le_bytes());
        input[4..8].copy_from_slice(&99u32.to_le_bytes());
        let mut ir = [0.0f32; 4];
        converter.convert_to(&input, &mut ir);
        assert_eq!(ir[0] as u32, 42);
        assert_eq!(ir[1] as u32, 99);
    }
}
