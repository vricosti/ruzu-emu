// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/common/settings_setting.h
//!
//! This file provides the `Setting<T, RANGED>` and `SwitchableSetting<T, RANGED>`
//! template equivalents. In the existing Rust codebase, `settings_common.rs`
//! already contains `Setting<T>` and `SwitchableSetting<T>` implementations.
//!
//! This module re-exports and extends those types to provide the additional
//! methods present in the C++ `settings_setting.h` that are not yet covered by
//! `settings_common.rs`, specifically the string serialization and type
//! introspection methods from `BasicSetting` / `Setting<Type, ranged>`.
//!
//! For the base `Setting` / `SwitchableSetting` structs, see `settings_common.rs`.
//! This file adds trait-based extensions for the `BasicSetting` virtual methods.

use std::any::TypeId;
use std::fmt;
use std::str::FromStr;

use crate::settings_common::{Setting, SwitchableSetting};
use crate::settings_enums::Category;

// ---------------------------------------------------------------------------
// BasicSetting trait -- mirrors the C++ abstract class
// ---------------------------------------------------------------------------

/// Trait covering the virtual methods of the C++ `BasicSetting` class.
///
/// Every `Setting<T>` and `SwitchableSetting<T>` implements this when `T`
/// satisfies the necessary bounds.
pub trait BasicSetting {
    /// Whether this setting participates in configuration persistence.
    fn save(&self) -> bool;

    /// String representation of the current value (respects global state).
    fn to_string_repr(&self) -> String;

    /// String representation of the global value.
    fn to_string_global(&self) -> String;

    /// String representation of the default value.
    fn default_to_string(&self) -> String;

    /// Load a value from a string representation.
    fn load_string(&mut self, input: &str);

    /// Returns a canonicalized string (enum name for enums, else same as
    /// `to_string_repr`).
    fn canonicalize(&self) -> String;

    /// Whether the underlying type is an enum.
    fn is_enum(&self) -> bool;

    /// Whether this is a switchable setting.
    fn switchable(&self) -> bool;

    /// Whether a switchable setting currently inherits its global value.
    fn using_global(&self) -> bool;

    /// Select the global or custom storage of a switchable setting.
    fn set_global(&mut self, global: bool);

    /// Whether this is a ranged setting.
    fn ranged(&self) -> bool;

    /// Whether the underlying type is floating-point.
    fn is_floating_point(&self) -> bool;

    /// Whether the underlying type is integral.
    fn is_integral(&self) -> bool;

    /// The setting's label.
    fn label(&self) -> &str;

    /// The setting's category.
    fn category(&self) -> Category;

    /// Min value as a string.
    fn min_val(&self) -> String;

    /// Max value as a string.
    fn max_val(&self) -> String;
}

// ---------------------------------------------------------------------------
// SettingType trait -- bounds for types usable in Setting
// ---------------------------------------------------------------------------

/// Types that can be stored in a `Setting`.
///
/// The conversion methods mirror `Setting<Type>::ToString` and
/// `Setting<Type>::Canonicalize`. They are deliberately separate from
/// `Display`: enum settings are persisted as their numeric underlying value,
/// while frontends use their canonical variant name.
pub trait SettingType: Clone + PartialOrd + fmt::Display + FromStr + 'static {
    fn to_config_string(&self) -> String;

    /// Deserialize before typed SetValue validation. Enum implementations clamp
    /// raw numeric values before constructing a Rust enum (C++ allows the cast).
    fn from_config_string(input: &str) -> Result<Self, ()> {
        input.parse().map_err(|_| ())
    }

    fn canonicalize_value(&self) -> String {
        self.to_config_string()
    }

    fn is_enum_type() -> bool {
        false
    }
}

macro_rules! impl_setting_type {
    ($($type:ty),+ $(,)?) => {
        $(
            impl SettingType for $type {
                fn to_config_string(&self) -> String {
                    self.to_string()
                }
            }
        )+
    };
}

impl_setting_type!(bool, String, i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize,);

impl SettingType for f32 {
    fn to_config_string(&self) -> String {
        format!("{self:.6}")
    }
}

