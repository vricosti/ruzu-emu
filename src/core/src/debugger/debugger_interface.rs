// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/debugger/debugger_interface.h
//! Debugger backend and frontend interface traits.

/// Actions the debugger can request of the emulation engine.
///
/// Corresponds to upstream `Core::DebuggerAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebuggerAction {
    /// Stop emulation as soon as possible.
    Interrupt,
    /// Resume emulation.
    Continue,
    /// Resume only the threads selected by the frontend.
    ContinueThreads,
    /// Step the active thread and resume only the threads selected by the frontend.
    StepThread,
    /// Shut down the emulator.
    ShutdownEmulation,
}

#[cfg(test)]
mod tests {
    use super::DebuggerAction;

    #[test]
    fn debugger_actions_match_upstream_order() {
        assert_eq!(DebuggerAction::Interrupt as usize, 0);
        assert_eq!(DebuggerAction::Continue as usize, 1);
        assert_eq!(DebuggerAction::ContinueThreads as usize, 2);
        assert_eq!(DebuggerAction::StepThread as usize, 3);
        assert_eq!(DebuggerAction::ShutdownEmulation as usize, 4);
    }
}

/// Backend interface for the debugger (network I/O and thread management).
///
/// Corresponds to upstream `Core::DebuggerBackend`.
pub trait DebuggerBackend {
    /// Synchronously wait for more data from the client.
    /// Returns as soon as at least one byte is received. Reads up to 4096 bytes.
    fn read_from_client(&mut self) -> &[u8];

    /// Write data to the client. Returns immediately after sending.
    fn write_to_client(&mut self, data: &[u8]);

    /// Gets the currently active thread when the debugger is stopped.
    /// Returns an opaque thread identifier.
    fn get_active_thread(&self) -> u64;

    /// Sets the currently active thread when the debugger is stopped.
    fn set_active_thread(&mut self, thread_id: u64);
}

/// Frontend interface for the debugger (protocol implementation).
///
/// Corresponds to upstream `Core::DebuggerFrontend`.
pub trait DebuggerFrontend {
    /// Called after the client has successfully connected to the port.
    fn connected(&mut self, backend: &mut dyn DebuggerBackend);

    /// Called when emulation has stopped.
    fn stopped(&mut self, backend: &mut dyn DebuggerBackend, thread_id: u64);

    /// Called when emulation is shutting down.
    fn shutting_down(&mut self, backend: &mut dyn DebuggerBackend);

    /// Called when emulation has stopped on a watchpoint.
    fn watchpoint(
        &mut self,
        backend: &mut dyn DebuggerBackend,
        thread_id: u64,
        watch_addr: u64,
        watch_type: u8,
    );

    /// Called when new data is asynchronously received on the client socket.
    /// Returns a list of actions to perform.
    fn client_data(
        &mut self,
        backend: &mut dyn DebuggerBackend,
        data: &[u8],
    ) -> Vec<DebuggerAction>;
}
