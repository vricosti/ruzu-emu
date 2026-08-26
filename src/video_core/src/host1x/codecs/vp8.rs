// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/codecs/vp8.h` and `vp8.cpp`.
//!
//! VP8 video decoder implementation.

use crate::host1x::codec_types::Vp8PictureInfo;
use crate::host1x::codecs::decoder::{DecoderImpl, DecoderState};
use crate::host1x::host1x::FrameQueue;
use crate::host1x::nvdec_common::{NvdecRegisters, VideoCodec};
use crate::memory_manager::MemoryManager;
use std::sync::Arc;

/// Surface indices used by the VP8 decoder.
///
/// Port of `Tegra::Decoders::Vp8SurfaceIndex`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vp8SurfaceIndex {
    Last = 0,
    Golden = 1,
    AltRef = 2,
    Current = 3,
}

/// Writes the VP8 frame tag and, for key frames, the uncompressed key-frame
/// header. This is the header-building block from upstream `VP8::ComposeFrame`.
fn write_frame_header(context: &Vp8PictureInfo, output: &mut [u8]) -> usize {
    let is_key_frame = context.key_frame == 1;
    let header_size = if is_key_frame { 10 } else { 3 };
    assert!(output.len() >= header_size);

    output[0] = u8::from(!is_key_frame);
    output[0] |= (context.version & 7) << 1;
    output[0] |= 1 << 4;
    output[0] |= ((context.first_part_size & 7) << 5) as u8;
    output[1] = ((context.first_part_size & 0x7f8) >> 3) as u8;
    output[2] = ((context.first_part_size & 0x7f800) >> 11) as u8;

    if is_key_frame {
        output[3..6].copy_from_slice(&[0x9d, 0x01, 0x2a]);
        output[6] = context.frame_width as u8;
        output[7] = ((context.frame_width >> 8) & 0x3f) as u8;
        output[8] = context.frame_height as u8;
        output[9] = ((context.frame_height >> 8) & 0x3f) as u8;
    }
    header_size
}

/// VP8 video decoder.
///
/// Port of `Tegra::Decoders::VP8`.
pub struct Vp8 {
    pub state: DecoderState,
    frame_scratch: Vec<u8>,
    current_context: Vp8PictureInfo,
}

impl Vp8 {
    pub fn new(
        id: i32,
        memory_manager: Arc<parking_lot::Mutex<MemoryManager>>,
        frame_queue: Arc<FrameQueue>,
    ) -> Self {
        let mut state = DecoderState::new(id, memory_manager, frame_queue);
        state.codec = VideoCodec::VP8;
        state.initialized = state.decode_api.initialize(VideoCodec::VP8);
        Self {
            state,
            frame_scratch: Vec::new(),
            current_context: Vp8PictureInfo::default(),
        }
    }
}

impl DecoderImpl for Vp8 {
    fn compose_frame(&mut self, regs: &NvdecRegisters) -> Vec<u8> {
        let memory_manager = Arc::clone(&self.state.memory_manager);
        let context_bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut self.current_context as *mut Vp8PictureInfo).cast::<u8>(),
                std::mem::size_of::<Vp8PictureInfo>(),
            )
        };
        memory_manager
            .lock()
            .read_block(regs.picture_info_offset().address(), context_bytes);
        self.state.set_frame_dimensions(
            self.current_context.frame_width as i32,
            self.current_context.frame_height as i32,
        );

        let bitstream_size = self.current_context.vld_buffer_size as usize;
        let header_size = if self.current_context.key_frame == 1 {
            10
        } else {
            3
        };
        self.frame_scratch.resize(header_size + bitstream_size, 0);
        let written_header_size = write_frame_header(
            &self.current_context,
            &mut self.frame_scratch[..header_size],
        );
        debug_assert_eq!(written_header_size, header_size);

        memory_manager.lock().read_block(
            regs.frame_bitstream_offset().address(),
            &mut self.frame_scratch[header_size..],
        );
        self.frame_scratch.clone()
    }

    fn get_progressive_offsets(&self, regs: &NvdecRegisters) -> (u64, u64) {
        let current = Vp8SurfaceIndex::Current as usize;
        (
            regs.surface_luma_offset(current).address(),
            regs.surface_chroma_offset(current).address(),
        )
    }

    fn get_interlaced_offsets(&self, regs: &NvdecRegisters) -> (u64, u64, u64, u64) {
        let current = Vp8SurfaceIndex::Current as usize;
        let luma = regs.surface_luma_offset(current).address();
        let chroma = regs.surface_chroma_offset(current).address();
        (luma, luma, chroma, chroma)
    }

    fn is_interlaced(&self) -> bool {
        false
    }

    fn get_current_codec_name(&self) -> &str {
        "VP8"
    }

    fn get_current_codec(&self) -> VideoCodec {
        self.state.codec
    }

    fn state(&self) -> &DecoderState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut DecoderState {
        &mut self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_headers_match_upstream_vp8_bit_layout() {
        let key_context = Vp8PictureInfo {
            frame_width: 1920,
            frame_height: 1080,
            key_frame: 1,
            version: 3,
            first_part_size: 0x5_432,
            ..Default::default()
        };
        let mut key_header = [0u8; 10];
        assert_eq!(write_frame_header(&key_context, &mut key_header), 10);
        assert_eq!(key_header[0..3], [0x56, 0x86, 0x0a]);
        assert_eq!(key_header[3..6], [0x9d, 0x01, 0x2a]);
        assert_eq!(key_header[6..10], [0x80, 0x07, 0x38, 0x04]);

        let inter_context = Vp8PictureInfo {
            key_frame: 0,
            version: 2,
            first_part_size: 0x12345,
            ..Default::default()
        };
        let mut inter_header = [0u8; 3];
        assert_eq!(write_frame_header(&inter_context, &mut inter_header), 3);
        assert_eq!(inter_header, [0xb5, 0x68, 0x24]);
    }
}
