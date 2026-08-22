// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/debugger/gdbstub.h and gdbstub.cpp
//! GDB stub for remote debugging.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::debugger::debugger_interface::{DebuggerAction, DebuggerBackend, DebuggerFrontend};
use crate::debugger::gdbstub_arch::{GdbStubA32, GdbStubA64, GdbStubArch};
use crate::hle::kernel::k_process::{DebugWatchpoint, DebugWatchpointType, ProcessLock};
use crate::hle::kernel::k_thread::KThreadLock;

// GDB protocol constants (matching upstream)
const GDB_STUB_START: u8 = b'$';
const GDB_STUB_END: u8 = b'#';
const GDB_STUB_ACK: u8 = b'+';
const GDB_STUB_NACK: u8 = b'-';
const GDB_STUB_INT3: u8 = 0x03;
const GDB_STUB_SIGTRAP: u8 = 5;

const GDB_STUB_REPLY_ERR: &str = "E01";
const GDB_STUB_REPLY_OK: &str = "OK";
const GDB_STUB_REPLY_EMPTY: &str = "";

/// GDB stub frontend.
///
/// Corresponds to upstream `Core::GDBStub`.
pub struct GdbStub {
    debug_process: Arc<ProcessLock>,
    arch: Box<dyn GdbStubArch>,
    current_command: Vec<u8>,
    replaced_instructions: BTreeMap<u64, u32>,
    pub(crate) resume_threads: Vec<Arc<KThreadLock>>,
    no_ack: bool,
}

impl GdbStub {
    /// Create a new GDB stub.
    ///
    /// Corresponds to upstream `GDBStub::GDBStub`.
    pub fn new(debug_process: Arc<ProcessLock>) -> Self {
        let is_64bit = debug_process.lock().unwrap().is_64bit();
        let arch: Box<dyn GdbStubArch> = if is_64bit {
            Box::new(GdbStubA64)
        } else {
            Box::new(GdbStubA32)
        };

        Self {
            debug_process,
            arch,
            current_command: Vec::new(),
            replaced_instructions: BTreeMap::new(),
            resume_threads: Vec::new(),
            no_ack: false,
        }
    }
}

impl DebuggerFrontend for GdbStub {
    fn connected(&mut self, _backend: &mut dyn DebuggerBackend) {
        // Nothing to do on connection
    }

    fn stopped(&mut self, backend: &mut dyn DebuggerBackend, thread: Arc<KThreadLock>) {
        let thread = thread.lock().unwrap();
        let reply = self.arch.thread_status(
            &thread.thread_context,
            thread.get_thread_id(),
            GDB_STUB_SIGTRAP,
        );
        self.send_reply(backend, &reply);
    }

    fn shutting_down(&mut self, _backend: &mut dyn DebuggerBackend) {
        // Nothing to do on shutdown
    }

    fn watchpoint(
        &mut self,
        backend: &mut dyn DebuggerBackend,
        thread: Arc<KThreadLock>,
        watch: DebugWatchpoint,
    ) {
        let thread = thread.lock().unwrap();
        let status = self.arch.thread_status(
            &thread.thread_context,
            thread.get_thread_id(),
            GDB_STUB_SIGTRAP,
        );
        let kind = match watch.type_ {
            DebugWatchpointType::READ => "rwatch",
            DebugWatchpointType::WRITE => "watch",
            _ => "awatch",
        };
        self.send_reply(
            backend,
            &format!("{}{}:{:x};", status, kind, watch.start_address.get()),
        );
    }

    fn client_data(
        &mut self,
        backend: &mut dyn DebuggerBackend,
        data: &[u8],
    ) -> Vec<DebuggerAction> {
        let mut actions = Vec::new();
        self.current_command.extend_from_slice(data);

        while !self.current_command.is_empty() {
            if !self.process_data(backend, &mut actions) {
                break;
            }
        }

        actions
    }
}

