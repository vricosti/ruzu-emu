// SPDX-FileCopyrightText: Copyright 2018 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden src/web_service/verify_user_jwt.h and verify_user_jwt.cpp
//!
//! Provides JWT-based user verification against the web service.

use crate::web_backend::Client;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde_json::Value;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Public key fetching
// ---------------------------------------------------------------------------

/// Cached public key for JWT verification.
// Unlike the C++ static string, synchronize concurrent constructors and do not
// reuse the previous service's key after the configured API host changes.
static PUBLIC_KEY: Mutex<Option<(String, String)>> = Mutex::new(None);

/// Fetches the public key from the web service for JWT verification.
/// Maps to C++ `WebService::GetPublicKey`.
///
pub fn get_public_key(host: &str) -> String {
    let mut cached = PUBLIC_KEY.lock().unwrap_or_else(|error| error.into_inner());
    if let Some((cached_host, key)) = cached.as_ref() {
        if cached_host == host && !key.is_empty() {
            return key.clone();
        }
    }
    let mut client = Client::new(host.to_string(), String::new(), String::new());
    let key = client
        .get_plain("/jwt/external/key.pem", true)
        .returned_data;
    if key.is_empty() {
        log::error!("Could not fetch external JWT public key, verification may fail");
    } else {
        log::info!("Fetched external JWT public key (size={})", key.len());
    }
    // Empty responses remain retryable, as in upstream GetPublicKey.
    *cached = Some((host.to_owned(), key.clone()));
    key
}

// ---------------------------------------------------------------------------
// VerifyUserJWT
// ---------------------------------------------------------------------------

/// JWT-based user verification backend.
/// Maps to C++ `WebService::VerifyUserJWT`.
///
/// The network crate supplies the mechanical Backend trait adapter to avoid
/// a circular dependency. Verification behavior remains owned by this module.
pub struct VerifyUserJwt {
    pub_key: String,
}

/// User data returned from JWT verification.
/// Re-uses the same field layout as `network::verify_user::UserData`.
#[derive(Clone, Debug, Default)]
pub struct UserData {
    pub username: String,
    pub display_name: String,
    pub avatar_url: String,
    pub moderator: bool,
}

impl VerifyUserJwt {
    pub fn new(host: &str) -> Self {
        Self {
            pub_key: get_public_key(host),
        }
    }

    /// Verifies the given token and loads user data from the JWT claims.
    ///
    pub fn load_user_data(&self, verify_uid: &str, token: &str) -> UserData {
        let verify = || -> Option<UserData> {
            let audience = format!("external-{verify_uid}");
            let key = DecodingKey::from_rsa_pem(self.pub_key.as_bytes()).ok()?;
            let mut validation = Validation::new(Algorithm::RS256);
            validation.set_issuer(&["citra-core"]);
            validation.set_audience(&[&audience]);
            // cpp-jwt requires iss/aud, checks exp/nbf only when present,
            // and uses zero leeway. Its iat check is numeric presence, not
            // a future-date restriction; jti is presence, not replay tracking.
            validation.set_required_spec_claims(&["iss", "aud"]);
            validation.leeway = 0;
            validation.validate_nbf = true;
            let claims = decode::<Value>(token, &key, &validation).ok()?.claims;
            if claims.get("iss")?.as_str()? != "citra-core"
                || claims.get("aud")?.as_str()? != audience
                || claims.get("iat")?.as_u64().is_none()
                || claims.get("jti").is_none()
            {
                return None;
            }
            // jsonwebtoken ignores malformed optional NumericDate values;
            // upstream's numeric conversion fails instead. Reject them.
            for field in ["exp", "nbf"] {
                if let Some(value) = claims.get(field) {
                    value.as_u64()?;
                }
            }
            let mut data = UserData::default();
            for (field, target) in [
                ("username", &mut data.username),
                ("displayName", &mut data.display_name),
                ("avatarUrl", &mut data.avatar_url),
            ] {
                if let Some(value) = claims.get(field) {
                    *target = value.as_str()?.to_owned();
                }
            }
            if let Some(roles) = claims.get("roles") {
                for role in roles.as_array()? {
                    if role.as_str()? == "moderator" {
                        data.moderator = true;
                    }
                }
            }
            Some(data)
        };
        match verify() {
            Some(data) => data,
            None => {
                // Do not log the token, credentials or unverified claims.
                log::info!("JWT verification failed");
                UserData::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_cache_retries_empty_responses_and_respects_host_changes() {
        use std::io::{Read, Write};
        fn serve(bodies: Vec<String>) -> (String, std::thread::JoinHandle<()>) {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let host = format!("http://{}", listener.local_addr().unwrap());
            listener.set_nonblocking(true).unwrap();
            let worker = std::thread::spawn(move || {
                for body in bodies {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(
                                    std::time::Instant::now() < deadline,
                                    "missing public-key request"
                                );
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                            Err(error) => panic!("{error}"),
                        }
                    };
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).unwrap();
                        request.push(byte[0]);
                    }
                    let request = String::from_utf8(request).unwrap();
                    assert!(request.starts_with("GET /jwt/external/key.pem HTTP/1.1\r\n"));
                    assert!(!request.to_ascii_lowercase().contains("authorization:"));
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            });
            (host, worker)
        }
        let key = key_pair().1.clone();
        let (host, worker) = serve(vec![String::new(), key.clone()]);
        assert!(get_public_key(&host).is_empty());
        assert_eq!(get_public_key(&host), key);
        worker.join().unwrap();
        // Listener is gone: a cached successful key does not make a new request.
        assert_eq!(get_public_key(&host), key);
        let other_key = format!("{key}\n");
        let (other_host, worker) = serve(vec![other_key.clone()]);
        assert_eq!(get_public_key(&other_host), other_key);
        worker.join().unwrap();
    }

