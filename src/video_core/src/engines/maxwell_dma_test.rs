// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::engines::draw_manager::Maxwell3DClearView;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::rasterizer_interface::RasterizerDownloadArea;

#[test]
fn register_count_matches_eden_maxwell_dma() {
    let engine = MaxwellDMA::default();
    assert_eq!(NUM_REGS, 0x800);
    assert_eq!(engine.regs.len(), NUM_REGS);
}

#[derive(Default)]
struct RasterizerCalls {
    flushes: Vec<(u64, u64)>,
    invalidations: Vec<(u64, u64)>,
    queries: Vec<(u64, u32, QueryPropertiesFlags, u32, u32)>,
    dma_buffer_copies: Vec<(u64, u64, u64)>,
    dma_buffer_clears: Vec<(u64, u64, u32)>,
    dma_image_to_buffers: Vec<(dma::ImageCopy, dma::ImageOperand, dma::BufferOperand)>,
    dma_buffer_to_images: Vec<(dma::ImageCopy, dma::BufferOperand, dma::ImageOperand)>,
    query_observed_memory: Vec<Vec<u8>>,
}

struct TestRasterizer {
    calls: Arc<Mutex<RasterizerCalls>>,
    accelerate_buffer_copy: bool,
    accelerate_buffer_clear: bool,
    accelerate_image_to_buffer: bool,
    accelerate_buffer_to_image: bool,
    query_observer: Option<(Arc<Mutex<Vec<u8>>>, std::ops::Range<usize>)>,
}

impl TestRasterizer {
    fn new(calls: Arc<Mutex<RasterizerCalls>>) -> Self {
        Self {
            calls,
            accelerate_buffer_copy: false,
            accelerate_buffer_clear: false,
            accelerate_image_to_buffer: false,
            accelerate_buffer_to_image: false,
            query_observer: None,
        }
    }

    fn observe_memory_at_query(
        mut self,
        memory: Arc<Mutex<Vec<u8>>>,
        range: std::ops::Range<usize>,
    ) -> Self {
        self.query_observer = Some((memory, range));
        self
    }
}

impl AccelerateDMAInterface for TestRasterizer {
    fn buffer_copy(&mut self, src_address: u64, dest_address: u64, amount: u64) -> bool {
        self.calls
            .lock()
            .dma_buffer_copies
            .push((src_address, dest_address, amount));
        self.accelerate_buffer_copy
    }

    fn buffer_clear(&mut self, dst_address: u64, amount: u64, value: u32) -> bool {
        self.calls
            .lock()
            .dma_buffer_clears
            .push((dst_address, amount, value));
        self.accelerate_buffer_clear
    }

    fn image_to_buffer(
        &mut self,
        copy_info: &dma::ImageCopy,
        src: &dma::ImageOperand,
        dst: &dma::BufferOperand,
    ) -> bool {
        self.calls
            .lock()
            .dma_image_to_buffers
            .push((*copy_info, *src, *dst));
        self.accelerate_image_to_buffer
    }

    fn buffer_to_image(
        &mut self,
        copy_info: &dma::ImageCopy,
        src: &dma::BufferOperand,
        dst: &dma::ImageOperand,
    ) -> bool {
        self.calls
            .lock()
            .dma_buffer_to_images
            .push((*copy_info, *src, *dst));
        self.accelerate_buffer_to_image
    }
}

