// SPDX-FileCopyrightText: Copyright 2018 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/
//! HLE service infrastructure and all service implementations.

// Infrastructure
pub mod cmif_serialization;
pub mod cmif_types;
pub mod hle_ipc;
pub mod ipc_helpers;
pub mod kernel_helpers;
pub mod server_manager;
pub mod service;
pub mod nx2_stub_interface;
pub mod nx2_gsv;
pub mod services;

// Service Manager
pub mod sm;

// Services (alphabetical)
pub mod acc;
pub mod am;
pub mod aoc;
pub mod apm;
pub mod audio;
pub mod bcat;
pub mod bpc;
pub mod btdrv;
pub mod btm;
pub mod caps;
pub mod chat;
pub mod erpt;
pub mod es;
pub mod eupld;
pub mod fatal;
pub mod fgm;
pub mod filesystem;
pub mod friend;
pub mod glue;
pub mod grc;
pub mod hid;
pub mod jit;
pub mod lbl;
pub mod ldn;
pub mod ldr;
pub mod lm;
pub mod mig;
pub mod mii;
pub mod mm;
pub mod mnpp;
pub mod ncm;
pub mod nfc;
pub mod nfp;
pub mod ngc;
pub mod nifm;
pub mod nim;
pub mod npns;
pub mod ns;
pub mod nvdrv;
pub mod nvnflinger;
pub mod olsc;
pub mod omm;
pub mod os;
pub mod pcie;
pub mod pctl;
pub mod pcv;
pub mod pm;
pub mod prepo;
pub mod psc;
pub mod ptm;
pub mod ro;
pub mod set;
pub mod sockets;
pub mod spl;
pub mod ssl;
pub mod tma;
pub mod usb;
pub mod vi;
pub mod wlan;