impl GdbStub {
    /// Process incoming data and generate debugger actions.
    ///
    /// Corresponds to upstream `GDBStub::ProcessData`.
    fn process_data(
        &mut self,
        backend: &mut dyn DebuggerBackend,
        actions: &mut Vec<DebuggerAction>,
    ) -> bool {
        if self.current_command.is_empty() {
            return false;
        }

        let c = self.current_command[0];

        // Acknowledgement
        if c == GDB_STUB_ACK || c == GDB_STUB_NACK {
            self.current_command.remove(0);
            return true;
        }

        // Interrupt
        if c == GDB_STUB_INT3 {
            log::info!("GDB: Received interrupt");
            self.current_command.remove(0);
            actions.push(DebuggerAction::Interrupt);
            self.send_status(backend, GDB_STUB_ACK);
            return true;
        }

        // Require start of command
        if c != GDB_STUB_START {
            log::error!("GDB: Invalid command buffer contents");
            self.current_command.clear();
            self.send_status(backend, GDB_STUB_NACK);
            return false;
        }

        // Find the end marker '#' followed by 2 checksum hex chars.
        let end_pos = self.current_command.iter().position(|&b| b == GDB_STUB_END);
        let end_pos = match end_pos {
            Some(pos) if pos + 2 < self.current_command.len() => pos,
            _ => {
                // Incomplete command — wait for more data.
                return false;
            }
        };

        // Extract command body (between '$' and '#')
        let command_body: Vec<u8> = self.current_command[1..end_pos].to_vec();

        // Extract and validate checksum
        let checksum_str =
            std::str::from_utf8(&self.current_command[end_pos + 1..end_pos + 3]).unwrap_or("00");
        let received_checksum = u8::from_str_radix(checksum_str, 16).unwrap_or(0);
        let computed_checksum = Self::calculate_checksum(&command_body);

        // Consume the processed bytes
        self.current_command = self.current_command[end_pos + 3..].to_vec();

        if received_checksum != computed_checksum {
            log::warn!(
                "GDB: Checksum mismatch (received {:02x}, computed {:02x})",
                received_checksum,
                computed_checksum
            );
            self.send_status(backend, GDB_STUB_NACK);
            return true;
        }

        self.send_status(backend, GDB_STUB_ACK);

        // Dispatch command (upstream: ExecuteCommand)
        let command_str = String::from_utf8_lossy(&command_body).to_string();
        log::debug!("GDB command: {}", command_str);

        // Minimal command handling matching upstream dispatch
        if command_str.starts_with('?') {
            if let Some(thread) = backend.get_active_thread() {
                let thread = thread.lock().unwrap();
                let reply = self.arch.thread_status(
                    &thread.thread_context,
                    thread.get_thread_id(),
                    GDB_STUB_SIGTRAP,
                );
                self.send_reply(backend, &reply);
            } else {
                self.send_reply(backend, GDB_STUB_REPLY_ERR);
            }
        } else if command_str == "D" {
            // Detach
            self.send_reply(backend, GDB_STUB_REPLY_OK);
            actions.push(DebuggerAction::Continue);
        } else if command_str.starts_with("qSupported") {
            self.send_reply(
                backend,
                "PacketSize=4000;qXfer:features:read+;qXfer:threads:read+;qXfer:libraries:read+;vContSupported+;QStartNoAckMode+",
            );
        } else if command_str == "k" {
            // Kill
            actions.push(DebuggerAction::ShutdownEmulation);
        } else if command_str == "c" {
            actions.push(DebuggerAction::Continue);
        } else if command_str == "s" {
            actions.push(DebuggerAction::StepThread);
        } else {
            self.send_reply(backend, GDB_STUB_REPLY_EMPTY);
        }

        true
    }

    /// Calculate GDB checksum.
    ///
    /// Corresponds to upstream `CalculateChecksum`.
    fn calculate_checksum(data: &[u8]) -> u8 {
        data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
    }

    /// Escape special GDB characters.
    ///
    /// Corresponds to upstream `EscapeGDB`.
    fn escape_gdb(data: &str) -> String {
        let mut escaped = String::with_capacity(data.len());
        for c in data.bytes() {
            match c {
                b'#' => escaped.push_str("}\x03"),
                b'$' => escaped.push_str("}\x04"),
                b'*' => escaped.push_str("}\x0a"),
                b'}' => escaped.push_str("}\x5d"),
                _ => escaped.push(c as char),
            }
        }
        escaped
    }

    /// Send one escaped GDB remote packet.
    ///
    /// Corresponds to upstream `GDBStub::SendReply`.
    fn send_reply(&self, backend: &mut dyn DebuggerBackend, data: &str) {
        let escaped = Self::escape_gdb(data);
        let output = format!(
            "{}{}{}{:02x}",
            GDB_STUB_START as char,
            escaped,
            GDB_STUB_END as char,
            Self::calculate_checksum(escaped.as_bytes())
        );
        log::trace!("GDB: writing reply: {output}");
        backend.write_to_client(output.as_bytes());
    }

    /// Send an acknowledgement unless no-ack mode has been negotiated.
    ///
    /// Corresponds to upstream `GDBStub::SendStatus`.
    fn send_status(&self, backend: &mut dyn DebuggerBackend, status: u8) {
        if !self.no_ack {
            backend.write_to_client(&[status]);
        }
    }
}

/// Breakpoint types.
///
/// Corresponds to upstream anonymous `BreakpointType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum BreakpointType {
    Software = 0,
    Hardware = 1,
    WriteWatch = 2,
    ReadWatch = 3,
    AccessWatch = 4,
}
