// SPDX-FileCopyrightText: 2014 Citra Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! ruzu-cmd: SDL3 command-line emulator frontend.
//!
//! Port of upstream `yuzu_cmd/yuzu.cpp`.
//!
//! This binary is the SDL3-based command-line frontend for the ruzu Nintendo
//! Switch emulator. It parses command-line arguments, loads an SDL3
//! configuration, picks the correct renderer backend (OpenGL, Vulkan, or
//! Null), initialises the emulator core, and runs the main event loop.
//!
//! # Argument mapping
//!
//! | C++ long option      | Rust (`clap`) field      |
//! |----------------------|--------------------------|
//! | `-c` / `--config`    | `config`                 |
//! | `-f` / `--fullscreen`| `fullscreen`             |
//! | `-g` / `--game`      | `game`                   |
//! | `-m` / `--multiplayer`| `multiplayer`           |
//! | `-p` / `--program`   | `program`                |
//! | `-u` / `--user`      | `user`                   |
//! | `-v` / `--version`   | handled by clap          |

use clap::{CommandFactory, Parser};
use common::settings_enums::RendererBackend;
use libc;
use network::room::{StatusMessageTypes, DEFAULT_ROOM_PORT, NO_PREFERRED_IP};
use network::room_member::{ChatEntry, RoomMemberError, RoomMemberState, StatusMessageEntry};
use std::ffi::OsStr;

pub mod emu_window;
pub mod sdl_config;

use emu_window::{
    emu_window_sdl3_gl::EmuWindowSdl3Gl, emu_window_sdl3_null::EmuWindowSdl3Null,
    emu_window_sdl3_vk::EmuWindowSdl3Vk,
};
use sdl_config::SdlConfig;

fn resolve_renderer_backend(
    renderer_override: Option<&str>,
    configured_backend: RendererBackend,
) -> &'static str {
    let configured_name = match configured_backend {
        RendererBackend::OpenGlGlsl
        | RendererBackend::OpenGlGlasm
        | RendererBackend::OpenGlSpirV => "opengl",
        RendererBackend::Vulkan => "vulkan",
        RendererBackend::Null => "null",
    };

    let Some(renderer_override) = renderer_override else {
        return configured_name;
    };

    match renderer_override.to_lowercase().as_str() {
        "opengl" | "gl" | "0" => "opengl",
        "vulkan" | "vk" | "1" => "vulkan",
        "null" | "2" => "null",
        other => {
            log::warn!(
                "Unknown renderer '{}', using configured backend {}",
                other,
                configured_name
            );
            configured_name
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_multiplayer_config, resolve_renderer_backend, MultiplayerConfig, RendererBackend,
    };
    use network::room::DEFAULT_ROOM_PORT;

    #[test]
    fn configured_renderer_is_used_without_cli_override() {
        assert_eq!(
            resolve_renderer_backend(None, RendererBackend::OpenGlGlsl),
            "opengl"
        );
        assert_eq!(
            resolve_renderer_backend(None, RendererBackend::Vulkan),
            "vulkan"
        );
        assert_eq!(
            resolve_renderer_backend(None, RendererBackend::Null),
            "null"
        );
    }

    #[test]
    fn cli_renderer_overrides_configured_renderer() {
        assert_eq!(
            resolve_renderer_backend(Some("vulkan"), RendererBackend::OpenGlGlsl),
            "vulkan"
        );
        assert_eq!(
            resolve_renderer_backend(Some("GL"), RendererBackend::Vulkan),
            "opengl"
        );
    }

    #[test]
    fn multiplayer_argument_matches_upstream_fields_and_default_port() {
        assert_eq!(
            parse_multiplayer_config("FreePlayer:secret@example.org:24872"),
            Ok(MultiplayerConfig {
                nickname: "FreePlayer".into(),
                password: "secret".into(),
                address: "example.org".into(),
                port: 24872,
            })
        );
        assert_eq!(
            parse_multiplayer_config("Free Player@example.org")
                .unwrap()
                .port,
            DEFAULT_ROOM_PORT
        );
    }

    #[test]
    fn multiplayer_port_preserves_upstream_base_zero_and_u16_cast() {
        assert_eq!(
            parse_multiplayer_config("FreePlayer@example.org:010")
                .unwrap()
                .port,
            8
        );
        assert_eq!(
            parse_multiplayer_config("FreePlayer@example.org:65537")
                .unwrap()
                .port,
            1
        );
        assert_eq!(
            parse_multiplayer_config("FreePlayer@example.org:09")
                .unwrap()
                .port,
            0
        );
        assert_eq!(
            parse_multiplayer_config(
                "FreePlayer@example.org:999999999999999999999999999999999999999"
            )
            .unwrap()
            .port,
            u16::MAX
        );
    }

    #[test]
    fn multiplayer_argument_rejects_upstream_invalid_forms() {
        assert!(parse_multiplayer_config("missing-address").is_err());
        assert!(parse_multiplayer_config("invalid/name@example.org").is_err());
    }
}

// ---------------------------------------------------------------------------
// CLI argument definitions
// ---------------------------------------------------------------------------

/// ruzu-cmd — SDL3 command-line Nintendo Switch emulator frontend.
///
/// Maps to the `getopt_long` option table in C++ `main()`.
#[derive(Parser, Debug)]
#[command(name = "ruzu-cmd")]
#[command(version)]
#[command(about = "ruzu Nintendo Switch emulator (SDL3 CLI frontend)")]
struct Args {
    /// Load the specified configuration file.
    ///
    /// Maps to C++ `-c` / `--config`.
    #[arg(short = 'c', long = "config", value_name = "PATH")]
    config: Option<String>,

    /// Start in fullscreen mode.
    ///
    /// Maps to C++ `-f` / `--fullscreen`.
    #[arg(short = 'f', long = "fullscreen")]
    fullscreen: bool,

    /// File path of the game to load.
    ///
    /// Maps to C++ `-g` / `--game`.
    #[arg(short = 'g', long = "game", value_name = "PATH")]
    game: Option<String>,

    /// Multiplayer connection string: `nick[:password]@address[:port]`.
    ///
    /// Maps to C++ `-m` / `--multiplayer`.
    #[arg(
        short = 'm',
        long = "multiplayer",
        value_name = "nick:password@address:port"
    )]
    multiplayer: Option<String>,

    /// Arguments to pass to the guest executable.
    ///
    /// Maps to C++ `-p` / `--program`.
    #[arg(short = 'p', long = "program", value_name = "ARGS")]
    program: Option<String>,

    /// Select a specific user profile (0–7).
    ///
    /// Maps to C++ `-u` / `--user`.
    #[arg(short = 'u', long = "user", value_name = "INDEX")]
    user: Option<u8>,

    /// Renderer backend: opengl, vulkan, or null.
    ///
    /// Not in upstream CLI — added for convenience while Settings is not fully ported.
    #[arg(short = 'r', long = "renderer", value_name = "BACKEND")]
    renderer: Option<String>,

    /// Positional game path (alternative to --game).
    ///
    /// Maps to C++ bare positional argument handling in the `getopt_long` loop.
    #[arg(value_name = "FILENAME")]
    filename: Option<String>,
}

// ---------------------------------------------------------------------------
// Network callbacks
//
// Maps to the static helpers `OnStateChanged`, `OnNetworkError`,
// `OnMessageReceived`, `OnStatusMessageReceived` in `yuzu.cpp`.
// ---------------------------------------------------------------------------

/// Called when the room-member connection state changes.
///
/// Maps to C++ `static void OnStateChanged(const Network::RoomMember::State&)`.
fn on_state_changed(state: &RoomMemberState) {
    match state {
        RoomMemberState::Idle => log::debug!("Network: Network is idle"),
        RoomMemberState::Joining => {
            log::debug!("Network: Connection sequence to room started")
        }
        RoomMemberState::Joined => log::debug!("Network: Successfully joined to the room"),
        RoomMemberState::Moderator => {
            log::debug!("Network: Successfully joined the room as a moderator")
        }
        RoomMemberState::Uninitialized => {}
    }
}

