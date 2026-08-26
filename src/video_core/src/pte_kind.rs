// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/pte_kind.h`.
//!
//! Tegra page table entry kinds.
//! https://github.com/NVIDIA/open-gpu-doc/blob/master/manuals/volta/gv100/dev_mmu.ref.txt

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct PteKind(u8);

impl PteKind {
    pub const INVALID: Self = Self(0xff);
    pub const PITCH: Self = Self(0x00);
    pub const Z16: Self = Self(0x01);
    pub const Z16_2C: Self = Self(0x02);
    pub const Z16_MS2_2C: Self = Self(0x03);
    pub const Z16_MS4_2C: Self = Self(0x04);
    pub const Z16_MS8_2C: Self = Self(0x05);
    pub const Z16_MS16_2C: Self = Self(0x06);
    pub const Z16_2Z: Self = Self(0x07);
    pub const Z16_MS2_2Z: Self = Self(0x08);
    pub const Z16_MS4_2Z: Self = Self(0x09);
    pub const Z16_MS8_2Z: Self = Self(0x0a);
    pub const Z16_MS16_2Z: Self = Self(0x0b);
    pub const Z16_4CZ: Self = Self(0x0c);
    pub const Z16_MS2_4CZ: Self = Self(0x0d);
    pub const Z16_MS4_4CZ: Self = Self(0x0e);
    pub const Z16_MS8_4CZ: Self = Self(0x0f);
    pub const Z16_MS16_4CZ: Self = Self(0x10);
    pub const S8Z24: Self = Self(0x11);
    pub const S8Z24_1Z: Self = Self(0x12);
    pub const S8Z24_MS2_1Z: Self = Self(0x13);
    pub const S8Z24_MS4_1Z: Self = Self(0x14);
    pub const S8Z24_MS8_1Z: Self = Self(0x15);
    pub const S8Z24_MS16_1Z: Self = Self(0x16);
    pub const S8Z24_2CZ: Self = Self(0x17);
    pub const S8Z24_MS2_2CZ: Self = Self(0x18);
    pub const S8Z24_MS4_2CZ: Self = Self(0x19);
    pub const S8Z24_MS8_2CZ: Self = Self(0x1a);
    pub const S8Z24_MS16_2CZ: Self = Self(0x1b);
    pub const S8Z24_2CS: Self = Self(0x1c);
    pub const S8Z24_MS2_2CS: Self = Self(0x1d);
    pub const S8Z24_MS4_2CS: Self = Self(0x1e);
    pub const S8Z24_MS8_2CS: Self = Self(0x1f);
    pub const S8Z24_MS16_2CS: Self = Self(0x20);
    pub const S8Z24_4CSZV: Self = Self(0x21);
    pub const S8Z24_MS2_4CSZV: Self = Self(0x22);
    pub const S8Z24_MS4_4CSZV: Self = Self(0x23);
    pub const S8Z24_MS8_4CSZV: Self = Self(0x24);
    pub const S8Z24_MS16_4CSZV: Self = Self(0x25);
    pub const V8Z24_MS4_VC12: Self = Self(0x26);
    pub const V8Z24_MS4_VC4: Self = Self(0x27);
    pub const V8Z24_MS8_VC8: Self = Self(0x28);
    pub const V8Z24_MS8_VC24: Self = Self(0x29);
    pub const V8Z24_MS4_VC12_1ZV: Self = Self(0x2e);
    pub const V8Z24_MS4_VC4_1ZV: Self = Self(0x2f);
    pub const V8Z24_MS8_VC8_1ZV: Self = Self(0x30);
    pub const V8Z24_MS8_VC24_1ZV: Self = Self(0x31);
    pub const V8Z24_MS4_VC12_2CS: Self = Self(0x32);
    pub const V8Z24_MS4_VC4_2CS: Self = Self(0x33);
    pub const V8Z24_MS8_VC8_2CS: Self = Self(0x34);
    pub const V8Z24_MS8_VC24_2CS: Self = Self(0x35);
    pub const V8Z24_MS4_VC12_2CZV: Self = Self(0x3a);
    pub const V8Z24_MS4_VC4_2CZV: Self = Self(0x3b);
    pub const V8Z24_MS8_VC8_2CZV: Self = Self(0x3c);
    pub const V8Z24_MS8_VC24_2CZV: Self = Self(0x3d);
    pub const V8Z24_MS4_VC12_2ZV: Self = Self(0x3e);
    pub const V8Z24_MS4_VC4_2ZV: Self = Self(0x3f);
    pub const V8Z24_MS8_VC8_2ZV: Self = Self(0x40);
    pub const V8Z24_MS8_VC24_2ZV: Self = Self(0x41);
    pub const V8Z24_MS4_VC12_4CSZV: Self = Self(0x42);
    pub const V8Z24_MS4_VC4_4CSZV: Self = Self(0x43);
    pub const V8Z24_MS8_VC8_4CSZV: Self = Self(0x44);
    pub const V8Z24_MS8_VC24_4CSZV: Self = Self(0x45);
    pub const Z24S8: Self = Self(0x46);
    pub const Z24S8_1Z: Self = Self(0x47);
    pub const Z24S8_MS2_1Z: Self = Self(0x48);
    pub const Z24S8_MS4_1Z: Self = Self(0x49);
    pub const Z24S8_MS8_1Z: Self = Self(0x4a);
    pub const Z24S8_MS16_1Z: Self = Self(0x4b);
    pub const Z24S8_2CS: Self = Self(0x4c);
    pub const Z24S8_MS2_2CS: Self = Self(0x4d);
    pub const Z24S8_MS4_2CS: Self = Self(0x4e);
    pub const Z24S8_MS8_2CS: Self = Self(0x4f);
    pub const Z24S8_MS16_2CS: Self = Self(0x50);
    pub const Z24S8_2CZ: Self = Self(0x51);
    pub const Z24S8_MS2_2CZ: Self = Self(0x52);
    pub const Z24S8_MS4_2CZ: Self = Self(0x53);
    pub const Z24S8_MS8_2CZ: Self = Self(0x54);
    pub const Z24S8_MS16_2CZ: Self = Self(0x55);
    pub const Z24S8_4CSZV: Self = Self(0x56);
    pub const Z24S8_MS2_4CSZV: Self = Self(0x57);
    pub const Z24S8_MS4_4CSZV: Self = Self(0x58);
    pub const Z24S8_MS8_4CSZV: Self = Self(0x59);
    pub const Z24S8_MS16_4CSZV: Self = Self(0x5a);
    pub const Z24V8_MS4_VC12: Self = Self(0x5b);
    pub const Z24V8_MS4_VC4: Self = Self(0x5c);
    pub const Z24V8_MS8_VC8: Self = Self(0x5d);
    pub const Z24V8_MS8_VC24: Self = Self(0x5e);
    pub const Z24V8_MS4_VC12_1ZV: Self = Self(0x63);
    pub const Z24V8_MS4_VC4_1ZV: Self = Self(0x64);
    pub const Z24V8_MS8_VC8_1ZV: Self = Self(0x65);
    pub const Z24V8_MS8_VC24_1ZV: Self = Self(0x66);
    pub const Z24V8_MS4_VC12_2CS: Self = Self(0x67);
    pub const Z24V8_MS4_VC4_2CS: Self = Self(0x68);
    pub const Z24V8_MS8_VC8_2CS: Self = Self(0x69);
    pub const Z24V8_MS8_VC24_2CS: Self = Self(0x6a);
    pub const Z24V8_MS4_VC12_2CZV: Self = Self(0x6f);
    pub const Z24V8_MS4_VC4_2CZV: Self = Self(0x70);
    pub const Z24V8_MS8_VC8_2CZV: Self = Self(0x71);
    pub const Z24V8_MS8_VC24_2CZV: Self = Self(0x72);
    pub const Z24V8_MS4_VC12_2ZV: Self = Self(0x73);
    pub const Z24V8_MS4_VC4_2ZV: Self = Self(0x74);
    pub const Z24V8_MS8_VC8_2ZV: Self = Self(0x75);
    pub const Z24V8_MS8_VC24_2ZV: Self = Self(0x76);
    pub const Z24V8_MS4_VC12_4CSZV: Self = Self(0x77);
    pub const Z24V8_MS4_VC4_4CSZV: Self = Self(0x78);
    pub const Z24V8_MS8_VC8_4CSZV: Self = Self(0x79);
    pub const Z24V8_MS8_VC24_4CSZV: Self = Self(0x7a);
    pub const ZF32: Self = Self(0x7b);
    pub const ZF32_1Z: Self = Self(0x7c);
    pub const ZF32_MS2_1Z: Self = Self(0x7d);
    pub const ZF32_MS4_1Z: Self = Self(0x7e);
    pub const ZF32_MS8_1Z: Self = Self(0x7f);
    pub const ZF32_MS16_1Z: Self = Self(0x80);
    pub const ZF32_2CS: Self = Self(0x81);
    pub const ZF32_MS2_2CS: Self = Self(0x82);
    pub const ZF32_MS4_2CS: Self = Self(0x83);
    pub const ZF32_MS8_2CS: Self = Self(0x84);
    pub const ZF32_MS16_2CS: Self = Self(0x85);
    pub const ZF32_2CZ: Self = Self(0x86);
    pub const ZF32_MS2_2CZ: Self = Self(0x87);
    pub const ZF32_MS4_2CZ: Self = Self(0x88);
    pub const ZF32_MS8_2CZ: Self = Self(0x89);
    pub const ZF32_MS16_2CZ: Self = Self(0x8a);
    pub const X8Z24_X16V8S8_MS4_VC12: Self = Self(0x8b);
    pub const X8Z24_X16V8S8_MS4_VC4: Self = Self(0x8c);
    pub const X8Z24_X16V8S8_MS8_VC8: Self = Self(0x8d);
    pub const X8Z24_X16V8S8_MS8_VC24: Self = Self(0x8e);
    pub const X8Z24_X16V8S8_MS4_VC12_1CS: Self = Self(0x8f);
    pub const X8Z24_X16V8S8_MS4_VC4_1CS: Self = Self(0x90);
    pub const X8Z24_X16V8S8_MS8_VC8_1CS: Self = Self(0x91);
    pub const X8Z24_X16V8S8_MS8_VC24_1CS: Self = Self(0x92);
    pub const X8Z24_X16V8S8_MS4_VC12_1ZV: Self = Self(0x97);
    pub const X8Z24_X16V8S8_MS4_VC4_1ZV: Self = Self(0x98);
    pub const X8Z24_X16V8S8_MS8_VC8_1ZV: Self = Self(0x99);
    pub const X8Z24_X16V8S8_MS8_VC24_1ZV: Self = Self(0x9a);
    pub const X8Z24_X16V8S8_MS4_VC12_1CZV: Self = Self(0x9b);
    pub const X8Z24_X16V8S8_MS4_VC4_1CZV: Self = Self(0x9c);
    pub const X8Z24_X16V8S8_MS8_VC8_1CZV: Self = Self(0x9d);
    pub const X8Z24_X16V8S8_MS8_VC24_1CZV: Self = Self(0x9e);
    pub const X8Z24_X16V8S8_MS4_VC12_2CS: Self = Self(0x9f);
    pub const X8Z24_X16V8S8_MS4_VC4_2CS: Self = Self(0xa0);
    pub const X8Z24_X16V8S8_MS8_VC8_2CS: Self = Self(0xa1);
    pub const X8Z24_X16V8S8_MS8_VC24_2CS: Self = Self(0xa2);
    pub const X8Z24_X16V8S8_MS4_VC12_2CSZV: Self = Self(0xa3);
    pub const X8Z24_X16V8S8_MS4_VC4_2CSZV: Self = Self(0xa4);
    pub const X8Z24_X16V8S8_MS8_VC8_2CSZV: Self = Self(0xa5);
    pub const X8Z24_X16V8S8_MS8_VC24_2CSZV: Self = Self(0xa6);
    pub const ZF32_X16V8S8_MS4_VC12: Self = Self(0xa7);
    pub const ZF32_X16V8S8_MS4_VC4: Self = Self(0xa8);
    pub const ZF32_X16V8S8_MS8_VC8: Self = Self(0xa9);
    pub const ZF32_X16V8S8_MS8_VC24: Self = Self(0xaa);
    pub const ZF32_X16V8S8_MS4_VC12_1CS: Self = Self(0xab);
    pub const ZF32_X16V8S8_MS4_VC4_1CS: Self = Self(0xac);
    pub const ZF32_X16V8S8_MS8_VC8_1CS: Self = Self(0xad);
    pub const ZF32_X16V8S8_MS8_VC24_1CS: Self = Self(0xae);
    pub const ZF32_X16V8S8_MS4_VC12_1ZV: Self = Self(0xb3);
    pub const ZF32_X16V8S8_MS4_VC4_1ZV: Self = Self(0xb4);
    pub const ZF32_X16V8S8_MS8_VC8_1ZV: Self = Self(0xb5);
    pub const ZF32_X16V8S8_MS8_VC24_1ZV: Self = Self(0xb6);
    pub const ZF32_X16V8S8_MS4_VC12_1CZV: Self = Self(0xb7);
    pub const ZF32_X16V8S8_MS4_VC4_1CZV: Self = Self(0xb8);
    pub const ZF32_X16V8S8_MS8_VC8_1CZV: Self = Self(0xb9);
    pub const ZF32_X16V8S8_MS8_VC24_1CZV: Self = Self(0xba);
    pub const ZF32_X16V8S8_MS4_VC12_2CS: Self = Self(0xbb);
    pub const ZF32_X16V8S8_MS4_VC4_2CS: Self = Self(0xbc);
    pub const ZF32_X16V8S8_MS8_VC8_2CS: Self = Self(0xbd);
    pub const ZF32_X16V8S8_MS8_VC24_2CS: Self = Self(0xbe);
    pub const ZF32_X16V8S8_MS4_VC12_2CSZV: Self = Self(0xbf);
    pub const ZF32_X16V8S8_MS4_VC4_2CSZV: Self = Self(0xc0);
    pub const ZF32_X16V8S8_MS8_VC8_2CSZV: Self = Self(0xc1);
    pub const ZF32_X16V8S8_MS8_VC24_2CSZV: Self = Self(0xc2);
    pub const ZF32_X24S8: Self = Self(0xc3);
    pub const ZF32_X24S8_1CS: Self = Self(0xc4);
    pub const ZF32_X24S8_MS2_1CS: Self = Self(0xc5);
    pub const ZF32_X24S8_MS4_1CS: Self = Self(0xc6);
    pub const ZF32_X24S8_MS8_1CS: Self = Self(0xc7);
    pub const ZF32_X24S8_MS16_1CS: Self = Self(0xc8);
    pub const ZF32_X24S8_2CSZV: Self = Self(0xce);
    pub const ZF32_X24S8_MS2_2CSZV: Self = Self(0xcf);
    pub const ZF32_X24S8_MS4_2CSZV: Self = Self(0xd0);
    pub const ZF32_X24S8_MS8_2CSZV: Self = Self(0xd1);
    pub const ZF32_X24S8_MS16_2CSZV: Self = Self(0xd2);
    pub const ZF32_X24S8_2CS: Self = Self(0xd3);
    pub const ZF32_X24S8_MS2_2CS: Self = Self(0xd4);
    pub const ZF32_X24S8_MS4_2CS: Self = Self(0xd5);
    pub const ZF32_X24S8_MS8_2CS: Self = Self(0xd6);
    pub const ZF32_X24S8_MS16_2CS: Self = Self(0xd7);
    pub const S8: Self = Self(0x2a);
    pub const S8_2S: Self = Self(0x2b);
    pub const GENERIC_16BX2: Self = Self(0xfe);
    pub const C32_2C: Self = Self(0xd8);
    pub const C32_2CBR: Self = Self(0xd9);
    pub const C32_2CBA: Self = Self(0xda);
    pub const C32_2CRA: Self = Self(0xdb);
    pub const C32_2BRA: Self = Self(0xdc);
    pub const C32_MS2_2C: Self = Self(0xdd);
    pub const C32_MS2_2CBR: Self = Self(0xde);
    pub const C32_MS2_2CRA: Self = Self(0xcc);
    pub const C32_MS4_2C: Self = Self(0xdf);
    pub const C32_MS4_2CBR: Self = Self(0xe0);
    pub const C32_MS4_2CBA: Self = Self(0xe1);
    pub const C32_MS4_2CRA: Self = Self(0xe2);
    pub const C32_MS4_2BRA: Self = Self(0xe3);
    pub const C32_MS8_MS16_2C: Self = Self(0xe4);
    pub const C32_MS8_MS16_2CRA: Self = Self(0xe5);
    pub const C64_2C: Self = Self(0xe6);
    pub const C64_2CBR: Self = Self(0xe7);
    pub const C64_2CBA: Self = Self(0xe8);
    pub const C64_2CRA: Self = Self(0xe9);
    pub const C64_2BRA: Self = Self(0xea);
    pub const C64_MS2_2C: Self = Self(0xeb);
    pub const C64_MS2_2CBR: Self = Self(0xec);
    pub const C64_MS2_2CRA: Self = Self(0xcd);
    pub const C64_MS4_2C: Self = Self(0xed);
    pub const C64_MS4_2CBR: Self = Self(0xee);
    pub const C64_MS4_2CBA: Self = Self(0xef);
    pub const C64_MS4_2CRA: Self = Self(0xf0);
    pub const C64_MS4_2BRA: Self = Self(0xf1);
    pub const C64_MS8_MS16_2C: Self = Self(0xf2);
    pub const C64_MS8_MS16_2CRA: Self = Self(0xf3);
    pub const C128_2C: Self = Self(0xf4);
    pub const C128_2CR: Self = Self(0xf5);
    pub const C128_MS2_2C: Self = Self(0xf6);
    pub const C128_MS2_2CR: Self = Self(0xf7);
    pub const C128_MS4_2C: Self = Self(0xf8);
    pub const C128_MS4_2CR: Self = Self(0xf9);
    pub const C128_MS8_MS16_2C: Self = Self(0xfa);
    pub const C128_MS8_MS16_2CR: Self = Self(0xfb);
    pub const X8C24: Self = Self(0xfc);
    pub const PITCH_NO_SWIZZLE: Self = Self(0xfd);
    pub const SMASKED_MESSAGE: Self = Self(0xca);
    pub const SMHOST_MESSAGE: Self = Self(0xcb);

