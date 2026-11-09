//! Reproducible CPU-only BC3 microbenchmark; not an emulator FPS benchmark.
use std::{
    hint::black_box,
    time::{Duration, Instant},
};
use video_core::textures::bcn::compress_bc3;

fn main() {
    for side in [16u32, 64, 256, 1024] {
        let mut input = vec![0u8; (side * side * 4) as usize];
        let mut state = 0x12345678u32;
        for byte in &mut input {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state as u8;
        }
        let mut output = vec![0u8; (side * side) as usize];
        compress_bc3(&input, side, side, 1, &mut output);
        let digest = output.iter().fold(0xcbf29ce484222325u64, |hash, &b| {
            (hash ^ u64::from(b)).wrapping_mul(0x100000001b3)
        });
        for trial in 0..5 {
            let start = Instant::now();
            let mut count = 0u32;
            while start.elapsed() < Duration::from_millis(300) {
                compress_bc3(black_box(&input), side, side, 1, black_box(&mut output));
                black_box(&output);
                count += 1;
            }
            println!(
                "side={side} trial={trial} us={:.3} digest={digest:016x}",
                start.elapsed().as_secs_f64() * 1e6 / f64::from(count)
            );
        }
    }
}