impl RasterizerInterface for TestRasterizer {
    fn draw(
        &mut self,
        _draw_view: crate::engines::draw_manager::Maxwell3DDrawView<'_>,
        _instance_count: u32,
    ) {
    }
    fn draw_texture(
        &mut self,
        _draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
    ) {
    }
    fn clear(&mut self, _clear_view: Maxwell3DClearView<'_>, _layer_count: u32) {}
    fn dispatch_compute(&mut self, _dispatch: &crate::engines::kepler_compute::DispatchCall) {}
    fn reset_counter(&mut self, _query_type: u32) {}
    fn query(
        &mut self,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
        subreport: u32,
    ) {
        if let Some((memory, range)) = &self.query_observer {
            self.calls
                .lock()
                .query_observed_memory
                .push(memory.lock()[range.clone()].to_vec());
        }
        self.calls
            .lock()
            .queries
            .push((gpu_addr, query_type, flags, payload, subreport));
    }
    fn bind_graphics_uniform_buffer(
        &mut self,
        _stage: usize,
        _index: u32,
        _gpu_addr: u64,
        _size: u32,
    ) {
    }
    fn disable_graphics_uniform_buffer(&mut self, _stage: usize, _index: u32) {}
    fn signal_fence(&mut self, _func: Box<dyn FnOnce() + Send>) {}
    fn sync_operation(&mut self, _func: Box<dyn FnOnce() + Send>) {}
    fn signal_sync_point(&mut self, _value: u32) {}
    fn signal_reference(&mut self) {}
    fn release_fences(&mut self, _force: bool) {}
    fn flush_all(&mut self) {}
    fn flush_region(&mut self, addr: u64, size: u64, _which: crate::cache_types::CacheType) {
        self.calls.lock().flushes.push((addr, size));
    }
    fn must_flush_region(
        &self,
        _addr: u64,
        _size: u64,
        _which: crate::cache_types::CacheType,
    ) -> bool {
        false
    }
    fn get_flush_area(&self, addr: u64, size: u64) -> RasterizerDownloadArea {
        RasterizerDownloadArea {
            start_address: addr,
            end_address: addr + size,
            preemtive: false,
        }
    }
    fn invalidate_region(&mut self, addr: u64, size: u64, _which: crate::cache_types::CacheType) {
        self.calls.lock().invalidations.push((addr, size));
    }
    fn on_cache_invalidation(&mut self, _addr: u64, _size: u64) {}
    fn on_cpu_write(&mut self, _addr: u64, _size: u64) -> bool {
        false
    }
    fn invalidate_gpu_cache(&mut self) {}
    fn unmap_memory(&mut self, _addr: u64, _size: u64) {}
    fn modify_gpu_memory(&mut self, _as_id: usize, _addr: u64, _size: u64) {}
    fn flush_and_invalidate_region(
        &mut self,
        _addr: u64,
        _size: u64,
        _which: crate::cache_types::CacheType,
    ) {
    }
    fn wait_for_idle(&mut self) {}
    fn fragment_barrier(&mut self) {}
    fn tiled_cache_barrier(&mut self) {}
    fn flush_commands(&mut self) {}
    fn tick_frame(&mut self) {}
    fn accelerate_inline_to_memory(&mut self, _address: u64, _copy_size: usize, _memory: &[u8]) {}
    fn access_accelerate_dma(&mut self) -> &mut dyn AccelerateDMAInterface {
        self
    }
}

fn new_test_engine() -> MaxwellDMA {
    MaxwellDMA::new(Arc::new(Mutex::new(MemoryManager::new(0))))
}

fn bind_memory_rasterizer(
    eng: &mut MaxwellDMA,
    rasterizer: &dyn RasterizerInterface,
    mappings: &[(u64, u64)],
) {
    eng.bind_rasterizer(rasterizer);
    let mut memory_manager = eng.memory_manager.lock();
    memory_manager.bind_rasterizer(rasterizer);
    for &(gpu_addr, device_addr) in mappings {
        memory_manager.map(
            gpu_addr,
            device_addr,
            0x1000,
            PteKind::PITCH.raw() as u32,
            false,
        );
    }
}

fn write_dma_params(eng: &mut MaxwellDMA, base: u32, params: dma::Parameters) {
    eng.write_reg(base, params.block_size.raw);
    eng.write_reg(base + 1, params.width);
    eng.write_reg(base + 2, params.height);
    eng.write_reg(base + 3, params.depth);
    eng.write_reg(base + 4, params.layer);
    eng.write_reg(base + 5, params.origin.raw);
}

const MULTI_LINE_PITCH_TO_PITCH_LAUNCH: u32 = LAUNCH_DATA_TRANSFER_NON_PIPELINED
    | LAUNCH_SRC_MEMORY_LAYOUT_PITCH
    | LAUNCH_DST_MEMORY_LAYOUT_PITCH
    | LAUNCH_MULTI_LINE_ENABLE;
const MULTI_LINE_BLOCKLINEAR_TO_PITCH_LAUNCH: u32 =
    LAUNCH_DATA_TRANSFER_NON_PIPELINED | LAUNCH_DST_MEMORY_LAYOUT_PITCH | LAUNCH_MULTI_LINE_ENABLE;
const MULTI_LINE_PITCH_TO_BLOCKLINEAR_LAUNCH: u32 =
    LAUNCH_DATA_TRANSFER_NON_PIPELINED | LAUNCH_SRC_MEMORY_LAYOUT_PITCH | LAUNCH_MULTI_LINE_ENABLE;
const MULTI_LINE_BLOCKLINEAR_TO_BLOCKLINEAR_LAUNCH: u32 =
    LAUNCH_DATA_TRANSFER_NON_PIPELINED | LAUNCH_MULTI_LINE_ENABLE;
const SINGLE_LINE_LAUNCH: u32 = LAUNCH_DATA_TRANSFER_NON_PIPELINED;
const RELEASE_ONE_WORD_SEMAPHORE_LAUNCH: u32 =
    MULTI_LINE_PITCH_TO_PITCH_LAUNCH | (LAUNCH_SEMAPHORE_TYPE_RELEASE_ONE_WORD << 3);
const RELEASE_FOUR_WORD_SEMAPHORE_LAUNCH: u32 =
    MULTI_LINE_PITCH_TO_PITCH_LAUNCH | (LAUNCH_SEMAPHORE_TYPE_RELEASE_FOUR_WORD << 3);

#[test]
fn test_address_accessors() {
    let mut eng = new_test_engine();
    eng.write_reg(SRC_ADDR_HIGH, 0xAB);
    eng.write_reg(SRC_ADDR_LOW, 0xCDEF_0000);
    assert_eq!(eng.src_addr(), 0xAB_CDEF_0000);

    eng.write_reg(DST_ADDR_HIGH, 0x12);
    eng.write_reg(DST_ADDR_LOW, 0x3456_7890);
    assert_eq!(eng.dst_addr(), 0x12_3456_7890);
}

#[test]
fn test_pitch_and_size_accessors() {
    let mut eng = new_test_engine();
    eng.write_reg(PITCH_IN, 5120);
    eng.write_reg(PITCH_OUT, 5120);
    eng.write_reg(LINE_LENGTH, 5120);
    eng.write_reg(LINE_COUNT, 720);
    assert_eq!(eng.pitch_in(), 5120);
    assert_eq!(eng.pitch_out(), 5120);
    assert_eq!(eng.line_length(), 5120);
    assert_eq!(eng.line_count(), 720);
}

#[test]
fn test_launch_trigger_sets_pending() {
    let mut eng = new_test_engine();
    assert!(!eng.pending_launch);

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x2000);
    eng.write_reg(PITCH_IN, 5120);
    eng.write_reg(PITCH_OUT, 5120);
    eng.write_reg(LINE_LENGTH, 5120);
    eng.write_reg(LINE_COUNT, 720);

    // Trigger DMA launch
    eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH);
    assert!(eng.pending_launch);
    assert_eq!(
        eng.launch_data_transfer_type(),
        LAUNCH_DATA_TRANSFER_NON_PIPELINED
    );
}