/// Called when a network error occurs. Logs the error and may exit.
///
/// Maps to C++ `static void OnNetworkError(const Network::RoomMember::Error&)`.
fn on_network_error(error: &RoomMemberError) {
    match error {
        RoomMemberError::LostConnection => log::debug!("Network: Lost connection to the room"),
        RoomMemberError::CouldNotConnect => {
            log::error!("Network: Error: Could not connect");
            std::process::exit(1);
        }
        RoomMemberError::NameCollision => {
            log::error!(
                "Network: You tried to use the same nickname as another user \
                 that is connected to the Room"
            );
            std::process::exit(1);
        }
        RoomMemberError::IpCollision => {
            log::error!(
                "Network: You tried to use the same fake IP-Address as another user \
                 that is connected to the Room"
            );
            std::process::exit(1);
        }
        RoomMemberError::WrongPassword => {
            log::error!("Network: Room replied with: Wrong password");
            std::process::exit(1);
        }
        RoomMemberError::WrongVersion => {
            log::error!(
                "Network: You are using a different version than the room \
                 you are trying to connect to"
            );
            std::process::exit(1);
        }
        RoomMemberError::RoomIsFull => {
            log::error!("Network: The room is full");
            std::process::exit(1);
        }
        RoomMemberError::HostKicked => log::error!("Network: You have been kicked by the host"),
        RoomMemberError::HostBanned => log::error!("Network: You have been banned by the host"),
        RoomMemberError::UnknownError => log::error!("Network: UnknownError"),
        RoomMemberError::PermissionDenied => log::error!("Network: PermissionDenied"),
        RoomMemberError::NoSuchUser => log::error!("Network: NoSuchUser"),
    }
}

/// Called when a chat message is received from another room member.
///
/// Maps to C++ `static void OnMessageReceived(const Network::ChatEntry&)`.
fn on_message_received(message: &ChatEntry) {
    println!("\n{}: {}\n", message.nickname, message.message);
}

