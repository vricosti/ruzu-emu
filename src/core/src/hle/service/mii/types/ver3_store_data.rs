// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/mii/types/ver3_store_data.h
//! Port of zuyu/src/core/hle/service/mii/types/ver3_store_data.cpp
//!
//! Ver3StoreData: the version 3 (3DS-compatible) Mii storage format.

use super::raw_data;
use super::store_data::StoreData;
use crate::hle::service::mii::mii_types::{
    BeardType, EyeType, EyebrowType, FacelineMake, FacelineType, FacelineWrinkle, FavoriteColor,
    Gender, HairType, MoleType, MouthType, MustacheType, Nickname, NoseType, MAX_BUILD,
    MAX_EYEBROW_ASPECT, MAX_EYEBROW_ROTATE, MAX_EYEBROW_SCALE, MAX_EYEBROW_X, MAX_EYEBROW_Y,
    MAX_EYE_ASPECT, MAX_EYE_ROTATE, MAX_EYE_SCALE, MAX_EYE_X, MAX_EYE_Y, MAX_GLASS_SCALE,
    MAX_GLASS_Y, MAX_HEIGHT, MAX_MOLE_SCALE, MAX_MOLE_X, MAX_MOLE_Y, MAX_MOUTH_ASPECT,
    MAX_MOUTH_SCALE, MAX_MOUTH_Y, MAX_MUSTACHE_SCALE, MAX_MUSTACHE_Y, MAX_NAME_SIZE,
    MAX_NOSE_SCALE, MAX_NOSE_Y, MAX_VER3_COMMON_COLOR, MAX_VER3_GLASS_TYPE,
};
use crate::hle::service::mii::mii_util;

const VERSION_OFFSET: usize = 0x00;
const REGION_INFORMATION_OFFSET: usize = 0x01;
const MII_INFORMATION_OFFSET: usize = 0x18;
const MII_NAME_OFFSET: usize = 0x1A;
const HEIGHT_OFFSET: usize = 0x2E;
const BUILD_OFFSET: usize = 0x2F;
const APPEARANCE_BITS1_OFFSET: usize = 0x30;
const APPEARANCE_BITS2_OFFSET: usize = 0x31;
const HAIR_TYPE_OFFSET: usize = 0x32;
const APPEARANCE_BITS3_OFFSET: usize = 0x33;
const APPEARANCE_BITS4_OFFSET: usize = 0x34;
const APPEARANCE_BITS5_OFFSET: usize = 0x38;
const APPEARANCE_BITS6_OFFSET: usize = 0x3C;
const APPEARANCE_BITS7_OFFSET: usize = 0x3E;
const APPEARANCE_BITS8_OFFSET: usize = 0x40;
const APPEARANCE_BITS9_OFFSET: usize = 0x42;
const APPEARANCE_BITS10_OFFSET: usize = 0x44;
const APPEARANCE_BITS11_OFFSET: usize = 0x46;
const CRC_OFFSET: usize = 0x5E;

#[inline]
fn get_bits_u8(value: u8, offset: u32, width: u32) -> u8 {
    ((value >> offset) & ((1u8 << width) - 1)) as u8
}

#[inline]
fn get_bits_u16(value: u16, offset: u32, width: u32) -> u16 {
    (value >> offset) & ((1u16 << width) - 1)
}

#[inline]
fn get_bits_u32(value: u32, offset: u32, width: u32) -> u32 {
    (value >> offset) & ((1u32 << width) - 1)
}

#[inline]
fn set_bits_u8(value: &mut u8, offset: u32, width: u32, field: u8) {
    let mask = ((1u8 << width) - 1) << offset;
    *value = (*value & !mask) | ((field << offset) & mask);
}

#[inline]
fn set_bits_u16(value: &mut u16, offset: u32, width: u32, field: u16) {
    let mask = ((1u16 << width) - 1) << offset;
    *value = (*value & !mask) | ((field << offset) & mask);
}