impl SettingType for f64 {
    fn to_config_string(&self) -> String {
        format!("{self:.6}")
    }
}

// ---------------------------------------------------------------------------
// IntegralMarker / FloatingPointMarker -- compile-time type classification
// ---------------------------------------------------------------------------

/// Returns true if `T` is one of the standard integer types.
fn is_integral_type<T: 'static>() -> bool {
    let id = TypeId::of::<T>();
    id == TypeId::of::<i8>()
        || id == TypeId::of::<i16>()
        || id == TypeId::of::<i32>()
        || id == TypeId::of::<i64>()
        || id == TypeId::of::<i128>()
        || id == TypeId::of::<u8>()
        || id == TypeId::of::<u16>()
        || id == TypeId::of::<u32>()
        || id == TypeId::of::<u64>()
        || id == TypeId::of::<u128>()
        || id == TypeId::of::<isize>()
        || id == TypeId::of::<usize>()
        || id == TypeId::of::<bool>()
}

/// Returns true if `T` is `f32` or `f64`.
fn is_floating_point_type<T: 'static>() -> bool {
    let id = TypeId::of::<T>();
    id == TypeId::of::<f32>() || id == TypeId::of::<f64>()
}

// ---------------------------------------------------------------------------
// BasicSetting impl for Setting<T>
// ---------------------------------------------------------------------------

impl<T> BasicSetting for Setting<T>
where
    T: SettingType,
{
    fn save(&self) -> bool {
        self.save
    }

    fn to_string_repr(&self) -> String {
        self.get_value().to_config_string()
    }

    fn to_string_global(&self) -> String {
        // Non-switchable: global == current.
        self.get_value().to_config_string()
    }

    fn default_to_string(&self) -> String {
        self.get_default().to_config_string()
    }

    fn load_string(&mut self, input: &str) {
        if input.is_empty() {
            self.set_value(self.get_default().clone());
            return;
        }
        match T::from_config_string(input) {
            Ok(val) => self.set_value(val),
            Err(_) => self.set_value(self.get_default().clone()),
        }
    }

    fn canonicalize(&self) -> String {
        self.get_value().canonicalize_value()
    }

    fn is_enum(&self) -> bool {
        T::is_enum_type()
    }

    fn switchable(&self) -> bool {
        false
    }

    fn using_global(&self) -> bool {
        true
    }

    fn set_global(&mut self, _global: bool) {}

    fn ranged(&self) -> bool {
        self.is_ranged()
    }

    fn is_floating_point(&self) -> bool {
        is_floating_point_type::<T>()
    }

    fn is_integral(&self) -> bool {
        is_integral_type::<T>()
    }

    fn label(&self) -> &str {
        self.label
    }

    fn category(&self) -> Category {
        self.category
    }

    fn min_val(&self) -> String {
        // For non-ranged settings, C++ returns numeric_limits::min().
        // We return the default string representation.
        self.default_to_string()
    }

    fn max_val(&self) -> String {
        self.default_to_string()
    }
}

// ---------------------------------------------------------------------------
// BasicSetting impl for SwitchableSetting<T>
// ---------------------------------------------------------------------------

