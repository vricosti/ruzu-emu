// SPDX-FileCopyrightText: Copyright 2017 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/dedicated_room/yuzu_room.cpp
//!
//! Headless multiplayer room server entry point. Parses CLI arguments,
//! creates a room, optionally starts an announce session, and waits for
//! the operator to quit.

use std::fs;
use std::io::{self, BufRead, Write};

use clap::Parser;

use common::announce_multiplayer_room::GameInfo;
use network::announce_multiplayer_session::AnnounceMultiplayerSession;
use network::network::RoomNetwork;
use network::room::{
    BanList, IpBanList, UsernameBanList, DEFAULT_ROOM_PORT, MAX_CONCURRENT_CONNECTIONS,
};
use network::verify_user::{Backend, NullBackend, UserData};

// ---------------------------------------------------------------------------
// VerifyUserJwt wrapper — bridges web_service and network crates
// ---------------------------------------------------------------------------

/// Wrapper around `web_service::verify_user_jwt::VerifyUserJwt` that implements
/// the `network::verify_user::Backend` trait. This glue lives in dedicated_room
/// because web_service cannot depend on network (circular dependency).
/// Upstream: `WebService::VerifyUserJWT` implements `Network::VerifyUser::Backend`.
struct VerifyUserJwtBackend {
    inner: web_service::verify_user_jwt::VerifyUserJwt,
}

impl VerifyUserJwtBackend {
    fn new(host: &str) -> Self {
        Self {
            inner: web_service::verify_user_jwt::VerifyUserJwt::new(host),
        }
    }
}

impl Backend for VerifyUserJwtBackend {
    fn load_user_data(&self, verify_uid: &str, token: &str) -> UserData {
        let ws_data = self.inner.load_user_data(verify_uid, token);
        UserData {
            username: ws_data.username,
            display_name: ws_data.display_name,
            avatar_url: ws_data.avatar_url,
            moderator: ws_data.moderator,
        }
    }
}

// ---------------------------------------------------------------------------
// Constants (from yuzu_room.cpp)
// ---------------------------------------------------------------------------

/// The magic text at the beginning of a yuzu-room ban list file.
/// Maps to C++ `BanListMagic`.
const BAN_LIST_MAGIC: &str = "YuzuRoom-BanList-1";

/// Delimiter separating username from token in a display token.
/// Maps to C++ `token_delimiter`.
const TOKEN_DELIMITER: char = ':';

// ---------------------------------------------------------------------------
// CLI argument definitions (from yuzu_room.cpp `long_options`)
// ---------------------------------------------------------------------------

/// Headless multiplayer room server for yuzu/ruzu.
#[derive(Parser, Debug)]
#[command(name = "dedicated-room")]
#[command(about = "yuzu dedicated room server")]
struct Args {
    /// The name of the room
    #[arg(long = "room-name", short = 'n')]
    room_name: Option<String>,

    /// The room description
    #[arg(long = "room-description", short = 'd')]
    room_description: Option<String>,

    /// The bind address for the room
    #[arg(long = "bind-address", short = 's')]
    bind_address: Option<String>,

    /// The port used for the room
    #[arg(long = "port", short = 'p', default_value_t = DEFAULT_ROOM_PORT as u32)]
    port: u32,

    /// The maximum number of players for this room
    #[arg(long = "max_members", short = 'm', default_value_t = 16)]
    max_members: u32,

    /// The password for the room
    #[arg(long = "password", short = 'w')]
    password: Option<String>,

    /// The preferred game for this room
    #[arg(long = "preferred-game", short = 'g')]
    preferred_game: Option<String>,

    /// The preferred game-id for this room (hex)
    #[arg(long = "preferred-game-id", short = 'i')]
    preferred_game_id: Option<String>,

    /// The username used for announce
    #[arg(long = "username", short = 'u')]
    username: Option<String>,

    /// The token used for announce
    #[arg(long = "token", short = 't')]
    token: Option<String>,

    /// yuzu Web API url
    #[arg(long = "web-api-url", short = 'a')]
    web_api_url: Option<String>,

    /// The file for storing the room ban list
    #[arg(long = "ban-list-file", short = 'b')]
    ban_list_file: Option<String>,

    /// The file for storing the room log
    #[arg(long = "log-file", short = 'l', default_value = "yuzu-room.log")]
    log_file: String,

    /// Allow yuzu Community Moderators to moderate on your room
    #[arg(long = "enable-yuzu-mods", short = 'e')]
    enable_yuzu_mods: bool,
}

// ---------------------------------------------------------------------------
// Token helpers (from yuzu_room.cpp)
// ---------------------------------------------------------------------------