/// Called when a status message (join/leave/kick/ban) is received.
///
/// Maps to C++ `static void OnStatusMessageReceived(const Network::StatusMessageEntry&)`.
fn on_status_message_received(status: &StatusMessageEntry) {
    let message = match status.message_type {
        StatusMessageTypes::IdMemberJoin => format!("{} has joined", status.nickname),
        StatusMessageTypes::IdMemberLeave => format!("{} has left", status.nickname),
        StatusMessageTypes::IdMemberKicked => format!("{} has been kicked", status.nickname),
        StatusMessageTypes::IdMemberBanned => format!("{} has been banned", status.nickname),
        StatusMessageTypes::IdAddressUnbanned => {
            format!("{} has been unbanned", status.nickname)
        }
    };
    println!("\n* {}\n", message);
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MultiplayerConfig {
    nickname: String,
    password: String,
    address: String,
    port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MultiplayerParseError {
    WrongFormat,
    InvalidNickname,
    EmptyAddress,
}

impl MultiplayerParseError {
    fn message(self) -> &'static str {
        match self {
            Self::WrongFormat => "Wrong format for option --multiplayer",
            Self::InvalidNickname => {
                "Nickname is not valid. Must be 4 to 20 alphanumeric characters"
            }
            Self::EmptyAddress => "Address to room must not be empty",
        }
    }
}

fn strtoul_base_zero_as_u16(value: &str) -> u16 {
    let (digits, radix) = if value.len() > 1 && value.starts_with('0') {
        (&value[1..], 8)
    } else {
        (value, 10)
    };
    let mut result = 0_u64;
    for digit in digits.bytes() {
        let value = match digit {
            b'0'..=b'7' => u64::from(digit - b'0'),
            b'8'..=b'9' if radix == 10 => u64::from(digit - b'0'),
            _ => break,
        };
        result = result.saturating_mul(radix).saturating_add(value);
    }
    result as u16
}

fn parse_multiplayer_config(value: &str) -> Result<MultiplayerConfig, MultiplayerParseError> {
    let format = regex::Regex::new(r"^([^:]+)(?::(.+))?@([^:]+)(?::([0-9]+))?$")
        .expect("upstream multiplayer expression is valid");
    let captures = format
        .captures(value)
        .ok_or(MultiplayerParseError::WrongFormat)?;
    let nickname = captures[1].to_string();
    let password = captures
        .get(2)
        .map_or_else(String::new, |value| value.as_str().to_string());
    let address = captures[3].to_string();
    let port = captures.get(4).map_or(DEFAULT_ROOM_PORT, |value| {
        strtoul_base_zero_as_u16(value.as_str())
    });
    let nickname_format =
        regex::Regex::new(r"^[a-zA-Z0-9._\- ]+$").expect("upstream nickname expression is valid");
    if !nickname_format.is_match(&nickname) {
        return Err(MultiplayerParseError::InvalidNickname);
    }
    if address.is_empty() {
        return Err(MultiplayerParseError::EmptyAddress);
    }
    Ok(MultiplayerConfig {
        nickname,
        password,
        address,
        port,
    })
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Application entry point.
///
/// Maps to C++ `int main(int argc, char** argv)` in `yuzu.cpp`.
///
/// Upstream order of operations:
/// 1. Parse args, create config
/// 2. system.Initialize()
/// 3. Create emu_window based on renderer backend
/// 4. system.SetContentProvider/SetFilesystem/CreateFactories
/// 5. system.Load(*emu_window, filepath, load_parameters)
/// 6. system.GPU().Start()
/// 7. system.GetCpuManager().OnGpuReady()
/// 8. system.Run()  — starts CPU threads in background
/// 9. while (emu_window->IsOpen()) { emu_window->WaitEvent(); }
/// 10. system.Pause(), system.ShutdownMainProcess()
// ── SIGILL trace — Linux x86_64 only (uses /proc/self/maps + ucontext gregs) ──
#[cfg(target_os = "linux")]
static FASTMEM_BASE_FOR_SIGILL: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(target_os = "linux")]
static DUMP_FASTMEM_VAS: std::sync::LazyLock<Vec<u64>> = std::sync::LazyLock::new(|| {
    std::env::var("RUZU_DUMP_FASTMEM_VAS")
        .ok()
        .map(|spec| {
            spec.split(',')
                .filter_map(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
                .collect()
        })
        .unwrap_or_default()
});

#[cfg(target_os = "linux")]
fn detect_fastmem_base_for_sigill() {
    let maps = match std::fs::read_to_string("/proc/self/maps") {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut best: (usize, usize) = (0, 0); // (size, start)
    for line in maps.lines() {
        // Format: addr-addr perm offset dev inode pathname
        let Some(dash) = line.find('-') else { continue };
        let space = match line[dash..].find(' ') {
            Some(p) => dash + p,
            None => continue,
        };
        let start = u64::from_str_radix(&line[..dash], 16).unwrap_or(0) as usize;
        let end = u64::from_str_radix(&line[dash + 1..space], 16).unwrap_or(0) as usize;
        let size = end.saturating_sub(start);
        let perm = &line[space + 1..space + 1 + 4.min(line.len() - space - 1)];
        if size > 100 * (1 << 30) && perm.contains("rw") {
            if size > best.0 {
                best = (size, start);
            }
        }
    }
    if best.1 != 0 {
        FASTMEM_BASE_FOR_SIGILL.store(best.1, std::sync::atomic::Ordering::Relaxed);
        eprintln!(
            "[SIGILL-init] fastmem arena base auto-detected: 0x{:016X} (size {} GB)",
            best.1,
            best.0 >> 30
        );
    }
}

/// SIGILL handler — installed when RUZU_SIGILL_TRACE=1. Linux x86_64 only.
#[cfg(target_os = "linux")]
extern "C" fn ruzu_sigill_handler(
    _sig: libc::c_int,
    _info: *mut libc::siginfo_t,
    ucontext: *mut libc::c_void,
) {
    unsafe {
        let uc = ucontext as *mut libc::ucontext_t;
        let mctx = &(*uc).uc_mcontext;
        let rip = mctx.gregs[libc::REG_RIP as usize] as u64;
        let rax = mctx.gregs[libc::REG_RAX as usize] as u64;
        let r11 = mctx.gregs[libc::REG_R11 as usize] as u64;
        let r15 = mctx.gregs[libc::REG_R15 as usize] as u64;
        let bytes_around: [u8; 8] = std::ptr::read_unaligned(rip as *const [u8; 8]);

        // Sentinel detection: rax==0xCAFEF00D is the legacy SetX trap
        // marker; r11==0xCAFEF00D (sign-extended to 0xFFFFFFFFCAFEF00D)
        // is the fastmem-W64 trap (uses R11 as scratch to avoid RAX
        // regalloc conflicts). Vaddr is at [RSP+16] for fastmem trap.
        //
        // The new RUZU_TRAP_FASTMEM_ANY_VADDR_RANGE trap encodes write
        // width in the low byte: 0xCAFEF008 (W8), 0xCAFEF010 (W16),
        // 0xCAFEF020 (W32), 0xCAFEF040 (W64). All such sentinels also
        // use the stack-recovery convention.
        let rsp = mctx.gregs[libc::REG_RSP as usize] as u64;
        let is_width_sentinel = |v: u64| -> bool {
            // High 32 bits of sign-extended sentinel are 0xFFFFFFFF; low 32
            // bits are 0xCAFEF0XX with XX ∈ {0D, 08, 10, 20, 40}.
            let low = v & 0xFFFF_FFFF;
            let high = v >> 32;
            (high == 0xFFFF_FFFF || high == 0)
                && (low & 0xFFFF_FF00) == 0xCAFE_F000
                && matches!(low & 0xFF, 0x0D | 0x08 | 0x10 | 0x20 | 0x40 | 0x80 | 0xE1)
        };
        let has_recovery_stack = is_width_sentinel(rax) || is_width_sentinel(r11);
        let recovered_vaddr = if has_recovery_stack {
            std::ptr::read_unaligned((rsp + 16) as *const u64)
        } else {
            0
        };
        let recovered_aux = if has_recovery_stack {
            std::ptr::read_unaligned((rsp + 24) as *const u64)
        } else {
            0
        };
        let s = format!(
            "[SIGILL] rip=0x{:016X} rax=0x{:016X} r11=0x{:016X} r15=0x{:016X} bytes={:02X?} recovered_vaddr=0x{:016X} recovered_aux=0x{:016X}\n",
            rip, rax, r11, r15, bytes_around, recovered_vaddr, recovered_aux
        );
        let bytes = s.as_bytes();
        libc::write(2, bytes.as_ptr() as *const libc::c_void, bytes.len());

        // R15 == JitState pointer when in JIT code. A64JitState layout:
        //   offset 0..248:  reg[0..31] (X0..X30)
        //   offset 248:     sp
        //   offset 256:     pc
        // RAX (the reg_index_imm we just wrote) tells which X-register
        // triggered the trap. Dump all guest GPRs + PC + SP + the
        // reg-being-set (RAX = reg_index).
        if r15 != 0 {
            // Cap the readback to avoid faulting if R15 is not a valid pointer.
            // We rely on stable struct layout matching `A64JitState`.
            let pc = std::ptr::read_unaligned((r15 + 256) as *const u64);
            let sp = std::ptr::read_unaligned((r15 + 248) as *const u64);
            let s2 = format!(
                "[SIGILL]   trapping reg index = X{}, guest PC = 0x{:016X}, guest SP = 0x{:016X}\n",
                rax, pc, sp
            );
            libc::write(2, s2.as_bytes().as_ptr() as *const libc::c_void, s2.len());
            // Dump X0..X30
            for i in 0..31 {
                let x = std::ptr::read_unaligned((r15 + (i * 8) as u64) as *const u64);
                let s3 = format!("[SIGILL]   X{:<2} = 0x{:016X}\n", i, x);
                libc::write(2, s3.as_bytes().as_ptr() as *const libc::c_void, s3.len());
            }

            // A32 traps share the same host R15 convention but the JitState
            // layout is `u32 reg[16]` followed by upper/CPSR shadow fields.
            // Print this alongside the A64 view so A32 fastmem diagnostics can
            // recover the actual guest PC (R15) without changing the trap ABI.
            let mut a32_regs = [0u32; 16];
            for i in 0..16 {
                a32_regs[i] = std::ptr::read_unaligned((r15 + (i * 4) as u64) as *const u32);
            }
            let a32_upper = std::ptr::read_unaligned((r15 + 64) as *const u32);
            let a32_cpsr_nzcv = std::ptr::read_unaligned((r15 + 76) as *const u32);
            let s4 = format!(
                "[SIGILL]   A32 R15/PC=0x{:08X} SP=0x{:08X} LR=0x{:08X} upper=0x{:08X} cpsr_nzcv=0x{:08X}\n",
                a32_regs[15], a32_regs[13], a32_regs[14], a32_upper, a32_cpsr_nzcv
            );
            libc::write(2, s4.as_bytes().as_ptr() as *const libc::c_void, s4.len());
            for i in 0..16 {
                let s5 = format!("[SIGILL]   R{:<2} = 0x{:08X}\n", i, a32_regs[i]);
                libc::write(2, s5.as_bytes().as_ptr() as *const libc::c_void, s5.len());
            }
        }

        // Auto-dump the value at recovered_vaddr (the trap's destination).
        // For any fastmem-trap with stack-recovery, recovered_vaddr is the
        // guest vaddr the JIT was writing to; after the trap fires, host
        // memory at [arena_base + recovered_vaddr] holds the just-stored
        // value. This is more useful than printing only the value reg
        // (which can be clobbered by ordered xchg or stale in JitState).
        let fastmem_base_value = FASTMEM_BASE_FOR_SIGILL.load(std::sync::atomic::Ordering::Relaxed);
        if has_recovery_stack && fastmem_base_value != 0 && recovered_vaddr != 0 {
            let host_addr = fastmem_base_value.wrapping_add(recovered_vaddr as usize);
            let stored = std::ptr::read_unaligned(host_addr as *const u64);
            let s = format!(
                "[SIGILL]   stored_value@vaddr=0x{:016X} = 0x{:016X}\n",
                recovered_vaddr, stored,
            );
            libc::write(2, s.as_bytes().as_ptr() as *const libc::c_void, s.len());
        }

        // RUZU_DUMP_FASTMEM_VAS=0xVADDR[,0xVADDR,...] — at SIGILL time, also
        // dump 16 bytes of host memory at fastmem_base + guest_vaddr for each
        // listed guest vaddr. Used to verify what the JIT's mov [r13+vaddr]
        // would have actually read.
        let fastmem_base = FASTMEM_BASE_FOR_SIGILL.load(std::sync::atomic::Ordering::Relaxed);
        if fastmem_base != 0 {
            // VAddrs to dump: encoded as RUZU_DUMP_FASTMEM_VAS env value.
            // Not safe to call env::var inside a signal handler (it allocates)
            // — so we read the env via a static parsed at startup.
            for &vaddr in &*DUMP_FASTMEM_VAS {
                let host_addr = fastmem_base.wrapping_add(vaddr as usize);
                let lo = std::ptr::read_unaligned(host_addr as *const u64);
                let hi = std::ptr::read_unaligned((host_addr + 8) as *const u64);
                let s = format!(
                    "[SIGILL]   *[host=0x{:016X}, guest=0x{:016X}] = 0x{:016X} 0x{:016X}\n",
                    host_addr, vaddr, lo, hi,
                );
                libc::write(2, s.as_bytes().as_ptr() as *const libc::c_void, s.len());
            }
        }

        // RUZU_SIGILL_MAX=N — log up to N traps then exit. Default 1 (legacy behavior).
        // Allows bisecting causal chain by capturing multiple corrupt-write events.
        use std::sync::atomic::{AtomicU32, Ordering};
        static SIGILL_COUNT: AtomicU32 = AtomicU32::new(0);
        let max = std::env::var("RUZU_SIGILL_MAX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1);
        let n = SIGILL_COUNT.fetch_add(1, Ordering::Relaxed);
        if n + 1 < max {
            // Skip the UD2 (2 bytes) and resume.
            (*uc).uc_mcontext.gregs[libc::REG_RIP as usize] = (rip + 2) as i64;
            return;
        }
        libc::_exit(132);
    }
}

fn main() {
    // Initialise logging backend (upstream: Common::Log::Initialize /
    // SetColorConsoleBackendEnabled / Start).
    common::logging::backend::initialize_from_env(None);

    // `RUZU_PROFILE_IPC=1` / `RUZU_PROFILE_SVC=1` — install a single SIGUSR2
    // handler that dumps whichever profile is enabled. atexit hooks fire on
    // normal exit only (not on signal-terminated runs).
    let want_ipc_profile = std::env::var_os("RUZU_PROFILE_IPC").is_some();
    let want_svc_profile = std::env::var_os("RUZU_PROFILE_SVC").is_some();
    let want_wake_profile = std::env::var_os("RUZU_PROFILE_WAKE").is_some();
    let want_gap_profile = std::env::var_os("RUZU_PROFILE_GAP").is_some();
    let want_svc_per_tid = std::env::var_os("RUZU_PROFILE_SVC_PER_TID").is_some();
    let want_svc_summary = std::env::var_os("RUZU_PROFILE_SVC_SUMMARY").is_some();
    let want_svc_ring = std::env::var_os("RUZU_PROFILE_SVC_RING").is_some();
    let want_nvdrv_ioctl_profile = std::env::var_os("RUZU_PROFILE_NVDRV_IOCTL").is_some();
    let want_nvdrv_ioctl_history = std::env::var_os("RUZU_DUMP_NVDRV_IOCTL_HISTORY").is_some();
    let want_ipc_service_profile = std::env::var_os("RUZU_PROFILE_IPC_SERVICE").is_some();
    let want_ipc_phase_profile = std::env::var_os("RUZU_PROFILE_IPC_PHASES").is_some();
    let want_bqp_slot_profile = std::env::var_os("RUZU_PROFILE_BQP_SLOTS").is_some();
    let want_binder_txn_profile = std::env::var_os("RUZU_PROFILE_BINDER_TXN").is_some();
    let want_bqp_wait_profile = std::env::var_os("RUZU_PROFILE_BQP_WAIT").is_some();
    let want_nvnflinger_history = std::env::var_os("RUZU_DUMP_NVNFLINGER_HISTORY").is_some();
    let want_hwc_cache_profile = std::env::var_os("RUZU_PROFILE_HWC_CACHE").is_some();
    let want_vsync_profile = std::env::var_os("RUZU_PROFILE_VSYNC").is_some();
    let want_submit_gpfifo_profile = std::env::var_os("RUZU_PROFILE_SUBMIT_GPFIFO").is_some();
    let want_gpu_thread_profile = std::env::var_os("RUZU_PROFILE_GPU_THREAD").is_some();
    let want_gl_draw_stall_profile = std::env::var_os("RUZU_PROFILE_GL_DRAW_STALL").is_some();
    let want_refresh_stages_stall_profile =
        std::env::var_os("RUZU_PROFILE_REFRESH_STAGES_STALL").is_some();
    let want_make_shader_info_stall_profile =
        std::env::var_os("RUZU_PROFILE_MAKE_SHADER_INFO_STALL").is_some();
    let want_shader_register_stall_profile =
        std::env::var_os("RUZU_PROFILE_SHADER_REGISTER_STALL").is_some();
    let want_rasterizer_mark_cached_stall_profile =
        std::env::var_os("RUZU_PROFILE_RASTERIZER_MARK_CACHED_STALL").is_some();
    if want_ipc_profile
        || want_svc_profile
        || want_wake_profile
        || want_gap_profile
        || want_svc_per_tid
        || want_svc_summary
        || want_svc_ring
        || want_nvdrv_ioctl_profile
        || want_nvdrv_ioctl_history
        || want_ipc_service_profile
        || want_ipc_phase_profile
        || want_bqp_slot_profile
        || want_binder_txn_profile
        || want_bqp_wait_profile
        || want_nvnflinger_history
        || want_hwc_cache_profile
        || want_vsync_profile
        || want_submit_gpfifo_profile
        || want_gpu_thread_profile
        || want_gl_draw_stall_profile
        || want_refresh_stages_stall_profile
        || want_make_shader_info_stall_profile
        || want_shader_register_stall_profile
        || want_rasterizer_mark_cached_stall_profile
    {
        #[cfg(unix)]
        extern "C" fn profile_signal(_signum: libc::c_int) {
            ruzu_core::hle::kernel::svc::svc_ipc::dump_ipc_profile();
            ruzu_core::hle::kernel::svc_dispatch::dump_svc_profile();
            ruzu_core::hle::kernel::svc_dispatch::dump_svc_per_tid_profile();
            ruzu_core::hle::kernel::svc_dispatch::dump_svc_summary_profile();
            ruzu_core::hle::kernel::svc_dispatch::dump_svc_ring_profile();
            ruzu_core::hle::kernel::svc_dispatch::dump_wake_latency();
            ruzu_core::hle::kernel::svc_dispatch::dump_gap_profile();
            ruzu_core::hle::service::nvdrv::nvdrv_interface::dump_nvdrv_ioctl_profile();
            ruzu_core::hle::service::nvdrv::nvdrv_interface::dump_nvdrv_ioctl_history("sigusr2");
            ruzu_core::hle::service::nvdrv::devices::nvhost_gpu::dump_submit_gpfifo_profile();
            video_core::gpu_thread::dump_gpu_thread_profile();
            ruzu_core::hle::kernel::svc::svc_ipc::dump_ipc_service_profile();
            ruzu_core::hle::kernel::svc::svc_ipc::dump_ipc_phase_profile();
            ruzu_core::hle::service::hle_ipc::dump_hle_handler_profile();
            ruzu_core::hle::service::nvnflinger::buffer_queue_producer::dump_bqp_slot_profile();
            ruzu_core::hle::service::nvnflinger::hos_binder_driver::dump_binder_txn_profile();
            ruzu_core::hle::service::nvnflinger::buffer_queue_core::dump_bqp_wait_profile();
            ruzu_core::hle::service::nvnflinger::diagnostics::dump("sigusr2");
            ruzu_core::hle::service::nvnflinger::hardware_composer::dump_hwc_cache_profile();
            ruzu_core::hle::service::vi::conductor::dump_vsync_profile();
            video_core::shader_cache::dump_refresh_stages_stall_profile();
            video_core::shader_cache::dump_make_shader_info_stall_profile();
            video_core::shader_cache::dump_shader_register_stall_profile();
            ruzu_core::memory::memory::dump_rasterizer_mark_cached_stall_profile();
        }
        #[cfg(unix)]
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = profile_signal as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(libc::SIGUSR2, &sa, std::ptr::null_mut());
        }
        extern "C" fn profile_atexit() {
            ruzu_core::hle::kernel::svc::svc_ipc::dump_ipc_profile();
            ruzu_core::hle::kernel::svc_dispatch::dump_svc_profile();
            ruzu_core::hle::kernel::svc_dispatch::dump_svc_per_tid_profile();
            ruzu_core::hle::kernel::svc_dispatch::dump_svc_summary_profile();
            ruzu_core::hle::kernel::svc_dispatch::dump_svc_ring_profile();
            ruzu_core::hle::kernel::svc_dispatch::dump_wake_latency();
            ruzu_core::hle::kernel::svc_dispatch::dump_gap_profile();
            ruzu_core::hle::service::nvdrv::nvdrv_interface::dump_nvdrv_ioctl_profile();
            ruzu_core::hle::service::nvdrv::nvdrv_interface::dump_nvdrv_ioctl_history("atexit");
            ruzu_core::hle::service::nvdrv::devices::nvhost_gpu::dump_submit_gpfifo_profile();
            video_core::gpu_thread::dump_gpu_thread_profile();
            ruzu_core::hle::kernel::svc::svc_ipc::dump_ipc_service_profile();
            ruzu_core::hle::kernel::svc::svc_ipc::dump_ipc_phase_profile();
            ruzu_core::hle::service::hle_ipc::dump_hle_handler_profile();
            ruzu_core::hle::service::nvnflinger::buffer_queue_producer::dump_bqp_slot_profile();
            ruzu_core::hle::service::nvnflinger::hos_binder_driver::dump_binder_txn_profile();
            ruzu_core::hle::service::nvnflinger::buffer_queue_core::dump_bqp_wait_profile();
            ruzu_core::hle::service::nvnflinger::diagnostics::dump("atexit");
            ruzu_core::hle::service::nvnflinger::hardware_composer::dump_hwc_cache_profile();
            ruzu_core::hle::service::vi::conductor::dump_vsync_profile();
            video_core::shader_cache::dump_refresh_stages_stall_profile();
            video_core::shader_cache::dump_make_shader_info_stall_profile();
            video_core::shader_cache::dump_shader_register_stall_profile();
            ruzu_core::memory::memory::dump_rasterizer_mark_cached_stall_profile();
        }
        unsafe {
            libc::atexit(profile_atexit);
        }
    }

    #[cfg(target_os = "linux")]
    if std::env::var_os("RUZU_SIGILL_TRACE").is_some() {
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = ruzu_sigill_handler as usize;
            sa.sa_flags = libc::SA_SIGINFO;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(libc::SIGILL, &sa, std::ptr::null_mut());
        }
        // Spawn background detector for fastmem base (populates global
        // FASTMEM_BASE_FOR_SIGILL atomic that the handler reads to dump
        // host memory at trap moments).
        std::thread::Builder::new()
            .name("sigill-fastmem-detect".into())
            .spawn(|| {
                // Wait until fastmem is initialized (gives Memory::new time).
                for _ in 0..200 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    detect_fastmem_base_for_sigill();
                    if FASTMEM_BASE_FOR_SIGILL.load(std::sync::atomic::Ordering::Relaxed) != 0 {
                        return;
                    }
                }
            })
            .ok();
    }

    // RUZU_ALLOW_PTRACER=1 — opt into being ptraceable from any process.
    // YAMA's ptrace_scope=1 (default on Ubuntu) restricts ptrace to direct
    // descendants. Setting `PR_SET_PTRACER` to `PR_SET_PTRACER_ANY` (-1)
    // lets external gdb/strace attach without sudo. Used by
    // `scripts/hw_watch_wedge.py` to set a hardware watchpoint on the
    // fastmem arena page that wedges STK's allocator.
    // RUZU_BLOCK_COUNT_PC=0xLO-0xHI: install a SIGUSR2 handler that dumps
    // per-emulator-core block-entry counts. The counters live in rdynarmic.
    // Gentle alternative to RUZU_BLOCK_TRACE_PC for distributions where
    // eprintln-based tracing would mask the race timing.
    if std::env::var_os("RUZU_BLOCK_COUNT_PC").is_some()
        || std::env::var_os("RUZU_COUNT_W64_BY_CORE_AT_VADDR").is_some()
        || std::env::var_os("RUZU_BLOCK_PROLOGUE_COUNT_PC").is_some()
        || std::env::var_os("RUZU_BLOCK_PROLOGUE_TOP_PC").is_some()
        || std::env::var_os("RUZU_FIRST_PCS_PER_CORE").is_some()
    {
        // SIGUSR2 path — useful when the JIT thread isn't flooding stderr.
        #[cfg(unix)]
        extern "C" fn dump_counts(_: libc::c_int) {
            rdynarmic::jit::dump_block_count_summary();
            eprintln!("{}", rdynarmic::jit::block_prologue_count_summary_string());
            eprintln!("{}", rdynarmic::jit::block_prologue_top_summary_string());
            eprintln!("{}", rdynarmic::jit::first_pcs_per_core_summary_string());
            ruzu_core::arm::dynarmic::arm_dynarmic_64::dump_w64_by_core_counters();
        }
        #[cfg(unix)]
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = dump_counts as usize;
            sa.sa_flags = 0;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(libc::SIGUSR2, &sa, std::ptr::null_mut());
        }
        // RUZU_COUNT_DUMP_FILE=/tmp/x.log — periodic background dump every 5s.
        // Robust against eprintln-flood deadlocks where SIGUSR2 races stderr.
        // Writes both BLOCK_COUNT and W64_BY_CORE summaries.
        if let Ok(path) = std::env::var("RUZU_COUNT_DUMP_FILE") {
            std::thread::Builder::new()
                .name("count-dumper".into())
                .spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    if let Ok(file) = std::fs::OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(true)
                        .open(&path)
                    {
                        use std::io::Write;
                        let mut w = std::io::BufWriter::new(file);
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let _ = writeln!(w, "[count-dumper] ts={}", ts);
                        let _ = writeln!(w, "{}", rdynarmic::jit::block_count_summary_string());
                        let _ = writeln!(
                            w,
                            "{}",
                            rdynarmic::jit::block_prologue_count_summary_string()
                        );
                        let _ =
                            writeln!(w, "{}", rdynarmic::jit::block_prologue_top_summary_string());
                        let _ =
                            writeln!(w, "{}", rdynarmic::jit::first_pcs_per_core_summary_string());
                        let _ = writeln!(
                            w,
                            "{}",
                            ruzu_core::arm::dynarmic::arm_dynarmic_64::w64_by_core_summary_string()
                        );
                    }
                })
                .ok();
        }
        eprintln!(
            "[BLOCK_COUNT/W64_BY_CORE] enabled — send SIGUSR2 to print summary, \
             or set RUZU_COUNT_DUMP_FILE=/tmp/x.log for periodic file dumps"
        );
    }

    if std::env::var_os("RUZU_ALLOW_PTRACER").is_some() {
        // PR_SET_PTRACER (0x59616d61, "Yama") with PR_SET_PTRACER_ANY (-1)
        // tells YAMA to permit any process to ptrace this one. Avoids
        // ptrace_scope=1 restriction without requiring sudo. Linux-only.
        #[cfg(target_os = "linux")]
        unsafe {
            let rc = libc::prctl(libc::PR_SET_PTRACER, libc::PR_SET_PTRACER_ANY, 0, 0, 0);
            log::info!(
                "RUZU_ALLOW_PTRACER=1 → prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY)={}",
                rc
            );
        }
    }

    // Parse CLI arguments (upstream: getopt_long loop).
    let args = Args::parse();
    let multiplayer = args.multiplayer.as_deref().map(|value| {
        parse_multiplayer_config(value).unwrap_or_else(|error| {
            if error == MultiplayerParseError::WrongFormat {
                println!("{}", error.message());
                let _ = Args::command().print_help();
                println!();
            } else {
                log::error!("{}", error.message());
            }
            std::process::exit(1);
        })
    });

    // Resolve game path from --game flag or positional argument.
    // Maps to C++ `filepath` variable.
    let filepath = args.game.or(args.filename).unwrap_or_default();

    if filepath.is_empty() {
        log::error!("Failed to load ROM: No ROM specified");
        std::process::exit(-1);
    }

    // Load SDL configuration.
    // Maps to C++ `SdlConfig config{config_path}`.
    let _config = SdlConfig::new(args.config);

    // Apply optional per-run overrides.
    // Maps to C++ `Settings::values.program_args = program_args` and
    // `Settings::values.current_user = clamp(*selected_user, 0, 7)`.
    if let Some(ref program_args) = args.program {
        common::settings::values_mut()
            .program_args
            .set_value(program_args.clone());
    }
    if let Some(user_index) = args.user {
        let clamped = (user_index as i32).clamp(0, 7);
        common::settings::values_mut()
            .current_user
            .set_value(clamped);
    }

    // Investigation-only frontend override. Upstream normally sources these
    // from Config, but ruzu-cmd's SDL Config plumbing is not fully ported yet.
    if std::env::var_os("RUZU_RNG_SEED_ENABLED").is_some_and(|value| value != OsStr::new("0")) {
        common::settings::values_mut()
            .rng_seed_enabled
            .set_value(true);
    }
    if let Some(seed_text) = std::env::var_os("RUZU_RNG_SEED") {
        let seed_text = seed_text.to_string_lossy();
        let seed_text = seed_text
            .strip_prefix("0x")
            .or_else(|| seed_text.strip_prefix("0X"))
            .unwrap_or(&seed_text);
        match u32::from_str_radix(seed_text, 16).or_else(|_| seed_text.parse::<u32>()) {
            Ok(seed) => {
                let mut values = common::settings::values_mut();
                values.rng_seed_enabled.set_value(true);
                values.rng_seed.set_value(seed);
                log::info!("Using investigation RNG seed override: 0x{seed:08X}");
            }
            Err(err) => {
                log::error!("Invalid RUZU_RNG_SEED value: {seed_text} ({err})");
                std::process::exit(1);
            }
        }
    }

    // Log configuration settings.
    // Matches upstream: Settings::LogSettings() called in EmuWindow constructor.
    common::settings::log_settings(&common::settings::values());

    // Initialise core system.
    // Maps to C++ `Core::System system{}; system.Initialize();`.
    let mut system = ruzu_core::core::System::new();
    system.initialize();

    // Determine renderer backend.
    // Maps to C++ switch on `Settings::values.renderer_backend.GetValue()`.
    let configured_backend = *common::settings::values().renderer_backend.get_value();
    let renderer_backend = resolve_renderer_backend(args.renderer.as_deref(), configured_backend);
    log::info!("Renderer backend: {}", renderer_backend);

    // -----------------------------------------------------------------------
    // Step 3 (upstream): Create emu_window BEFORE loading.
    // Maps to C++:
    //   switch (Settings::values.renderer_backend.GetValue()) {
    //     case OpenGL:  emu_window = make_unique<EmuWindow_SDL3_GL>(...);
    //     case Vulkan:  emu_window = make_unique<EmuWindow_SDL3_VK>(...);
    //     case Null:    emu_window = make_unique<EmuWindow_SDL3_Null>(...);
    //   }
    // -----------------------------------------------------------------------
    enum EmuWindow {
        Gl(EmuWindowSdl3Gl),
        Vk(EmuWindowSdl3Vk),
        Null(EmuWindowSdl3Null),
    }

    let emu_window_system_ref = ruzu_core::core::SystemRef::from_ref(&system);
    let mut emu_window = match renderer_backend {
        "opengl" => EmuWindow::Gl(EmuWindowSdl3Gl::new(emu_window_system_ref, args.fullscreen)),
        "vulkan" => EmuWindow::Vk(EmuWindowSdl3Vk::new(emu_window_system_ref, args.fullscreen)),
        _ => EmuWindow::Null(EmuWindowSdl3Null::new(
            emu_window_system_ref,
            args.fullscreen,
        )),
    };

    // -----------------------------------------------------------------------
    // Step 4 (upstream lines 367-370):
    //   system.SetContentProvider(make_unique<FileSys::ContentProviderUnion>());
    //   system.SetFilesystem(make_shared<FileSys::RealVfsFilesystem>());
    //   system.GetFileSystemController().CreateFactories(*system.GetFilesystem());
    //   system.GetUserChannel().clear();
    // -----------------------------------------------------------------------
    {
        use ruzu_core::file_sys::registered_cache::ContentProviderUnion;
        system.set_content_provider(std::sync::Arc::new(std::sync::Mutex::new(
            ContentProviderUnion::new(),
        )));
        // VFS is already created in system.initialize(); ensure it's set.
        if system.get_filesystem().is_none() {
            system.set_filesystem(ruzu_core::file_sys::vfs::vfs_real::RealVfsFilesystem::new());
        }
        system
            .get_filesystem_controller()
            .lock()
            .unwrap()
            .create_factories(system.get_filesystem().unwrap().clone(), false);
        // Note: get_filesystem_controller() returns Arc clone, lock it.
        system.clear_user_channel();
    }

    // -----------------------------------------------------------------------
    // Step 5: Register subsystem factory.
    // Upstream creates Host1x, GPU, AudioCore inside SetupForApplicationProcess()
    // (core.cpp:277-283). In Rust, video_core/audio_core crates can't be created
    // from core due to circular dependencies, so we provide a factory callback.
    // -----------------------------------------------------------------------

    // Capture the raw SDL window pointer for creating GL contexts inside the factory.
    // The window pointer is valid for the lifetime of emu_window (which outlives System).
    // Store as usize to avoid Send issues with raw pointers.
    let sdl_window_ptr_usize: usize = match &emu_window {
        EmuWindow::Gl(w) => w.raw_window() as usize,
        EmuWindow::Vk(w) => w.raw_window() as usize,
        EmuWindow::Null(w) => w.raw_window() as usize,
    };
    let strict_gl_context_required = match &emu_window {
        EmuWindow::Gl(w) => w.strict_context_required(),
        _ => false,
    };
    let opengl_framebuffer_layout = match &emu_window {
        EmuWindow::Gl(w) => Some(w.framebuffer_layout()),
        _ => None,
    };
    let vulkan_window_info = match &emu_window {
        EmuWindow::Vk(w) => Some((
            w.window_info().clone(),
            w.drawable_size(),
            w.shown_state(),
            w.framebuffer_layout(),
        )),
        _ => None,
    };
    let renderer_backend_str = renderer_backend.to_string();

    system.set_subsystem_factory(Box::new(move |system| {
        use std::sync::Arc;

        // Host1x (upstream core.cpp:277): host1x_core = make_unique<Host1x>(system)
        let host1x = video_core::host1x::host1x::Host1x::new_with_system(
            ruzu_core::core::SystemRef::from_ref(system),
        );
        let syncpoints = host1x.syncpoint_manager().clone();
        // Upstream: `auto& device_memory = system.Host1x().MemoryManager();`.
        // Single shared instance — passed by `Arc::clone` into the
        // renderer/rasterizer/shader-cache chain below.
        let device_memory = host1x.memory_manager().clone();
        system.set_host1x_core(Box::new(host1x));

        // GPU (upstream core.cpp:278): gpu_core = VideoCore::CreateGPU(emu_window, system)
        //
        // Upstream flow:
        //   auto context = emu_window.CreateSharedContext();
        //   auto scope = context->Acquire();
        //   auto renderer = CreateRenderer(system, emu_window, *gpu, context);
        //   gpu->BindRenderer(renderer);
        // `VideoCore::CreateGPU` updates the derived resolution state first.
        // This frontend-owned factory has to retain that ordering because the
        // renderer needs the GPU while it is being constructed.
        {
            let mut values = common::settings::values_mut();
            common::settings::update_rescaling_info(&mut values);
        }
        let system_ref = ruzu_core::core::SystemRef::from_ref(&system);
        let use_async_gpu =
            *common::settings::values().use_asynchronous_gpu_emulation.get_value()
                && std::env::var_os("RUZU_DISABLE_ASYNC_GPU").is_none();
        let use_nvdec = *common::settings::values().nvdec_emulation.get_value()
            != common::settings_enums::NvdecEmulation::Off;
        let gpu = Box::new(video_core::gpu::Gpu::new(use_async_gpu, use_nvdec));
        gpu.set_system_ref(system_ref);
        let gpu_ptr = gpu.as_ref() as *const video_core::gpu::Gpu as usize;

        // One-time capture of a raw pointer to the process `Memory`, shared
        // by the GPU-side memory callbacks below. They run on the GPU thread
        // while it holds rasterizer cache locks; taking the global Memory
        // mutex there deadlocks against CPU/DSP threads that hold the mutex
        // during guest writes which re-enter the rasterizer (`on_cpu_write`)
        // and lock-free; every `Memory` method used below takes `&self`. The
        // pointee lives inside the `Arc<Mutex<..>>` allocation, which the
        // system keeps alive for the whole emulation session.
        let memory_raw: std::sync::Arc<std::sync::OnceLock<usize>> =
            std::sync::Arc::new(std::sync::OnceLock::new());
        fn memory_raw_of(
            cell: &std::sync::OnceLock<usize>,
            memory: &std::sync::Arc<std::sync::Mutex<ruzu_core::memory::memory::Memory>>,
        ) -> *const ruzu_core::memory::memory::Memory {
            *cell.get_or_init(|| {
                let guard = memory.lock().unwrap();
                &*guard as *const ruzu_core::memory::memory::Memory as usize
            }) as *const ruzu_core::memory::memory::Memory
        }

        let renderer: Box<dyn video_core::renderer_base::RendererBase> =
            match renderer_backend_str.as_str() {
                "opengl" => {
                    let window_ptr = sdl_window_ptr_usize as *mut sdl3::sys::everything::SDL_Window;
                    let context = Box::new(emu_window::emu_window_sdl3_gl::SdlGlContext::new(
                        window_ptr,
                    ));
                    let shared_context_factory: video_core::renderer_opengl::gl_shader_context::SharedContextFactory =
                        Arc::new(move || {
                            Box::new(
                                emu_window::emu_window_sdl3_gl::SdlGlContext::new(
                                    sdl_window_ptr_usize as *mut sdl3::sys::everything::SDL_Window,
                                ),
                            )
                        });
                    let frame_end_notify = Arc::new(move || unsafe {
                        let gpu_ref = &*(gpu_ptr as *const video_core::gpu::Gpu);
                        gpu_ref.renderer_frame_end_notify();
                    });
                    let frame_displayed_notify = Arc::new(|| {});
                    let framebuffer_layout = opengl_framebuffer_layout.as_ref().ok_or_else(|| {
                        "OpenGL renderer selected without framebuffer layout".to_owned()
                    })?;
                    let mut renderer = video_core::renderer_opengl::RendererOpenGL::new(
                        |s| {
                            let cs = std::ffi::CString::new(s).unwrap();
                            unsafe {
                                sdl3::sys::everything::SDL_GL_GetProcAddress(cs.as_ptr()).map_or(
                                    std::ptr::null(),
                                    |proc| proc as *const () as *const std::os::raw::c_void,
                                )
                            }
                        },
                        syncpoints.clone(),
                        device_memory.clone(),
                        // SAFETY: this renderer is immediately bound to `gpu` below;
                        // `Gpu` drops the renderer and its shader workers first.
                        unsafe { gpu.shader_notify_handle() },
                        strict_gl_context_required,
                        context,
                        Some(shared_context_factory),
                        Arc::clone(framebuffer_layout),
                        frame_end_notify,
                        frame_displayed_notify,
                    )
                    .map_err(|error| format!("Failed to create OpenGL renderer: {error}"))?;
                    renderer
                        .rasterizer_mut()
                        .set_invalidate_gpu_cache_callback(Arc::new(move || unsafe {
                            (&*(gpu_ptr as *const video_core::gpu::Gpu)).invalidate_gpu_cache();
                        }));
                    // Install the GPU virtual address reader on the OpenGL
                    // shader cache. The closure walks the same SystemRef
                    // path that `set_guest_memory_reader` uses (below) for
                    // the CPU side, and resolves GPU VAs through the bound
                    // channel's `MemoryManager` via `Gpu::read_gpu_memory`.
                    let system_ref_gpu = ruzu_core::core::SystemRef::from_ref(&system);
                    let memory_raw_shader = memory_raw.clone();
                    renderer.rasterizer_mut().set_gpu_memory_reader(Arc::new(
                        move |gpu_va, dst: &mut [u8]| {
                            let cpu_reader = |addr: u64, out: &mut [u8]| {
                                let sys = system_ref_gpu.get();
                                if let Some(memory) = sys.memory_shared() {
                                    // Mutex-free read — see `memory_raw` above.
                                    let m =
                                        unsafe { &*memory_raw_of(&memory_raw_shader, &memory) };
                                    if m.read_block(addr, out) {
                                        return;
                                    }
                                }
                                // Fallback: direct DeviceMemory access for
                                // addresses not in the process page table
                                // (e.g. nvmap-only mappings).
                                let sys = system_ref_gpu.get();
                                let dm = sys.device_memory();
                                let base = ruzu_core::device_memory::dram_memory_map::BASE;
                                if addr >= base {
                                    let offset = (addr - base) as usize;
                                    let backing = dm.buffer.backing_base_pointer();
                                    unsafe {
                                        std::ptr::copy_nonoverlapping(
                                            backing.add(offset),
                                            out.as_mut_ptr(),
                                            out.len(),
                                        );
                                    }
                                }
                            };
                            unsafe {
                                let gpu_ref = &*(gpu_ptr as *const video_core::gpu::Gpu);
                                gpu_ref.read_gpu_memory(gpu_va, dst, &cpu_reader);
                            }
                        },
                    ));
                    Box::new(renderer)
                }
                "vulkan" => {
                    let Some((window_info, drawable_size, shown_state, framebuffer_layout)) =
                        vulkan_window_info.as_ref()
                    else {
                        return Err(
                            "Vulkan renderer selected without Vulkan window info".to_owned(),
                        );
                    };
                    let Some(host1x_core) = system.host1x_core() else {
                        return Err(
                            "Vulkan renderer selected before Host1x initialization".to_owned(),
                        );
                    };
                    let Some(host1x) = host1x_core
                        .as_any()
                        .downcast_ref::<video_core::host1x::host1x::Host1x>()
                    else {
                        return Err(
                            "Vulkan renderer could not resolve Host1x memory manager".to_owned(),
                        );
                    };
                    let device_memory = std::sync::Arc::clone(host1x.memory_manager());
                    // Upstream calls render_window.OnFrameDisplayed() from RendererVulkan::Composite
                    // via SCOPE_EXIT. SDL's current Rust frontend does not override the default
                    // no-op hook, so pass an explicit no-op until renderer construction owns
                    // an EmuWindow reference.
                    let frame_displayed_notify = Arc::new(|| {});
                    let frame_end_notify = Arc::new(move || unsafe {
                        let gpu_ref = &*(gpu_ptr as *const video_core::gpu::Gpu);
                        gpu_ref.renderer_frame_end_notify();
                    });
                    #[cfg(target_os = "macos")]
                    {
                        let _ = drawable_size;
                        Box::new(
                            video_core::renderer_metal::renderer_metal::RendererMetal::new(
                                window_info,
                                Arc::clone(shown_state),
                                Arc::clone(framebuffer_layout),
                                frame_displayed_notify,
                                frame_end_notify,
                                syncpoints.clone(),
                                device_memory,
                            )
                            .map_err(|error| format!("Failed to create Metal renderer: {error}"))?,
                        )
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        Box::new(
                            video_core::renderer_vulkan::renderer_vulkan::RendererVulkan::new(
                                // SAFETY: this renderer is immediately bound to `gpu` below;
                                // `Gpu` drops the renderer before its shader notifier.
                                unsafe { gpu.shader_notify_handle() },
                                window_info,
                                *drawable_size,
                                Arc::clone(shown_state),
                                Arc::clone(framebuffer_layout),
                                frame_displayed_notify,
                                frame_end_notify,
                                syncpoints.clone(),
                                device_memory,
                            )
                            .map_err(|error| format!("Failed to create Vulkan renderer: {error}"))?,
                        )
                    }
                }
                _ => Box::new(video_core::renderer_null::renderer_null::RendererNull::new(
                    syncpoints.clone(),
                )),
            };
        gpu.bind_renderer(renderer);
        let memory_raw_reader = memory_raw.clone();
        gpu.set_guest_memory_reader(Arc::new(move |addr, output: &mut [u8]| {
            // The address handed to us is a *device address* coming from the
            // GPU's GMMU after walking GPU VA → device. Resolve in this order:
            //   1. SMMU page table on Host1x (`nvmap::pin_handle` populates
            //      it via `smmu_map`). This is the upstream-faithful path
            //      and the only one that returns the same host backing the
            //      ARM CPU writes through via fastmem.
            //   2. Guest CPU page-table read (legacy fallback for code paths
            //      that still hand us a CPU VA — until they all migrate to
            //      device addresses).
            //   3. Direct DeviceMemory access for addresses inside the DRAM
            //      window (`dram_memory_map::BASE`+).
            let sys = system_ref.get();
            if let Some(host1x) = sys.host1x_core() {
                let host_ptr = host1x.smmu_lookup(addr);
                if host_ptr != 0 {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            host_ptr as *const u8,
                            output.as_mut_ptr(),
                            output.len(),
                        );
                    }
                    if let Ok(raw_trace_addr) = std::env::var("RUZU_TRACE_GUEST_MEMORY_READER") {
                        let raw_trace_addr = raw_trace_addr.trim();
                        let parsed_trace_addr = raw_trace_addr
                            .strip_prefix("0x")
                            .or_else(|| raw_trace_addr.strip_prefix("0X"))
                            .and_then(|digits| u64::from_str_radix(digits, 16).ok())
                            .or_else(|| raw_trace_addr.parse::<u64>().ok());
                        if parsed_trace_addr == Some(addr) {
                            let preview_words: Vec<String> = output
                                .chunks_exact(4)
                                .take(8)
                                .map(|word| {
                                    let value = u32::from_le_bytes(word.try_into().unwrap());
                                    format!("{value:08X}")
                                })
                                .collect();
                            log::info!(
                                "[GUEST_MEMORY_READER] addr=0x{:X} len={} smmu_host=0x{:X} words=[{}]",
                                addr,
                                output.len(),
                                host_ptr,
                                preview_words.join(" "),
                            );
                        }
                    }
                    return true;
                }
            }
            if let Some(memory) = sys.memory_shared() {
                // Mutex-free read — see `memory_raw` above.
                let m = unsafe { &*memory_raw_of(&memory_raw_reader, &memory) };
                if m.read_block(addr, output) {
                    return true;
                }

                let dm = sys.device_memory();
                let base = ruzu_core::device_memory::dram_memory_map::BASE;
                if addr >= base {
                    let offset = (addr - base) as usize;
                    let backing = dm.buffer.backing_base_pointer();
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            backing.add(offset),
                            output.as_mut_ptr(),
                            output.len(),
                        );
                    }
                    return true;
                }
            }
            false
        }));
        let system_ref2 = ruzu_core::core::SystemRef::from_ref(&system);
        // RUZU_TRACE_GPU_WRITE_VADDR=0xADDR — on each callback invocation
        // log a backtrace if [addr, addr+data.len()) overlaps the target
        // guest vaddr. Catches direct fastmem-bypassing host-pointer writes
        // (SMMU path and DRAM-direct fallback below) that would otherwise
        // miss the JIT trap, write_raw, and write_block instrumentation.
        let trace_gpu_write_vaddr: Option<u64> = std::env::var("RUZU_TRACE_GPU_WRITE_VADDR")
            .ok()
            .and_then(|s| {
                let s = s.trim();
                s.strip_prefix("0x")
                    .or_else(|| s.strip_prefix("0X"))
                    .and_then(|d| u64::from_str_radix(d, 16).ok())
                    .or_else(|| s.parse::<u64>().ok())
            });
        let memory_raw_writer = memory_raw.clone();
        gpu.set_guest_memory_writer(Arc::new(move |addr, data: &[u8]| {
            if let Some(target) = trace_gpu_write_vaddr {
                let end = addr.saturating_add(data.len() as u64);
                if addr <= target && target < end {
                    let off = (target - addr) as usize;
                    let preview: Vec<String> = data
                        .iter()
                        .skip(off)
                        .take(16)
                        .map(|b| format!("{:02X}", b))
                        .collect();
                    let bt = std::backtrace::Backtrace::force_capture();
                    eprintln!(
                        "[GUEST_MEMORY_WRITER] target=0x{:016X} addr=0x{:016X} len={} off={} bytes=[{}]\n{}",
                        target, addr, data.len(), off, preview.join(" "), bt
                    );
                }
            }
            let sys = system_ref2.get();
            // Same resolution order as the reader — SMMU first, then guest
            // page table, then DRAM-direct.
            if let Some(host1x) = sys.host1x_core() {
                let host_ptr = host1x.smmu_lookup(addr);
                if host_ptr != 0 {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            data.as_ptr(),
                            host_ptr as *mut u8,
                            data.len(),
                        );
                    }
                    return;
                }
            }
            if let Some(memory) = sys.memory_shared() {
                // Mutex-free write — see `memory_raw` above. `write_block`
                // takes `&self` (interior atomics) like the read path.
                let m = unsafe { &*memory_raw_of(&memory_raw_writer, &memory) };
                if m.write_block(addr, data) {
                    return;
                }
            }
            let dm = sys.device_memory();
            let base = ruzu_core::device_memory::dram_memory_map::BASE;
            if addr >= base {
                let offset = (addr - base) as usize;
                let backing = dm.buffer.backing_base_pointer();
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        data.as_ptr(),
                        backing.add(offset) as *mut u8,
                        data.len(),
                    );
                }
            }
        }));
        // Install GPU VA → CPU VA translator on the renderer so
        // rasterizer-side query writes (e.g. semaphore_trigger) translate
        // GPU VAs into CPU VAs before reaching `guest_memory_writer`.
        // addresses (the GPU VA is passed straight through).
        //
        // SAFETY: `gpu_ptr` points to the Box<Gpu> we're about to move
        // into `system.set_gpu_core`. System owns the Gpu for the full
        // emulation lifetime; the translator closure is also bound by
        // that lifetime via the renderer. The renderer is dropped on
        // System shutdown before the Gpu is dropped.
        let gpu_ptr_for_translator = gpu.as_ref() as *const video_core::gpu::Gpu;
        unsafe { gpu.install_gpu_to_cpu_translator(gpu_ptr_for_translator) };

        system.set_gpu_core(gpu);

        // AudioCore (upstream core.cpp:283): audio_core = make_unique<AudioCore>(system)
        let ac = audio_core::AudioCore::new(ruzu_core::core::SystemRef::from_ref(system));
        system.set_audio_core(Box::new(ac));
        Ok(())
    }));

    // -----------------------------------------------------------------------
    // Step 6 (upstream): system.Load(*emu_window, filepath, load_parameters).
    // Upstream passes the window reference to Load; our Rust Load doesn't
    // accept one yet, so we call with the current signature.
    // The factory callback above will be called during load() ->
    // setup_for_application_process() to create Host1x/GPU/AudioCore.
    // -----------------------------------------------------------------------
    let load_result = system.load(&filepath);
    if load_result != ruzu_core::core::SystemResultStatus::Success {
        log::error!("Failed to load ROM '{}': {:?}", filepath, load_result,);
        std::process::exit(-1);
    }

    let mut multiplayer_network = None;
    if let Some(multiplayer) = multiplayer {
        let mut room_network = network::network::RoomNetwork::new();
        if !room_network.init() {
            log::error!("Network: initialization failed");
            std::process::exit(1);
        }
        let Some(member) = room_network.get_room_member().upgrade() else {
            log::error!("Network: Could not access RoomMember");
            std::process::exit(1);
        };
        member.bind_on_chat_message_received(on_message_received);
        member.bind_on_status_message_received(on_status_message_received);
        member.bind_on_state_changed(on_state_changed);
        member.bind_on_error(on_network_error);
        log::debug!(
            "Network: Start connection to {}:{} with nickname {}",
            multiplayer.address,
            multiplayer.port,
            multiplayer.nickname
        );
        member.join(
            &multiplayer.nickname,
            &multiplayer.address,
            multiplayer.port,
            0,
            &NO_PREFERRED_IP,
            &multiplayer.password,
            "",
        );
        multiplayer_network = Some(room_network);
    }

    // Upstream loads and fully builds the disk pipeline cache before starting
    // GPU/CPU execution. Starting the guest first lets pipeline workers contend
    // with presentation and can hide entire early animations behind the cache
    if *common::settings::values().use_disk_shader_cache.get_value()
        && std::env::var_os("RUZU_SKIP_DISK_SHADER_CACHE").is_none()
    {
        if let Some(gpu_any) = system.gpu_core() {
            if let Some(gpu) = gpu_any.as_any().downcast_ref::<video_core::gpu::Gpu>() {
                let mut renderer_guard = gpu.renderer();
                if let Some(renderer) = renderer_guard.as_mut() {
                    let rasterizer = renderer.read_rasterizer();
                    unsafe {
                        if let Some(rasterizer) = rasterizer.as_mut() {
                            rasterizer.load_disk_resources(
                                system.runtime_program_id(),
                                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                                std::sync::Arc::new(|_, _, _| {}),
                            );
                        }
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Step 6 (upstream): system.GPU().Start()
    // Core is loaded, start the GPU (makes the GPU contexts current to this thread).
    // -----------------------------------------------------------------------
    if let Some(gpu_any) = system.gpu_core() {
        if let Some(gpu) = gpu_any.as_any().downcast_ref::<video_core::gpu::Gpu>() {
            gpu.start();
        } else {
            log::warn!("GPU core is not a video_core::gpu::Gpu — cannot call start()");
        }
    } else {
        log::warn!("No GPU core set — skipping GPU().Start()");
    }

    // -----------------------------------------------------------------------
    // Step 7 (upstream): system.GetCpuManager().OnGpuReady()
    // -----------------------------------------------------------------------
    system.get_cpu_manager().on_gpu_ready();

    // -----------------------------------------------------------------------
    // Upstream: system.RegisterExitCallback([&] { exit(0); })
    // The SDL frontend exits immediately when the core requests application exit.
    // -----------------------------------------------------------------------
    system.register_exit_callback(Box::new(|| {
        std::process::exit(0);
    }));

    // -----------------------------------------------------------------------
    // Step 8 (upstream): system.Run()
    // Starts CPU threads running in background via CpuManager.
    // -----------------------------------------------------------------------
    // RUZU_BOOT_DELAY_MS=NNN: Diagnostic — sleep before starting the guest CPU
    // ~595ms earlier than zuyu, causing pthread_create-child to skip the
    // wait branch (state=READY already set by parent before child runs).
    if let Ok(ms_str) = std::env::var("RUZU_BOOT_DELAY_MS") {
        if let Ok(ms) = ms_str.parse::<u64>() {
            log::info!("RUZU_BOOT_DELAY_MS={ms} — sleeping before system.run()");
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
    system.run();

    // -----------------------------------------------------------------------
    // Step 9 (upstream): while (emu_window->IsOpen()) { emu_window->WaitEvent(); }
    // Main thread stays in the window event loop.
    // -----------------------------------------------------------------------
    // Reset SIGTERM to default AFTER all signal handlers (SIGSEGV for fastmem)
    // have been installed. This ensures `timeout` and Ctrl-C cleanly kill the
    // process instead of being swallowed by custom handlers.
    #[cfg(unix)]
    unsafe {
        // SIGTERM = 15, SIG_DFL = 0
        extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
        }
        signal(15, 0);
    }

    // emu_window::emu_window_sdl3::schedule_auto_lr_if_requested();
    // emu_window::emu_window_sdl3::schedule_auto_a_if_requested();
    emu_window::emu_window_sdl3::schedule_perf_log_if_requested(emu_window_system_ref);
    log::info!("Entering main event loop");
    let poll_events_loop = std::env::var_os("RUZU_POLL_EVENTS_LOOP").is_some();
    match &mut emu_window {
        EmuWindow::Gl(w) => {
            if poll_events_loop {
                while w.is_open() {
                    w.poll_events();
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            } else {
                while w.is_open() {
                    w.wait_event();
                }
            }
        }
        EmuWindow::Vk(w) => {
            if poll_events_loop {
                while w.is_open() {
                    w.poll_events();
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            } else {
                while w.is_open() {
                    w.wait_event();
                }
            }
        }
        EmuWindow::Null(w) => {
            while w.is_open() {
                w.wait_event();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Step 10 (upstream): system.Pause(); system.ShutdownMainProcess();
    // Cleanup after window closes.
    // -----------------------------------------------------------------------
    log::info!("Window closed, shutting down");
    log::info!("Shutdown phase: system.pause() begin");
    system.pause();
    log::info!("Shutdown phase: system.pause() end");
    log::info!("Shutdown phase: system.shutdown_main_process() begin");
    system.shutdown_main_process();
    log::info!("Shutdown phase: system.shutdown_main_process() end");

    if let Some(mut room_network) = multiplayer_network {
        room_network.shutdown();
    }

    // Force-exit the process. Some background threads (CPU core idle loops,
    // audio ADSP, CoreTiming timer) block on condvars that aren't cleanly
    // woken during shutdown. Upstream C++ exits via main() return which calls
    // exit() and doesn't join detached threads; Rust's implicit drop of
    // statics + JoinHandle would hang. This matches upstream behavior.
    std::process::exit(0);
}
