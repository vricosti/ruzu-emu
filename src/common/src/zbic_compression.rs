//! Port of Eden common/zbic_compression.h/.cpp.
//! Uses the same pinned modified zstd codec, with private native symbols.

use std::ffi::c_void;

extern "C" {
    fn ruzu_zbic_decompress(dst: *mut c_void, dst_size: usize, src: *const c_void, src_size: usize) -> i32;
}

/// Upstream IsZBIC: frame signature only, not a full validity check.
pub fn is_zbic(src: &[u8]) -> bool {
    src.starts_with(b"ZBIC")
}

/// Upstream DecompressDataZBIC: decoded byte count, or -1 for invalid input.
pub fn decompress_data_zbic(dst: &mut [u8], src: &[u8]) -> i32 {
    if dst.is_empty() || src.is_empty() {
        return -1;
    }
    // Both slice allocations remain live for the call; the codec respects the
    // supplied destination capacity and does not retain either pointer.
    unsafe { ruzu_zbic_decompress(dst.as_mut_ptr().cast(), dst.len(), src.as_ptr().cast(), src.len()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" {
        fn ruzu_zbic_compress_bound(size: usize) -> usize;
        fn ruzu_zbic_compress(dst: *mut c_void, dst_size: usize, src: *const c_void, src_size: usize) -> usize;
    }

    #[test]
    fn signature_and_error_contract() {
        for input in [b"".as_slice(), b"Z", b"ZBI", b"zBIC", &[0x28, 0xb5, 0x2f, 0xfd]] {
            assert!(!is_zbic(input));
        }
        assert!(is_zbic(b"ZBIC"));
        assert!(is_zbic(b"ZBICpayload"));
        assert_eq!(decompress_data_zbic(&mut [], b"ZBIC"), -1);
        assert_eq!(decompress_data_zbic(&mut [0; 32], &[]), -1);
        assert_eq!(decompress_data_zbic(&mut [0; 32], b"ZBICbad"), -1);
    }

    #[test]
    fn zbic_raw_frame_golden() {
        // Standard raw-block framing with ZBIC magic: single segment, size5,
        // final raw block (5<<3)|1, then literal bytes. No encoder dependency.
        let frame = b"ZBIC\x20\x05\x29\x00\x00hello";
        let mut output = [0xa5; 5];
        assert_eq!(decompress_data_zbic(&mut output, frame), 5);
        assert_eq!(&output, b"hello");
        assert_eq!(decompress_data_zbic(&mut [0; 4], frame), -1);
        for end in 1..frame.len() {
            assert_eq!(decompress_data_zbic(&mut output, &frame[..end]), -1);
        }
    }

    #[test]
    fn bic_entropy_roundtrip_coexists_with_standard_zstd() {
        let source: Vec<u8> = (0..65536usize)
            .map(|i| if i % 17 == 0 { (i / 17) as u8 } else { (i % 7) as u8 }).collect();
        let mut compressed = vec![0; unsafe { ruzu_zbic_compress_bound(source.len()) }];
        let size = unsafe { ruzu_zbic_compress(compressed.as_mut_ptr().cast(), compressed.len(),
            source.as_ptr().cast(), source.len()) };
        assert!(size > 0 && size < source.len());
        compressed.truncate(size);
        assert!(is_zbic(&compressed));
        let mut output = vec![0; source.len()];
        assert_eq!(decompress_data_zbic(&mut output, &compressed), source.len() as i32);
        assert_eq!(output, source);
        let standard = zstd::encode_all(source.as_slice(), 3).unwrap();
        assert!(!is_zbic(&standard));
        assert_eq!(zstd::decode_all(standard.as_slice()).unwrap(), source);
        assert_eq!(decompress_data_zbic(&mut output, &standard), -1);
        assert!(zstd::decode_all(compressed.as_slice()).is_err());
        let mut renamed = compressed.clone();
        renamed[..4].copy_from_slice(&[0x28, 0xb5, 0x2f, 0xfd]);
        assert!(zstd::decode_all(renamed.as_slice()).is_err(),
            "fixture must exercise BIC tables, not just a different magic");
    }
}