#[test]
fn test_no_trigger_without_launch_method() {
    let mut eng = new_test_engine();
    eng.write_reg(0x200, 42); // Random register
    assert!(!eng.pending_launch);
}

#[test]
fn test_bind_rasterizer_stores_reference() {
    let syncpoints = std::sync::Arc::new(crate::host1x::syncpoint_manager::SyncpointManager::new());
    let rasterizer = crate::renderer_null::null_rasterizer::RasterizerNull::new(syncpoints);
    let mut eng = new_test_engine();
    assert!(eng.rasterizer.is_none());
    eng.bind_rasterizer(&rasterizer);
    assert!(eng.rasterizer.is_some());
}

#[test]
fn test_dma_copies_lines() {
    let mut eng = new_test_engine();

    // 2 lines of 8 bytes each, same pitch.
    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x2000);
    eng.write_reg(PITCH_IN, 8);
    eng.write_reg(PITCH_OUT, 8);
    eng.write_reg(LINE_LENGTH, 8);
    eng.write_reg(LINE_COUNT, 2);

    eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH);
    assert!(eng.pending_launch);

    // Source data.
    let src: Vec<u8> = (0..16).collect();

    let writes = eng.execute_pending(&|addr, buf| {
        if addr == 0x2000 {
            buf.fill(0);
            return;
        }
        let offset = (addr - 0x1000) as usize;
        let len = buf.len();
        buf.copy_from_slice(&src[offset..offset + len]);
    });

    assert!(!eng.pending_launch);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, 0x2000);
    assert_eq!(writes[0].data, src);
}

#[test]
fn test_dma_different_pitches() {
    let mut eng = new_test_engine();

    // Copy 4 bytes per line, 2 lines. pitch_in=8, pitch_out=16.
    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x2000);
    eng.write_reg(PITCH_IN, 8);
    eng.write_reg(PITCH_OUT, 16);
    eng.write_reg(LINE_LENGTH, 4);
    eng.write_reg(LINE_COUNT, 2);

    eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH);

    // Source memory: pitch_in=8 per line.
    let src = vec![
        1, 2, 3, 4, 0xAA, 0xBB, 0xCC, 0xDD, // line 0 (4 useful + 4 padding)
        5, 6, 7, 8, 0xEE, 0xFF, 0x11, 0x22, // line 1 (4 useful + 4 padding)
    ];

    let writes = eng.execute_pending(&|addr, buf| {
        if addr == 0x2000 {
            buf.fill(0x7E);
            return;
        }
        let offset = (addr - 0x1000) as usize;
        let len = buf.len();
        buf.copy_from_slice(&src[offset..offset + len]);
    });

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, 0x2000);
    let dst = &writes[0].data;
    assert_eq!(dst.len(), 20); // pitch_out * (line_count - 1) + line_length
                               // Line 0: 4 bytes copied + 12 untouched bytes.
    assert_eq!(&dst[0..4], &[1, 2, 3, 4]);
    assert_eq!(&dst[4..16], &[0x7E; 12]);
    // Line 1 starts at pitch_out and contributes only its copied bytes.
    assert_eq!(&dst[16..20], &[5, 6, 7, 8]);
}

#[test]
fn test_immediate_pitch_copy_executes_lines_sequentially() {
    let mut eng = new_test_engine();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(Arc::clone(&calls));
    bind_memory_rasterizer(
        &mut eng,
        &rasterizer,
        &[(0x1000, 0x1_1000), (0x8000, 0x1_8000)],
    );

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(PITCH_IN, 8);
    eng.write_reg(PITCH_OUT, 16);
    eng.write_reg(LINE_LENGTH, 4);
    eng.write_reg(LINE_COUNT, 2);

    eng.call_method(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH, true);

    let calls = calls.lock();
    assert_eq!(
        calls.flushes,
        vec![(0x1_1000, 4), (0x1_8000, 4), (0x1_1008, 4), (0x1_8010, 4),]
    );
    assert_eq!(calls.invalidations, vec![(0x1_8000, 4), (0x1_8010, 4)]);
}

#[test]
fn test_dma_pitch_to_pitch_preserves_overlapping_upstream_pitch() {
    let mut eng = new_test_engine();

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x2000);
    eng.write_reg(PITCH_IN, 2);
    eng.write_reg(PITCH_OUT, 2);
    eng.write_reg(LINE_LENGTH, 4);
    eng.write_reg(LINE_COUNT, 3);
    eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH);

    let src: Vec<u8> = (0x10..0x20).collect();
    let writes = eng.execute_pending(&|addr, buf| {
        if addr == 0x2000 {
            buf.fill(0);
            return;
        }
        let offset = (addr - 0x1000) as usize;
        buf.copy_from_slice(&src[offset..offset + buf.len()]);
    });

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, 0x2000);
    assert_eq!(
        writes[0].data,
        vec![0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]
    );
}

#[test]
fn test_single_line_pitch_copy_tries_accelerated_buffer_copy_before_fallback() {
    let mut eng = new_test_engine();
    {
        let mut mm = eng.memory_manager.lock();
        mm.map(0x1000, 0x1000, 0x1000, PteKind::PITCH.raw() as u32, false);
        mm.map(0x8000, 0x8000, 0x1000, PteKind::PITCH.raw() as u32, false);
    }
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
    rasterizer.accelerate_buffer_copy = true;
    eng.bind_rasterizer(&rasterizer);

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(LINE_LENGTH, 12);
    eng.write_reg(LINE_COUNT, 0);
    eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH);

    let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));

    assert!(writes.is_empty());
    let calls = calls.lock();
    assert_eq!(calls.dma_buffer_copies, vec![(0x1000, 0x8000, 12)]);
    assert!(calls.flushes.is_empty());
    assert!(calls.invalidations.is_empty());
}

