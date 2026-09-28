//! Port of zuyu/src/common/settings_common.h and zuyu/src/common/settings_common.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05
//!
//! In C++, `Setting<T>` is a template class wrapping a value with metadata (label, category,
//! default, min/max range). In Rust, we use a generic `Setting<T>` struct that stores the value
//! inline. The `SwitchableSetting<T>` adds a per-game custom value and a `use_global` flag.

use std::fmt;
use std::str::FromStr;

use crate::settings_enums::Category;

// ── Specialization flags ────────────────────────────────────────────────────

pub const SPECIALIZATION_TYPE_MASK: u8 = 0x0f;
pub const SPECIALIZATION_ATTRIBUTE_MASK: u8 = 0xf0;
pub const SPECIALIZATION_ATTRIBUTE_OFFSET: u8 = 4;

/// Extra metadata hints for frontend representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Specialization(pub u32);

impl Specialization {
    pub const DEFAULT: Self = Self(0);
    pub const TIME: Self = Self(1);
    pub const HEX: Self = Self(2);
    pub const LIST: Self = Self(3);
    pub const RUNTIME_LIST: Self = Self(4);
    pub const SCALAR: Self = Self(5);
    pub const COUNTABLE: Self = Self(6);
    pub const PAIRED: Self = Self(7);
    pub const RADIO: Self = Self(8);
    pub const PERCENTAGE: Self = Self(1 << 4);
}

impl std::ops::BitOr for Specialization {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

// ── Setting<T> ──────────────────────────────────────────────────────────────

/// A single configuration setting with a current value, default, optional range,
/// and metadata (label, category, etc.).
///
/// This is the Rust equivalent of the C++ `Setting<Type, ranged>` template.
#[derive(Debug, Clone)]
pub struct Setting<T: Clone> {
    value: T,
    default_value: T,
    minimum: Option<T>,
    maximum: Option<T>,
    pub label: &'static str,
    pub category: Category,
    pub save: bool,
    pub runtime_modifiable: bool,
    pub specialization: Specialization,
}

impl<T: Clone> Setting<T> {
    /// Create a new non-ranged setting.
    pub fn new(default_val: T, label: &'static str, category: Category) -> Self {
        Self {
            value: default_val.clone(),
            default_value: default_val,
            minimum: None,
            maximum: None,
            label,
            category,
            save: true,
            runtime_modifiable: false,
            specialization: Specialization::DEFAULT,
        }
    }

    /// Create a new non-ranged setting with full options.
    pub fn with_options(
        default_val: T,
        label: &'static str,
        category: Category,
        specialization: Specialization,
        save: bool,
        runtime_modifiable: bool,
    ) -> Self {
        Self {
            value: default_val.clone(),
            default_value: default_val,
            minimum: None,
            maximum: None,
            label,
            category,
            save,
            runtime_modifiable,
            specialization,
        }
    }

    /// Returns a reference to the current value.
    pub fn get_value(&self) -> &T {
        &self.value
    }

    /// Sets the value (clamping if ranged).
    pub fn set_value(&mut self, val: T)
    where
        T: PartialOrd,
    {
        if let (Some(min), Some(max)) = (&self.minimum, &self.maximum) {
            if val < *min {
                self.value = min.clone();
            } else if val > *max {
                self.value = max.clone();
            } else {
                self.value = val;
            }
        } else {
            self.value = val;
        }
    }

    /// Sets the value without clamping.
    pub fn set_value_unclamped(&mut self, val: T) {
        self.value = val;
    }

    /// Returns the default value.
    pub fn get_default(&self) -> &T {
        &self.default_value
    }

    /// Returns whether this is a ranged setting.
    pub fn is_ranged(&self) -> bool {
        self.minimum.is_some()
    }
}

impl<T: Clone + PartialOrd> Setting<T> {
    /// Create a new ranged setting.
    pub fn ranged(
        default_val: T,
        min_val: T,
        max_val: T,
        label: &'static str,
        category: Category,
    ) -> Self {
        Self {
            value: default_val.clone(),
            default_value: default_val,
            minimum: Some(min_val),
            maximum: Some(max_val),
            label,
            category,
            save: true,
            runtime_modifiable: false,
            specialization: Specialization::DEFAULT,
        }
    }

    /// Create a new ranged setting with full options.
    pub fn ranged_with_options(
        default_val: T,
        min_val: T,
        max_val: T,
        label: &'static str,
        category: Category,
        specialization: Specialization,
        save: bool,
        runtime_modifiable: bool,
    ) -> Self {
        Self {
            value: default_val.clone(),
            default_value: default_val,
            minimum: Some(min_val),
            maximum: Some(max_val),
            label,
            category,
            save,
            runtime_modifiable,
            specialization,
        }
    }
}

impl<T: Clone + fmt::Display> Setting<T> {
    pub fn to_string_value(&self) -> String {
        format!("{}", self.value)
    }
}

impl<T: Clone + FromStr + fmt::Display> Setting<T>
where
    T: PartialOrd,
{
    /// Load a value from a string, falling back to default on parse failure.
    pub fn load_string(&mut self, input: &str)
    where
        T: crate::settings_setting::SettingType,
    {
        crate::settings_setting::BasicSetting::load_string(self, input);
    }
}

// ── SwitchableSetting<T> ────────────────────────────────────────────────────

/// A setting that supports per-game overrides.
/// When `use_global` is true, the global value is returned.
/// When false, the custom (per-game) value is used.
///
/// This is the Rust equivalent of the C++ `SwitchableSetting<Type, ranged>`.
#[derive(Debug, Clone)]
pub struct SwitchableSetting<T: Clone> {
    pub setting: Setting<T>,
    use_global: bool,
    custom: T,
}

impl<T: Clone> SwitchableSetting<T> {
    /// Create a new non-ranged switchable setting.
    pub fn new(default_val: T, label: &'static str, category: Category) -> Self {
        let custom = default_val.clone();
        Self {
            setting: Setting::new(default_val, label, category),
            use_global: true,
            custom,
        }
    }