#[inline]
fn set_bits_u32(value: &mut u32, offset: u32, width: u32, field: u32) {
    let mask = ((1u32 << width) - 1) << offset;
    *value = (*value & !mask) | ((field << offset) & mask);
}

/// Corresponds to `NfpStoreDataExtension` in upstream ver3_store_data.h.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NfpStoreDataExtension {
    pub faceline_color: u8,
    pub hair_color: u8,
    pub eye_color: u8,
    pub eyebrow_color: u8,
    pub mouth_color: u8,
    pub beard_color: u8,
    pub glass_color: u8,
    pub glass_type: u8,
}

impl NfpStoreDataExtension {
    pub fn set_from_store_data(&mut self, store_data: &StoreData) {
        self.faceline_color = (store_data.get_faceline_color() as u8) & 0x0f;
        self.hair_color = store_data.get_hair_color().0 & 0x7f;
        self.eye_color = store_data.get_eye_color().0 & 0x7f;
        self.eyebrow_color = store_data.get_eyebrow_color().0 & 0x7f;
        self.mouth_color = store_data.get_mouth_color().0 & 0x7f;
        self.beard_color = store_data.get_beard_color().0 & 0x7f;
        self.glass_color = store_data.get_glass_color().0 & 0x7f;
        self.glass_type = (store_data.get_glass_type() as u8) & 0x1f;
    }
}

const _: () = assert!(core::mem::size_of::<NfpStoreDataExtension>() == 0x8);

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Ver3StoreData {
    pub data: [u8; 0x60],
}

impl Ver3StoreData {
    pub fn new() -> Self {
        Self { data: [0u8; 0x60] }
    }

    fn read_u8(&self, offset: usize) -> u8 {
        self.data[offset]
    }