#[test]
fn test_single_line_const_a_clear_calls_accelerated_buffer_clear_then_fallback() {
    let mut eng = new_test_engine();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
    rasterizer.accelerate_buffer_clear = true;
    bind_memory_rasterizer(&mut eng, &rasterizer, &[(0x8000, 0x1_8000)]);

    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(LINE_LENGTH, 4);
    eng.write_reg(LINE_COUNT, 0);
    eng.write_reg(REMAP_CONSTA_VALUE, 0x1122_3344);
    eng.write_reg(REMAP_COMPONENTS, REMAP_SWIZZLE_CONST_A | (3 << 16));
    eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH | LAUNCH_REMAP_ENABLE);

    let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, 0x8000);
    assert_eq!(
        writes[0].data,
        [
            0x44, 0x33, 0x22, 0x11, 0x44, 0x33, 0x22, 0x11, 0x44, 0x33, 0x22, 0x11, 0x44, 0x33,
            0x22, 0x11
        ]
    );
    let calls = calls.lock();
    assert_eq!(calls.dma_buffer_clears, vec![(0x8000, 4, 0x1122_3344)]);
    assert!(calls.invalidations.is_empty());
    assert_eq!(writes[0].mode, PendingWriteMode::Unsafe);
}

#[test]
fn test_multi_line_blocklinear_to_pitch_tries_image_to_buffer_before_fallback() {
    let mut eng = new_test_engine();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
    rasterizer.accelerate_image_to_buffer = true;
    eng.bind_rasterizer(&rasterizer);

    let params = dma::Parameters {
        block_size: dma::BlockSize { raw: 0 },
        width: 16,
        height: 4,
        depth: 1,
        layer: 0,
        origin: dma::Origin { raw: 0 },
    };
    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(PITCH_OUT, 16);
    eng.write_reg(LINE_LENGTH, 16);
    eng.write_reg(LINE_COUNT, 4);
    write_dma_params(&mut eng, SRC_PARAMS, params);
    eng.write_reg(LAUNCH_DMA, MULTI_LINE_BLOCKLINEAR_TO_PITCH_LAUNCH);

    let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));

    assert!(writes.is_empty());
    let calls = calls.lock();
    assert_eq!(calls.dma_image_to_buffers.len(), 1);
    let (copy, src, dst) = calls.dma_image_to_buffers[0];
    assert_eq!(
        copy,
        dma::ImageCopy {
            length_x: 16,
            length_y: 4
        }
    );
    assert_eq!(
        src,
        dma::ImageOperand {
            bytes_per_pixel: 1,
            params,
            address: 0x1000
        }
    );
    assert_eq!(
        dst,
        dma::BufferOperand {
            pitch: 16,
            width: 16,
            height: 4,
            address: 0x8000
        }
    );
}

#[test]
fn test_multi_line_pitch_to_blocklinear_tries_buffer_to_image_before_fallback() {
    let mut eng = new_test_engine();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
    rasterizer.accelerate_buffer_to_image = true;
    eng.bind_rasterizer(&rasterizer);

    let params = dma::Parameters {
        block_size: dma::BlockSize { raw: 0 },
        width: 16,
        height: 4,
        depth: 1,
        layer: 0,
        origin: dma::Origin { raw: 0 },
    };
    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(PITCH_IN, 16);
    eng.write_reg(LINE_LENGTH, 16);
    eng.write_reg(LINE_COUNT, 4);
    write_dma_params(&mut eng, DST_PARAMS, params);
    eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_BLOCKLINEAR_LAUNCH);

    let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));

    assert!(writes.is_empty());
    let calls = calls.lock();
    assert_eq!(calls.dma_buffer_to_images.len(), 1);
    let (copy, src, dst) = calls.dma_buffer_to_images[0];
    assert_eq!(
        copy,
        dma::ImageCopy {
            length_x: 16,
            length_y: 4
        }
    );
    assert_eq!(
        src,
        dma::BufferOperand {
            pitch: 16,
            width: 16,
            height: 4,
            address: 0x1000
        }
    );
    assert_eq!(
        dst,
        dma::ImageOperand {
            bytes_per_pixel: 1,
            params,
            address: 0x8000
        }
    );
}

#[test]
fn test_multi_line_blocklinear_to_pitch_unswizzles_subrect() {
    let mut eng = new_test_engine();
    let src_addr = 0x1000;
    let dst_addr = 0x8000;
    let width = 16;
    let height = 4;
    let depth = 1;
    let block_height = 0;
    let block_depth = 0;
    let line_length = 16;
    let line_count = 4;
    let linear: Vec<u8> = (0..64).collect();
    let mut tiled =
        vec![0u8; calculate_size(true, 1, width, height, depth, block_height, block_depth)];
    swizzle_subrect(
        &mut tiled,
        &linear,
        1,
        width,
        height,
        depth,
        0,
        0,
        line_length,
        line_count,
        block_height,
        block_depth,
        line_length,
    );

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, src_addr);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, dst_addr);
    eng.write_reg(PITCH_OUT, line_length);
    eng.write_reg(LINE_LENGTH, line_length);
    eng.write_reg(LINE_COUNT, line_count);
    write_dma_params(
        &mut eng,
        SRC_PARAMS,
        dma::Parameters {
            block_size: dma::BlockSize {
                raw: block_height << 4 | block_depth << 8,
            },
            width,
            height,
            depth,
            layer: 0,
            origin: dma::Origin { raw: 0 },
        },
    );
    eng.write_reg(LAUNCH_DMA, MULTI_LINE_BLOCKLINEAR_TO_PITCH_LAUNCH);

    let writes = eng.execute_pending(&|addr, buf| {
        if addr == dst_addr as u64 {
            buf.fill(0);
            return;
        }
        let offset = (addr - src_addr as u64) as usize;
        buf.copy_from_slice(&tiled[offset..offset + buf.len()]);
    });

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, dst_addr as u64);
    assert_eq!(writes[0].data, linear);
}