    fn key_pair() -> &'static (String, String) {
        static KEYS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
        KEYS.get_or_init(|| {
            let rsa = openssl::rsa::Rsa::generate(2048).unwrap();
            let key = openssl::pkey::PKey::from_rsa(rsa).unwrap();
            (
                String::from_utf8(key.private_key_to_pem_pkcs8().unwrap()).unwrap(),
                String::from_utf8(key.public_key_to_pem().unwrap()).unwrap(),
            )
        })
    }

    fn claims() -> Value {
        serde_json::json!({
            "iss": "citra-core", "aud": "external-synthetic-room",
            "iat": 1, "jti": "synthetic-token", "username": "SyntheticUser",
            "displayName": "Synthetic User", "avatarUrl": "",
            "roles": ["member", "moderator"]
        })
    }

    fn signed(claims: &Value) -> String {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::RS256),
            claims,
            &jsonwebtoken::EncodingKey::from_rsa_pem(key_pair().0.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn verified_claims_populate_identity_and_exact_moderator_role() {
        let verifier = VerifyUserJwt {
            pub_key: key_pair().1.clone(),
        };
        let mut payload = claims();
        let data = verifier.load_user_data("synthetic-room", &signed(&payload));
        assert_eq!(data.username, "SyntheticUser");
        assert_eq!(data.display_name, "Synthetic User");
        assert!(data.avatar_url.is_empty());
        assert!(data.moderator);
        // The upstream library does not require exp or reject future iat.
        payload["iat"] = serde_json::json!(jsonwebtoken::get_current_timestamp() + 3600);
        payload["roles"] = serde_json::json!(["Moderator", "member"]);
        let data = verifier.load_user_data("synthetic-room", &signed(&payload));
        assert_eq!(data.username, "SyntheticUser");
        assert!(!data.moderator);
    }

    #[test]
    fn rejects_wrong_claims_dates_and_malformed_identity_without_panicking() {
        let verifier = VerifyUserJwt {
            pub_key: key_pair().1.clone(),
        };
        for field in ["iss", "aud", "iat", "jti"] {
            let mut payload = claims();
            payload.as_object_mut().unwrap().remove(field);
            let data = verifier.load_user_data("synthetic-room", &signed(&payload));
            assert!(data.username.is_empty(), "accepted missing {field}");
            assert!(!data.moderator);
        }
        for (field, value) in [
            ("iss", serde_json::json!("wrong-issuer")),
            ("aud", serde_json::json!("external-other-room")),
            ("aud", serde_json::json!(["external-synthetic-room"])),
            ("iat", serde_json::json!("not-a-number")),
            ("exp", serde_json::json!(1)),
            ("exp", serde_json::json!("not-a-number")),
            (
                "nbf",
                serde_json::json!(jsonwebtoken::get_current_timestamp() + 3600),
            ),
            ("nbf", serde_json::json!("not-a-number")),
            ("roles", serde_json::json!(["moderator", 1])),
            ("username", serde_json::json!(1)),
        ] {
            let mut payload = claims();
            payload[field] = value;
            let data = verifier.load_user_data("synthetic-room", &signed(&payload));
            assert!(data.username.is_empty(), "accepted invalid {field}");
            assert!(!data.moderator);
        }
    }

    #[test]
    fn rejects_tampering_wrong_keys_and_algorithm_confusion() {
        let verifier = VerifyUserJwt {
            pub_key: key_pair().1.clone(),
        };
        let token = signed(&claims());
        let mut corrupted = token.clone().into_bytes();
        let signature = token.rfind('.').unwrap() + 1;
        corrupted[signature] = if corrupted[signature] == b'A' {
            b'B'
        } else {
            b'A'
        };
        let hmac = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::HS256),
            &claims(),
            &jsonwebtoken::EncodingKey::from_secret(key_pair().1.as_bytes()),
        )
        .unwrap();
        for invalid in [
            String::from_utf8(corrupted).unwrap(),
            hmac,
            "eyJhbGciOiJub25lIn0.e30.".into(),
            "malformed".into(),
        ] {
            assert!(verifier
                .load_user_data("synthetic-room", &invalid)
                .username
                .is_empty());
        }
        let other =
            openssl::pkey::PKey::from_rsa(openssl::rsa::Rsa::generate(2048).unwrap()).unwrap();
        let verifier = VerifyUserJwt {
            pub_key: String::from_utf8(other.public_key_to_pem().unwrap()).unwrap(),
        };
        assert!(verifier
            .load_user_data("synthetic-room", &token)
            .username
            .is_empty());
    }

    #[test]
    fn test_verify_user_jwt_empty_key() {
        let verifier = VerifyUserJwt {
            pub_key: String::new(),
        };
        let data = verifier.load_user_data("uid", "");
        assert!(data.username.is_empty());
    }
}
