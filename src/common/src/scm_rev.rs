// SPDX-License-Identifier: GPL-3.0-or-later

//! Build-time source-control and compiler identity.
//!
//! Rust counterpart of Eden's generated `common/scm_rev.cpp`. `build.rs`
//! obtains these values from Git and the selected native C++ compiler instead
//! of embedding host-specific values in source code.

pub const SCM_REV: &str = env!("GIT_REV");
pub const SCM_BRANCH: &str = env!("GIT_BRANCH");
pub const SCM_DESC: &str = env!("GIT_DESC");
pub const BUILD_NAME: &str = env!("BUILD_NAME");
pub const BUILD_VERSION: &str = env!("BUILD_VERSION");
pub const COMPILER_ID: &str = env!("COMPILER_ID");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_version_matches_release_or_development_identity() {
        let release_version = format!("v{}", env!("CARGO_PKG_VERSION"));
        if BUILD_VERSION.starts_with('v') {
            assert_eq!(BUILD_VERSION, release_version);
        } else {
            assert!(BUILD_VERSION.starts_with(&SCM_REV[..SCM_REV.len().min(10)]));
            assert!(BUILD_VERSION.ends_with(SCM_BRANCH));
        }
        assert_eq!(SCM_DESC, BUILD_VERSION);
        assert!(!COMPILER_ID.is_empty());
        assert_ne!(COMPILER_ID, "Unknown compiler");
        #[cfg(target_env = "msvc")]
        {
            let version = COMPILER_ID.strip_prefix("MSVC ").unwrap();
            assert_eq!(version.split('.').count(), 4);
        }
    }
}
