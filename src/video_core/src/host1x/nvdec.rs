// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/nvdec.h` and `nvdec.cpp`.
//!
//! NVDEC video decoder engine — processes register writes and dispatches
//! codec-specific decoding.

use std::sync::Arc;

use log::info;

use crate::cdma_pusher::ProcessMethodHook;
use crate::host1x::codecs::decoder;
use crate::host1x::codecs::h264::H264;
use crate::host1x::codecs::vp8::Vp8;
use crate::host1x::codecs::vp9::Vp9;
use crate::host1x::host1x::FrameQueue;
use crate::host1x::nvdec_common::{NvdecRegisters, VideoCodec, REG_EXECUTE, REG_SET_CODEC_ID};
use crate::memory_manager::MemoryManager;

enum Decoder {
    H264(H264),
    Vp8(Vp8),
    Vp9(Vp9),
    None,
}

/// NVDEC video decoder device.
///
/// Port of `Tegra::Host1x::Nvdec`.
pub struct Nvdec {
    id: i32,
    syncpoint: u32,
    frame_queue: Arc<FrameQueue>,
    memory_manager: Arc<parking_lot::Mutex<MemoryManager>>,
    regs: NvdecRegisters,
    decoder: Decoder,
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
            decoder: Decoder::None,
        }
    }

    pub fn get_syncpoint(&self) -> u32 {
        self.syncpoint
    }

    /// Writes the method into the state; invokes Execute() if encountered.
    ///
    /// Port of `Nvdec::ProcessMethod`.
    pub fn process_method(&mut self, method: u32, argument: u32) {
        self.regs.reg_array[method as usize] = argument as u64;

        if method as usize == REG_SET_CODEC_ID {
            self.create_decoder(VideoCodec::from(argument as u64));
        } else if method as usize == REG_EXECUTE {
            self.execute();
        }
    }

    /// Create the decoder when the codec ID is set.
    ///
    /// Port of `Nvdec::CreateDecoder`.
    fn create_decoder(&mut self, codec: VideoCodec) {
        if !matches!(self.decoder, Decoder::None) {
            return;
        }

        self.decoder = match codec {
            VideoCodec::H264 => Decoder::H264(H264::new(
                self.id,
                Arc::clone(&self.memory_manager),
                Arc::clone(&self.frame_queue),
            )),
            VideoCodec::VP8 => Decoder::Vp8(Vp8::new(
                self.id,
                Arc::clone(&self.memory_manager),
                Arc::clone(&self.frame_queue),
            )),
            VideoCodec::VP9 => Decoder::Vp9(Vp9::new(
                self.id,
                Arc::clone(&self.memory_manager),
                Arc::clone(&self.frame_queue),
            )),
            _ => Decoder::None,
        };

        info!("Created decoder {:?} for id {}", codec, self.id);
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

        match &mut self.decoder {
            Decoder::H264(h264) => decoder::decode(h264, &self.regs),
            Decoder::Vp8(vp8) => decoder::decode(vp8, &self.regs),
            Decoder::Vp9(vp9) => decoder::decode(vp9, &self.regs),
            Decoder::None => log::error!("Unrecognized codec executed?"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_nvdec() -> Nvdec {
        Nvdec::new(
            7,
            11,
            Arc::new(FrameQueue::new()),
            Arc::new(parking_lot::Mutex::new(MemoryManager::new(0))),
        )
    }

    #[test]
    fn process_method_writes_the_64_bit_register_slot() {
        let mut nvdec = make_nvdec();

        nvdec.process_method(4, 0xfedc_ba98);

        assert_eq!(nvdec.regs.reg_array[4], 0x0000_0000_fedc_ba98);
        assert_eq!(nvdec.get_syncpoint(), 11);
    }

    #[test]
    fn unsupported_codec_keeps_the_monostate() {
        let mut nvdec = make_nvdec();

        nvdec.create_decoder(VideoCodec::H265);

        assert!(matches!(nvdec.decoder, Decoder::None));
    }

    #[test]
    fn decoder_is_created_only_for_the_first_supported_codec() {
        let mut nvdec = make_nvdec();

        nvdec.create_decoder(VideoCodec::H264);
        assert!(matches!(nvdec.decoder, Decoder::H264(_)));

        nvdec.create_decoder(VideoCodec::VP8);
        assert!(matches!(nvdec.decoder, Decoder::H264(_)));
    }
}