#[test]
fn test_multi_line_blocklinear_to_pitch_remap_uses_component_size() {
    let mut eng = new_test_engine();
    let src_addr = 0x1000;
    let dst_addr = 0x8000;
    let width = 16;
    let height = 4;
    let depth = 1;
    let bytes_per_pixel = 4;
    let line_count = height;
    let pitch = width * bytes_per_pixel;
    let linear: Vec<u8> = (0..pitch * line_count).map(|value| value as u8).collect();
    let mut tiled = vec![0u8; calculate_size(true, bytes_per_pixel, width, height, depth, 0, 0,)];
    swizzle_subrect(
        &mut tiled,
        &linear,
        bytes_per_pixel,
        width,
        height,
        depth,
        0,
        0,
        width,
        line_count,
        0,
        0,
        pitch,
    );

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, src_addr);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, dst_addr);
    eng.write_reg(PITCH_OUT, pitch);
    eng.write_reg(LINE_LENGTH, width);
    eng.write_reg(LINE_COUNT, line_count);
    eng.write_reg(REMAP_COMPONENTS, 3 << 16);
    write_dma_params(
        &mut eng,
        SRC_PARAMS,
        dma::Parameters {
            block_size: dma::BlockSize { raw: 0 },
            width,
            height,
            depth,
            layer: 0,
            origin: dma::Origin { raw: 0 },
        },
    );
    eng.write_reg(
        LAUNCH_DMA,
        MULTI_LINE_BLOCKLINEAR_TO_PITCH_LAUNCH | LAUNCH_REMAP_ENABLE,
    );

    let writes = eng.execute_pending(&|addr, buf| {
        if addr == dst_addr as u64 {
            buf.fill(0);
            return;
        }
        let offset = (addr - src_addr as u64) as usize;
        buf.copy_from_slice(&tiled[offset..offset + buf.len()]);
    });

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, dst_addr as u64);
    assert_eq!(writes[0].data, linear);
}

#[test]
fn test_multi_line_pitch_to_blocklinear_swizzles_subrect() {
    let mut eng = new_test_engine();
    let src_addr = 0x1000;
    let dst_addr = 0x8000;
    let width = 16;
    let height = 4;
    let depth = 1;
    let block_height = 0;
    let block_depth = 0;
    let line_length = 16;
    let line_count = 4;
    let linear: Vec<u8> = (0..64).collect();
    let mut expected =
        vec![0u8; calculate_size(true, 1, width, height, depth, block_height, block_depth)];
    swizzle_subrect(
        &mut expected,
        &linear,
        1,
        width,
        height,
        depth,
        0,
        0,
        line_length,
        line_count,
        block_height,
        block_depth,
        line_length,
    );

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, src_addr);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, dst_addr);
    eng.write_reg(PITCH_IN, line_length);
    eng.write_reg(LINE_LENGTH, line_length);
    eng.write_reg(LINE_COUNT, line_count);
    write_dma_params(
        &mut eng,
        DST_PARAMS,
        dma::Parameters {
            block_size: dma::BlockSize {
                raw: block_height << 4 | block_depth << 8,
            },
            width,
            height,
            depth,
            layer: 0,
            origin: dma::Origin { raw: 0 },
        },
    );
    eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_BLOCKLINEAR_LAUNCH);

    let writes = eng.execute_pending(&|addr, buf| {
        if addr == dst_addr as u64 {
            buf.fill(0);
            return;
        }
        let offset = (addr - src_addr as u64) as usize;
        buf.copy_from_slice(&linear[offset..offset + buf.len()]);
    });

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, dst_addr as u64);
    assert_eq!(writes[0].data, expected);
}

#[test]
fn test_multi_line_blocklinear_to_blocklinear_deswizzles_then_reswizzles() {
    let mut eng = new_test_engine();
    let src_addr = 0x1000;
    let dst_addr = 0x8000;
    let width = 16;
    let height = 4;
    let depth = 1;
    let block_height = 0;
    let block_depth = 0;
    let line_length = 16;
    let line_count = 4;
    let linear: Vec<u8> = (0..64).collect();
    let mut tiled =
        vec![0u8; calculate_size(true, 1, width, height, depth, block_height, block_depth)];
    swizzle_subrect(
        &mut tiled,
        &linear,
        1,
        width,
        height,
        depth,
        0,
        0,
        line_length,
        line_count,
        block_height,
        block_depth,
        line_length,
    );

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, src_addr);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, dst_addr);
    eng.write_reg(LINE_LENGTH, line_length);
    eng.write_reg(LINE_COUNT, line_count);
    let params = dma::Parameters {
        block_size: dma::BlockSize {
            raw: block_height << 4 | block_depth << 8,
        },
        width,
        height,
        depth,
        layer: 0,
        origin: dma::Origin { raw: 0 },
    };
    write_dma_params(&mut eng, SRC_PARAMS, params);
    write_dma_params(&mut eng, DST_PARAMS, params);
    eng.write_reg(LAUNCH_DMA, MULTI_LINE_BLOCKLINEAR_TO_BLOCKLINEAR_LAUNCH);

    let writes = eng.execute_pending(&|addr, buf| {
        if addr == dst_addr as u64 {
            buf.fill(0);
            return;
        }
        let offset = (addr - src_addr as u64) as usize;
        buf.copy_from_slice(&tiled[offset..offset + buf.len()]);
    });

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, dst_addr as u64);
    assert_eq!(writes[0].data, tiled);
}

