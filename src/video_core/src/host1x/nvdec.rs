// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/nvdec.h` and `nvdec.cpp`.
//!
//! NVDEC video decoder engine — processes register writes and dispatches
//! codec-specific decoding.

use std::sync::Arc;

use log::info;

use crate::cdma_pusher::ProcessMethodHook;
use crate::host1x::codecs::decoder::{self, DecoderImpl};
use crate::host1x::codecs::h264::H264;
use crate::host1x::codecs::vp8::Vp8;
use crate::host1x::codecs::vp9::Vp9;
use crate::host1x::host1x::FrameQueue;
use crate::host1x::nvdec_common::{NvdecRegisters, VideoCodec, REG_EXECUTE, REG_SET_CODEC_ID};
use crate::memory_manager::MemoryManager;

/// NVDEC video decoder device.
///
/// Port of `Tegra::Host1x::Nvdec`.
pub struct Nvdec {
    id: i32,
    syncpoint: u32,
    frame_queue: Arc<FrameQueue>,
    memory_manager: Arc<parking_lot::Mutex<MemoryManager>>,
    regs: NvdecRegisters,
    decoder: Option<Box<dyn DecoderImpl>>,
    wait_needed: bool,
}

impl Nvdec {
    pub fn new(
        id: i32,
        syncpt: u32,
        frame_queue: Arc<FrameQueue>,
        memory_manager: Arc<parking_lot::Mutex<MemoryManager>>,
    ) -> Self {
        info!("Created nvdec {}", id);
        frame_queue.open(id);
        Self {
            id,
            syncpoint: syncpt,
            frame_queue,
            memory_manager,
            regs: NvdecRegisters::default(),
            decoder: None,
            wait_needed: false,
        }
    }

    pub fn get_syncpoint(&self) -> u32 {
        self.syncpoint
    }

    pub fn set_wait(&mut self) {
        self.wait_needed = true;
    }

    /// Writes the method into the state; invokes Execute() if encountered.
    ///
    /// Port of `Nvdec::ProcessMethod`.
    pub fn process_method(&mut self, method: u32, argument: u32) {
        self.regs.reg_array[method as usize] = argument as u64;

        if method as usize == REG_SET_CODEC_ID {
            self.create_decoder(VideoCodec::from(argument as u64));
        } else if method as usize == REG_EXECUTE {
            if self.wait_needed {
                std::thread::sleep(std::time::Duration::from_millis(32));
                self.wait_needed = false;
            }
            self.execute();
        }
    }

    /// Create the decoder when the codec ID is set.
    ///
    /// Port of `Nvdec::CreateDecoder`.
    fn create_decoder(&mut self, codec: VideoCodec) {
        if self.decoder.is_some() {
            return;
        }

        let decoder: Box<dyn DecoderImpl> = match codec {
            VideoCodec::H264 => Box::new(H264::new(
                self.id,
                Arc::clone(&self.memory_manager),
                Arc::clone(&self.frame_queue),
            )),
            VideoCodec::VP8 => Box::new(Vp8::new(
                self.id,
                Arc::clone(&self.memory_manager),
                Arc::clone(&self.frame_queue),
            )),
            VideoCodec::VP9 => Box::new(Vp9::new(
                self.id,
                Arc::clone(&self.memory_manager),
                Arc::clone(&self.frame_queue),
            )),
            _ => {
                log::error!("Unimplemented codec {:?}", codec);
                return;
            }
        };

        info!(
            "Created decoder {} for id {}",
            decoder.get_current_codec_name(),
            self.id
        );
        self.decoder = Some(decoder);
    }

    /// Invoke codec to decode a frame.
    ///
    /// Port of `Nvdec::Execute`.
    fn execute(&mut self) {
        if *common::settings::values().nvdec_emulation.get_value()
            == common::settings_enums::NvdecEmulation::Off
        {
            // Upstream delays disabled NVDEC work so games do not observe an
            // unrealistically fast syncpoint signal.
            std::thread::sleep(std::time::Duration::from_millis(8));
            return;
        }

        if let Some(ref mut dec) = self.decoder {
            let _ = common::trace::emit(
                common::trace::cat::HOST1X_VIDEO,
                &[
                    3,
                    self.id as u64,
                    dec.get_current_codec() as u64,
                    dec.state().initialized as u64,
                ],
            );
            match dec.get_current_codec() {
                VideoCodec::H264 | VideoCodec::VP8 | VideoCodec::VP9 => {
                    decoder::decode(dec.as_mut(), &self.regs);
                }
                _ => {
                    log::error!("Unimplemented codec {}", dec.get_current_codec_name());
                }
            }
        }
    }
}

impl Drop for Nvdec {
    fn drop(&mut self) {
        info!("Destroying nvdec {}", self.id);
        self.frame_queue.close(self.id);
    }
}

impl ProcessMethodHook for Nvdec {
    fn process_method(&mut self, method: u32, arg: u32) {
        Nvdec::process_method(self, method, arg);
    }
}