impl<T> BasicSetting for SwitchableSetting<T>
where
    T: SettingType,
{
    fn save(&self) -> bool {
        self.setting.save
    }

    fn to_string_repr(&self) -> String {
        self.get_value().to_config_string()
    }

    fn to_string_global(&self) -> String {
        self.get_value_global().to_config_string()
    }

    fn default_to_string(&self) -> String {
        self.get_default().to_config_string()
    }

    fn load_string(&mut self, input: &str) {
        if input.is_empty() {
            self.set_value(self.get_default().clone());
            return;
        }
        match T::from_config_string(input) {
            Ok(val) => self.set_value(val),
            Err(_) => self.set_value(self.get_default().clone()),
        }
    }

    fn canonicalize(&self) -> String {
        self.get_value().canonicalize_value()
    }

    fn is_enum(&self) -> bool {
        T::is_enum_type()
    }

    fn switchable(&self) -> bool {
        true
    }

    fn using_global(&self) -> bool {
        self.using_global()
    }

    fn set_global(&mut self, global: bool) {
        self.set_global(global);
    }

    fn ranged(&self) -> bool {
        self.setting.is_ranged()
    }

    fn is_floating_point(&self) -> bool {
        is_floating_point_type::<T>()
    }

    fn is_integral(&self) -> bool {
        is_integral_type::<T>()
    }

    fn label(&self) -> &str {
        self.setting.label
    }

    fn category(&self) -> Category {
        self.setting.category
    }

    fn min_val(&self) -> String {
        self.default_to_string()
    }

    fn max_val(&self) -> String {
        self.default_to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_setting_basic_setting_trait() {
        let s: Setting<i32> = Setting::new(42, "test_int", Category::Core);
        assert_eq!(s.to_string_repr(), "42");
        assert_eq!(s.default_to_string(), "42");
        assert!(!s.switchable());
        assert!(!s.ranged());
        assert!(s.is_integral());
        assert!(!s.is_floating_point());
    }

    #[test]
    fn test_setting_load_string() {
        let mut s: Setting<i32> = Setting::new(0, "test", Category::Core);
        BasicSetting::load_string(&mut s, "123");
        assert_eq!(*s.get_value(), 123);

        BasicSetting::load_string(&mut s, "");
        assert_eq!(*s.get_value(), 0); // default

        BasicSetting::load_string(&mut s, "not_a_number");
        assert_eq!(*s.get_value(), 0); // default
    }

    #[test]
    fn test_switchable_setting_trait() {
        let s: SwitchableSetting<f32> = SwitchableSetting::new(1.0, "test_float", Category::Audio);
        assert!(s.switchable());
        assert!(s.is_floating_point());
        assert!(!s.is_integral());
        assert_eq!(s.to_string_repr(), "1.000000");

        let fractional = Setting::new(1.25_f64, "fractional", Category::Audio);
        assert_eq!(fractional.to_string_repr(), "1.250000");
    }

    #[test]
    fn enum_setting_uses_numeric_config_and_canonical_ui_representations() {
        use crate::settings_enums::RendererBackend;

        let setting = Setting::new(RendererBackend::Null, "backend", Category::Renderer);

        assert_eq!(setting.to_string_repr(), "2");
        assert_eq!(setting.default_to_string(), "2");
        assert_eq!(setting.canonicalize(), "Null");
        assert!(setting.is_enum());
    }

    #[test]
    fn test_switchable_global_vs_custom() {
        let mut s: SwitchableSetting<i32> =
            SwitchableSetting::new(10, "test_switch", Category::Core);
        assert_eq!(s.to_string_repr(), "10");
        assert_eq!(s.to_string_global(), "10");

        s.set_global(false);
        s.set_value(99);
        assert_eq!(s.to_string_repr(), "99");
        assert_eq!(s.to_string_global(), "10");
    }

    #[test]
    fn removed_strict_fence_value_clamps_to_accurate_in_all_storages() {
        use crate::settings_enums::GpuFenceBehavior as Fence;
        let mut plain = Setting::new(Fence::Default, "fence", Category::RendererAdvanced);
        let mut values = crate::settings::Values::default();
        for (input, expected) in [
            ("4", Fence::Accurate), ("3", Fence::Accurate),
            ("1", Fence::Immediate), ("-1", Fence::Accurate),
            ("4294967296", Fence::Default), (" 4 ", Fence::Accurate),
            (" +4suffix", Fence::Accurate), ("-4294967295", Fence::Immediate),
            ("9223372036854775808", Fence::Default), ("invalid", Fence::Default),
        ] {
            plain.load_string(input);
            assert_eq!(*plain.get_value(), expected);
            values.gpu_fence_behavior.set_global(true);
            BasicSetting::load_string(&mut values.gpu_fence_behavior, input);
            assert_eq!(*values.gpu_fence_behavior.get_value(), expected);
            values.gpu_fence_behavior.set_global(false);
            BasicSetting::load_string(&mut values.gpu_fence_behavior, input);
            assert_eq!(*values.gpu_fence_behavior.get_value(), expected);
        }
    }
}
