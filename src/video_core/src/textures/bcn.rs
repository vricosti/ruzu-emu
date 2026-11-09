// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/textures/bcn.h` and `bcn.cpp`.
//!
//! BC1 and BC3 block compression for textures. The upstream implementation
//! uses the `stb_dxt` library for the actual block compression; this port
//! provides the same interface and dispatching logic.

use common::alignment::divide_up;

use super::workers::get_thread_workers;

// ── Types ────────────────────────────────────────────────────────────────────

/// Type alias for a BCN block compressor function.
///
/// Port of `BCNCompressor` typedef from `bcn.cpp`.
///
/// Parameters: `(block_output, block_input, any_alpha)`
type BcnCompressor = fn(block_output: &mut [u8], block_input: &[u8], any_alpha: bool);

extern "C" {
    fn ruzu_stb_compress_bc1_block(dest: *mut u8, src: *const u8, alpha: i32);
    fn ruzu_stb_compress_bc3_block(dest: *mut u8, src: *const u8);
    fn ruzu_bc7enc_compress_block(dest: *mut u8, src: *const u8);
}

// ── Internal ─────────────────────────────────────────────────────────────────

/// Generic BCN compression dispatcher.
///
/// Port of the `CompressBCN<BytesPerBlock, ThresholdAlpha>` template from `bcn.cpp`.
///
/// Iterates over 4x4 blocks, gathers RGBA texels, and calls the compressor `f`.
fn compress_bcn<const BYTES_PER_BLOCK: u32, const THRESHOLD_ALPHA: bool, F>(
    data: &[u8],
    width: u32,
    height: u32,
    depth: u32,
    output: &mut [u8],
    f: F,
) where
    F: Fn(&mut [u8], &[u8], bool) + Send + Copy + 'static,
{
    const ALPHA_THRESHOLD: u8 = 128;
    const BYTES_PER_PX: u32 = 4;

    #[derive(Clone, Copy)]
    struct SendConstPtr(*const u8);
    unsafe impl Send for SendConstPtr {}
    unsafe impl Sync for SendConstPtr {}
    impl SendConstPtr {
        unsafe fn as_slice<'a>(self, len: usize) -> &'a [u8] {
            std::slice::from_raw_parts(self.0, len)
        }
    }

    #[derive(Clone, Copy)]
    struct SendPtr(*mut u8);
    unsafe impl Send for SendPtr {}
    unsafe impl Sync for SendPtr {}
    impl SendPtr {
        unsafe fn slice_at<'a>(self, offset: usize, len: usize) -> &'a mut [u8] {
            std::slice::from_raw_parts_mut(self.0.add(offset), len)
        }
    }

    let plane_dim = width * height;
    let bytes_per_row = BYTES_PER_BLOCK * divide_up(u64::from(width), 4) as u32;
    let bytes_per_plane = bytes_per_row * divide_up(u64::from(height), 4) as u32;

    let required_input_len = usize::try_from(plane_dim)
        .unwrap()
        .checked_mul(depth as usize)
        .and_then(|size| size.checked_mul(BYTES_PER_PX as usize))
        .expect("BCN input dimensions overflow the host address space");
    let required_output_len = usize::try_from(bytes_per_plane)
        .unwrap()
        .checked_mul(depth as usize)
        .expect("BCN output dimensions overflow the host address space");
    assert!(data.len() >= required_input_len);
    assert!(output.len() >= required_output_len);

    let workers = get_thread_workers();
    let send_data = SendConstPtr(data.as_ptr());
    let data_len = data.len();
    let send_output = SendPtr(output.as_mut_ptr());

    for z in 0..depth {
        for y in (0..height).step_by(4) {
            workers.queue_stateless_work(move || {
                // SAFETY: all queued rows complete before the borrowed spans
                // can cease to be valid, matching Eden's by-value span capture.
                let data = unsafe { send_data.as_slice(data_len) };

                for x in (0..width).step_by(4) {
                    // Gather 4x4 block of RGBA texels.
                    let mut input_colors = [0u8; 4 * 4 * 4];
                    let mut any_alpha = false;

                    for j in 0..4u32 {
                        for i in 0..4u32 {
                            let coord = (z * plane_dim + (y + j) * width + (x + i)) as usize
                                * BYTES_PER_PX as usize;
                            let dst_idx = ((j * 4 + i) * BYTES_PER_PX) as usize;

                            if (x + i < width) && (y + j < height) {
                                if THRESHOLD_ALPHA {
                                    if data[coord + 3] >= ALPHA_THRESHOLD {
                                        input_colors[dst_idx] = data[coord];
                                        input_colors[dst_idx + 1] = data[coord + 1];
                                        input_colors[dst_idx + 2] = data[coord + 2];
                                        input_colors[dst_idx + 3] = 255;
                                    } else {
                                        any_alpha = true;
                                        input_colors[dst_idx..dst_idx + BYTES_PER_PX as usize]
                                            .fill(0);
                                    }
                                } else {
                                    input_colors[dst_idx..dst_idx + BYTES_PER_PX as usize]
                                        .copy_from_slice(
                                            &data[coord..coord + BYTES_PER_PX as usize],
                                        );
                                }
                            } else {
                                input_colors[dst_idx..dst_idx + BYTES_PER_PX as usize].fill(0);
                            }
                        }
                    }

                    let offset = (z * bytes_per_plane
                        + (y / 4) * bytes_per_row
                        + (x / 4) * BYTES_PER_BLOCK) as usize;

                    // SAFETY: each queued row writes to a distinct output row,
                    // and each block occupies a non-overlapping range within it.
                    let out_slice =
                        unsafe { send_output.slice_at(offset, BYTES_PER_BLOCK as usize) };
                    f(out_slice, &input_colors, any_alpha);
                }
            });
        }
        workers.wait_for_requests_helping();
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Compress RGBA8 data into BC1 format.
///
/// Port of `Tegra::Texture::BCN::CompressBC1`.
pub fn compress_bc1(data: &[u8], width: u32, height: u32, depth: u32, output: &mut [u8]) {
    compress_bcn::<8, true, _>(
        data,
        width,
        height,
        depth,
        output,
        |block_output, block_input, any_alpha| unsafe {
            ruzu_stb_compress_bc1_block(
                block_output.as_mut_ptr(),
                block_input.as_ptr(),
                any_alpha as i32,
            );
        },
    );
}

/// Compress RGBA8 data into BC7 format.
///
/// ruzu extension beyond Eden's BCN set: same 16-byte blocks and dispatch as
/// `compress_bc3`, encoded with the vendored bc7enc encoder for much higher
/// quality at a higher one-time CPU cost.
pub fn compress_bc7(data: &[u8], width: u32, height: u32, depth: u32, output: &mut [u8]) {
    compress_bcn::<16, false, _>(
        data,
        width,
        height,
        depth,
        output,
        |block_output, block_input, _any_alpha| unsafe {
            ruzu_bc7enc_compress_block(block_output.as_mut_ptr(), block_input.as_ptr());
        },
    );
}

/// Compress RGBA8 data into BC3 format.
///
/// Port of `Tegra::Texture::BCN::CompressBC3`.
pub fn compress_bc3(data: &[u8], width: u32, height: u32, depth: u32, output: &mut [u8]) {
    compress_bcn::<16, false, _>(
        data,
        width,
        height,
        depth,
        output,
        |block_output, block_input, _any_alpha| unsafe {
            ruzu_stb_compress_bc3_block(block_output.as_mut_ptr(), block_input.as_ptr());
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc3_matches_eden_texel_gather_for_full_and_partial_blocks() {
        // Independent scalar reference from Eden's CompressBCN<16, false>.
        // Exercise every edge remainder, multiple planes, and output guards.
        for width in 1..=9u32 {
            for height in 1..=9u32 {
                let depth = 2;
                let data: Vec<u8> = (0..width * height * depth * 4)
                    .map(|i| (i.wrapping_mul(37) ^ (i >> 3)) as u8)
                    .collect();
                let blocks_x = width.div_ceil(4);
                let blocks_y = height.div_ceil(4);
                let size = (blocks_x * blocks_y * depth * 16) as usize;
                let mut expected = vec![0u8; size];
                for z in 0..depth {
                    for y in (0..height).step_by(4) {
                        for x in (0..width).step_by(4) {
                            let mut block = [0u8; 64];
                            for j in 0..4 {
                                for i in 0..4 {
                                    if x + i < width && y + j < height {
                                        let src = ((z * height * width + (y + j) * width + x + i)
                                            * 4)
                                            as usize;
                                        let dst = ((j * 4 + i) * 4) as usize;
                                        block[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
                                    }
                                }
                            }
                            let offset = ((z * blocks_y * blocks_x + (y / 4) * blocks_x + x / 4)
                                * 16) as usize;
                            unsafe {
                                ruzu_stb_compress_bc3_block(
                                    expected[offset..].as_mut_ptr(),
                                    block.as_ptr(),
                                );
                            }
                        }
                    }
                }
                let mut actual = vec![0xa5u8; size + 32];
                compress_bc3(&data, width, height, depth, &mut actual[16..16 + size]);
                assert_eq!(&actual[16..16 + size], &expected, "{width}x{height}");
                assert_eq!(&actual[..16], &[0xa5; 16]);
                assert_eq!(&actual[16 + size..], &[0xa5; 16]);
            }
        }
    }

    #[test]
    fn bc1_uses_stb_dxt_output() {
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[255, 0, 0, 255]);
        }
        let mut output = [0u8; 8];
        compress_bc1(&rgba, 4, 4, 1, &mut output);
        assert_ne!(output, [0u8; 8]);
    }

    #[test]
    fn bc3_uses_stb_dxt_output() {
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&[0, 255, 0, (index * 17) as u8]);
        }
        let mut output = [0u8; 16];
        compress_bc3(&rgba, 4, 4, 1, &mut output);
        assert_ne!(output, [0u8; 16]);
    }

    #[test]
    fn threaded_bc3_matches_independently_compressed_rows_and_planes() {
        const WIDTH: u32 = 9;
        const HEIGHT: u32 = 7;
        const DEPTH: u32 = 2;

        let mut rgba = vec![0u8; (WIDTH * HEIGHT * DEPTH * 4) as usize];
        for (index, byte) in rgba.iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(37).wrapping_add(11);
        }

        let blocks_x = divide_up(u64::from(WIDTH), 4) as usize;
        let blocks_y = divide_up(u64::from(HEIGHT), 4) as usize;
        let row_bytes = blocks_x * 16;
        let plane_bytes = row_bytes * blocks_y;
        let mut threaded = vec![0u8; plane_bytes * DEPTH as usize];
        compress_bc3(&rgba, WIDTH, HEIGHT, DEPTH, &mut threaded);

        let mut independently_compressed = vec![0u8; threaded.len()];
        let source_row_bytes = WIDTH as usize * 4;
        let source_plane_bytes = source_row_bytes * HEIGHT as usize;
        for z in 0..DEPTH as usize {
            for block_y in 0..blocks_y {
                let y = block_y * 4;
                let row_height = (HEIGHT as usize - y).min(4);
                let source_offset = z * source_plane_bytes + y * source_row_bytes;
                let source_len = row_height * source_row_bytes;
                let output_offset = z * plane_bytes + block_y * row_bytes;
                compress_bc3(
                    &rgba[source_offset..source_offset + source_len],
                    WIDTH,
                    row_height as u32,
                    1,
                    &mut independently_compressed[output_offset..output_offset + row_bytes],
                );
            }
        }

        assert_eq!(threaded, independently_compressed);
    }

    #[test]
    fn bc7_produces_nonzero_blocks_of_sixteen_bytes() {
        let mut rgba = vec![0u8; 8 * 8 * 4];
        for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&[index as u8, 255 - index as u8, 64, 255]);
        }
        let mut output = [0u8; 4 * 16];
        compress_bc7(&rgba, 8, 8, 1, &mut output);
        for block in output.chunks_exact(16) {
            assert_ne!(block, [0u8; 16]);
        }
        // Deterministic for identical input and parameters.
        let mut second = [0u8; 4 * 16];
        compress_bc7(&rgba, 8, 8, 1, &mut second);
        assert_eq!(output, second);
    }

    /// Reference BC7 mode 6 decoder for validating the analytical encoder.
    fn decode_bc7_mode6(block: &[u8; 16]) -> [[u8; 4]; 16] {
        const WEIGHTS4: [u32; 16] =
            [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];
        let lo = u64::from_le_bytes(block[0..8].try_into().unwrap());
        let hi = u64::from_le_bytes(block[8..16].try_into().unwrap());
        let get = |start: u32, count: u32| -> u32 {
            let mut v = 0u32;
            for i in 0..count {
                let bit = start + i;
                let b = if bit < 64 { (lo >> bit) & 1 } else { (hi >> (bit - 64)) & 1 };
                v |= (b as u32) << i;
            }
            v
        };
        assert_eq!(get(0, 7), 1 << 6, "mode 6 tag");
        let mut e = [[0u32; 2]; 4];
        for c in 0..4 {
            e[c][0] = get(7 + c as u32 * 14, 7);
            e[c][1] = get(14 + c as u32 * 14, 7);
        }
        let p0 = get(63, 1);
        let p1 = get(64, 1);
        let mut out = [[0u8; 4]; 16];
        for i in 0..16 {
            let idx = if i == 0 { get(65, 3) } else { get(68 + (i as u32 - 1) * 4, 4) };
            let w = WEIGHTS4[idx as usize];
            for c in 0..4 {
                let a = (e[c][0] << 1) | p0;
                let b = (e[c][1] << 1) | p1;
                out[i][c] = (((64 - w) * a + w * b + 32) >> 6) as u8;
            }
        }
        out
    }

    #[test]
    fn bc7_analytical_mode6_decodes_close_to_source() {
        // Gradient block: the analytical encoder must reproduce it closely.
        let mut rgba = [0u8; 64];
        for i in 0..16 {
            let v = (i * 16) as u8;
            rgba[i * 4..i * 4 + 4].copy_from_slice(&[v, 255 - v, 128, 255]);
        }
        let mut block = [0u8; 16];
        unsafe { ruzu_bc7enc_compress_block(block.as_mut_ptr(), rgba.as_ptr()) };
        let decoded = decode_bc7_mode6(&block);
        let mut total_err = 0u32;
        for i in 0..16 {
            for c in 0..4 {
                total_err += (decoded[i][c] as i32 - rgba[i * 4 + c] as i32).unsigned_abs();
            }
        }
        let mean_err = total_err as f32 / 64.0;
        assert!(mean_err < 6.0, "mean abs error {mean_err} too high");

        // Constant block must decode essentially exactly.
        let flat = [[17u8, 200, 40, 255]; 16];
        let flat_bytes: Vec<u8> = flat.iter().flatten().copied().collect();
        unsafe { ruzu_bc7enc_compress_block(block.as_mut_ptr(), flat_bytes.as_ptr()) };
        let decoded = decode_bc7_mode6(&block);
        for texel in decoded {
            for c in 0..4 {
                assert!((texel[c] as i32 - flat[0][c] as i32).abs() <= 2);
            }
        }
    }

    #[test]
    fn bc1_alpha_threshold_matches_eden_boundary() {
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[9, 31, 127, 128]);
        }
        let mut opaque = [0u8; 8];
        compress_bc1(&rgba, 4, 4, 1, &mut opaque);

        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = 127;
        }
        let mut transparent = [0u8; 8];
        compress_bc1(&rgba, 4, 4, 1, &mut transparent);

        assert_ne!(opaque, transparent);
    }
}