/// Pads a base64-encoded token with '=' characters until it round-trips
/// correctly through decode/encode.
///
/// Maps to C++ `PadToken`.
fn pad_token(token: &mut String) {
    for _ in 0..3 {
        // Try to decode then re-encode; if the re-encoded form matches, stop.
        match base64_decode(token.as_bytes()) {
            Some(decoded) => {
                let roundtrip = base64_encode(&decoded);
                if roundtrip == *token {
                    break;
                }
            }
            None => {}
        }
        token.push('=');
    }
}

/// Standard base64 alphabet used by mbedtls_base64_encode / mbedtls_base64_decode.
const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Decodes a base64 string, returning the raw bytes or None on invalid input.
///
/// Maps to the `mbedtls_base64_decode` call in the upstream C++ code.
fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    // Build a reverse lookup table: char -> 6-bit value (0-63), or 0xFF for invalid.
    let mut decode_table = [0xFFu8; 256];
    for (i, &c) in BASE64_CHARS.iter().enumerate() {
        decode_table[c as usize] = i as u8;
    }

    // Strip trailing '=' padding characters.
    let input = {
        let mut end = input.len();
        while end > 0 && input[end - 1] == b'=' {
            end -= 1;
        }
        &input[..end]
    };

    let mut output = Vec::with_capacity((input.len() * 3) / 4 + 1);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;

    for &byte in input {
        if byte == b'\r' || byte == b'\n' || byte == b' ' {
            continue;
        }
        let val = decode_table[byte as usize];
        if val == 0xFF {
            return None;
        }
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buf >> bits) & 0xFF) as u8);
        }
    }

    Some(output)
}

/// Encodes raw bytes to a standard base64 string (with '=' padding).
///
/// Maps to the `mbedtls_base64_encode` call in the upstream C++ code.
fn base64_encode(input: &[u8]) -> String {
    let mut output = Vec::with_capacity(((input.len() + 2) / 3) * 4);

    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        output.push(BASE64_CHARS[((triple >> 18) & 0x3F) as usize]);
        output.push(BASE64_CHARS[((triple >> 12) & 0x3F) as usize]);
        if chunk.len() > 1 {
            output.push(BASE64_CHARS[((triple >> 6) & 0x3F) as usize]);
        } else {
            output.push(b'=');
        }
        if chunk.len() > 2 {
            output.push(BASE64_CHARS[(triple & 0x3F) as usize]);
        } else {
            output.push(b'=');
        }
    }

    // SAFETY: BASE64_CHARS and '=' are all ASCII; output contains only those bytes.
    unsafe { String::from_utf8_unchecked(output) }
}

/// Extracts the username from a display token (base64-encoded "username:token").
///
/// Maps to C++ `UsernameFromDisplayToken`.
fn username_from_display_token(display_token: &str) -> String {
    let decoded_bytes =
        base64_decode(display_token.as_bytes()).expect("display token must be valid base64");
    let decoded = String::from_utf8_lossy(&decoded_bytes);
    decoded
        .find(TOKEN_DELIMITER)
        .map(|pos| decoded[..pos].to_string())
        .unwrap_or_default()
}

/// Extracts the token from a display token (base64-encoded "username:token").
///
/// Maps to C++ `TokenFromDisplayToken`.
fn token_from_display_token(display_token: &str) -> String {
    let decoded_bytes =
        base64_decode(display_token.as_bytes()).expect("display token must be valid base64");
    let decoded = String::from_utf8_lossy(&decoded_bytes);
    decoded
        .find(TOKEN_DELIMITER)
        .map(|pos| decoded[pos + 1..].to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Ban list I/O (from yuzu_room.cpp)
// ---------------------------------------------------------------------------

/// Loads the ban list from a file.
///
/// Maps to C++ `LoadBanList`.
///
/// The file format is:
/// ```text
/// YuzuRoom-BanList-1
/// <username1>
/// <username2>
///
/// <ip1>
/// <ip2>
/// ```
/// An empty line separates the username list from the IP list.
fn load_ban_list(path: &str) -> BanList {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => {
            log::error!("Could not open ban list!");
            return (Vec::new(), Vec::new());
        }
    };

    let reader = io::BufReader::new(file);
    let mut lines = reader.lines();

    // Validate magic header
    let magic = match lines.next() {
        Some(Ok(line)) => line,
        _ => {
            log::error!("Could not open ban list!");
            return (Vec::new(), Vec::new());
        }
    };
    if magic != BAN_LIST_MAGIC {
        log::error!("Ban list is not valid!");
        return (Vec::new(), Vec::new());
    }

    // false = username ban list section, true = ip ban list section
    let mut ban_list_type = false;
    let mut username_ban_list: UsernameBanList = Vec::new();
    let mut ip_ban_list: IpBanList = Vec::new();

    for line_result in lines {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };
        // Strip null bytes and whitespace, matching upstream StripSpaces + erase('\0')
        let line: String = line.chars().filter(|&c| c != '\0').collect();
        let line = line.trim().to_string();

        if line.is_empty() {
            // An empty line marks the start of the IP ban list
            ban_list_type = true;
            continue;
        }
        if ban_list_type {
            ip_ban_list.push(line);
        } else {
            username_ban_list.push(line);
        }
    }

    (username_ban_list, ip_ban_list)
}

