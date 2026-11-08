//! Port of zuyu/src/core/hle/service/audio/
//! Audio service modules.

pub mod audio;
pub mod audio_controller;
pub mod audio_device;
pub mod audio_in;
pub mod audio_in_manager;
pub mod audio_out;
pub mod audio_out_manager;
pub mod audio_renderer;
pub mod audio_renderer_manager;
pub mod errors;
pub mod final_output_recorder_manager;
pub mod final_output_recorder_manager_for_applet;
pub mod hardware_opus_decoder;
pub mod hardware_opus_decoder_manager;

pub mod nx2_audio_user_client;