    pub const fn from_raw(raw: u8) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u8 {
        self.0
    }
}

impl Default for PteKind {
    fn default() -> Self {
        Self::INVALID
    }
}

/// Returns true if the PTE kind is a pitch (linear) kind.
pub const fn is_pitch_kind(kind: PteKind) -> bool {
    kind.raw() == PteKind::PITCH.raw() || kind.raw() == PteKind::PITCH_NO_SWIZZLE.raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_pte_kinds_preserve_upstream_names_and_values() {
        assert_eq!(PteKind::C32_MS2_2CRA.raw(), 0xcc);
        assert_eq!(PteKind::C64_MS2_2CRA.raw(), 0xcd);
        assert_eq!(PteKind::SMASKED_MESSAGE.raw(), 0xca);
        assert_eq!(PteKind::SMHOST_MESSAGE.raw(), 0xcb);
        assert_eq!(PteKind::GENERIC_16BX2.raw(), 0xfe);
        assert_eq!(PteKind::INVALID.raw(), 0xff);
    }

    #[test]
    fn only_upstream_pitch_kinds_are_linear() {
        assert!(is_pitch_kind(PteKind::PITCH));
        assert!(is_pitch_kind(PteKind::PITCH_NO_SWIZZLE));
        assert!(!is_pitch_kind(PteKind::Z16));
        assert!(!is_pitch_kind(PteKind::INVALID));
    }
}