#[test]
fn test_single_line_pitch_page_kind_copies_line_length_without_line_count() {
    let mut eng = new_test_engine();
    {
        let mut mm = eng.memory_manager.lock();
        mm.map(0x1000, 0x1000, 0x1000, PteKind::PITCH.raw() as u32, false);
        mm.map(0x8000, 0x8000, 0x1000, PteKind::PITCH.raw() as u32, false);
    }

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(PITCH_IN, 0);
    eng.write_reg(PITCH_OUT, 0);
    eng.write_reg(LINE_LENGTH, 12);
    eng.write_reg(LINE_COUNT, 0);
    eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH);

    let src: Vec<u8> = (0x40..0x80).collect();
    let writes = eng.execute_pending(&|addr, buf| {
        let offset = (addr - 0x1000) as usize;
        buf.copy_from_slice(&src[offset..offset + buf.len()]);
    });

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, 0x8000);
    assert_eq!(writes[0].data, src[..12]);
}

#[test]
fn test_single_line_non_pitch_alignment_reports_and_continues_like_upstream() {
    let mut eng = new_test_engine();
    {
        let mut mm = eng.memory_manager.lock();
        mm.map(0x1000, 0x1000, 0x1000, PteKind::Z16.raw() as u32, false);
        mm.map(0x8000, 0x8000, 0x1000, PteKind::PITCH.raw() as u32, false);
    }

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(LINE_LENGTH, 12);
    eng.write_reg(LINE_COUNT, 0);
    eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH);

    let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, 0x8000);
    assert_eq!(writes[0].data, vec![0xAA; 16]);
}

#[test]
fn test_single_line_blocklinear_to_pitch_uses_upstream_address_conversion() {
    let mut eng = new_test_engine();
    {
        let mut mm = eng.memory_manager.lock();
        mm.map(0x1000, 0x1000, 0x1000, PteKind::Z16.raw() as u32, false);
        mm.map(0x8000, 0x8000, 0x1000, PteKind::PITCH.raw() as u32, false);
    }

    let src_addr = 0x1040;
    let dst_addr = 0x8000;
    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, src_addr);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, dst_addr);
    eng.write_reg(LINE_LENGTH, 32);
    eng.write_reg(LINE_COUNT, 0);
    eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH);

    let first = convert_linear_2_blocklinear_addr(src_addr as u64);
    let second = convert_linear_2_blocklinear_addr(src_addr as u64 + 16);
    let writes = eng.execute_pending(&|addr, buf| {
        if addr == first {
            buf.copy_from_slice(&[0x11; 16]);
        } else if addr == second {
            buf.copy_from_slice(&[0x22; 16]);
        } else {
            panic!("unexpected source address 0x{addr:X}");
        }
    });

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, dst_addr as u64);
    assert_eq!(&writes[0].data[..16], &[0x11; 16]);
    assert_eq!(&writes[0].data[16..32], &[0x22; 16]);
}

#[test]
fn test_single_line_pitch_to_blocklinear_emits_converted_writes() {
    let mut eng = new_test_engine();
    {
        let mut mm = eng.memory_manager.lock();
        mm.map(0x1000, 0x1000, 0x1000, PteKind::PITCH.raw() as u32, false);
        mm.map(0x8000, 0x8000, 0x1000, PteKind::Z16.raw() as u32, false);
    }

    let src_addr = 0x1000;
    let dst_addr = 0x8040;
    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, src_addr);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, dst_addr);
    eng.write_reg(LINE_LENGTH, 32);
    eng.write_reg(LINE_COUNT, 0);
    eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH);

    let writes = eng.execute_pending(&|addr, buf| {
        if addr == src_addr as u64 {
            buf.copy_from_slice(&[0x33; 16]);
        } else if addr == src_addr as u64 + 16 {
            buf.copy_from_slice(&[0x44; 16]);
        } else {
            panic!("unexpected source address 0x{addr:X}");
        }
    });

    assert_eq!(writes.len(), 2);
    assert_eq!(
        writes[0].gpu_va,
        convert_linear_2_blocklinear_addr(dst_addr as u64)
    );
    assert_eq!(
        writes[1].gpu_va,
        convert_linear_2_blocklinear_addr(dst_addr as u64 + 16)
    );
    assert_eq!(writes[0].data, vec![0x33; 16]);
    assert_eq!(writes[1].data, vec![0x44; 16]);
}

#[test]
fn test_single_line_remap_const_a_clears_u32_words_like_upstream() {
    let mut eng = new_test_engine();

    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(LINE_LENGTH, 3);
    eng.write_reg(LINE_COUNT, 0);
    eng.write_reg(REMAP_CONSTA_VALUE, 0x1122_3344);
    eng.write_reg(
        REMAP_COMPONENTS,
        REMAP_SWIZZLE_CONST_A | (3 << 16), // dst_x = CONST_A, 4-byte component.
    );
    eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH | LAUNCH_REMAP_ENABLE);

    let writes = eng.execute_pending(&|_, _| panic!("CONST_A clear must not read source"));

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, 0x8000);
    assert_eq!(
        writes[0].data,
        vec![0x44, 0x33, 0x22, 0x11, 0x44, 0x33, 0x22, 0x11, 0x44, 0x33, 0x22, 0x11]
    );
}

#[test]
fn test_single_line_remap_const_a_invalid_size_reports_and_continues_like_upstream() {
    let mut eng = new_test_engine();

    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(LINE_LENGTH, 1);
    eng.write_reg(REMAP_CONSTA_VALUE, 0x1122_3344);
    eng.write_reg(
        REMAP_COMPONENTS,
        REMAP_SWIZZLE_CONST_A | (2 << 16), // Three-byte size triggers upstream ASSERT.
    );
    eng.write_reg(LAUNCH_DMA, SINGLE_LINE_LAUNCH | LAUNCH_REMAP_ENABLE);

    let writes = eng.execute_pending(&|_, _| {});
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].data, vec![0x44, 0x33, 0x22]);
}

