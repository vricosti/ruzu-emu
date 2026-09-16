// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Internal network modules.
//! Port of zuyu/src/core/internal_network/

pub mod emu_net_state;
pub mod network;
pub mod network_interface;
pub mod socket_proxy;
pub mod sockets;
pub mod wifi_scanner;
mod wifi_scanner_dummy;
