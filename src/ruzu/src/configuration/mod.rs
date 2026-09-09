// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of the upstream configuration-dialog directory
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/`.
//
// One module per upstream `configure_*.cpp`, at the same relative path, so a
// reviewer can map each Rust file back to its Qt original. `shared_widget` and
// `shared_translation` mirror the two upstream helper translation units of the
// same names.
//
// Upstream files with no counterpart yet (each is a separate dialog reached
// from a Configure button rather than a tab of the main dialog):
//   configure_camera, configure_debug_controller,
//   configure_ringcon,
//   configure_touchscreen_advanced, configure_touch_widget.
//
// `qt_config` covers only the game-directory array of its upstream counterpart
// so far; the rest of `Config::Read*Values` / `Save*Values` is handled by the
// `frontend_common` crate.

pub mod configure_applets;
pub mod configure_audio;
pub mod configure_cpu;
pub mod configure_cpu_debug;
pub mod configure_debug;
pub mod configure_debug_tab;
pub mod configure_dialog;
pub mod configure_filesystem;
pub mod configure_general;
pub mod configure_graphics;
pub mod configure_graphics_advanced;
pub mod configure_graphics_extensions;
pub mod configure_hotkeys;
pub mod configure_input;
pub mod configure_debug_controller;
pub mod configure_input_advanced;
pub mod configure_input_per_game;
pub mod configure_input_player;
pub mod configure_input_profile_dialog;
pub mod configure_motion_touch;
pub mod configure_touchscreen_advanced;
pub mod configure_mouse_panning;
pub mod configure_network;
pub mod configure_per_game;
pub mod configure_per_game_addons;
pub mod configure_profile_manager;
pub mod configure_system;
pub mod configure_tas;
pub mod configure_touch_from_button;
pub mod configure_ui;
pub mod configure_vibration;
pub mod configure_web;
pub mod controller_outlines;
pub mod controller_preview;
pub mod input_profiles;
pub mod qt_config;
pub mod shared_translation;
pub mod shared_widget;

pub use configure_dialog::ConfigureDialog;