#[test]
fn test_dma_fallback_flushes_source_and_defers_cached_destination_invalidation() {
    let mut eng = new_test_engine();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(Arc::clone(&calls));
    bind_memory_rasterizer(
        &mut eng,
        &rasterizer,
        &[(0x1000, 0x2_1000), (0x8000, 0x2_8000)],
    );

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(PITCH_IN, 16);
    eng.write_reg(PITCH_OUT, 32);
    eng.write_reg(LINE_LENGTH, 8);
    eng.write_reg(LINE_COUNT, 3);
    eng.write_reg(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH);

    let src = vec![0x55; 0x100];
    let writes = eng.execute_pending(&|addr, buf| {
        if addr == 0x8000 {
            buf.fill(0);
            return;
        }
        let offset = (addr - 0x1000) as usize;
        buf.copy_from_slice(&src[offset..offset + buf.len()]);
    });

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].mode, PendingWriteMode::Cached);
    let calls = calls.lock();
    assert_eq!(calls.flushes, vec![(0x2_1000, 40)]);
    assert!(calls.invalidations.is_empty());
}

#[test]
fn test_runtime_dma_writes_destination_before_releasing_semaphore() {
    let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
    let backing = Arc::new(Mutex::new(vec![0u8; 0x2000]));
    {
        let mut memory = backing.lock();
        memory[..8].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        device_memory.smmu_set_physical_base_for_test(memory.as_mut_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x8000,
            memory.as_mut_ptr(),
            0x4000,
            memory.len(),
            1,
            true,
        );
    }
    let memory_manager = Arc::new(Mutex::new(
        MemoryManager::new_with_geometry_and_device_memory(
            13,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        ),
    ));
    {
        let mut memory_manager = memory_manager.lock();
        memory_manager.map(0x1000, 0x8000, 0x1000, PteKind::PITCH.raw() as u32, false);
        memory_manager.map(0x2000, 0x9000, 0x1000, PteKind::PITCH.raw() as u32, false);
    }

    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(Arc::clone(&calls))
        .observe_memory_at_query(Arc::clone(&backing), 0x1000..0x1008);
    let mut eng = MaxwellDMA::new(Arc::clone(&memory_manager));
    eng.bind_rasterizer(&rasterizer);
    memory_manager.lock().bind_rasterizer(&rasterizer);

    eng.call_method(SRC_ADDR_LOW, 0x1000, true);
    eng.call_method(DST_ADDR_LOW, 0x2000, true);
    eng.call_method(LINE_LENGTH, 8, true);
    eng.call_method(SEMAPHORE_ADDR_LOW, 0x3000, true);
    eng.call_method(SEMAPHORE_PAYLOAD, 0xCAFE_BABE, true);
    eng.call_method(
        LAUNCH_DMA,
        SINGLE_LINE_LAUNCH | (LAUNCH_SEMAPHORE_TYPE_RELEASE_ONE_WORD << 3),
        true,
    );

    assert_eq!(
        calls.lock().query_observed_memory,
        vec![vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]]
    );
}

#[test]
fn test_release_one_word_semaphore_queries_payload_fence_after_dma() {
    let mut eng = new_test_engine();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(Arc::clone(&calls));
    eng.bind_rasterizer(&rasterizer);

    eng.write_reg(SEMAPHORE_ADDR_HIGH, 0x123);
    eng.write_reg(SEMAPHORE_ADDR_LOW, 0x4567_8000);
    eng.write_reg(SEMAPHORE_PAYLOAD, 0xCAFE_BABE);
    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(PITCH_IN, 16);
    eng.write_reg(PITCH_OUT, 16);
    eng.write_reg(LINE_LENGTH, 8);
    eng.write_reg(LINE_COUNT, 1);
    eng.write_reg(LAUNCH_DMA, RELEASE_ONE_WORD_SEMAPHORE_LAUNCH);

    let src = vec![0x55; 0x100];
    let writes = eng.execute_pending(&|addr, buf| {
        if addr == 0x8000 {
            buf.fill(0);
            return;
        }
        let offset = (addr - 0x1000) as usize;
        buf.copy_from_slice(&src[offset..offset + buf.len()]);
    });

    assert_eq!(writes.len(), 1);
    let calls = calls.lock();
    assert_eq!(
        calls.queries,
        vec![(
            0x23_4567_8000,
            QueryType::Payload as u32,
            QueryPropertiesFlags::IS_A_FENCE,
            0xCAFE_BABE,
            0,
        )]
    );
}

#[test]
fn test_release_four_word_semaphore_adds_timeout_flag() {
    let mut eng = new_test_engine();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(Arc::clone(&calls));
    eng.bind_rasterizer(&rasterizer);

    eng.write_reg(SEMAPHORE_ADDR_HIGH, 0);
    eng.write_reg(SEMAPHORE_ADDR_LOW, 0x9000);
    eng.write_reg(SEMAPHORE_PAYLOAD, 0x1357_9BDF);
    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(PITCH_IN, 16);
    eng.write_reg(PITCH_OUT, 16);
    eng.write_reg(LINE_LENGTH, 8);
    eng.write_reg(LINE_COUNT, 1);
    eng.write_reg(LAUNCH_DMA, RELEASE_FOUR_WORD_SEMAPHORE_LAUNCH);

    let src = vec![0x66; 0x100];
    let writes = eng.execute_pending(&|addr, buf| {
        if addr == 0x8000 {
            buf.fill(0);
            return;
        }
        let offset = (addr - 0x1000) as usize;
        buf.copy_from_slice(&src[offset..offset + buf.len()]);
    });

    assert_eq!(writes.len(), 1);
    let calls = calls.lock();
    assert_eq!(
        calls.queries,
        vec![(
            0x9000,
            QueryType::Payload as u32,
            QueryPropertiesFlags::IS_A_FENCE | QueryPropertiesFlags::HAS_TIMEOUT,
            0x1357_9BDF,
            0,
        )]
    );
}

