// SPDX-FileCopyrightText: 2026 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Ruzu-only lookups for environment-gated JIT diagnostics.
//!
//! These knobs have no upstream counterpart and many are queried while
//! compiling every block or memory access. On Windows each `std::env` lookup
//! takes the environment lock and converts the name (~200 ns), which made the
//! diagnostics checks a large share of block compilation time. Release builds
//! read each variable once per call site; test builds keep live reads because
//! tests may change gates at runtime (same convention as `common::env_flag!`).

/// Cached `std::env::var_os(name)` for a literal variable name.
#[macro_export]
macro_rules! debug_env_var_os {
    ($name:literal) => {{
        #[cfg(test)]
        {
            ::std::env::var_os($name)
        }
        #[cfg(not(test))]
        {
            static VALUE: ::std::sync::OnceLock<::std::option::Option<::std::ffi::OsString>> =
                ::std::sync::OnceLock::new();
            VALUE.get_or_init(|| ::std::env::var_os($name)).clone()
        }
    }};
}

/// Cached `std::env::var(name)` for a literal variable name.
#[macro_export]
macro_rules! debug_env_var {
    ($name:literal) => {{
        match $crate::debug_env_var_os!($name) {
            ::std::option::Option::Some(value) => value
                .into_string()
                .map_err(::std::env::VarError::NotUnicode),
            ::std::option::Option::None => {
                ::std::result::Result::Err(::std::env::VarError::NotPresent)
            }
        }
    }};
}

/// Uncached `std::env::var` for callers that pass a computed name and cache
/// the parsed result themselves.
pub fn var(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(name)
}

#[cfg(test)]
mod tests {
    #[test]
    fn absent_variable_is_not_present() {
        assert!(crate::debug_env_var_os!("RDYNARMIC_DEBUG_ENV_DEFINITELY_ABSENT").is_none());
        assert!(matches!(
            crate::debug_env_var!("RDYNARMIC_DEBUG_ENV_DEFINITELY_ABSENT"),
            Err(std::env::VarError::NotPresent)
        ));
    }
}
