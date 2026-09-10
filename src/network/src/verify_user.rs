// SPDX-FileCopyrightText: Copyright 2018 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/network/verify_user.h and verify_user.cpp
//!
//! Provides the user verification backend trait and a null implementation.

/// Data associated with a verified user.
/// Maps to C++ `Network::VerifyUser::UserData`.
#[derive(Clone, Debug, Default)]
pub struct UserData {
    pub username: String,
    pub display_name: String,
    pub avatar_url: String,
    /// Whether the user is a moderator.
    pub moderator: bool,
}

/// A backend used for verifying users and loading user data.
/// Maps to C++ `Network::VerifyUser::Backend`.
pub trait Backend: Send + Sync {
    /// Verifies the given token and loads the information into a `UserData`.
    ///
    /// # Arguments
    /// * `verify_uid` - A GUID that may be used for verification.
    /// * `token` - A token that contains user data and verification data.
    fn load_user_data(&self, verify_uid: &str, token: &str) -> UserData;
}

// The C++ VerifyUserJWT inherits this interface. The Rust adapter must live
// on the network side of the dependency to avoid network <-> web_service.
// All verification and claim extraction stays in web_service/verify_user_jwt.
impl Backend for web_service::verify_user_jwt::VerifyUserJwt {
    fn load_user_data(&self, verify_uid: &str, token: &str) -> UserData {
        let data = self.load_user_data(verify_uid, token);
        UserData {
            username: data.username,
            display_name: data.display_name,
            avatar_url: data.avatar_url,
            moderator: data.moderator,
        }
    }
}

/// A null backend where the token is ignored.
/// No verification is performed here and the function returns an empty `UserData`.
/// Maps to C++ `Network::VerifyUser::NullBackend`.
pub struct NullBackend;

impl Backend for NullBackend {
    fn load_user_data(&self, _verify_uid: &str, _token: &str) -> UserData {
        UserData::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_backend_returns_default() {
        let backend = NullBackend;
        let data = backend.load_user_data("some-uid", "some-token");
        assert!(data.username.is_empty());
        assert!(data.display_name.is_empty());
        assert!(data.avatar_url.is_empty());
        assert!(!data.moderator);
    }
}
