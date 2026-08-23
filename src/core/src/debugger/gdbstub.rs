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
use crate::hle::kernel::k_typed_address::KProcessAddress;
use crate::hle::kernel::svc::svc_types::{MemoryAttribute, MemoryPermission, MemoryState};

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
        self.execute_command(backend, &command_str, actions);

        true
    }

    /// Execute one decoded GDB packet.
    ///
    /// Corresponds to upstream `GDBStub::ExecuteCommand`.
    fn execute_command(
        &mut self,
        backend: &mut dyn DebuggerBackend,
        packet: &str,
        actions: &mut Vec<DebuggerAction>,
    ) {
        log::trace!("GDB: executing command: {packet}");

        if packet.is_empty() {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        }
        if let Some(command) = packet.strip_prefix("vCont") {
            self.handle_vcont(backend, command, actions);
            return;
        }

        let command = &packet[1..];
        match packet.as_bytes()[0] {
            b'H' => {
                let thread_id = parse_hex_i64(command.get(1..).unwrap_or_default());
                let thread = if thread_id >= 1 {
                    self.get_thread_by_id(thread_id as u64)
                } else {
                    backend.get_active_thread()
                };
                if let Some(thread) = thread {
                    self.send_reply(backend, GDB_STUB_REPLY_OK);
                    backend.set_active_thread(thread);
                } else {
                    self.send_reply(backend, GDB_STUB_REPLY_ERR);
                }
            }
            b'T' => {
                let thread_id = parse_hex_i64(command);
                let reply = if thread_id >= 0 && self.get_thread_by_id(thread_id as u64).is_some() {
                    GDB_STUB_REPLY_OK
                } else {
                    GDB_STUB_REPLY_ERR
                };
                self.send_reply(backend, reply);
            }
            b'Q' | b'q' => self.handle_query(backend, command),
            b'?' => {
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
            }
            b'k' => {
                log::info!("GDB: shutting down emulation");
                actions.push(DebuggerAction::ShutdownEmulation);
            }
            b'g' => {
                if let Some(thread) = backend.get_active_thread() {
                    let thread = thread.lock().unwrap();
                    let reply = self.arch.read_registers(&thread.thread_context);
                    self.send_reply(backend, &reply);
                } else {
                    self.send_reply(backend, GDB_STUB_REPLY_ERR);
                }
            }
            b'G' => {
                if let Some(thread) = backend.get_active_thread() {
                    let mut thread = thread.lock().unwrap();
                    self.arch
                        .write_registers(&mut thread.thread_context, command);
                    self.send_reply(backend, GDB_STUB_REPLY_OK);
                } else {
                    self.send_reply(backend, GDB_STUB_REPLY_ERR);
                }
            }
            b'p' => {
                if let Some(thread) = backend.get_active_thread() {
                    let thread = thread.lock().unwrap();
                    let register = parse_hex_u64(command) as usize;
                    let reply = self.arch.reg_read(&thread.thread_context, register);
                    self.send_reply(backend, &reply);
                } else {
                    self.send_reply(backend, GDB_STUB_REPLY_ERR);
                }
            }
            b'P' => {
                if let Some(thread) = backend.get_active_thread() {
                    let separator = command.find('=').map_or(command.len(), |index| index + 1);
                    let register = parse_hex_u64(command) as usize;
                    let mut thread = thread.lock().unwrap();
                    self.arch.reg_write(
                        &mut thread.thread_context,
                        register,
                        command.get(separator..).unwrap_or_default(),
                    );
                    self.send_reply(backend, GDB_STUB_REPLY_OK);
                } else {
                    self.send_reply(backend, GDB_STUB_REPLY_ERR);
                }
            }
            b'm' => self.handle_memory_read(backend, command),
            b'M' => self.handle_memory_write(backend, command),
            b's' => {
                self.resume_threads.clear();
                actions.push(DebuggerAction::StepThread);
            }
            b'C' | b'c' => {
                self.resume_threads.clear();
                actions.push(DebuggerAction::Continue);
            }
            b'Z' => self.handle_breakpoint_insert(backend, command),
            b'z' => self.handle_breakpoint_remove(backend, command),
            _ => self.send_reply(backend, GDB_STUB_REPLY_EMPTY),
        }
    }

    /// Handle `mADDR,SIZE`.
    ///
    /// Corresponds to the `m` branch in upstream `GDBStub::ExecuteCommand`.
    fn handle_memory_read(&self, backend: &mut dyn DebuggerBackend, command: &str) {
        let Some((address, size)) = parse_address_and_size(command) else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };
        let Ok(size_usize) = usize::try_from(size) else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };

        let memory = self.debug_process.lock().unwrap().get_memory();
        let Some(memory) = memory else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };

        let mut bytes = vec![0; size_usize];
        if !memory.lock().unwrap().read_block(address, &mut bytes) {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        }

        // GDB must see the original instructions, not the trap opcodes that
        // implement software breakpoints.
        let end_address = address.wrapping_add(size);
        for (&breakpoint_address, &original_instruction) in
            self.replaced_instructions.range(address..)
        {
            if breakpoint_address >= end_address {
                break;
            }
            let output_offset = (breakpoint_address - address) as usize;
            let count = (size_usize - output_offset).min(std::mem::size_of::<u32>());
            bytes[output_offset..output_offset + count]
                .copy_from_slice(&original_instruction.to_le_bytes()[..count]);
        }

        self.send_reply(backend, &hex::encode(bytes));
    }

    /// Handle `MADDR,SIZE:DATA`.
    ///
    /// Corresponds to the `M` branch in upstream `GDBStub::ExecuteCommand`.
    fn handle_memory_write(&mut self, backend: &mut dyn DebuggerBackend, command: &str) {
        let Some(comma) = command.find(',') else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };
        let Some(colon) = command.find(':') else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };
        let address = parse_hex_u64(&command[..comma]);
        let size = parse_hex_u64(&command[comma + 1..colon]);
        let Ok(size_usize) = usize::try_from(size) else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };
        let bytes = hex::decode(&command[colon + 1..]).unwrap_or_default();
        if bytes.len() < size_usize {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        }

        let memory = self.debug_process.lock().unwrap().get_memory();
        let Some(memory) = memory else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };
        if !memory
            .lock()
            .unwrap()
            .write_block(address, &bytes[..size_usize])
        {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        }

        crate::arm::debug::invalidate_instruction_cache_range(
            &mut self.debug_process.lock().unwrap(),
            address,
            size,
        );
        self.send_reply(backend, GDB_STUB_REPLY_OK);
    }

    /// Insert a software breakpoint or memory watchpoint.
    ///
    /// Corresponds to upstream `GDBStub::HandleBreakpointInsert`.
    fn handle_breakpoint_insert(&mut self, backend: &mut dyn DebuggerBackend, command: &str) {
        let Some((breakpoint_type, address, size)) = parse_breakpoint(command) else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };
        let memory = self.debug_process.lock().unwrap().get_memory();
        let Some(memory) = memory else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };
        if !memory
            .lock()
            .unwrap()
            .is_valid_virtual_address_range(address, size)
        {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        }

        let success = match breakpoint_type {
            BreakpointType::Software => {
                let original_instruction = memory.lock().unwrap().read_32(address);
                self.replaced_instructions
                    .insert(address, original_instruction);
                memory
                    .lock()
                    .unwrap()
                    .write_32(address, self.arch.breakpoint_instruction());
                crate::arm::debug::invalidate_instruction_cache_range(
                    &mut self.debug_process.lock().unwrap(),
                    address,
                    std::mem::size_of::<u32>() as u64,
                );
                true
            }
            BreakpointType::WriteWatch => self.debug_process.lock().unwrap().insert_watchpoint(
                KProcessAddress::new(address),
                size,
                DebugWatchpointType::WRITE,
            ),
            BreakpointType::ReadWatch => self.debug_process.lock().unwrap().insert_watchpoint(
                KProcessAddress::new(address),
                size,
                DebugWatchpointType::READ,
            ),
            BreakpointType::AccessWatch => self.debug_process.lock().unwrap().insert_watchpoint(
                KProcessAddress::new(address),
                size,
                DebugWatchpointType::READ_OR_WRITE,
            ),
            BreakpointType::Hardware => {
                self.send_reply(backend, GDB_STUB_REPLY_EMPTY);
                return;
            }
        };

        self.send_reply(
            backend,
            if success {
                GDB_STUB_REPLY_OK
            } else {
                GDB_STUB_REPLY_ERR
            },
        );
    }

    /// Remove a software breakpoint or memory watchpoint.
    ///
    /// Corresponds to upstream `GDBStub::HandleBreakpointRemove`.
    fn handle_breakpoint_remove(&mut self, backend: &mut dyn DebuggerBackend, command: &str) {
        let Some((breakpoint_type, address, size)) = parse_breakpoint(command) else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };
        let memory = self.debug_process.lock().unwrap().get_memory();
        let Some(memory) = memory else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };
        if !memory
            .lock()
            .unwrap()
            .is_valid_virtual_address_range(address, size)
        {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        }

        let success = match breakpoint_type {
            BreakpointType::Software => {
                if let Some(original_instruction) =
                    self.replaced_instructions.get(&address).copied()
                {
                    memory
                        .lock()
                        .unwrap()
                        .write_32(address, original_instruction);
                    crate::arm::debug::invalidate_instruction_cache_range(
                        &mut self.debug_process.lock().unwrap(),
                        address,
                        std::mem::size_of::<u32>() as u64,
                    );
                    self.replaced_instructions.remove(&address);
                    true
                } else {
                    false
                }
            }
            BreakpointType::WriteWatch => self.debug_process.lock().unwrap().remove_watchpoint(
                KProcessAddress::new(address),
                size,
                DebugWatchpointType::WRITE,
            ),
            BreakpointType::ReadWatch => self.debug_process.lock().unwrap().remove_watchpoint(
                KProcessAddress::new(address),
                size,
                DebugWatchpointType::READ,
            ),
            BreakpointType::AccessWatch => self.debug_process.lock().unwrap().remove_watchpoint(
                KProcessAddress::new(address),
                size,
                DebugWatchpointType::READ_OR_WRITE,
            ),
            BreakpointType::Hardware => {
                self.send_reply(backend, GDB_STUB_REPLY_EMPTY);
                return;
            }
        };

        self.send_reply(
            backend,
            if success {
                GDB_STUB_REPLY_OK
            } else {
                GDB_STUB_REPLY_ERR
            },
        );
    }

    /// Handle `q` and `Q` packets.
    ///
    /// Corresponds to upstream `GDBStub::HandleQuery`.
    fn handle_query(&mut self, backend: &mut dyn DebuggerBackend, command: &str) {
        if command.starts_with("TStatus") {
            self.send_reply(backend, "T0");
        } else if command.starts_with("Supported") {
            self.send_reply(
                backend,
                "PacketSize=4000;qXfer:features:read+;qXfer:threads:read+;qXfer:libraries:read+;vContSupported+;QStartNoAckMode+",
            );
        } else if command.starts_with("Xfer:features:read:target.xml:") {
            let reply = paginate_buffer(
                self.arch.get_target_xml(),
                command.get(30..).unwrap_or_default(),
            );
            self.send_reply(backend, &reply);
        } else if command.starts_with("Offsets") {
            let process = self.debug_process.lock().unwrap();
            let main_offset = crate::arm::debug::find_main_module_entrypoint(&process);
            drop(process);
            self.send_reply(backend, &format!("TextSeg={main_offset:x}"));
        } else if command.starts_with("Xfer:libraries:read::") {
            let process = self.debug_process.lock().unwrap();
            let modules = crate::arm::debug::find_modules(&process);
            drop(process);

            let mut buffer = String::from(r#"<?xml version="1.0"?><library-list>"#);
            for (base, name) in modules {
                buffer.push_str(&format!(
                    r#"<library name="{}"><segment address="{base:#x}"/></library>"#,
                    escape_xml(&name)
                ));
            }
            buffer.push_str("</library-list>");
            let reply = paginate_buffer(&buffer, command.get(21..).unwrap_or_default());
            self.send_reply(backend, &reply);
        } else if command.starts_with("fThreadInfo") {
            let thread_ids = self
                .process_threads()
                .iter()
                .map(|thread| format!("{:x}", thread.lock().unwrap().get_thread_id()))
                .collect::<Vec<_>>()
                .join(",");
            self.send_reply(backend, &format!("m{thread_ids}"));
        } else if command.starts_with("sThreadInfo") {
            self.send_reply(backend, "l");
        } else if command.starts_with("Xfer:threads:read::") {
            let mut buffer = String::from(r#"<?xml version="1.0"?><threads>"#);
            for thread in self.process_threads() {
                let thread = thread.lock().unwrap();
                let thread_id = thread.get_thread_id();
                let thread_name = crate::arm::debug::get_thread_name(&thread)
                    .unwrap_or_else(|| format!("Thread {thread_id}"));
                buffer.push_str(&format!(
                    r#"<thread id="{thread_id:x}" core="{}" name="{}">{}</thread>"#,
                    thread.get_active_core(),
                    escape_xml(&thread_name),
                    crate::arm::debug::get_thread_state(&thread)
                ));
            }
            buffer.push_str("</threads>");
            let reply = paginate_buffer(&buffer, command.get(19..).unwrap_or_default());
            self.send_reply(backend, &reply);
        } else if command.starts_with("Attached") {
            self.send_reply(backend, "0");
        } else if command.starts_with("StartNoAckMode") {
            self.no_ack = true;
            self.send_reply(backend, GDB_STUB_REPLY_OK);
        } else if let Some(command) = command.strip_prefix("Rcmd,") {
            self.handle_rcmd(backend, &hex::decode(command).unwrap_or_default());
        } else {
            self.send_reply(backend, GDB_STUB_REPLY_EMPTY);
        }
    }

    /// Handle the GDB `vCont` packet.
    ///
    /// Corresponds to upstream `GDBStub::HandleVCont`.
    fn handle_vcont(
        &mut self,
        backend: &mut dyn DebuggerBackend,
        command: &str,
        actions: &mut Vec<DebuggerAction>,
    ) {
        if command == "?" {
            self.send_reply(backend, "vCont;c;C;s;S");
            return;
        }
        let Some(remaining) = command.strip_prefix(';') else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        };

        #[derive(Clone, Copy)]
        enum VContAction {
            Continue,
            Step,
        }
        struct VContDirective {
            action: VContAction,
            thread: Option<Arc<KThreadLock>>,
            all_threads: bool,
        }
        impl VContDirective {
            fn matches(&self, candidate: &Arc<KThreadLock>) -> bool {
                self.all_threads
                    || self
                        .thread
                        .as_ref()
                        .is_some_and(|thread| Arc::ptr_eq(thread, candidate))
            }
        }

        self.resume_threads.clear();
        let mut directives = Vec::new();
        for entry in remaining.split(';') {
            if entry.is_empty() {
                self.send_reply(backend, GDB_STUB_REPLY_ERR);
                return;
            }
            let (action_token, thread_token) = entry
                .split_once(':')
                .map_or((entry, None), |(action, thread)| (action, Some(thread)));
            if action_token.is_empty() {
                self.send_reply(backend, GDB_STUB_REPLY_ERR);
                return;
            }

            let action = if action_token == "c"
                || action_token.strip_prefix('C').is_some_and(is_hex_byte)
            {
                VContAction::Continue
            } else if action_token == "s" || action_token.strip_prefix('S').is_some_and(is_hex_byte)
            {
                VContAction::Step
            } else {
                self.send_reply(backend, GDB_STUB_REPLY_ERR);
                return;
            };

            let (thread, all_threads) = match thread_token {
                None | Some("-1") => (None, true),
                Some("0") => (backend.get_active_thread(), false),
                Some(token) if token.starts_with('p') => {
                    self.send_reply(backend, GDB_STUB_REPLY_ERR);
                    return;
                }
                Some(token) if is_hex_string(token) => {
                    (self.get_thread_by_id(parse_hex_u64(token)), false)
                }
                Some(_) => {
                    self.send_reply(backend, GDB_STUB_REPLY_ERR);
                    return;
                }
            };
            directives.push(VContDirective {
                action,
                thread,
                all_threads,
            });
        }

        if directives.is_empty() {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
            return;
        }

        let threads = self.process_threads();
        let mut stepped_thread = None;
        let mut continue_threads = Vec::new();
        for thread in &threads {
            let Some(directive) = directives
                .iter()
                .find(|candidate| candidate.matches(thread))
            else {
                continue;
            };
            match directive.action {
                VContAction::Continue => continue_threads.push(Arc::clone(thread)),
                VContAction::Step => {
                    if stepped_thread.is_some() {
                        self.send_reply(backend, GDB_STUB_REPLY_ERR);
                        return;
                    }
                    stepped_thread = Some(Arc::clone(thread));
                }
            }
        }

        if let Some(thread) = stepped_thread {
            backend.set_active_thread(thread);
            self.resume_threads = continue_threads;
            actions.push(DebuggerAction::StepThread);
        } else if continue_threads.len() == threads.len() {
            actions.push(DebuggerAction::Continue);
        } else if !continue_threads.is_empty() {
            self.resume_threads = continue_threads;
            actions.push(DebuggerAction::ContinueThreads);
        } else {
            self.send_reply(backend, GDB_STUB_REPLY_ERR);
        }
    }

    /// Handle a hex-encoded GDB monitor command.
    ///
    /// Corresponds to upstream `GDBStub::HandleRcmd`.
    fn handle_rcmd(&self, backend: &mut dyn DebuggerBackend, command: &[u8]) {
        let command = String::from_utf8_lossy(command);
        let process = self.debug_process.lock().unwrap();
        let page_table = &process.page_table;

        let reply = if command == "fastmem" || command == "get fastmem" {
            let values = common::settings::values();
            let fastmem_enabled = common::settings::is_fastmem_enabled(&values);
            drop(values);
            if fastmem_enabled {
                if let Some(page_table_impl) = page_table.get_base().get_impl() {
                    let region = page_table_impl.fastmem_arena as usize;
                    let region_bits = page_table_impl.current_address_space_width_in_bits;
                    let region_size = 1usize.wrapping_shl(region_bits as u32);
                    format!(
                        "Region bits:  {region_bits}\nHost address: {region:#x} - {:#x}\n",
                        region.wrapping_add(region_size).wrapping_sub(1)
                    )
                } else {
                    "Fastmem is not enabled.\n".to_string()
                }
            } else {
                "Fastmem is not enabled.\n".to_string()
            }
        } else if command == "info" || command == "get info" {
            let modules = crate::arm::debug::find_modules(&process);
            let alias_start = page_table.get_alias_region_start().get() as usize;
            let heap_start = page_table.get_heap_region_start().get() as usize;
            let aslr_start = page_table.get_alias_code_region_start().get() as usize;
            let stack_start = page_table.get_stack_region_start().get() as usize;
            let mut reply = format!(
                "Process:     {:#x} ({})\nProgram Id:  {:#018x}\nLayout:\n  Alias: {:#012x} - {:#012x}\n  Heap:  {:#012x} - {:#012x}\n  Aslr:  {:#012x} - {:#012x}\n  Stack: {:#012x} - {:#012x}\nModules:\n",
                process.get_process_id(),
                process.get_name(),
                process.get_program_id(),
                alias_start,
                alias_start
                    .wrapping_add(page_table.get_alias_region_size())
                    .wrapping_sub(1),
                heap_start,
                heap_start
                    .wrapping_add(page_table.get_heap_region_size())
                    .wrapping_sub(1),
                aslr_start,
                aslr_start
                    .wrapping_add(page_table.get_alias_code_region_size())
                    .wrapping_sub(1),
                stack_start,
                stack_start
                    .wrapping_add(page_table.get_stack_region_size())
                    .wrapping_sub(1),
            );
            for (address, name) in modules {
                reply.push_str(&format!(
                    "  {address:#012x} - {:#012x} {name}\n",
                    crate::arm::debug::get_module_end(&process, address)
                ));
            }
            reply
        } else if command == "mappings" || command == "get mappings" {
            let mut reply = String::from("Mappings:\n");
            let mut current_address = 0usize;
            loop {
                let memory_info = page_table
                    .query_info(current_address)
                    .expect("process page-table query must succeed")
                    .get_svc_memory_info();
                let last_address = memory_info
                    .base_address
                    .wrapping_add(memory_info.size)
                    .wrapping_sub(1);

                if memory_info.state != MemoryState::Inaccessible as u32 || last_address != u64::MAX
                {
                    let attributes = memory_info.attribute;
                    reply.push_str(&format!(
                        "  {:#012x} - {:#012x} {} {} {}{}{}{}{} [{}, {}]\n",
                        memory_info.base_address,
                        last_address,
                        memory_permission_string(memory_info.state, memory_info.permission),
                        memory_state_name(memory_info.state),
                        attribute_marker(attributes, MemoryAttribute::Locked, 'L'),
                        attribute_marker(attributes, MemoryAttribute::IpcLocked, 'I'),
                        attribute_marker(attributes, MemoryAttribute::DeviceShared, 'D'),
                        attribute_marker(attributes, MemoryAttribute::Uncached, 'U'),
                        attribute_marker(attributes, MemoryAttribute::PermissionLocked, 'P'),
                        memory_info.ipc_count,
                        memory_info.device_count,
                    ));
                }

                let next_address = memory_info.base_address.wrapping_add(memory_info.size) as usize;
                if next_address <= current_address {
                    break;
                }
                current_address = next_address;
            }
            reply
        } else {
            "Commands: fastmem, info, mappings\n".to_string()
        };
        drop(process);

        self.send_reply(backend, &hex::encode(reply.as_bytes()));
    }

    /// Find a process thread by guest thread ID.
    ///
    /// Corresponds to upstream `GDBStub::GetThreadByID`.
    fn get_thread_by_id(&self, thread_id: u64) -> Option<Arc<KThreadLock>> {
        let process = self.debug_process.lock().unwrap();
        process
            .thread_list
            .iter()
            .find(|candidate| **candidate == thread_id)
            .and_then(|candidate| process.get_thread_by_thread_id(*candidate))
    }

    fn process_threads(&self) -> Vec<Arc<KThreadLock>> {
        let process = self.debug_process.lock().unwrap();
        process
            .thread_list
            .iter()
            .filter_map(|thread_id| process.get_thread_by_thread_id(*thread_id))
            .collect()
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

fn parse_hex_u64(value: &str) -> u64 {
    let digits = value
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_hexdigit())
        .count();
    u64::from_str_radix(&value[..digits], 16).unwrap_or(0)
}

fn parse_hex_i64(value: &str) -> i64 {
    if let Some(value) = value.strip_prefix('-') {
        -(parse_hex_u64(value) as i64)
    } else {
        parse_hex_u64(value) as i64
    }
}

fn parse_address_and_size(command: &str) -> Option<(u64, u64)> {
    let (address, size) = command.split_once(',')?;
    Some((parse_hex_u64(address), parse_hex_u64(size)))
}

fn parse_breakpoint(command: &str) -> Option<(BreakpointType, u64, u64)> {
    let mut fields = command.split(',');
    let breakpoint_type = match parse_hex_u64(fields.next()?) {
        0 => BreakpointType::Software,
        1 => BreakpointType::Hardware,
        2 => BreakpointType::WriteWatch,
        3 => BreakpointType::ReadWatch,
        4 => BreakpointType::AccessWatch,
        _ => BreakpointType::Hardware,
    };
    let address = parse_hex_u64(fields.next()?);
    let size = parse_hex_u64(fields.next()?);
    Some((breakpoint_type, address, size))
}

fn is_hex_byte(value: &str) -> bool {
    value.len() == 2 && is_hex_string(value)
}

fn is_hex_string(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Corresponds to upstream `PaginateBuffer`.
fn paginate_buffer(buffer: &str, request: &str) -> String {
    let (offset, amount) = request.split_once(',').unwrap_or((request, ""));
    let offset = parse_hex_u64(offset) as usize;
    let amount = parse_hex_u64(amount) as usize;
    let end = offset.wrapping_add(amount);

    if end > buffer.len() {
        format!("l{}", buffer.get(offset..).unwrap_or_default())
    } else {
        format!("m{}", buffer.get(offset..end).unwrap_or_default())
    }
}

/// Corresponds to upstream `EscapeXML`.
fn escape_xml(data: &str) -> String {
    let mut escaped = String::new();
    for character in data.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            character if character as u32 > 0x7f => {
                escaped.push_str(&format!("&#{};", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

/// Corresponds to upstream `GetMemoryStateName`.
fn memory_state_name(state: u32) -> &'static str {
    match state {
        value if value == MemoryState::Free as u32 => "Free",
        value if value == MemoryState::Io as u32 => "Io",
        value if value == MemoryState::Static as u32 => "Static",
        value if value == MemoryState::Code as u32 => "Code",
        value if value == MemoryState::CodeData as u32 => "CodeData",
        value if value == MemoryState::Normal as u32 => "Normal",
        value if value == MemoryState::Shared as u32 => "Shared",
        value if value == MemoryState::AliasCode as u32 => "AliasCode",
        value if value == MemoryState::AliasCodeData as u32 => "AliasCodeData",
        value if value == MemoryState::Ipc as u32 => "Ipc",
        value if value == MemoryState::Stack as u32 => "Stack",
        value if value == MemoryState::ThreadLocal as u32 => "ThreadLocal",
        value if value == MemoryState::Transferred as u32 => "Transferred",
        value if value == MemoryState::SharedTransferred as u32 => "SharedTransferred",
        value if value == MemoryState::SharedCode as u32 => "SharedCode",
        value if value == MemoryState::Inaccessible as u32 => "Inaccessible",
        value if value == MemoryState::NonSecureIpc as u32 => "NonSecureIpc",
        value if value == MemoryState::NonDeviceIpc as u32 => "NonDeviceIpc",
        value if value == MemoryState::Kernel as u32 => "Kernel",
        value if value == MemoryState::GeneratedCode as u32 => "GeneratedCode",
        value if value == MemoryState::CodeOut as u32 => "CodeOut",
        value if value == MemoryState::Coverage as u32 => "Coverage",
        _ => "Unknown",
    }
}

/// Corresponds to upstream `GetMemoryPermissionString`.
fn memory_permission_string(state: u32, permission: u32) -> &'static str {
    if state == MemoryState::Free as u32 {
        "   "
    } else if permission == MemoryPermission::ReadExecute as u32 {
        "r-x"
    } else if permission == MemoryPermission::Read as u32 {
        "r--"
    } else if permission == MemoryPermission::ReadWrite as u32 {
        "rw-"
    } else {
        "---"
    }
}

fn attribute_marker(attributes: u32, attribute: MemoryAttribute, marker: char) -> char {
    if attributes & attribute as u32 != 0 {
        marker
    } else {
        '-'
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_process::KProcess;

    #[derive(Default)]
    struct TestBackend {
        input: Vec<u8>,
        output: Vec<Vec<u8>>,
        active_thread: Option<Arc<KThreadLock>>,
    }

    impl DebuggerBackend for TestBackend {
        fn read_from_client(&mut self) -> &[u8] {
            &self.input
        }

        fn write_to_client(&mut self, data: &[u8]) {
            self.output.push(data.to_vec());
        }

        fn get_active_thread(&self) -> Option<Arc<KThreadLock>> {
            self.active_thread.clone()
        }

        fn set_active_thread(&mut self, thread: Arc<KThreadLock>) {
            self.active_thread = Some(thread);
        }
    }

    fn stub() -> GdbStub {
        GdbStub::new(Arc::new(ProcessLock::new(KProcess::new())))
    }

    fn packet(command: &str) -> Vec<u8> {
        format!(
            "${command}#{:02x}",
            GdbStub::calculate_checksum(command.as_bytes())
        )
        .into_bytes()
    }

    fn reply(payload: &str) -> Vec<u8> {
        let escaped = GdbStub::escape_gdb(payload);
        format!(
            "${escaped}#{:02x}",
            GdbStub::calculate_checksum(escaped.as_bytes())
        )
        .into_bytes()
    }

    #[test]
    fn supported_query_matches_upstream_packet_and_ack_order() {
        let mut stub = stub();
        let mut backend = TestBackend::default();

        let actions = stub.client_data(&mut backend, &packet("qSupported:multiprocess+"));

        assert!(actions.is_empty());
        assert_eq!(backend.output[0], vec![GDB_STUB_ACK]);
        assert_eq!(
            backend.output[1],
            reply(
                "PacketSize=4000;qXfer:features:read+;qXfer:threads:read+;qXfer:libraries:read+;vContSupported+;QStartNoAckMode+"
            )
        );
    }

    #[test]
    fn start_no_ack_mode_suppresses_only_later_status_bytes() {
        let mut stub = stub();
        let mut backend = TestBackend::default();

        stub.client_data(&mut backend, &packet("QStartNoAckMode"));
        stub.client_data(&mut backend, &packet("qTStatus"));

        assert_eq!(
            backend.output,
            vec![vec![GDB_STUB_ACK], reply(GDB_STUB_REPLY_OK), reply("T0")]
        );
    }

    #[test]
    fn vcont_capabilities_and_invalid_resume_match_upstream() {
        let mut stub = stub();
        let mut backend = TestBackend::default();
        let mut actions = Vec::new();

        stub.execute_command(&mut backend, "vCont?", &mut actions);
        stub.execute_command(&mut backend, "vCont;c:p1.1", &mut actions);

        assert!(actions.is_empty());
        assert_eq!(
            backend.output,
            vec![reply("vCont;c;C;s;S"), reply(GDB_STUB_REPLY_ERR)]
        );
    }

    #[test]
    fn pagination_and_xml_escaping_match_upstream_boundaries() {
        assert_eq!(paginate_buffer("abcdef", "0,6"), "mabcdef");
        assert_eq!(paginate_buffer("abcdef", "2,5"), "lcdef");
        assert_eq!(
            escape_xml("free&homebrew<\"é\">"),
            "free&amp;homebrew&lt;&quot;&#233;&quot;&gt;"
        );
    }

    #[test]
    fn hexadecimal_parser_matches_strtoll_prefix_behavior() {
        assert_eq!(parse_hex_u64("1f=0011"), 0x1f);
        assert_eq!(parse_hex_u64("xyz"), 0);
        assert_eq!(parse_hex_i64("-1"), -1);
    }
}