/// Saves the ban list to a file.
///
/// Maps to C++ `SaveBanList`.
fn save_ban_list(ban_list: &BanList, path: &str) {
    let mut file = match fs::File::create(path) {
        Ok(f) => f,
        Err(_) => {
            log::error!("Could not save ban list!");
            return;
        }
    };

    if writeln!(file, "{}", BAN_LIST_MAGIC).is_err() {
        log::error!("Could not save ban list!");
        return;
    }

    // Username ban list
    for username in &ban_list.0 {
        let _ = writeln!(file, "{}", username);
    }
    let _ = writeln!(file);

    // IP ban list
    for ip in &ban_list.1 {
        let _ = writeln!(file, "{}", ip);
    }
}

// ---------------------------------------------------------------------------
// Logging initialisation (from yuzu_room.cpp InitializeLogging)
// ---------------------------------------------------------------------------

/// Initialises the logging backend.
///
/// Maps to C++ `InitializeLogging`.
fn initialize_logging(_log_file: &str) {
    env_logger::init();
}

// ---------------------------------------------------------------------------
// Application entry point (from yuzu_room.cpp main)
// ---------------------------------------------------------------------------

/// Application entry point.
///
/// Maps to C++ `main` in `yuzu_room.cpp`.
fn main() {
    let args = Args::parse();

    initialize_logging(&args.log_file);

    // --- Validate required arguments ---

    let room_name = match args.room_name {
        Some(ref n) if !n.is_empty() => n.clone(),
        _ => {
            log::error!("Room name is empty!");
            std::process::exit(-1);
        }
    };

    let preferred_game = match args.preferred_game {
        Some(ref g) if !g.is_empty() => g.clone(),
        _ => {
            log::error!("Preferred game is empty!");
            std::process::exit(-1);
        }
    };

    // Parse preferred_game_id as hex (u64), matching upstream `strtoull(optarg, &endarg, 16)`
    let preferred_game_id: u64 = match &args.preferred_game_id {
        Some(s) => {
            let s = s.trim_start_matches("0x").trim_start_matches("0X");
            u64::from_str_radix(s, 16).unwrap_or(0)
        }
        None => 0,
    };

    if preferred_game_id == 0 {
        log::error!(
            "preferred-game-id not set!\nThis should get set to allow users to find your room.\nSet with --preferred-game-id id"
        );
    }

    if args.max_members > MAX_CONCURRENT_CONNECTIONS || args.max_members < 2 {
        log::error!(
            "max_members needs to be in the range 2 - {}!",
            MAX_CONCURRENT_CONNECTIONS
        );
        std::process::exit(-1);
    }

    let bind_address = args.bind_address.unwrap_or_default();
    if bind_address.is_empty() {
        log::info!("Bind address is empty: defaulting to 0.0.0.0");
    }

    if args.port > u16::MAX as u32 {
        log::error!("Port needs to be in the range 0 - 65535!");
        std::process::exit(-1);
    }
    let port = args.port as u16;

    let ban_list_file = args.ban_list_file.unwrap_or_default();
    if ban_list_file.is_empty() {
        log::error!(
            "Ban list file not set!\nThis should get set to load and save room ban list.\nSet with --ban-list-file <file>"
        );
    }

    let mut token = args.token.unwrap_or_default();
    let mut username = args.username.unwrap_or_default();
    let web_api_url = args.web_api_url.unwrap_or_default();
    let password = args.password.unwrap_or_default();
    let room_description = args.room_description.unwrap_or_default();
    let mut enable_yuzu_mods = args.enable_yuzu_mods;

    let mut announce = true;
    if token.is_empty() && announce {
        announce = false;
        log::info!("Token is empty: Hosting a private room");
    }
    if web_api_url.is_empty() && announce {
        announce = false;
        log::info!("Endpoint url is empty: Hosting a private room");
    }
    if announce {
        let mut settings = common::settings::values_mut();
        settings.web_api_url.set_value(web_api_url.clone());
        if username.is_empty() {
            log::info!("Hosting a public room");
            pad_token(&mut token);
            settings
                .eden_username
                .set_value(username_from_display_token(&token));
            username.clone_from(settings.eden_username.get_value());
            settings
                .eden_token
                .set_value(token_from_display_token(&token));
        } else {
            log::info!("Hosting a public room");
            settings.eden_username.set_value(username.clone());
            settings.eden_token.set_value(token.clone());
        }
    }
    if !announce && enable_yuzu_mods {
        enable_yuzu_mods = false;
        log::info!("Can not enable yuzu Moderators for private rooms");
    }

    // Load the ban list
    let ban_list: BanList = if !ban_list_file.is_empty() {
        load_ban_list(&ban_list_file)
    } else {
        (Vec::new(), Vec::new())
    };

    // Build verify backend.
    // Upstream: uses WebService::VerifyUserJWT when ENABLE_WEB_SERVICE is defined.
    // The VerifyUserJwtBackend wrapper bridges web_service and network crates.
    let verify_backend: Box<dyn Backend> = if announce {
        Box::new(VerifyUserJwtBackend::new(&web_api_url))
    } else {
        Box::new(NullBackend)
    };

    let mut network = RoomNetwork::new();
    network.init();

    if let Some(room) = network.get_room().upgrade() {
        let preferred_game_info = GameInfo {
            name: preferred_game.clone(),
            id: preferred_game_id,
            version: String::new(),
        };

        if !room.create(
            &room_name,
            &room_description,
            &bind_address,
            port,
            &password,
            args.max_members,
            &username,
            preferred_game_info,
            Some(verify_backend),
            &ban_list,
            enable_yuzu_mods,
        ) {
            log::info!("Failed to create room: ");
            std::process::exit(-1);
        }

        log::info!("Room is open. Close with Q+Enter...");

        let announce_session = AnnounceMultiplayerSession::new(network.get_room());
        if announce {
            announce_session.start();
        }

        // Wait for operator input (any non-empty line exits)
        let stdin = io::stdin();
        loop {
            use network::room::RoomState;
            if room.get_state() != RoomState::Open {
                break;
            }
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) if !line.trim().is_empty() => break,
                _ => {}
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        if announce {
            announce_session.stop();
        }
        // announce_session is dropped here (matching upstream `announce_session.reset()`)

        // Save the ban list
        if !ban_list_file.is_empty() {
            save_ban_list(&room.get_ban_list(), &ban_list_file);
        }
        room.destroy();
    }

    network.shutdown();
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode_decode_roundtrip() {
        let cases: &[(&[u8], &str)] = &[
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foobar", "Zm9vYmFy"),
            (b"Man", "TWFu"),
        ];
        for (raw, encoded) in cases {
            assert_eq!(base64_encode(raw), *encoded, "encode failed for {:?}", raw);
            if encoded.is_empty() {
                assert_eq!(
                    base64_decode(encoded.as_bytes()),
                    Some(Vec::new()),
                    "decode failed for {:?}",
                    encoded
                );
            } else {
                assert_eq!(
                    base64_decode(encoded.as_bytes()),
                    Some(raw.to_vec()),
                    "decode failed for {:?}",
                    encoded
                );
            }
        }
    }

    #[test]
    fn test_ban_list_magic_constant() {
        assert_eq!(BAN_LIST_MAGIC, "YuzuRoom-BanList-1");
    }

    #[test]
    fn test_token_delimiter_constant() {
        assert_eq!(TOKEN_DELIMITER, ':');
    }

    #[test]
    fn test_display_token_extracts_eden_credentials() {
        let display_token = base64_encode(b"reviewer:secret-token");

        assert_eq!(username_from_display_token(&display_token), "reviewer");
        assert_eq!(token_from_display_token(&display_token), "secret-token");
    }

    #[test]
    fn test_load_ban_list_missing_file() {
        // A non-existent file should return an empty ban list without panicking.
        let result = load_ban_list("/nonexistent/path/banlist.txt");
        assert!(result.0.is_empty());
        assert!(result.1.is_empty());
    }

    #[test]
    fn test_save_and_load_ban_list_roundtrip() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("banlist.txt");
        let path_str = path.to_str().unwrap();

        let ban_list: BanList = (
            vec!["user1".to_string(), "user2".to_string()],
            vec!["192.168.1.1".to_string()],
        );

        save_ban_list(&ban_list, path_str);
        let loaded = load_ban_list(path_str);

        assert_eq!(loaded.0, vec!["user1", "user2"]);
        assert_eq!(loaded.1, vec!["192.168.1.1"]);
    }

    #[test]
    fn test_load_ban_list_invalid_magic() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("bad_banlist.txt");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "BadMagic").unwrap();
        writeln!(f, "user1").unwrap();

        let result = load_ban_list(path.to_str().unwrap());
        assert!(result.0.is_empty());
        assert!(result.1.is_empty());
    }
}