    /// Create a new non-ranged switchable setting with full options.
    pub fn with_options(
        default_val: T,
        label: &'static str,
        category: Category,
        specialization: Specialization,
        save: bool,
        runtime_modifiable: bool,
    ) -> Self {
        let custom = default_val.clone();
        Self {
            setting: Setting::with_options(
                default_val,
                label,
                category,
                specialization,
                save,
                runtime_modifiable,
            ),
            use_global: true,
            custom,
        }
    }

    /// Returns the effective value (global or custom depending on state).
    pub fn get_value(&self) -> &T {
        if self.use_global {
            self.setting.get_value()
        } else {
            &self.custom
        }
    }

    /// Returns the global value regardless of the use_global flag.
    pub fn get_value_global(&self) -> &T {
        self.setting.get_value()
    }

    /// Sets the value depending on the global state.
    pub fn set_value(&mut self, val: T)
    where
        T: PartialOrd,
    {
        if self.use_global {
            self.setting.set_value(val);
        } else {
            // Clamp if ranged
            if let (Some(min), Some(max)) = (&self.setting.minimum, &self.setting.maximum) {
                if val < *min {
                    self.custom = min.clone();
                } else if val > *max {
                    self.custom = max.clone();
                } else {
                    self.custom = val;
                }
            } else {
                self.custom = val;
            }
        }
    }

    /// Sets the value without clamping.
    pub fn set_value_unclamped(&mut self, val: T) {
        if self.use_global {
            self.setting.set_value_unclamped(val);
        } else {
            self.custom = val;
        }
    }

    pub fn set_global(&mut self, global: bool) {
        self.use_global = global;
    }

    pub fn using_global(&self) -> bool {
        self.use_global
    }

    pub fn get_default(&self) -> &T {
        self.setting.get_default()
    }

    pub fn label(&self) -> &'static str {
        self.setting.label
    }

    pub fn category(&self) -> Category {
        self.setting.category
    }
}

impl<T: Clone + PartialOrd> SwitchableSetting<T> {
    /// Create a new ranged switchable setting.
    pub fn ranged(
        default_val: T,
        min_val: T,
        max_val: T,
        label: &'static str,
        category: Category,
    ) -> Self {
        let custom = default_val.clone();
        Self {
            setting: Setting::ranged(default_val, min_val, max_val, label, category),
            use_global: true,
            custom,
        }
    }

    /// Create a new ranged switchable setting with full options.
    pub fn ranged_with_options(
        default_val: T,
        min_val: T,
        max_val: T,
        label: &'static str,
        category: Category,
        specialization: Specialization,
        save: bool,
        runtime_modifiable: bool,
    ) -> Self {
        let custom = default_val.clone();
        Self {
            setting: Setting::ranged_with_options(
                default_val,
                min_val,
                max_val,
                label,
                category,
                specialization,
                save,
                runtime_modifiable,
            ),
            use_global: true,
            custom,
        }
    }
}

// ── InputSetting<T> ─────────────────────────────────────────────────────────

/// A setting for input configuration that supports global/custom switching.
/// Unlike SwitchableSetting, this is not based on Setting<T> -- it stores
/// the global and custom values directly.
#[derive(Debug, Clone)]
pub struct InputSetting<T: Clone + Default> {
    use_global: bool,
    global: T,
    custom: T,
}

impl<T: Clone + Default> InputSetting<T> {
    pub fn new() -> Self {
        Self {
            use_global: true,
            global: T::default(),
            custom: T::default(),
        }
    }

    pub fn with_value(val: T) -> Self {
        Self {
            use_global: true,
            global: val,
            custom: T::default(),
        }
    }

    pub fn set_global(&mut self, to_global: bool) {
        self.use_global = to_global;
    }

    pub fn using_global(&self) -> bool {
        self.use_global
    }

    pub fn get_value(&self) -> &T {
        if self.use_global {
            &self.global
        } else {
            &self.custom
        }
    }

    pub fn get_value_mut(&mut self) -> &mut T {
        if self.use_global {
            &mut self.global
        } else {
            &mut self.custom
        }
    }

    pub fn get_value_explicit(&self, need_global: bool) -> &T {
        if self.use_global || need_global {
            &self.global
        } else {
            &self.custom
        }
    }

    pub fn get_value_explicit_mut(&mut self, need_global: bool) -> &mut T {
        if self.use_global || need_global {
            &mut self.global
        } else {
            &mut self.custom
        }
    }
}

impl<T: Clone + Default> Default for InputSetting<T> {
    fn default() -> Self {
        Self::new()
    }
}
