// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Lichii `src/video_core/engines/native_upscale.h` (commit aae0b063f7, #721).
//!
//! Helpers that find which images of a native compute program form an upscale: an image it reads
//! (or a retained input) and an image it stores that upscales it. The pair predicate and the image
//! reader are supplied by the caller (`KeplerCompute::dispatch_native_compute`), as in upstream's
//! templates.

use crate::textures::texture::TicEntry;

/// Port of `FindNativeUpscaleOutput`: the first stored image (cbuf0 byte offset) that upscales the
/// retained `input`, with formats allowed to differ.
pub fn find_native_upscale_output<Image, ReadImage, IsUpscalePair>(
    input: &Option<Image>,
    stores: &[u32],
    mut image: ReadImage,
    mut upscale_pair: IsUpscalePair,
) -> Option<u32>
where
    ReadImage: FnMut(u32) -> Option<Image>,
    IsUpscalePair: FnMut(&Option<Image>, &Option<Image>, bool) -> bool,
{
    if input.is_none() {
        return None;
    }
    for &store in stores {
        if upscale_pair(input, &image(store), false) {
            return Some(store);
        }
    }
    None
}

/// Port of `FindNativeUpscaleSlots`: for the first stored image that upscales one of the read
/// images, the (read, store) cbuf0 byte offsets, preferring a read image of the same format and,
/// among equals, the largest one.
///
/// Upstream's template reads `from->Width()`/`from->Height()` and is only meant for `TICEntry`;
/// the Rust port names that type. Upstream has no caller of this template at aae0b063f7 either.
#[allow(dead_code)]
pub fn find_native_upscale_slots<ReadImage, IsUpscalePair>(
    reads: &[u32],
    stores: &[u32],
    mut image: ReadImage,
    mut upscale_pair: IsUpscalePair,
) -> Option<(u32, u32)>
where
    ReadImage: FnMut(u32) -> Option<TicEntry>,
    IsUpscalePair: FnMut(&Option<TicEntry>, &Option<TicEntry>, bool) -> bool,
{
    for &store in stores {
        let to = image(store);
        for same_format in [true, false] {
            let mut selected: Option<(u32, u32)> = None;
            let mut area = 0u64;
            for &read in reads {
                let from = image(read);
                if upscale_pair(&from, &to, same_format) {
                    let from = from.as_ref().unwrap();
                    let from_area = u64::from(from.width()) * u64::from(from.height());
                    if selected.is_none() || from_area > area {
                        selected = Some((read, store));
                        area = from_area;
                    }
                }
            }
            if selected.is_some() {
                return selected;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (width, height) stand-in for a TIC entry.
    type Image = (u64, u64);

    #[test]
    fn output_requires_a_retained_input() {
        let mut reads = 0;
        let found = find_native_upscale_output::<Image, _, _>(
            &None,
            &[0x60],
            |_| {
                reads += 1;
                Some((1920, 1080))
            },
            |_, _, _| true,
        );
        assert_eq!(found, None);
        assert_eq!(reads, 0, "no store is read without an input");
    }

    #[test]
    fn output_is_the_first_store_accepted_without_same_format() {
        let images = |offset: u32| match offset {
            0x60 => Some((640, 360)),
            0x68 => Some((1920, 1080)),
            0x70 => Some((3840, 2160)),
            _ => None,
        };
        let mut formats_checked = Vec::new();
        let found = find_native_upscale_output(
            &Some((960u64, 540u64)),
            &[0x50, 0x60, 0x68, 0x70],
            images,
            |from: &Option<Image>, to: &Option<Image>, same_format| {
                formats_checked.push(same_format);
                matches!((from, to), (Some(f), Some(t)) if t.0 > f.0)
            },
        );
        assert_eq!(found, Some(0x68));
        assert!(formats_checked.iter().all(|&same| !same));
        assert_eq!(formats_checked.len(), 3, "stops at the first match");
    }

    fn tic(width: u32, height: u32, format: u32) -> TicEntry {
        let mut words = [0u32; 8];
        words[0] = format;
        words[4] = width - 1;
        words[5] = height - 1;
        unsafe { std::mem::transmute::<[u32; 8], TicEntry>(words) }
    }

    #[test]
    fn slots_prefer_same_format_then_the_largest_read() {
        let images = |offset: u32| match offset {
            0x10 => Some(tic(800, 450, 0x21)),
            0x14 => Some(tic(960, 540, 0x08)),
            0x18 => Some(tic(640, 360, 0x21)),
            0x20 => Some(tic(1920, 1080, 0x21)),
            _ => None,
        };
        let pair = |from: &Option<TicEntry>, to: &Option<TicEntry>, same_format: bool| {
            let (Some(from), Some(to)) = (from, to) else {
                return false;
            };
            (!same_format || from.format() == to.format()) && to.width() > from.width()
        };
        assert_eq!(
            find_native_upscale_slots(&[0x18, 0x14, 0x10], &[0x20], images, pair),
            Some((0x10, 0x20))
        );
        // No same-format read: the largest read of any format.
        assert_eq!(
            find_native_upscale_slots(&[0x14], &[0x20], images, pair),
            Some((0x14, 0x20))
        );
        assert_eq!(find_native_upscale_slots(&[0x14], &[0x30], images, pair), None);
    }
}
