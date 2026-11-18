//! VectorTableLookup64 / VectorTableLookup128 (Eden emit_x64_vector.cpp
//! EmitVectorTableLookup64/128) against the A64 TBL/TBX definition, which
//! the legacy rdynarmic fallback implemented (`fallback_vector_table_lookup`,
//! its loop is reproduced in `reference`).

use super::*;

const V0_INITIAL: Vector = [0xdead_beef_dead_beef, 0x0123_4567_89ab_cdef];

/// TBL/TBX Vd.<8B|16B>, {V1.16B .. V<len>.16B}, V<len+1>.<8B|16B>.
fn tbl_insn(q: bool, len: u32, tbx: bool) -> u32 {
    let rm = len + 1;
    0x0E00_0000
        | ((q as u32) << 30)
        | (rm << 16)
        | ((len - 1) << 13)
        | ((tbx as u32) << 12)
        | (1 << 5)
}

/// Legacy fallback loop (`fallback_vector_table_lookup`), then the A64
/// frontend's VectorZeroUpper for the 64-bit form.
fn reference(q: bool, tbx: bool, tables: &[Vector], indices: Vector, v0: Vector) -> Vector {
    let lane_count = if q { 16 } else { 8 };
    let table_entry_size = 16;
    let bytes = |v: Vector| -> [u8; 16] { vec_to_bytes(&v) };
    let table: Vec<[u8; 16]> = tables.iter().map(|&t| bytes(t)).collect();
    let idx = bytes(indices);
    let mut result = if tbx { bytes(v0) } else { [0u8; 16] };
    for i in 0..lane_count {
        let index = idx[i] as usize;
        let table_index = index / table_entry_size;
        let element_index = index % table_entry_size;
        if table_index < table.len() {
            result[i] = table[table_index][element_index];
        }
    }
    let mut out = bytes_to_vec(&result);
    if !q {
        out[1] = 0;
    }
    out
}

fn index_vector(rng: &mut Rng) -> Vector {
    let mut b = [0u8; 16];
    for byte in b.iter_mut() {
        let r = rng.next();
        *byte = match r % 8 {
            0 => (r >> 8) as u8, // any value, mostly out of range
            1 => [63, 64, 47, 48, 31, 32, 15, 16, 127, 128, 255, 0][(r >> 8) as usize % 12],
            _ => ((r >> 8) % 72) as u8, // in range for up to 4 tables
        };
    }
    bytes_to_vec(&b)
}

fn run_tbl(q: bool) {
    let iters = iterations().max(1);
    let mut checked = 0usize;
    for (features_name, features) in feature_sets() {
        for len in 1..=4u32 {
            for tbx in [false, true] {
                let insn = tbl_insn(q, len, tbx);
                let mut harness = Harness::new(&[insn, 0xD400_0001], features);
                let mut rng = Rng::new(0x5151_7272 ^ insn as u64);
                for _ in 0..iters / 4 {
                    let tables: Vec<Vector> = (0..len).map(|_| [rng.next(), rng.next()]).collect();
                    let indices = index_vector(&mut rng);
                    let v0 = [rng.next(), rng.next()];
                    let mut inputs = tables.clone();
                    inputs.push(indices);
                    let (got, _) = harness.run(0, v0, &inputs);
                    let want = reference(q, tbx, &tables, indices, v0);
                    assert_eq!(
                        got, want,
                        "TBL q={q} len={len} tbx={tbx} [{features_name}] tables={tables:x?} idx={indices:x?} v0={v0:x?}"
                    );
                    checked += 1;
                }
                // The harness' fixed V0 as defaults, all index bytes 0..=255.
                for start in (0..=255u32).step_by(16) {
                    let mut b = [0u8; 16];
                    for (i, byte) in b.iter_mut().enumerate() {
                        *byte = (start as usize + i) as u8;
                    }
                    let indices = bytes_to_vec(&b);
                    let tables: Vec<Vector> = (0..len).map(|_| [rng.next(), rng.next()]).collect();
                    let mut inputs = tables.clone();
                    inputs.push(indices);
                    let (got, _) = harness.run(0, V0_INITIAL, &inputs);
                    assert_eq!(got, reference(q, tbx, &tables, indices, V0_INITIAL));
                    checked += 1;
                }
            }
        }
    }
    eprintln!(
        "VectorTableLookup128 (TBL/TBX {}): {checked} vectors checked",
        if q { "16B" } else { "8B" }
    );
}

// A64 TBL/TBX always use VectorTableLookup128 (128-bit table registers);
// the 8B form only adds VectorZeroUpper.
#[test]
fn vector_table_lookup128_tbl_8b_matches_reference() {
    run_tbl(false);
}

#[test]
fn vector_table_lookup128_tbl_16b_matches_reference() {
    run_tbl(true);
}

// ---------------------------------------------------------------------------
// VectorTableLookup64: only produced by A32 VTBL/VTBX (64-bit D-register
// tables; VTBL passes an immediate-zero default, exercising is_defaults_zero).
// ---------------------------------------------------------------------------

mod a32 {
    use super::*;
    use crate::interface::a32::config::{
        Exception as A32Exception, UserCallbacks as A32UserCallbacks, UserConfig as A32UserConfig,
    };
    use crate::A32Jit;