    fn read_u16_le(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.data[offset], self.data[offset + 1]])
    }

    fn read_u32_le(&self, offset: usize) -> u32 {
        u32::from_le_bytes([
            self.data[offset],
            self.data[offset + 1],
            self.data[offset + 2],
            self.data[offset + 3],
        ])
    }

    fn write_u8(&mut self, offset: usize, value: u8) {
        self.data[offset] = value;
    }

    fn write_u16_le(&mut self, offset: usize, value: u16) {
        self.data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u16_be(&mut self, offset: usize, value: u16) {
        self.data[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn write_u32_le(&mut self, offset: usize, value: u32) {
        self.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_nickname(&mut self, offset: usize, nickname: Nickname) {
        for (index, ch) in nickname.data.into_iter().enumerate() {
            let base = offset + index * 2;
            self.data[base..base + 2].copy_from_slice(&ch.to_le_bytes());
        }
    }

    fn nickname_at(&self, offset: usize) -> Nickname {
        let mut nickname = Nickname::default();
        let mut index = 0;
        while index < MAX_NAME_SIZE {
            let base = offset + index * 2;
            nickname.data[index] = u16::from_le_bytes([self.data[base], self.data[base + 1]]);
            index += 1;
        }
        nickname
    }

    fn version(&self) -> u8 {
        self.read_u8(VERSION_OFFSET)
    }

    fn font_region(&self) -> u8 {
        get_bits_u8(self.read_u8(REGION_INFORMATION_OFFSET), 4, 2)
    }

    fn gender(&self) -> u8 {
        get_bits_u16(self.read_u16_le(MII_INFORMATION_OFFSET), 0, 1) as u8
    }

    fn birth_month(&self) -> u8 {
        get_bits_u16(self.read_u16_le(MII_INFORMATION_OFFSET), 1, 4) as u8
    }

    fn birth_day(&self) -> u8 {
        get_bits_u16(self.read_u16_le(MII_INFORMATION_OFFSET), 5, 5) as u8
    }

    fn favorite_color(&self) -> u8 {
        get_bits_u16(self.read_u16_le(MII_INFORMATION_OFFSET), 10, 4) as u8
    }

    fn nickname(&self) -> Nickname {
        self.nickname_at(MII_NAME_OFFSET)
    }

    fn height(&self) -> u8 {
        self.read_u8(HEIGHT_OFFSET)
    }

    fn build(&self) -> u8 {
        self.read_u8(BUILD_OFFSET)
    }

    fn appearance_bits1(&self) -> u8 {
        self.read_u8(APPEARANCE_BITS1_OFFSET)
    }

    fn appearance_bits2(&self) -> u8 {
        self.read_u8(APPEARANCE_BITS2_OFFSET)
    }

    fn hair_type(&self) -> u8 {
        self.read_u8(HAIR_TYPE_OFFSET)
    }

    fn appearance_bits3(&self) -> u8 {
        self.read_u8(APPEARANCE_BITS3_OFFSET)
    }

    fn appearance_bits4(&self) -> u32 {
        self.read_u32_le(APPEARANCE_BITS4_OFFSET)
    }

    fn appearance_bits5(&self) -> u32 {
        self.read_u32_le(APPEARANCE_BITS5_OFFSET)
    }

    fn appearance_bits6(&self) -> u16 {
        self.read_u16_le(APPEARANCE_BITS6_OFFSET)
    }

    fn appearance_bits7(&self) -> u16 {
        self.read_u16_le(APPEARANCE_BITS7_OFFSET)
    }

    fn appearance_bits8(&self) -> u8 {
        self.read_u8(APPEARANCE_BITS8_OFFSET)
    }

    fn appearance_bits9(&self) -> u16 {
        self.read_u16_le(APPEARANCE_BITS9_OFFSET)
    }

    fn appearance_bits10(&self) -> u16 {
        self.read_u16_le(APPEARANCE_BITS10_OFFSET)
    }

    fn appearance_bits11(&self) -> u16 {
        self.read_u16_le(APPEARANCE_BITS11_OFFSET)
    }

    pub fn build_to_store_data(&self, out_store_data: &mut StoreData) {
        out_store_data.build_base(Gender::Male);

        out_store_data
            .core_data
            .set_gender(unsafe { core::mem::transmute(self.gender()) });
        out_store_data
            .core_data
            .set_favorite_color(unsafe { core::mem::transmute(self.favorite_color()) });
        out_store_data.core_data.set_height(self.height());
        out_store_data.core_data.set_build(self.build());
        out_store_data.core_data.set_nickname(self.nickname());
        out_store_data
            .core_data
            .set_font_region(unsafe { core::mem::transmute(self.font_region()) });

        let bits1 = self.appearance_bits1();
        let bits2 = self.appearance_bits2();
        let bits3 = self.appearance_bits3();
        let bits4 = self.appearance_bits4();
        let bits5 = self.appearance_bits5();
        let bits6 = self.appearance_bits6();
        let bits7 = self.appearance_bits7();
        let bits8 = self.appearance_bits8();
        let bits9 = self.appearance_bits9();
        let bits10 = self.appearance_bits10();
        let bits11 = self.appearance_bits11();

        out_store_data
            .core_data
            .set_faceline_type(unsafe { core::mem::transmute(get_bits_u8(bits1, 1, 4)) });
        out_store_data
            .core_data
            .set_faceline_color(raw_data::get_faceline_color_from_ver3(
                get_bits_u8(bits1, 5, 3) as u32,
            ));
        out_store_data
            .core_data
            .set_faceline_wrinkle(unsafe { core::mem::transmute(get_bits_u8(bits2, 0, 4)) });
        out_store_data
            .core_data
            .set_faceline_make(unsafe { core::mem::transmute(get_bits_u8(bits2, 4, 4)) });

        out_store_data
            .core_data
            .set_hair_type(unsafe { core::mem::transmute(self.hair_type()) });
        out_store_data
            .core_data
            .set_hair_color(
                raw_data::get_hair_color_from_ver3(get_bits_u8(bits3, 0, 3) as u32) as u32,
            );
        out_store_data
            .core_data
            .set_hair_flip(unsafe { core::mem::transmute(get_bits_u8(bits3, 3, 1)) });

        out_store_data
            .core_data
            .set_eye_type(unsafe { core::mem::transmute(get_bits_u32(bits4, 0, 6) as u8) });
        out_store_data
            .core_data
            .set_eye_color(raw_data::get_eye_color_from_ver3(get_bits_u32(bits4, 6, 3)) as u32);
        out_store_data
            .core_data
            .set_eye_scale(get_bits_u32(bits4, 9, 4) as u8);
        out_store_data
            .core_data
            .set_eye_aspect(get_bits_u32(bits4, 13, 3) as u8);
        out_store_data
            .core_data
            .set_eye_rotate(get_bits_u32(bits4, 16, 5) as u8);
        out_store_data
            .core_data
            .set_eye_x(get_bits_u32(bits4, 21, 4) as u8);
        out_store_data
            .core_data
            .set_eye_y(get_bits_u32(bits4, 25, 5) as u8);

        out_store_data
            .core_data
            .set_eyebrow_type(unsafe { core::mem::transmute(get_bits_u32(bits5, 0, 5) as u8) });
        out_store_data.core_data.set_eyebrow_color(
            raw_data::get_hair_color_from_ver3(get_bits_u32(bits5, 5, 3)) as u32,
        );
        out_store_data
            .core_data
            .set_eyebrow_scale(get_bits_u32(bits5, 8, 4) as u8);
        out_store_data
            .core_data
            .set_eyebrow_aspect(get_bits_u32(bits5, 12, 3) as u8);
        out_store_data
            .core_data
            .set_eyebrow_rotate(get_bits_u32(bits5, 16, 4) as u8);
        out_store_data
            .core_data
            .set_eyebrow_x(get_bits_u32(bits5, 21, 4) as u8);
        out_store_data
            .core_data
            .set_eyebrow_y((get_bits_u32(bits5, 25, 5) as i32 - 3) as u8);

        out_store_data
            .core_data
            .set_nose_type(unsafe { core::mem::transmute(get_bits_u16(bits6, 0, 5) as u8) });
        out_store_data
            .core_data
            .set_nose_scale(get_bits_u16(bits6, 5, 4) as u8);
        out_store_data
            .core_data
            .set_nose_y(get_bits_u16(bits6, 9, 5) as u8);

        out_store_data
            .core_data
            .set_mouth_type(unsafe { core::mem::transmute(get_bits_u16(bits7, 0, 6) as u8) });
        out_store_data
            .core_data
            .set_mouth_color(
                raw_data::get_mouth_color_from_ver3(get_bits_u16(bits7, 6, 3) as u32) as u32,
            );
        out_store_data
            .core_data
            .set_mouth_scale(get_bits_u16(bits7, 9, 4) as u8);
        out_store_data
            .core_data
            .set_mouth_aspect(get_bits_u16(bits7, 13, 3) as u8);
        out_store_data
            .core_data
            .set_mouth_y(get_bits_u8(bits8, 0, 5));

        out_store_data
            .core_data
            .set_mustache_type_enum(unsafe { core::mem::transmute(get_bits_u8(bits8, 5, 3)) });
        out_store_data
            .core_data
            .set_mustache_scale(get_bits_u16(bits9, 6, 4) as u8);
        out_store_data
            .core_data
            .set_mustache_y(get_bits_u16(bits9, 10, 5) as u8);
        out_store_data
            .core_data
            .set_beard_type_enum(unsafe { core::mem::transmute(get_bits_u16(bits9, 0, 3) as u8) });
        out_store_data
            .core_data
            .set_beard_color(
                raw_data::get_hair_color_from_ver3(get_bits_u16(bits9, 3, 3) as u32) as u32,
            );

        out_store_data
            .core_data
            .set_glass_type(unsafe { core::mem::transmute(get_bits_u16(bits10, 0, 4) as u8) });
        out_store_data
            .core_data
            .set_glass_color(
                raw_data::get_glass_color_from_ver3(get_bits_u16(bits10, 4, 3) as u32) as u32,
            );
        out_store_data
            .core_data
            .set_glass_scale(get_bits_u16(bits10, 7, 4) as u8);
        out_store_data
            .core_data
            .set_glass_y(get_bits_u16(bits10, 11, 5) as u8);

        out_store_data
            .core_data
            .set_mole_type_enum(unsafe { core::mem::transmute(get_bits_u16(bits11, 0, 1) as u8) });
        out_store_data
            .core_data
            .set_mole_scale(get_bits_u16(bits11, 1, 4) as u8);
        out_store_data
            .core_data
            .set_mole_x(get_bits_u16(bits11, 5, 5) as u8);
        out_store_data
            .core_data
            .set_mole_y(get_bits_u16(bits11, 10, 5) as u8);

        out_store_data.set_checksum();
    }

    pub fn is_valid(&self) -> bool {
        let bits1 = self.appearance_bits1();
        let bits2 = self.appearance_bits2();
        let bits3 = self.appearance_bits3();
        let bits4 = self.appearance_bits4();
        let bits5 = self.appearance_bits5();
        let bits6 = self.appearance_bits6();
        let bits7 = self.appearance_bits7();
        let bits8 = self.appearance_bits8();
        let bits9 = self.appearance_bits9();
        let bits10 = self.appearance_bits10();
        let bits11 = self.appearance_bits11();

        let is_valid = (self.version() == 0 || self.version() == 3)
            && self.nickname().data[0] != 0
            && self.birth_month() < 13
            && self.birth_day() < 32
            && self.favorite_color() <= FavoriteColor::MAX as u8
            && self.height() <= MAX_HEIGHT
            && self.build() <= MAX_BUILD
            && get_bits_u8(bits1, 1, 4) <= FacelineType::MAX as u8
            && get_bits_u8(bits1, 5, 3) <= MAX_VER3_COMMON_COLOR - 2
            && get_bits_u8(bits2, 0, 4) <= FacelineWrinkle::MAX as u8
            && get_bits_u8(bits2, 4, 4) <= FacelineMake::MAX as u8
            && self.hair_type() <= HairType::MAX as u8
            && get_bits_u8(bits3, 0, 3) <= MAX_VER3_COMMON_COLOR
            && get_bits_u32(bits4, 0, 6) as u8 <= EyeType::MAX as u8
            && get_bits_u32(bits4, 6, 3) as u8 <= MAX_VER3_COMMON_COLOR - 2
            && get_bits_u32(bits4, 9, 4) as u8 <= MAX_EYE_SCALE
            && get_bits_u32(bits4, 13, 3) as u8 <= MAX_EYE_ASPECT
            && get_bits_u32(bits4, 16, 5) as u8 <= MAX_EYE_ROTATE
            && get_bits_u32(bits4, 21, 4) as u8 <= MAX_EYE_X
            && get_bits_u32(bits4, 25, 5) as u8 <= MAX_EYE_Y
            && get_bits_u32(bits5, 0, 5) as u8 <= EyebrowType::MAX as u8
            && get_bits_u32(bits5, 5, 3) as u8 <= MAX_VER3_COMMON_COLOR
            && get_bits_u32(bits5, 8, 4) as u8 <= MAX_EYEBROW_SCALE
            && get_bits_u32(bits5, 12, 3) as u8 <= MAX_EYEBROW_ASPECT
            && get_bits_u32(bits5, 16, 4) as u8 <= MAX_EYEBROW_ROTATE
            && get_bits_u32(bits5, 21, 4) as u8 <= MAX_EYEBROW_X
            && get_bits_u32(bits5, 25, 5) as u8 <= MAX_EYEBROW_Y
            && get_bits_u16(bits6, 0, 5) as u8 <= NoseType::MAX as u8
            && get_bits_u16(bits6, 5, 4) as u8 <= MAX_NOSE_SCALE
            && get_bits_u16(bits6, 9, 5) as u8 <= MAX_NOSE_Y
            && get_bits_u16(bits7, 0, 6) as u8 <= MouthType::MAX as u8
            && get_bits_u16(bits7, 6, 3) as u8 <= MAX_VER3_COMMON_COLOR - 3
            && get_bits_u16(bits7, 9, 4) as u8 <= MAX_MOUTH_SCALE
            && get_bits_u16(bits7, 13, 3) as u8 <= MAX_MOUTH_ASPECT
            && get_bits_u8(bits8, 0, 5) <= MAX_MOUTH_Y
            && get_bits_u8(bits8, 5, 3) <= MustacheType::MAX as u8
            && (get_bits_u16(bits9, 6, 4) as u8) < MAX_MUSTACHE_SCALE
            && get_bits_u16(bits9, 10, 5) as u8 <= MAX_MUSTACHE_Y
            && get_bits_u16(bits9, 0, 3) as u8 <= BeardType::MAX as u8
            && get_bits_u16(bits9, 3, 3) as u8 <= MAX_VER3_COMMON_COLOR
            && get_bits_u16(bits10, 0, 4) as u8 <= MAX_VER3_GLASS_TYPE
            && get_bits_u16(bits10, 4, 3) as u8 <= MAX_VER3_COMMON_COLOR - 2
            && get_bits_u16(bits10, 7, 4) as u8 <= MAX_GLASS_SCALE
            && get_bits_u16(bits10, 11, 5) as u8 <= MAX_GLASS_Y
            && get_bits_u16(bits11, 0, 1) as u8 <= MoleType::MAX as u8
            && get_bits_u16(bits11, 1, 4) as u8 <= MAX_MOLE_SCALE
            && get_bits_u16(bits11, 5, 5) as u8 <= MAX_MOLE_X
            && get_bits_u16(bits11, 10, 5) as u8 <= MAX_MOLE_Y;

        is_valid
    }

    pub fn build_from_store_data(&mut self, store_data: &StoreData) {
        self.write_u8(VERSION_OFFSET, 3);

        let mut mii_information = self.read_u16_le(MII_INFORMATION_OFFSET);
        set_bits_u16(&mut mii_information, 0, 1, store_data.get_gender() as u16);
        set_bits_u16(
            &mut mii_information,
            10,
            4,
            store_data.get_favorite_color() as u16,
        );
        self.write_u16_le(MII_INFORMATION_OFFSET, mii_information);
        self.write_u8(HEIGHT_OFFSET, store_data.get_height());
        self.write_u8(BUILD_OFFSET, store_data.get_build());

        self.write_nickname(MII_NAME_OFFSET, store_data.get_nickname());

        let mut region_information = self.read_u8(REGION_INFORMATION_OFFSET);
        set_bits_u8(
            &mut region_information,
            4,
            2,
            store_data.get_font_region() as u8,
        );
        self.write_u8(REGION_INFORMATION_OFFSET, region_information);

        let mut bits1 = self.read_u8(APPEARANCE_BITS1_OFFSET);
        let mut bits2 = self.read_u8(APPEARANCE_BITS2_OFFSET);
        let mut bits3 = self.read_u8(APPEARANCE_BITS3_OFFSET);
        let mut bits4 = self.read_u32_le(APPEARANCE_BITS4_OFFSET);
        let mut bits5 = self.read_u32_le(APPEARANCE_BITS5_OFFSET);
        let mut bits6 = self.read_u16_le(APPEARANCE_BITS6_OFFSET);
        let mut bits7 = self.read_u16_le(APPEARANCE_BITS7_OFFSET);
        let mut bits8 = self.read_u8(APPEARANCE_BITS8_OFFSET);
        let mut bits9 = self.read_u16_le(APPEARANCE_BITS9_OFFSET);
        let mut bits10 = self.read_u16_le(APPEARANCE_BITS10_OFFSET);
        let mut bits11 = self.read_u16_le(APPEARANCE_BITS11_OFFSET);

        set_bits_u8(&mut bits1, 1, 4, store_data.get_faceline_type() as u8);
        set_bits_u8(&mut bits2, 0, 4, store_data.get_faceline_wrinkle() as u8);
        set_bits_u8(&mut bits2, 4, 4, store_data.get_faceline_make() as u8);

        self.write_u8(HAIR_TYPE_OFFSET, store_data.get_hair_type() as u8);
        set_bits_u8(&mut bits3, 3, 1, store_data.get_hair_flip() as u8);

        set_bits_u32(&mut bits4, 0, 6, store_data.get_eye_type() as u32);
        set_bits_u32(&mut bits4, 9, 4, store_data.get_eye_scale() as u32);
        set_bits_u32(&mut bits4, 13, 3, store_data.get_eyebrow_aspect() as u32);
        set_bits_u32(&mut bits4, 16, 5, store_data.get_eye_rotate() as u32);
        set_bits_u32(&mut bits4, 21, 4, store_data.get_eye_x() as u32);
        set_bits_u32(&mut bits4, 25, 5, store_data.get_eye_y() as u32);

        set_bits_u32(&mut bits5, 0, 5, store_data.get_eyebrow_type() as u32);
        set_bits_u32(&mut bits5, 8, 4, store_data.get_eyebrow_scale() as u32);
        set_bits_u32(&mut bits5, 12, 3, store_data.get_eyebrow_aspect() as u32);
        set_bits_u32(&mut bits5, 16, 4, store_data.get_eyebrow_rotate() as u32);
        set_bits_u32(&mut bits5, 21, 4, store_data.get_eyebrow_x() as u32);
        set_bits_u32(&mut bits5, 25, 5, store_data.get_eyebrow_y() as u32);

        set_bits_u16(&mut bits6, 0, 5, store_data.get_nose_type() as u16);
        set_bits_u16(&mut bits6, 5, 4, store_data.get_nose_scale() as u16);
        set_bits_u16(&mut bits6, 9, 5, store_data.get_nose_y() as u16);

        set_bits_u16(&mut bits7, 0, 6, store_data.get_mouth_type() as u16);
        set_bits_u16(&mut bits7, 9, 4, store_data.get_mouth_scale() as u16);
        set_bits_u16(&mut bits7, 13, 3, store_data.get_mouth_aspect() as u16);
        set_bits_u8(&mut bits8, 0, 5, store_data.get_mouth_y());

        set_bits_u8(&mut bits8, 5, 3, store_data.get_mustache_type() as u8);
        set_bits_u16(&mut bits9, 6, 4, store_data.get_mustache_scale() as u16);
        set_bits_u16(&mut bits9, 10, 5, store_data.get_mustache_y() as u16);

        set_bits_u16(&mut bits9, 0, 3, store_data.get_beard_type() as u16);
        set_bits_u16(&mut bits10, 7, 4, store_data.get_glass_scale() as u16);
        set_bits_u16(&mut bits10, 11, 5, store_data.get_glass_y() as u16);

        set_bits_u16(&mut bits11, 0, 1, store_data.get_mole_type() as u16);
        set_bits_u16(&mut bits11, 1, 4, store_data.get_mole_scale() as u16);
        set_bits_u16(&mut bits11, 5, 5, store_data.get_mole_x() as u16);
        set_bits_u16(&mut bits11, 10, 5, store_data.get_mole_y() as u16);

        set_bits_u8(
            &mut bits1,
            5,
            3,
            raw_data::from_ver3_get_faceline_color(store_data.get_faceline_color() as u8),
        );
        set_bits_u8(
            &mut bits3,
            0,
            3,
            raw_data::from_ver3_get_hair_color(store_data.get_hair_color().0),
        );
        set_bits_u32(
            &mut bits4,
            6,
            3,
            raw_data::from_ver3_get_eye_color(store_data.get_eye_color().0) as u32,
        );
        set_bits_u32(
            &mut bits5,
            5,
            3,
            raw_data::from_ver3_get_hair_color(store_data.get_eyebrow_color().0) as u32,
        );
        set_bits_u16(
            &mut bits7,
            6,
            3,
            raw_data::from_ver3_get_mouthline_color(store_data.get_mouth_color().0) as u16,
        );
        set_bits_u16(
            &mut bits9,
            3,
            3,
            raw_data::from_ver3_get_hair_color(store_data.get_beard_color().0) as u16,
        );
        set_bits_u16(
            &mut bits10,
            4,
            3,
            raw_data::from_ver3_get_glass_color(store_data.get_glass_color().0) as u16,
        );
        set_bits_u16(
            &mut bits10,
            0,
            4,
            raw_data::from_ver3_get_glass_type(store_data.get_glass_type() as u8) as u16,
        );

        self.write_u8(APPEARANCE_BITS1_OFFSET, bits1);
        self.write_u8(APPEARANCE_BITS2_OFFSET, bits2);
        self.write_u8(APPEARANCE_BITS3_OFFSET, bits3);
        self.write_u32_le(APPEARANCE_BITS4_OFFSET, bits4);
        self.write_u32_le(APPEARANCE_BITS5_OFFSET, bits5);
        self.write_u16_le(APPEARANCE_BITS6_OFFSET, bits6);
        self.write_u16_le(APPEARANCE_BITS7_OFFSET, bits7);
        self.write_u8(APPEARANCE_BITS8_OFFSET, bits8);
        self.write_u16_le(APPEARANCE_BITS9_OFFSET, bits9);
        self.write_u16_le(APPEARANCE_BITS10_OFFSET, bits10);
        self.write_u16_le(APPEARANCE_BITS11_OFFSET, bits11);

        let crc = mii_util::calculate_crc16(&self.data[..CRC_OFFSET]);
        self.write_u16_be(CRC_OFFSET, crc);
    }

    #[allow(dead_code)]
    fn crc(&self) -> u16 {
        u16::from_be_bytes([self.data[CRC_OFFSET], self.data[CRC_OFFSET + 1]])
    }
}

impl Default for Ver3StoreData {
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(core::mem::size_of::<Ver3StoreData>() == 0x60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_to_store_data_round_trips_valid_defaultish_payload() {
        let mut ver3 = Ver3StoreData::new();
        ver3.data[VERSION_OFFSET] = 3;
        ver3.data[MII_INFORMATION_OFFSET..MII_INFORMATION_OFFSET + 2]
            .copy_from_slice(&(0u16 | (1 << 1) | (1 << 5) | (1 << 10)).to_le_bytes());
        let name = [b'A' as u16, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        for (index, ch) in name.into_iter().enumerate() {
            let offset = MII_NAME_OFFSET + index * 2;
            ver3.data[offset..offset + 2].copy_from_slice(&ch.to_le_bytes());
        }

        let mut store = StoreData::new();
        ver3.build_to_store_data(&mut store);
        assert_eq!(store.get_nickname().data[0], b'A' as u16);
    }

    #[test]
    fn is_valid_rejects_empty_name() {
        let mut ver3 = Ver3StoreData::new();
        ver3.data[VERSION_OFFSET] = 3;
        assert!(!ver3.is_valid());
    }

    #[test]
    fn build_from_store_data_sets_version_name_and_crc() {
        let mut store = StoreData::new();
        store.build_default(0);

        let mut ver3 = Ver3StoreData::new();
        ver3.build_from_store_data(&store);

        assert_eq!(ver3.data[VERSION_OFFSET], 3);
        assert_eq!(ver3.nickname().data, store.get_nickname().data);
        assert_eq!(
            ver3.crc(),
            mii_util::calculate_crc16(&ver3.data[..CRC_OFFSET])
        );
    }
}