#[test]
fn test_zero_length_valid_launch_still_releases_semaphore() {
    let mut eng = new_test_engine();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(Arc::clone(&calls));
    eng.bind_rasterizer(&rasterizer);

    eng.write_reg(SEMAPHORE_ADDR_HIGH, 0);
    eng.write_reg(SEMAPHORE_ADDR_LOW, 0x9000);
    eng.write_reg(SEMAPHORE_PAYLOAD, 0x2468_ACE0);
    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(PITCH_IN, 16);
    eng.write_reg(PITCH_OUT, 16);
    eng.write_reg(LINE_LENGTH, 0);
    eng.write_reg(LINE_COUNT, 1);
    eng.write_reg(LAUNCH_DMA, RELEASE_ONE_WORD_SEMAPHORE_LAUNCH);

    let writes = eng.execute_pending(&|_, _| panic!("zero-length DMA should not read"));

    assert!(writes.is_empty());
    let calls = calls.lock();
    assert_eq!(
        calls.queries,
        vec![(
            0x9000,
            QueryType::Payload as u32,
            QueryPropertiesFlags::IS_A_FENCE,
            0x2468_ACE0,
            0,
        )]
    );
}

#[test]
fn test_accelerated_blocklinear_to_pitch_releases_semaphore_without_pending_write() {
    let mut eng = new_test_engine();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
    rasterizer.accelerate_image_to_buffer = true;
    eng.bind_rasterizer(&rasterizer);

    eng.write_reg(SEMAPHORE_ADDR_HIGH, 0);
    eng.write_reg(SEMAPHORE_ADDR_LOW, 0xA000);
    eng.write_reg(SEMAPHORE_PAYLOAD, 0x1020_3040);
    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x8000);
    eng.write_reg(PITCH_IN, 16);
    eng.write_reg(PITCH_OUT, 16);
    eng.write_reg(LINE_LENGTH, 16);
    eng.write_reg(LINE_COUNT, 2);
    write_dma_params(
        &mut eng,
        SRC_PARAMS,
        dma::Parameters {
            block_size: dma::BlockSize { raw: 0 },
            width: 16,
            height: 2,
            depth: 1,
            layer: 0,
            origin: dma::Origin { raw: 0 },
        },
    );
    eng.write_reg(
        LAUNCH_DMA,
        MULTI_LINE_BLOCKLINEAR_TO_PITCH_LAUNCH | (LAUNCH_SEMAPHORE_TYPE_RELEASE_ONE_WORD << 3),
    );

    let writes = eng.execute_pending(&|_, _| panic!("accelerated DMA should not read"));

    assert!(writes.is_empty());
    let calls = calls.lock();
    assert_eq!(calls.dma_image_to_buffers.len(), 1);
    assert_eq!(
        calls.queries,
        vec![(
            0xA000,
            QueryType::Payload as u32,
            QueryPropertiesFlags::IS_A_FENCE,
            0x1020_3040,
            0,
        )]
    );
}

#[test]
fn test_call_method_launch_executes_immediately() {
    let mut eng = new_test_engine();
    assert!(!eng.pending_launch);
    eng.call_method(LAUNCH_DMA, MULTI_LINE_PITCH_TO_PITCH_LAUNCH, true);
    assert!(!eng.pending_launch);
}

#[test]
fn test_call_multi_method_launch_executes_immediately() {
    let mut eng = new_test_engine();
    assert!(!eng.pending_launch);
    eng.call_multi_method(LAUNCH_DMA, &[MULTI_LINE_PITCH_TO_PITCH_LAUNCH], 1, 1);
    assert!(!eng.pending_launch);
}

#[test]
fn test_call_multi_method_honors_amount() {
    let mut eng = new_test_engine();
    let method = 0x100;

    eng.call_multi_method(method, &[0x1111_1111, 0x2222_2222], 1, 2);

    assert_eq!(eng.regs[method as usize], 0x1111_1111);
}

#[test]
#[should_panic(expected = "MaxwellDMA launch interrupt type must be NONE")]
fn test_launch_rejects_interrupt_requests() {
    let mut eng = new_test_engine();
    eng.call_method(LAUNCH_DMA, 1 << LAUNCH_INTERRUPT_TYPE_SHIFT, true);
}

#[test]
fn test_dma_blocklinear_invalid_depth_reports_and_continues_like_upstream() {
    let mut eng = new_test_engine();

    eng.write_reg(SRC_ADDR_HIGH, 0);
    eng.write_reg(SRC_ADDR_LOW, 0x1000);
    eng.write_reg(DST_ADDR_HIGH, 0);
    eng.write_reg(DST_ADDR_LOW, 0x2000);
    eng.write_reg(PITCH_IN, 8);
    eng.write_reg(PITCH_OUT, 8);
    eng.write_reg(LINE_LENGTH, 8);
    eng.write_reg(LINE_COUNT, 2);
    eng.write_reg(
        LAUNCH_DMA,
        LAUNCH_DATA_TRANSFER_NON_PIPELINED
            | LAUNCH_DST_MEMORY_LAYOUT_PITCH
            | LAUNCH_MULTI_LINE_ENABLE,
    );

    let writes = eng.execute_pending(&|_, buf| buf.fill(0xAA));
    assert!(!writes.is_empty());
}

#[test]
fn test_constructor_keeps_memory_manager_owner() {
    let memory_manager = Arc::new(Mutex::new(MemoryManager::new(0x44)));
    let eng = MaxwellDMA::new(Arc::clone(&memory_manager));
    assert!(Arc::ptr_eq(&eng.memory_manager, &memory_manager));
}