    struct A32Env {
        code: Vec<u32>,
        halt_reason: Option<usize>,
    }

    impl A32UserCallbacks for A32Env {
        fn memory_read_code(&self, vaddr: u32) -> Option<u32> {
            Some(*self.code.get(vaddr as usize / 4).unwrap_or(&0xEF00_0000))
        }
        fn memory_read_8(&self, _vaddr: u32) -> u8 {
            0
        }
        fn memory_read_16(&self, _vaddr: u32) -> u16 {
            0
        }
        fn memory_read_32(&self, vaddr: u32) -> u32 {
            self.memory_read_code(vaddr).unwrap_or(0)
        }
        fn memory_read_64(&self, _vaddr: u32) -> u64 {
            0
        }
        fn memory_write_8(&mut self, _vaddr: u32, _value: u8) {}
        fn memory_write_16(&mut self, _vaddr: u32, _value: u16) {}
        fn memory_write_32(&mut self, _vaddr: u32, _value: u32) {}
        fn memory_write_64(&mut self, _vaddr: u32, _value: u64) {}
        fn call_svc(&mut self, _swi: u32) {
            let ptr = self.halt_reason.expect("halt reason pointer registered");
            unsafe {
                (*(ptr as *const std::sync::atomic::AtomicU32))
                    .fetch_or(HaltReason::SVC.bits(), std::sync::atomic::Ordering::SeqCst);
            }
        }
        fn exception_raised(&mut self, pc: u32, exception: A32Exception) {
            panic!("unexpected exception {exception:?} at 0x{pc:x}");
        }
        fn add_ticks(&mut self, _ticks: u64) {}
        fn get_ticks_remaining(&self) -> u64 {
            1000
        }
        fn set_halt_reason_ptr(&mut self, ptr: *const u32) {
            self.halt_reason = Some(ptr as usize);
        }
    }

    /// VTBL/VTBX.8 D0, {D2 .. D(1+len)}, D6.
    fn vtbl_insn(len: u32, tbx: bool) -> u32 {
        0xF3B0_0800 | (2 << 16) | ((len - 1) << 8) | ((tbx as u32) << 6) | 6
    }

    fn reference64(tbx: bool, tables: &[u64], indices: u64, d0: u64) -> u64 {
        let mut result = if tbx { d0.to_le_bytes() } else { [0u8; 8] };
        let idx = indices.to_le_bytes();
        for i in 0..8 {
            let table_index = idx[i] as usize / 8;
            let elem = idx[i] as usize % 8;
            if table_index < tables.len() {
                result[i] = tables[table_index].to_le_bytes()[elem];
            }
        }
        u64::from_le_bytes(result)
    }

    #[test]
    fn vector_table_lookup64_vtbl_matches_reference() {
        let iters = iterations().max(1);
        let mut checked = 0usize;
        for (features_name, features) in feature_sets() {
            for len in 1..=4u32 {
                for tbx in [false, true] {
                    let insn = vtbl_insn(len, tbx);
                    let mut config = A32UserConfig::new(Box::new(A32Env {
                        code: vec![insn, 0xEF00_0000],
                        halt_reason: None,
                    }));
                    config.enable_cycle_counting = false;
                    config.code_cache_size = 64 * 1024 * 1024;
                    test_host_feature_mask::set(features);
                    let mut jit = A32Jit::new(config).unwrap();
                    test_host_feature_mask::set(HostFeature::all());
                    let mut rng = Rng::new(0x7ab1_e64 ^ insn as u64);
                    for n in 0..iters / 4 + 16 {
                        let tables: Vec<u64> = (0..len).map(|_| rng.next()).collect();
                        let indices = if n < 16 {
                            u64::from_le_bytes(std::array::from_fn(|i| (n * 8 + i) as u8 * 2))
                        } else {
                            index_vector(&mut rng)[0] & 0x3f3f_3f3f_3f3f_3f3f
                                ^ (rng.next() & 0x8080_0000_0040_0000)
                        };
                        let d0 = rng.next();
                        jit.regs_mut()[15] = 0;
                        let ext = jit.ext_regs_mut();
                        ext[0] = d0 as u32;
                        ext[1] = (d0 >> 32) as u32;
                        for (i, t) in tables.iter().enumerate() {
                            ext[4 + 2 * i] = *t as u32;
                            ext[5 + 2 * i] = (*t >> 32) as u32;
                        }
                        ext[12] = indices as u32;
                        ext[13] = (indices >> 32) as u32;
                        let halt = jit.run();
                        assert!(halt.contains(HaltReason::SVC), "halt={halt:?}");
                        let ext = jit.ext_regs();
                        let got = ext[0] as u64 | (ext[1] as u64) << 32;
                        let want = reference64(tbx, &tables, indices, d0);
                        assert_eq!(
                            got, want,
                            "VTBL len={len} tbx={tbx} [{features_name}] tables={tables:x?} idx={indices:#x} d0={d0:#x}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        eprintln!("VectorTableLookup64 (A32 VTBL/VTBX): {checked} vectors checked");
    }
}
