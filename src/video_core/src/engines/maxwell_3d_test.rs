// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::rasterizer_interface::{RasterizerDownloadArea, RasterizerInterface};
use std::sync::{Arc, Mutex};

#[test]
fn register_count_matches_eden_maxwell_3d() {
    let engine = Maxwell3D::default();
    assert_eq!(NUM_REGS, 0xE00);
    assert_eq!(engine.regs.len(), NUM_REGS);
}

#[test]
fn stream_out_layout_matches_eden_register_layout() {
    let layout = StreamOutLayout::from_raw(0x4433_2211);

    assert_eq!(NUM_TRANSFORM_FEEDBACK_BUFFERS, 4);
    assert_eq!(std::mem::size_of::<StreamOutLayout>(), 4);
    assert_eq!(std::mem::align_of::<StreamOutLayout>(), 4);
    assert_eq!(layout.raw(), 0x4433_2211);
    assert_eq!(layout.attribute0(), 0x11);
    assert_eq!(layout.attribute1(), 0x22);
    assert_eq!(layout.attribute2(), 0x33);
    assert_eq!(layout.attribute3(), 0x44);
}

fn new_descriptor_owner_backed_engine(backing: &[u8], device_addr: u64) -> Maxwell3D {
    let device_memory =
        Arc::new(crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default());
    device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
    device_memory.smmu_map_with_cpu_backing(
        device_addr,
        backing.as_ptr(),
        0x4000_0000,
        backing.len(),
        1,
        true,
    );
    let memory_manager = Arc::new(parking_lot::Mutex::new(
        crate::memory_manager::MemoryManager::new_with_geometry_and_device_memory(
            1,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        ),
    ));
    memory_manager
        .lock()
        .map(device_addr, device_addr, backing.len() as u64, 0, false);
    Maxwell3D::new_with_memory_manager(memory_manager)
}

#[derive(Default, Clone)]
struct RasterizerCalls {
    wait_for_idle: u32,
    draw_texture: u32,
    clear_layers: Vec<u32>,
    signal_sync_point: Vec<u32>,
    reset_counter: Vec<u32>,
    query_writes: Vec<(u64, Vec<u8>)>,
    query_calls: Vec<(u64, u32, QueryPropertiesFlags, u32, u32)>,
    bound_uniforms: Vec<(usize, u32, u64, u32)>,
    disabled_uniforms: Vec<(usize, u32)>,
    accelerate_conditional_rendering: bool,
    inline_to_memory: Vec<(u64, usize, Vec<u8>)>,
    transform_feedback: Vec<u64>,
    has_draw_transform_feedback: bool,
    /// (instance_count, draw_indexed, shader_program_addresses) per `draw` call.
    draws: Vec<(u32, bool, [u64; 6])>,
    draw_states: Vec<crate::engines::draw_manager::DrawState>,
    draw_registers: Vec<crate::engines::draw_manager::Maxwell3DDrawRegisters>,
}

struct TestRasterizer {
    accelerate_dma: crate::rasterizer_interface::TestAccelerateDMA,
    calls: Arc<Mutex<RasterizerCalls>>,
}

impl TestRasterizer {
    fn new(calls: Arc<Mutex<RasterizerCalls>>) -> Self {
        Self {
            accelerate_dma: Default::default(),
            calls,
        }
    }
}

impl RasterizerInterface for TestRasterizer {
    fn access_accelerate_dma(
        &mut self,
    ) -> &mut dyn crate::engines::maxwell_dma::AccelerateDMAInterface {
        &mut self.accelerate_dma
    }

    fn draw(
        &mut self,
        draw_view: crate::engines::draw_manager::Maxwell3DDrawView<'_>,
        instance_count: u32,
    ) {
        let draw_state = draw_view.draw_state();
        self.calls.lock().unwrap().draws.push((
            instance_count,
            draw_view.is_indexed(),
            draw_view.shader_program_addresses(),
        ));
        self.calls
            .lock()
            .unwrap()
            .draw_states
            .push(draw_state.clone());
        self.calls
            .lock()
            .unwrap()
            .draw_registers
            .push(draw_view.registers());
    }
    fn draw_texture(
        &mut self,
        _draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
    ) {
        self.calls.lock().unwrap().draw_texture += 1;
    }
    fn clear(
        &mut self,
        _clear_view: crate::engines::draw_manager::Maxwell3DClearView<'_>,
        layer_count: u32,
    ) {
        self.calls.lock().unwrap().clear_layers.push(layer_count);
    }
    fn dispatch_compute(&mut self, _dispatch: &crate::engines::kepler_compute::DispatchCall) {}
    fn reset_counter(&mut self, query_type: u32) {
        self.calls.lock().unwrap().reset_counter.push(query_type);
    }
    fn query(
        &mut self,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
        subreport: u32,
    ) {
        let bytes = if flags.contains(QueryPropertiesFlags::HAS_TIMEOUT) {
            let mut buf = Vec::with_capacity(16);
            buf.extend_from_slice(&(payload as u64).to_le_bytes());
            buf.extend_from_slice(&0u64.to_le_bytes());
            buf
        } else {
            payload.to_le_bytes().to_vec()
        };
        self.calls
            .lock()
            .unwrap()
            .query_writes
            .push((gpu_addr, bytes));
        self.calls
            .lock()
            .unwrap()
            .query_calls
            .push((gpu_addr, query_type, flags, payload, subreport));
    }
    fn bind_graphics_uniform_buffer(&mut self, stage: usize, index: u32, gpu_addr: u64, size: u32) {
        self.calls
            .lock()
            .unwrap()
            .bound_uniforms
            .push((stage, index, gpu_addr, size));
    }
    fn disable_graphics_uniform_buffer(&mut self, stage: usize, index: u32) {
        self.calls
            .lock()
            .unwrap()
            .disabled_uniforms
            .push((stage, index));
    }
    fn signal_fence(&mut self, _func: Box<dyn FnOnce() + Send>) {}
    fn sync_operation(&mut self, _func: Box<dyn FnOnce() + Send>) {}
    fn signal_sync_point(&mut self, value: u32) {
        self.calls.lock().unwrap().signal_sync_point.push(value);
    }
    fn signal_reference(&mut self) {}
    fn release_fences(&mut self, _force: bool) {}
    fn flush_all(&mut self) {}
    fn flush_region(&mut self, _addr: u64, _size: u64, _which: crate::cache_types::CacheType) {}
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
    fn invalidate_region(&mut self, _addr: u64, _size: u64, _which: crate::cache_types::CacheType) {
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
    fn wait_for_idle(&mut self) {
        self.calls.lock().unwrap().wait_for_idle += 1;
    }
    fn fragment_barrier(&mut self) {}
    fn tiled_cache_barrier(&mut self) {}
    fn flush_commands(&mut self) {}
    fn tick_frame(&mut self) {}
    fn accelerate_conditional_rendering(&mut self) -> bool {
        self.calls.lock().unwrap().accelerate_conditional_rendering
    }
    fn accelerate_inline_to_memory(&mut self, address: u64, copy_size: usize, memory: &[u8]) {
        self.calls
            .lock()
            .unwrap()
            .inline_to_memory
            .push((address, copy_size, memory.to_vec()));
    }
    fn register_transform_feedback(&mut self, tfb_object_addr: u64) {
        self.calls
            .lock()
            .unwrap()
            .transform_feedback
            .push(tfb_object_addr);
    }
    fn has_draw_transform_feedback(&self) -> bool {
        self.calls.lock().unwrap().has_draw_transform_feedback
    }
}

// ── Existing tests ───────────────────────────────────────────────────

#[test]
fn test_write_reg() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(0x100, 0xDEAD);
    assert_eq!(engine.regs[0x100], 0xDEAD);
}

#[test]
fn test_write_reg_high_method() {
    // Regression: methods above 0xFFF byte offset were silently dropped.
    // The register file stores upstream word indices, not raw byte offsets.
    let mut engine = Maxwell3D::new();
    engine.write_reg(CLEAR_SURFACE, 0x1234);
    assert_eq!(engine.regs[CLEAR_SURFACE as usize], 0x1234);
}

#[test]
fn test_rt_accessors() {
    let mut engine = Maxwell3D::new();
    let rt0_base = RT_BASE as usize;

    // Set RT0: address = 0x0001_0000_2000, width=1280, height=720, format=0xD5
    engine.regs[rt0_base + RT_OFF_ADDRESS_HIGH as usize] = 0x0001;
    engine.regs[rt0_base + RT_OFF_ADDRESS_LOW as usize] = 0x0000_2000;
    engine.regs[rt0_base + RT_OFF_WIDTH as usize] = 1280;
    engine.regs[rt0_base + RT_OFF_HEIGHT as usize] = 720;
    engine.regs[rt0_base + RT_OFF_FORMAT as usize] = 0xD5;

    assert_eq!(engine.rt_address(0), 0x0001_0000_2000);
    assert_eq!(engine.rt_width(0), 1280);
    assert_eq!(engine.rt_height(0), 720);
    assert_eq!(engine.rt_format(0), 0xD5);
}

#[test]
fn test_clear_color_accessor() {
    let mut engine = Maxwell3D::new();
    let base = CLEAR_COLOR_BASE as usize;

    engine.regs[base] = f32::to_bits(1.0); // R
    engine.regs[base + 1] = f32::to_bits(0.5); // G
    engine.regs[base + 2] = f32::to_bits(0.0); // B
    engine.regs[base + 3] = f32::to_bits(0.75); // A

    let color = engine.clear_color_rgba();
    assert_eq!(color[0], 1.0);
    assert_eq!(color[1], 0.5);
    assert_eq!(color[2], 0.0);
    assert_eq!(color[3], 0.75);
}

#[test]
fn test_call_method_clear_surface_routes_via_draw_manager_clear_owner() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);

    let flags = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5);
    engine.call_method(CLEAR_SURFACE, flags, true);

    assert_eq!(calls.lock().unwrap().clear_layers, vec![1]);
}

#[test]
fn test_draw_logs_without_crash() {
    let mut engine = Maxwell3D::new();
    // Just ensure draw begin/end doesn't panic.
    engine.write_reg(DRAW_BEGIN, 0x0004); // Triangles
    engine.write_reg(DRAW_END, 0);
    assert_eq!(engine.take_draw_calls().len(), 1);
}

#[test]
fn test_clear_via_write_reg() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);

    // Trigger clear via write_reg (this is the actual path).
    let flags = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5);
    engine.write_reg(CLEAR_SURFACE, flags);

    assert_eq!(calls.lock().unwrap().clear_layers, vec![1]);
}

// ── Draw state tracking tests ────────────────────────────────────────

#[test]
fn test_vertex_stream_accessors() {
    let mut engine = Maxwell3D::new();
    let base = VERTEX_STREAM_BASE as usize;

    // Stream 0: stride=64, enabled, addr=0x0000_1000_2000.
    engine.regs[base] = 64 | (1 << 12); // stride=64, enable bit 12
    engine.regs[base + 1] = 0x0000_1000; // addr_high
    engine.regs[base + 2] = 0x0000_2000; // addr_low
    engine.regs[base + 3] = 7; // frequency

    let info = engine.vertex_stream_info(0);
    assert_eq!(info.index, 0);
    assert_eq!(info.stride, 64);
    assert_eq!(info.frequency, 7);
    assert!(info.enabled);
    assert_eq!(info.address, 0x0000_1000_0000_2000);
}

#[test]
fn test_vertex_stream_disabled() {
    let mut engine = Maxwell3D::new();
    let base = VERTEX_STREAM_BASE as usize;

    // Stream 0: stride=32, NOT enabled (bit 12 clear).
    engine.regs[base] = 32;
    engine.regs[base + 1] = 0;
    engine.regs[base + 2] = 0x5000;

    let info = engine.vertex_stream_info(0);
    assert_eq!(info.stride, 32);
    assert_eq!(info.frequency, 0);
    assert!(!info.enabled);
}

#[test]
fn test_vertex_stream_instance_accessor() {
    let mut engine = Maxwell3D::new();

    engine.regs[VERTEX_STREAM_INSTANCE_BASE as usize + 3] = 1;

    assert_eq!(engine.vertex_stream_instance(0), 0);
    assert_eq!(engine.vertex_stream_instance(3), 1);
}

#[test]
fn test_index_buffer_accessors() {
    let mut engine = Maxwell3D::new();
    let base = IB_BASE as usize;

    engine.regs[base + IB_OFF_ADDR_HIGH as usize] = 0x0000_00AB;
    engine.regs[base + IB_OFF_ADDR_LOW as usize] = 0xCDEF_0000;
    engine.regs[base + IB_OFF_LIMIT_HIGH as usize] = 0x0000_00AB;
    engine.regs[base + IB_OFF_LIMIT_LOW as usize] = 0xCDEF_FFFF;
    engine.regs[base + IB_OFF_FORMAT as usize] = 1; // UnsignedShort
    engine.regs[base + IB_OFF_FIRST as usize] = 10;
    engine.regs[base + IB_OFF_COUNT as usize] = 500;

    assert_eq!(engine.index_buffer_addr(), 0xAB_CDEF_0000);
    assert_eq!(
        <Maxwell3D as dm::Maxwell3DAccess>::index_buffer_addr_end(&engine),
        0xAB_CDEF_FFFF
    );
    assert_eq!(engine.index_buffer_format(), IndexFormat::UnsignedShort);
    assert_eq!(engine.index_buffer_format().size_bytes(), 2);
    assert_eq!(engine.index_buffer_first(), 10);
    assert_eq!(engine.index_buffer_count(), 500);
}

#[test]
fn test_viewport_info() {
    let mut engine = Maxwell3D::new();
    let base = VP_TRANSFORM_BASE as usize;

    // VP0: scale=(640, -360, 0.5), translate=(640, 360, 0.5)
    // => x=0, y=0, width=1280, height=720, near=0, far=1
    engine.regs[base] = f32::to_bits(640.0); // scale_x
    engine.regs[base + 1] = f32::to_bits(-360.0); // scale_y
    engine.regs[base + 2] = f32::to_bits(0.5); // scale_z
    engine.regs[base + 3] = f32::to_bits(640.0); // translate_x
    engine.regs[base + 4] = f32::to_bits(360.0); // translate_y
    engine.regs[base + 5] = f32::to_bits(0.5); // translate_z

    let vp = engine.viewport_info(0);
    assert_eq!(vp.x, 0.0);
    assert_eq!(vp.y, 0.0);
    assert_eq!(vp.width, 1280.0);
    assert_eq!(vp.height, 720.0);
    assert_eq!(vp.depth_near, 0.0);
    assert_eq!(vp.depth_far, 1.0);
}

#[test]
fn test_scissor_info() {
    let mut engine = Maxwell3D::new();
    let base = SCISSOR_BASE as usize;

    // Scissor 0: enabled, min_x=10, max_x=1270, min_y=20, max_y=700.
    engine.regs[base] = 1; // enabled
    engine.regs[base + 1] = 10 | (1270 << 16); // min_x | max_x
    engine.regs[base + 2] = 20 | (700 << 16); // min_y | max_y

    let sc = engine.scissor_info(0);
    assert!(sc.enabled);
    assert_eq!(sc.min_x, 10);
    assert_eq!(sc.max_x, 1270);
    assert_eq!(sc.min_y, 20);
    assert_eq!(sc.max_y, 700);
}

#[test]
fn test_draw_begin_sets_topology() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(DRAW_BEGIN, 4); // Triangles
    assert_eq!(engine.current_topology(), PrimitiveTopology::Triangles);

    engine.write_reg(DRAW_BEGIN, 1); // Lines
    assert_eq!(engine.current_topology(), PrimitiveTopology::Lines);
}

#[test]
fn test_draw_end_creates_draw_call() {
    let mut engine = Maxwell3D::new();

    // Set up vertex stream 0.
    let vs_base = VERTEX_STREAM_BASE;
    engine.write_reg(vs_base, 32 | (1 << 12)); // stride=32, enabled
    engine.write_reg(vs_base + 1, 0); // addr_high
    engine.write_reg(vs_base + 2, 0x10000); // addr_low

    // Set vertex buffer first/count.
    engine.write_reg(VB_FIRST, 0);
    engine.write_reg(VB_COUNT, 36);

    // Set viewport 0.
    let vp_base = VP_TRANSFORM_BASE;
    engine.write_reg(vp_base, f32::to_bits(640.0));
    engine.write_reg(vp_base + 1, f32::to_bits(-360.0));
    engine.write_reg(vp_base + 2, f32::to_bits(0.5));
    engine.write_reg(vp_base + 3, f32::to_bits(640.0));
    engine.write_reg(vp_base + 4, f32::to_bits(360.0));
    engine.write_reg(vp_base + 5, f32::to_bits(0.5));

    // Draw: begin(Triangles) + end.
    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    let d = &draws[0];
    assert_eq!(d.topology, PrimitiveTopology::Triangles);
    assert_eq!(d.vertex_first, 0);
    assert_eq!(d.vertex_count, 36);
    assert!(!d.indexed);
    assert_eq!(d.vertex_streams.len(), NUM_VERTEX_ARRAYS as usize);
    assert_eq!(d.vertex_streams[0].stride, 32);
    assert_eq!(d.viewports[0].width, 1280.0);
}

#[test]
fn draw_call_render_targets_preserve_the_complete_register_group() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(ANTI_ALIAS_SAMPLES_MODE, 0x5a);
    engine.write_reg(SURFACE_CLIP_BASE, 12 | (800 << 16));
    engine.write_reg(SURFACE_CLIP_BASE + 1, 34 | (450 << 16));
    engine.write_reg(DRAW_BEGIN, PrimitiveTopology::Triangles as u32);
    engine.write_reg(DRAW_END, 0);

    let draw = engine.take_draw_calls().remove(0);
    let targets = draw.render_targets();
    assert_eq!(targets.anti_alias_samples_mode, 0x5a);
    assert_eq!(targets.surface_clip.x, 12);
    assert_eq!(targets.surface_clip.y, 34);
    assert_eq!(targets.surface_clip.width, 800);
    assert_eq!(targets.surface_clip.height, 450);
}

#[test]
fn test_multiple_draw_calls() {
    let mut engine = Maxwell3D::new();

    engine.write_reg(DRAW_BEGIN, 4); // Triangles
    engine.write_reg(DRAW_END, 0);
    engine.write_reg(DRAW_BEGIN, 1); // Lines
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 2);
    assert_eq!(draws[0].topology, PrimitiveTopology::Triangles);
    assert_eq!(draws[1].topology, PrimitiveTopology::Lines);

    // After take, should be empty.
    let draws2 = engine.take_draw_calls();
    assert!(draws2.is_empty());
}

#[test]
fn test_draw_indexed_flag() {
    let mut engine = Maxwell3D::new();

    // Write IB count > 0 → sets draw_indexed.
    engine.write_reg(IB_BASE + IB_OFF_COUNT, 100);
    engine.write_reg(IB_BASE + IB_OFF_FORMAT, 2); // UnsignedInt

    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    assert!(draws[0].indexed);
    assert_eq!(draws[0].index_format, IndexFormat::UnsignedInt);
    assert_eq!(draws[0].index_buffer_count, 100);
}

// ── Enum encoding tests ──────────────────────────────────────────────

#[test]
fn test_comparison_op_gl_encoding() {
    // D3D encoding
    assert_eq!(ComparisonOp::from_raw(1), ComparisonOp::Never);
    assert_eq!(ComparisonOp::from_raw(2), ComparisonOp::Less);
    assert_eq!(ComparisonOp::from_raw(8), ComparisonOp::Always);
    // GL encoding
    assert_eq!(ComparisonOp::from_raw(0x200), ComparisonOp::Never);
    assert_eq!(ComparisonOp::from_raw(0x201), ComparisonOp::Less);
    assert_eq!(ComparisonOp::from_raw(0x207), ComparisonOp::Always);
    // Unknown defaults to Always
    assert_eq!(ComparisonOp::from_raw(0xFFFF), ComparisonOp::Always);
}

#[test]
fn test_stencil_op_gl_encoding() {
    // D3D encoding
    assert_eq!(StencilOp::from_raw(1), StencilOp::Keep);
    assert_eq!(StencilOp::from_raw(3), StencilOp::Replace);
    assert_eq!(StencilOp::from_raw(6), StencilOp::Invert);
    // GL encoding
    assert_eq!(StencilOp::from_raw(0x1E00), StencilOp::Keep);
    assert_eq!(StencilOp::from_raw(0x1E01), StencilOp::Replace);
    assert_eq!(StencilOp::from_raw(0x150A), StencilOp::Invert);
    assert_eq!(StencilOp::from_raw(0x8507), StencilOp::Incr);
    assert_eq!(StencilOp::from_raw(0x8508), StencilOp::Decr);
}

#[test]
fn test_blend_equation_gl_encoding() {
    // D3D encoding
    assert_eq!(BlendEquation::from_raw(1), BlendEquation::Add);
    assert_eq!(BlendEquation::from_raw(2), BlendEquation::Subtract);
    assert_eq!(BlendEquation::from_raw(5), BlendEquation::Max);
    // GL encoding
    assert_eq!(BlendEquation::from_raw(0x8006), BlendEquation::Add);
    assert_eq!(BlendEquation::from_raw(0x800A), BlendEquation::Subtract);
    assert_eq!(
        BlendEquation::from_raw(0x800B),
        BlendEquation::ReverseSubtract
    );
    assert_eq!(BlendEquation::from_raw(0x8007), BlendEquation::Min);
    assert_eq!(BlendEquation::from_raw(0x8008), BlendEquation::Max);
}

#[test]
fn test_blend_factor_gl_encoding() {
    // D3D encoding
    assert_eq!(BlendFactor::from_raw(0x01), BlendFactor::Zero);
    assert_eq!(BlendFactor::from_raw(0x02), BlendFactor::One);
    assert_eq!(BlendFactor::from_raw(0x05), BlendFactor::SrcAlpha);
    assert_eq!(BlendFactor::from_raw(0x06), BlendFactor::OneMinusSrcAlpha);
    assert_eq!(BlendFactor::from_raw(0x0B), BlendFactor::SrcAlphaSaturate);
    assert_eq!(BlendFactor::from_raw(0x0C), BlendFactor::ConstantAlpha);
    assert_eq!(
        BlendFactor::from_raw(0x0D),
        BlendFactor::OneMinusConstantAlpha
    );
    assert_eq!(BlendFactor::from_raw(0x0E), BlendFactor::ConstantColor);
    assert_eq!(
        BlendFactor::from_raw(0x0F),
        BlendFactor::OneMinusConstantColor
    );
    assert_eq!(BlendFactor::from_raw(0x10), BlendFactor::Src1Color);
    assert_eq!(BlendFactor::from_raw(0x11), BlendFactor::OneMinusSrc1Color);
    assert_eq!(BlendFactor::from_raw(0x12), BlendFactor::Src1Alpha);
    assert_eq!(BlendFactor::from_raw(0x13), BlendFactor::OneMinusSrc1Alpha);
    // GL encoding
    assert_eq!(BlendFactor::from_raw(0x4000), BlendFactor::Zero);
    assert_eq!(BlendFactor::from_raw(0x4001), BlendFactor::One);
    assert_eq!(BlendFactor::from_raw(0x4302), BlendFactor::SrcAlpha);
    assert_eq!(BlendFactor::from_raw(0xC001), BlendFactor::ConstantColor);
    assert_eq!(
        BlendFactor::from_raw(0xC903),
        BlendFactor::OneMinusSrc1Alpha
    );
}

#[test]
fn test_cull_face_values() {
    assert_eq!(CullFace::from_raw(0x0404), CullFace::Front);
    assert_eq!(CullFace::from_raw(0x0405), CullFace::Back);
    assert_eq!(CullFace::from_raw(0x0408), CullFace::FrontAndBack);
    // Unknown defaults to Back
    assert_eq!(CullFace::from_raw(0x0000), CullFace::Back);
}

#[test]
fn test_polygon_mode_values() {
    assert_eq!(PolygonMode::from_raw(0x1B00), PolygonMode::Point);
    assert_eq!(PolygonMode::from_raw(0x1B01), PolygonMode::Line);
    assert_eq!(PolygonMode::from_raw(0x1B02), PolygonMode::Fill);
    // Unknown defaults to Fill
    assert_eq!(PolygonMode::from_raw(0x0000), PolygonMode::Fill);
}

// ── Blend tests ──────────────────────────────────────────────────────

#[test]
fn test_blend_color_accessor() {
    let mut engine = Maxwell3D::new();
    let base = BLEND_COLOR_BASE as usize;
    engine.regs[base] = f32::to_bits(0.2);
    engine.regs[base + 1] = f32::to_bits(0.4);
    engine.regs[base + 2] = f32::to_bits(0.6);
    engine.regs[base + 3] = f32::to_bits(0.8);

    let bc = engine.blend_color_info();
    assert_eq!(bc.r, 0.2);
    assert_eq!(bc.g, 0.4);
    assert_eq!(bc.b, 0.6);
    assert_eq!(bc.a, 0.8);
}

#[test]
fn test_global_blend_info() {
    let mut engine = Maxwell3D::new();
    let base = BLEND_BASE as usize;

    // Enable blend for RT0.
    engine.regs[base + 9] = 1; // enable[0]
                               // Set separate alpha, color Add SrcAlpha/OneMinusSrcAlpha, alpha Add One/Zero.
    engine.regs[base] = 1; // separate_alpha
    engine.regs[base + 1] = 1; // color_op = Add (D3D)
    engine.regs[base + 2] = 0x05; // color_src = SrcAlpha (D3D)
    engine.regs[base + 3] = 0x06; // color_dst = OneMinusSrcAlpha (D3D)
    engine.regs[base + 4] = 1; // alpha_op = Add
    engine.regs[base + 5] = 0x02; // alpha_src = One
    engine.regs[base + 7] = 0x01; // alpha_dst = Zero

    let bi = engine.global_blend_info(0);
    assert!(bi.enabled);
    assert!(bi.separate_alpha);
    assert_eq!(bi.color_op, BlendEquation::Add);
    assert_eq!(bi.color_src, BlendFactor::SrcAlpha);
    assert_eq!(bi.color_dst, BlendFactor::OneMinusSrcAlpha);
    assert_eq!(bi.alpha_op, BlendEquation::Add);
    assert_eq!(bi.alpha_src, BlendFactor::One);
    assert_eq!(bi.alpha_dst, BlendFactor::Zero);
}

#[test]
fn test_blend_enable_per_rt() {
    let mut engine = Maxwell3D::new();
    let base = BLEND_BASE as usize;

    // Enable RT0, RT3, RT7.
    engine.regs[base + 9] = 1; // RT0
    engine.regs[base + 12] = 1; // RT3
    engine.regs[base + 16] = 1; // RT7

    assert!(engine.blend_enable(0));
    assert!(!engine.blend_enable(1));
    assert!(!engine.blend_enable(2));
    assert!(engine.blend_enable(3));
    assert!(!engine.blend_enable(4));
    assert!(!engine.blend_enable(5));
    assert!(!engine.blend_enable(6));
    assert!(engine.blend_enable(7));
}

#[test]
fn test_blend_per_target_info() {
    let mut engine = Maxwell3D::new();

    // Enable per-target blend override.
    engine.regs[BLEND_PER_TARGET_ENABLED as usize] = 1;

    // Set per-target blend for RT2.
    let rt2_base = (BLEND_PER_TARGET_BASE + 2 * BLEND_PER_TARGET_STRIDE) as usize;
    engine.regs[rt2_base] = 0; // no separate_alpha
    engine.regs[rt2_base + 1] = 2; // color_op = Subtract
    engine.regs[rt2_base + 2] = 0x09; // color_src = DstColor
    engine.regs[rt2_base + 3] = 0x01; // color_dst = Zero
    engine.regs[rt2_base + 4] = 1; // alpha_op = Add
    engine.regs[rt2_base + 5] = 0x02; // alpha_src = One
    engine.regs[rt2_base + 6] = 0x02; // alpha_dst = One

    // Enable RT2 blend.
    engine.regs[(BLEND_BASE + 11) as usize] = 1; // enable[2]

    let bi = engine.effective_blend_info(2);
    assert!(bi.enabled);
    assert!(!bi.separate_alpha);
    assert_eq!(bi.color_op, BlendEquation::Subtract);
    assert_eq!(bi.color_src, BlendFactor::DstColor);
    assert_eq!(bi.color_dst, BlendFactor::Zero);
}

#[test]
fn iterated_blend_enable_uses_upstream_register_bit() {
    let mut engine = Maxwell3D::new();
    assert!(!engine.iterated_blend_enabled());

    engine.regs[ITERATED_BLEND as usize] = 0b10;
    assert!(!engine.iterated_blend_enabled());

    engine.regs[ITERATED_BLEND as usize] = 0b11;
    assert!(engine.iterated_blend_enabled());
}

// ── Depth/Stencil tests ──────────────────────────────────────────────

#[test]
fn test_depth_state() {
    let mut engine = Maxwell3D::new();
    engine.regs[DEPTH_TEST_ENABLE as usize] = 1;
    engine.regs[DEPTH_WRITE_ENABLE as usize] = 1;
    engine.regs[DEPTH_TEST_FUNC as usize] = 2; // Less (D3D)
    engine.regs[DEPTH_MODE as usize] = 1; // ZeroToOne

    let ds = engine.depth_stencil_info();
    assert!(ds.depth_test_enable);
    assert!(ds.depth_write_enable);
    assert_eq!(ds.depth_func, ComparisonOp::Less);
    assert_eq!(ds.depth_mode, DepthMode::ZeroToOne);
}

#[test]
fn test_stencil_front_state() {
    let mut engine = Maxwell3D::new();
    engine.regs[STENCIL_ENABLE as usize] = 1;

    let front_base = STENCIL_FRONT_OP_BASE as usize;
    engine.regs[front_base] = 1; // fail = Keep (D3D)
    engine.regs[front_base + 1] = 1; // zfail = Keep
    engine.regs[front_base + 2] = 3; // zpass = Replace
    engine.regs[front_base + 3] = 8; // func = Always
    engine.regs[STENCIL_FRONT_REF as usize] = 0xFF;
    engine.regs[STENCIL_FRONT_FUNC_MASK as usize] = 0xFF;
    engine.regs[STENCIL_FRONT_MASK as usize] = 0xFF;

    let ds = engine.depth_stencil_info();
    assert!(ds.stencil_enable);
    assert_eq!(ds.front.fail_op, StencilOp::Keep);
    assert_eq!(ds.front.zpass_op, StencilOp::Replace);
    assert_eq!(ds.front.func, ComparisonOp::Always);
    assert_eq!(ds.front.ref_value, 0xFF);
    assert_eq!(ds.front.func_mask, 0xFF);
    assert_eq!(ds.front.write_mask, 0xFF);
}

#[test]
fn test_stencil_two_side() {
    let mut engine = Maxwell3D::new();
    engine.regs[STENCIL_ENABLE as usize] = 1;
    engine.regs[STENCIL_TWO_SIDE_ENABLE as usize] = 1;

    // Front: Replace on pass.
    let front_base = STENCIL_FRONT_OP_BASE as usize;
    engine.regs[front_base + 2] = 3; // zpass = Replace
    engine.regs[front_base + 3] = 8; // func = Always

    // Back: Invert on pass.
    let back_base = STENCIL_BACK_OP_BASE as usize;
    engine.regs[back_base + 2] = 6; // zpass = Invert
    engine.regs[back_base + 3] = 2; // func = Less
    engine.regs[STENCIL_BACK_REF as usize] = 0x80;

    let ds = engine.depth_stencil_info();
    assert!(ds.stencil_two_side);
    assert_eq!(ds.front.zpass_op, StencilOp::Replace);
    assert_eq!(ds.back.zpass_op, StencilOp::Invert);
    assert_eq!(ds.back.func, ComparisonOp::Less);
    assert_eq!(ds.back.ref_value, 0x80);
}

// ── Rasterizer tests ─────────────────────────────────────────────────

#[test]
fn test_rasterizer_state() {
    let mut engine = Maxwell3D::new();
    engine.regs[CULL_TEST_ENABLE as usize] = 1;
    engine.regs[FRONT_FACE as usize] = 0x0901; // CCW
    engine.regs[CULL_FACE as usize] = 0x0405; // Back
    engine.regs[POLYGON_MODE_FRONT as usize] = 0x1B02; // Fill
    engine.regs[POLYGON_MODE_BACK as usize] = 0x1B02; // Fill
    engine.regs[LINE_WIDTH_SMOOTH as usize] = f32::to_bits(1.0);
    engine.regs[LINE_WIDTH_ALIASED as usize] = f32::to_bits(1.0);

    let ri = engine.rasterizer_info();
    assert!(ri.cull_enable);
    assert_eq!(ri.front_face, FrontFace::CCW);
    assert_eq!(ri.cull_face, CullFace::Back);
    assert_eq!(ri.polygon_mode_front, PolygonMode::Fill);
    assert_eq!(ri.polygon_mode_back, PolygonMode::Fill);
    assert_eq!(ri.line_width_smooth, 1.0);
}

#[test]
fn test_rasterizer_wireframe() {
    let mut engine = Maxwell3D::new();
    engine.regs[POLYGON_MODE_FRONT as usize] = 0x1B01; // Line
    engine.regs[POLYGON_MODE_BACK as usize] = 0x1B01; // Line
    engine.regs[DEPTH_BIAS as usize] = f32::to_bits(0.5);
    engine.regs[SLOPE_SCALE_DEPTH_BIAS as usize] = f32::to_bits(1.5);
    engine.regs[DEPTH_BIAS_CLAMP as usize] = f32::to_bits(0.01);

    let ri = engine.rasterizer_info();
    assert_eq!(ri.polygon_mode_front, PolygonMode::Line);
    assert_eq!(ri.polygon_mode_back, PolygonMode::Line);
    assert_eq!(ri.depth_bias, 0.5);
    assert_eq!(ri.slope_scale_depth_bias, 1.5);
    assert_eq!(ri.depth_bias_clamp, 0.01);
}

#[test]
fn rasterizer_default_front_face_matches_upstream_nvn_default() {
    let engine = Maxwell3D::new();
    let ri = engine.rasterizer_info();
    assert_eq!(ri.front_face, FrontFace::CW);
}

// ── Constant buffer tests ────────────────────────────────────────────

#[test]
fn test_cb_bind() {
    let mut engine = Maxwell3D::new();

    // Set CB config: size=0x10000, addr=0x0000_0001_0000_0000.
    engine.write_reg(CB_CONFIG_BASE, 0x10000); // size
    engine.write_reg(CB_CONFIG_BASE + 1, 0x0001); // addr_high
    engine.write_reg(CB_CONFIG_BASE + 2, 0x0000_0000); // addr_low

    // Bind to stage 0 (vertex), slot 3: raw_config = valid | (3 << 4).
    let raw_config = 1 | (3 << 4);
    engine.write_reg(CB_BIND_TRIGGER_0, raw_config);

    let bindings = engine.const_buffer_bindings(0);
    assert!(bindings[3].enabled);
    assert_eq!(bindings[3].address, 0x0001_0000_0000);
    assert_eq!(bindings[3].size, 0x10000);
    // Other slots should be disabled.
    assert!(!bindings[0].enabled);
    assert!(!bindings[1].enabled);
}

#[test]
fn disabling_cb_keeps_the_current_address_and_size_like_upstream() {
    let mut engine = Maxwell3D::new();

    engine.write_reg(CB_CONFIG_BASE, 0x400);
    engine.write_reg(CB_CONFIG_BASE + 1, 0x2);
    engine.write_reg(CB_CONFIG_BASE + 2, 0x3000);
    engine.write_reg(CB_BIND_TRIGGER_0, 3 << 4);

    let binding = engine.const_buffer_bindings(0)[3];
    assert!(!binding.enabled);
    assert_eq!(binding.address, 0x2_0000_3000);
    assert_eq!(binding.size, 0x400);
}

#[test]
fn test_cb_data_increments_offset() {
    let mut engine = Maxwell3D::new();

    // Upstream asserts that the configured buffer address is non-zero and
    // that the current offset does not exceed its size.
    engine.write_reg(CB_CONFIG_BASE, 0x200);
    engine.write_reg(CB_CONFIG_BASE + 1, 0);
    engine.write_reg(CB_CONFIG_BASE + 2, 0x1000);
    engine.write_reg(CB_CONFIG_BASE + 3, 0x100); // offset = 0x100

    // Write CB_DATA — should auto-increment offset by 4 each time.
    engine.write_reg(CB_DATA_BASE, 0xAAAA);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 0x104);

    engine.write_reg(CB_DATA_BASE, 0xBBBB);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 0x108);
}

#[test]
fn test_cb_bind_multiple_stages() {
    let mut engine = Maxwell3D::new();

    // Bind CB to stage 0 slot 0.
    engine.write_reg(CB_CONFIG_BASE, 256);
    engine.write_reg(CB_CONFIG_BASE + 1, 0);
    engine.write_reg(CB_CONFIG_BASE + 2, 0x1000);
    engine.write_reg(CB_BIND_TRIGGER_0, 1 | (0 << 4));

    // Bind CB to stage 4 (fragment) slot 5.
    engine.write_reg(CB_CONFIG_BASE, 512);
    engine.write_reg(CB_CONFIG_BASE + 1, 0);
    engine.write_reg(CB_CONFIG_BASE + 2, 0x2000);
    engine.write_reg(CB_BIND_TRIGGER_4, 1 | (5 << 4));

    assert!(engine.const_buffer_bindings(0)[0].enabled);
    assert_eq!(engine.const_buffer_bindings(0)[0].address, 0x1000);
    assert_eq!(engine.const_buffer_bindings(0)[0].size, 256);

    assert!(engine.const_buffer_bindings(4)[5].enabled);
    assert_eq!(engine.const_buffer_bindings(4)[5].address, 0x2000);
    assert_eq!(engine.const_buffer_bindings(4)[5].size, 512);

    // Other stages should be unaffected.
    assert!(!engine.const_buffer_bindings(1)[0].enabled);
    assert!(!engine.const_buffer_bindings(2)[0].enabled);
}

// ── Shader program test ──────────────────────────────────────────────

#[test]
fn test_program_base_address() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(PROGRAM_REGION_BASE, 0x0002); // addr_high
    engine.write_reg(PROGRAM_REGION_BASE + 1, 0xABCD_0000); // addr_low

    assert_eq!(engine.program_base_address(), 0x0002_ABCD_0000);
}

// ── Draw integration tests ───────────────────────────────────────────

#[test]
fn test_draw_captures_depth_stencil() {
    let mut engine = Maxwell3D::new();

    // Set depth state.
    engine.write_reg(DEPTH_TEST_ENABLE, 1);
    engine.write_reg(DEPTH_WRITE_ENABLE, 1);
    engine.write_reg(DEPTH_TEST_FUNC, 0x201); // Less (GL)
    engine.write_reg(DEPTH_MODE, 1); // ZeroToOne

    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    let ds = &draws[0].depth_stencil;
    assert!(ds.depth_test_enable);
    assert!(ds.depth_write_enable);
    assert_eq!(ds.depth_func, ComparisonOp::Less);
    assert_eq!(ds.depth_mode, DepthMode::ZeroToOne);
}

#[test]
fn test_draw_captures_blend() {
    let mut engine = Maxwell3D::new();
    let base = BLEND_BASE as usize;

    // Enable blend for RT0 with SrcAlpha/OneMinusSrcAlpha.
    engine.regs[base + 9] = 1; // enable[0]
    engine.regs[base + 1] = 1; // color_op = Add
    engine.regs[base + 2] = 0x05; // color_src = SrcAlpha
    engine.regs[base + 3] = 0x06; // color_dst = OneMinusSrcAlpha
    engine.regs[base + 4] = 1; // alpha_op = Add
    engine.regs[base + 5] = 0x02; // alpha_src = One
    engine.regs[base + 7] = 0x01; // alpha_dst = Zero

    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    assert!(draws[0].blend[0].enabled);
    assert_eq!(draws[0].blend[0].color_src, BlendFactor::SrcAlpha);
    assert_eq!(draws[0].blend[0].color_dst, BlendFactor::OneMinusSrcAlpha);
    // RT1 should not be enabled.
    assert!(!draws[0].blend[1].enabled);
}

// ── Vertex attribute enum tests ─────────────────────────────────────

#[test]
fn test_vertex_attrib_size_values() {
    assert_eq!(
        VertexAttribSize::from_raw(0x01),
        VertexAttribSize::R32G32B32A32
    );
    assert_eq!(
        VertexAttribSize::from_raw(0x02),
        VertexAttribSize::R32G32B32
    );
    assert_eq!(
        VertexAttribSize::from_raw(0x03),
        VertexAttribSize::R16G16B16A16
    );
    assert_eq!(VertexAttribSize::from_raw(0x04), VertexAttribSize::R32G32);
    assert_eq!(VertexAttribSize::from_raw(0x0A), VertexAttribSize::R8G8B8A8);
    assert_eq!(VertexAttribSize::from_raw(0x12), VertexAttribSize::R32);
    assert_eq!(VertexAttribSize::from_raw(0x1D), VertexAttribSize::R8);
    assert_eq!(
        VertexAttribSize::from_raw(0x30),
        VertexAttribSize::A2B10G10R10
    );
    assert_eq!(
        VertexAttribSize::from_raw(0x31),
        VertexAttribSize::B10G11R11
    );
    assert_eq!(VertexAttribSize::from_raw(0x34), VertexAttribSize::A8);
    assert_eq!(VertexAttribSize::from_raw(0xFF), VertexAttribSize::Invalid);
}

#[test]
fn test_vertex_attrib_size_and_type_raw_roundtrip() {
    // `to_raw` must be the exact inverse of `from_raw`: FixedPipelineState
    // packs these raw hardware values and reads them back via `from_raw`
    // (disk pipeline rebuild). Enum ordinals stored by mistake shifted
    // every type by one and mapped most sizes to Invalid.
    for raw in 0u32..=0x3F {
        let size = VertexAttribSize::from_raw(raw);
        if size != VertexAttribSize::Invalid {
            assert_eq!(size.to_raw(), raw, "size raw 0x{raw:X}");
        }
    }
    assert_eq!(VertexAttribSize::Invalid.to_raw(), 0);
    for raw in 0u32..=7 {
        let attrib_type = VertexAttribType::from_raw(raw);
        if attrib_type != VertexAttribType::Invalid {
            assert_eq!(attrib_type.to_raw(), raw, "type raw {raw}");
        }
    }
    assert_eq!(VertexAttribType::Invalid.to_raw(), 0);
}

#[test]
fn test_vertex_attrib_size_bytes() {
    assert_eq!(VertexAttribSize::R32G32B32A32.size_bytes(), 16);
    assert_eq!(VertexAttribSize::R32G32B32.size_bytes(), 12);
    assert_eq!(VertexAttribSize::R16G16B16A16.size_bytes(), 8);
    assert_eq!(VertexAttribSize::R32G32.size_bytes(), 8);
    assert_eq!(VertexAttribSize::R8G8B8A8.size_bytes(), 4);
    assert_eq!(VertexAttribSize::R32.size_bytes(), 4);
    assert_eq!(VertexAttribSize::R8.size_bytes(), 1);
    assert_eq!(VertexAttribSize::A2B10G10R10.size_bytes(), 4);
    assert_eq!(VertexAttribSize::Invalid.size_bytes(), 0);
}

#[test]
fn test_vertex_attrib_size_component_count() {
    assert_eq!(VertexAttribSize::R32G32B32A32.component_count(), 4);
    assert_eq!(VertexAttribSize::R32G32B32.component_count(), 3);
    assert_eq!(VertexAttribSize::R32G32.component_count(), 2);
    assert_eq!(VertexAttribSize::R32.component_count(), 1);
    assert_eq!(VertexAttribSize::R8G8B8A8.component_count(), 4);
    assert_eq!(VertexAttribSize::B10G11R11.component_count(), 3);
    assert_eq!(VertexAttribSize::G8R8.component_count(), 2);
    assert_eq!(VertexAttribSize::A8.component_count(), 1);
    // Upstream asserts on an unknown size and returns 1; a component count
    // of 0 is not something `glVertexAttribFormat` accepts.
    assert_eq!(VertexAttribSize::Invalid.component_count(), 1);
}

#[test]
fn test_vertex_attrib_type_values() {
    assert_eq!(VertexAttribType::from_raw(1), VertexAttribType::SNorm);
    assert_eq!(VertexAttribType::from_raw(2), VertexAttribType::UNorm);
    assert_eq!(VertexAttribType::from_raw(3), VertexAttribType::SInt);
    assert_eq!(VertexAttribType::from_raw(4), VertexAttribType::UInt);
    assert_eq!(VertexAttribType::from_raw(5), VertexAttribType::UScaled);
    assert_eq!(VertexAttribType::from_raw(6), VertexAttribType::SScaled);
    assert_eq!(VertexAttribType::from_raw(7), VertexAttribType::Float);
    assert_eq!(VertexAttribType::from_raw(0), VertexAttribType::Invalid);
    assert_eq!(VertexAttribType::from_raw(99), VertexAttribType::Invalid);
}

#[test]
fn test_shader_stage_type_values() {
    assert_eq!(ShaderStageType::from_raw(0), ShaderStageType::VertexA);
    assert_eq!(ShaderStageType::from_raw(1), ShaderStageType::VertexB);
    assert_eq!(ShaderStageType::from_raw(2), ShaderStageType::TessInit);
    assert_eq!(ShaderStageType::from_raw(3), ShaderStageType::Tessellation);
    assert_eq!(ShaderStageType::from_raw(4), ShaderStageType::Geometry);
    assert_eq!(ShaderStageType::from_raw(5), ShaderStageType::Fragment);
    assert_eq!(ShaderStageType::from_raw(99), ShaderStageType::Invalid);
}

// ── Vertex attribute accessor tests ─────────────────────────────────

#[test]
fn test_vertex_attrib_info() {
    let mut engine = Maxwell3D::new();

    // Attrib 0: buffer=3, not constant, offset=16, size=R32G32B32A32(0x01),
    // type=Float(7), no bgra.
    // bits[4:0]=3, bit[6]=0, bits[20:7]=16, bits[26:21]=0x01, bits[29:27]=7, bit[31]=0
    let raw = 3u32 | (16 << 7) | (0x01 << 21) | (7 << 27);
    engine.regs[VERTEX_ATTRIB_BASE as usize] = raw;

    assert_eq!(engine.vertex_attrib_raw(0), raw);
    let info = engine.vertex_attrib_info(0);
    assert_eq!(info.buffer_index, 3);
    assert!(!info.constant);
    assert_eq!(info.offset, 16);
    assert_eq!(info.size, VertexAttribSize::R32G32B32A32);
    assert_eq!(info.attrib_type, VertexAttribType::Float);
    assert!(!info.bgra);
}

#[test]
fn test_vertex_attrib_constant_bgra() {
    let mut engine = Maxwell3D::new();

    // Attrib 5: buffer=0, constant=true, offset=0, size=R8G8B8A8(0x0A),
    // type=UNorm(2), bgra=true.
    let raw = 0u32
        | (1 << 6)       // constant
        | (0x0A << 21)   // R8G8B8A8
        | (2 << 27)      // UNorm
        | (1 << 31); // bgra
    engine.regs[(VERTEX_ATTRIB_BASE + 5) as usize] = raw;

    assert_eq!(engine.vertex_attrib_raw(5), raw);
    let info = engine.vertex_attrib_info(5);
    assert_eq!(info.buffer_index, 0);
    assert!(info.constant);
    assert_eq!(info.offset, 0);
    assert_eq!(info.size, VertexAttribSize::R8G8B8A8);
    assert_eq!(info.attrib_type, VertexAttribType::UNorm);
    assert!(info.bgra);
}

// ── Shader stage accessor tests ─────────────────────────────────────

#[test]
fn test_shader_stage_info() {
    let mut engine = Maxwell3D::new();
    let base = (PIPELINE_BASE + 1 * PIPELINE_STRIDE) as usize; // VertexB slot

    // word0: enabled=1, type=VertexB(1) at bits[7:4]
    engine.regs[base] = 1 | (1 << 4);
    engine.regs[base + 1] = 0x100; // offset
    engine.regs[base + 3] = 64; // register_count
    engine.regs[base + 4] = 0; // binding_group

    let info = engine.shader_stage_info(1);
    assert!(info.enabled);
    assert_eq!(info.program_type, ShaderStageType::VertexB);
    assert_eq!(info.offset, 0x100);
    assert_eq!(info.register_count, 64);
    assert_eq!(info.binding_group, 0);
}

#[test]
fn test_shader_stage_fragment() {
    let mut engine = Maxwell3D::new();
    let base = (PIPELINE_BASE + 5 * PIPELINE_STRIDE) as usize; // Fragment slot

    engine.regs[base] = 1 | (5 << 4); // enabled, Fragment
    engine.regs[base + 1] = 0x500;
    engine.regs[base + 3] = 32;
    engine.regs[base + 4] = 2;

    let info = engine.shader_stage_info(5);
    assert!(info.enabled);
    assert_eq!(info.program_type, ShaderStageType::Fragment);
    assert_eq!(info.offset, 0x500);
    assert_eq!(info.register_count, 32);
    assert_eq!(info.binding_group, 2);
}

#[test]
fn test_shader_stage_vertexb_always_enabled() {
    let engine = Maxwell3D::new();
    // VertexB (index 1) always returns enabled even with zero registers.
    assert!(engine.is_shader_stage_enabled(1));
    assert!(engine.shader_stage_info(1).enabled);
    // Other stages default to disabled.
    assert!(!engine.is_shader_stage_enabled(0));
    assert!(!engine.is_shader_stage_enabled(2));
    assert!(!engine.is_shader_stage_enabled(5));
}

#[test]
fn shader_config_enabled_matches_upstream_vertexb_and_enable_bit() {
    let mut engine = Maxwell3D::new();

    assert!(!engine.shader_config_enabled(ShaderStageType::VertexA));
    assert!(engine.shader_config_enabled(ShaderStageType::VertexB));
    assert!(!engine.shader_config_enabled(ShaderStageType::TessInit));
    assert!(!engine.shader_config_enabled(ShaderStageType::Tessellation));

    let tess_init = (PIPELINE_BASE + 2 * PIPELINE_STRIDE) as usize;
    let tess = (PIPELINE_BASE + 3 * PIPELINE_STRIDE) as usize;
    engine.regs[tess_init] = 1 | (2 << 4);
    engine.regs[tess] = 1 | (3 << 4);

    assert!(engine.shader_config_enabled(ShaderStageType::TessInit));
    assert!(engine.shader_config_enabled(ShaderStageType::Tessellation));
    assert!(!engine.shader_config_enabled(ShaderStageType::Invalid));
}

// ── Color mask tests ────────────────────────────────────────────────

#[test]
fn test_color_mask_info() {
    let mut engine = Maxwell3D::new();
    // RT0: R and A only. R=bit[0], G=bit[4], B=bit[8], A=bit[12].
    engine.regs[COLOR_MASK_BASE as usize] = (1 << 0) | (1 << 12);

    let mask = engine.color_mask_info(0);
    assert!(mask.r);
    assert!(!mask.g);
    assert!(!mask.b);
    assert!(mask.a);
}

#[test]
fn test_color_mask_common() {
    let mut engine = Maxwell3D::new();
    // Enable common mask mode.
    engine.regs[COLOR_MASK_COMMON as usize] = 1;
    // Set mask[0] to G+B only.
    engine.regs[COLOR_MASK_BASE as usize] = (1 << 4) | (1 << 8);
    // Set mask[3] differently (should be ignored in common mode).
    engine.regs[(COLOR_MASK_BASE + 3) as usize] = 0xFFFF;

    let mask3 = engine.color_mask_info(3);
    // Should use mask[0], not mask[3].
    assert!(!mask3.r);
    assert!(mask3.g);
    assert!(mask3.b);
    assert!(!mask3.a);
}

#[test]
fn test_color_mask_per_rt() {
    let mut engine = Maxwell3D::new();
    // Common mode off (default).
    // RT0: all channels.
    engine.regs[COLOR_MASK_BASE as usize] = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 12);
    // RT2: R only.
    engine.regs[(COLOR_MASK_BASE + 2) as usize] = 1 << 0;

    let mask0 = engine.color_mask_info(0);
    assert!(mask0.r && mask0.g && mask0.b && mask0.a);

    let mask2 = engine.color_mask_info(2);
    assert!(mask2.r);
    assert!(!mask2.g);
    assert!(!mask2.b);
    assert!(!mask2.a);
}

// ── RT control tests ────────────────────────────────────────────────

#[test]
fn test_rt_control_info() {
    let mut engine = Maxwell3D::new();
    // count=2, map[0]=0, map[1]=1 (identity).
    // bits[3:0]=2, bits[6:4]=0, bits[9:7]=1
    engine.regs[RT_CONTROL as usize] = 2 | (0 << 4) | (1 << 7);

    let rtc = engine.rt_control_info();
    assert_eq!(rtc.count, 2);
    assert_eq!(rtc.map[0], 0);
    assert_eq!(rtc.map[1], 1);
}

#[test]
fn test_rt_control_swizzled() {
    let mut engine = Maxwell3D::new();
    // count=3, map[0]=2, map[1]=0, map[2]=1 (swizzled).
    // bits[3:0]=3, bits[6:4]=2, bits[9:7]=0, bits[12:10]=1
    engine.regs[RT_CONTROL as usize] = 3 | (2 << 4) | (0 << 7) | (1 << 10);

    let rtc = engine.rt_control_info();
    assert_eq!(rtc.count, 3);
    assert_eq!(rtc.map[0], 2);
    assert_eq!(rtc.map[1], 0);
    assert_eq!(rtc.map[2], 1);
}

// ── Draw integration tests for new state ────────────────────────────

#[test]
fn test_draw_captures_vertex_attribs() {
    let mut engine = Maxwell3D::new();

    // Set attrib 0: buffer=0, offset=0, R32G32B32(0x02), Float(7).
    let raw0 = 0u32 | (0x02 << 21) | (7 << 27);
    engine.write_reg(VERTEX_ATTRIB_BASE, raw0);

    // Set attrib 1: buffer=0, offset=12, R8G8B8A8(0x0A), UNorm(2).
    let raw1 = 0u32 | (12 << 7) | (0x0A << 21) | (2 << 27);
    engine.write_reg(VERTEX_ATTRIB_BASE + 1, raw1);

    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws[0].vertex_attribs.len(), NUM_VERTEX_ATTRIBS as usize);
    assert_eq!(draws[0].vertex_attribs[0].size, VertexAttribSize::R32G32B32);
    assert_eq!(
        draws[0].vertex_attribs[0].attrib_type,
        VertexAttribType::Float
    );
    assert_eq!(draws[0].vertex_attribs[1].offset, 12);
    assert_eq!(draws[0].vertex_attribs[1].size, VertexAttribSize::R8G8B8A8);
}

#[test]
fn test_draw_captures_shader_stages() {
    let mut engine = Maxwell3D::new();

    // Enable VertexB (slot 1) and Fragment (slot 5).
    let vb_base = PIPELINE_BASE + 1 * PIPELINE_STRIDE;
    engine.write_reg(vb_base, 1 | (1 << 4));
    engine.write_reg(vb_base + 1, 0x100);
    engine.write_reg(vb_base + 3, 64);

    let frag_base = PIPELINE_BASE + 5 * PIPELINE_STRIDE;
    engine.write_reg(frag_base, 1 | (5 << 4));
    engine.write_reg(frag_base + 1, 0x500);
    engine.write_reg(frag_base + 3, 32);

    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert!(draws[0].shader_stages[1].enabled);
    assert_eq!(
        draws[0].shader_stages[1].program_type,
        ShaderStageType::VertexB
    );
    assert_eq!(draws[0].shader_stages[1].offset, 0x100);
    assert!(draws[0].shader_stages[5].enabled);
    assert_eq!(
        draws[0].shader_stages[5].program_type,
        ShaderStageType::Fragment
    );
    // Slot 0 should be disabled.
    assert!(!draws[0].shader_stages[0].enabled);
}

#[test]
fn test_draw_captures_color_masks_and_rt_control() {
    let mut engine = Maxwell3D::new();

    // Set RT0 mask: R+G only.
    engine.write_reg(COLOR_MASK_BASE, (1 << 0) | (1 << 4));
    // Set RT control: count=1, map[0]=0.
    engine.write_reg(RT_CONTROL, 1 | (0 << 4));

    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert!(draws[0].color_masks[0].r);
    assert!(draws[0].color_masks[0].g);
    assert!(!draws[0].color_masks[0].b);
    assert!(!draws[0].color_masks[0].a);
    assert_eq!(draws[0].rt_control.count, 1);
    assert_eq!(draws[0].rt_control.map[0], 0);
}

// ── Texture/Sampler descriptor tests ─────────────────────────────────

#[test]
fn test_texture_format_values() {
    assert_eq!(TextureFormat::from_raw(0x01), TextureFormat::R32G32B32A32);
    assert_eq!(TextureFormat::from_raw(0x09), TextureFormat::R32);
    assert_eq!(TextureFormat::from_raw(0x1D), TextureFormat::A8B8G8R8);
    assert_eq!(TextureFormat::from_raw(0x24), TextureFormat::R8G8B8A8);
    assert_eq!(TextureFormat::from_raw(0x12), TextureFormat::R8);
    assert_eq!(TextureFormat::from_raw(0x7F), TextureFormat::Invalid);
}

#[test]
fn test_texture_format_compressed() {
    assert_eq!(TextureFormat::from_raw(0x25), TextureFormat::Bc1Rgba);
    assert_eq!(TextureFormat::from_raw(0x27), TextureFormat::Bc3);
    assert_eq!(TextureFormat::from_raw(0x2A), TextureFormat::Bc7);
    assert_eq!(TextureFormat::from_raw(0x40), TextureFormat::Astc2d4x4);
    assert_eq!(TextureFormat::from_raw(0x4D), TextureFormat::Astc2d12x12);
}

#[test]
fn test_texture_type_values() {
    assert_eq!(TextureType::from_raw(0), TextureType::Texture1D);
    assert_eq!(TextureType::from_raw(1), TextureType::Texture2D);
    assert_eq!(TextureType::from_raw(2), TextureType::Texture3D);
    assert_eq!(TextureType::from_raw(3), TextureType::Cubemap);
    assert_eq!(TextureType::from_raw(5), TextureType::Array2D);
    assert_eq!(TextureType::from_raw(6), TextureType::Buffer1D);
    assert_eq!(TextureType::from_raw(8), TextureType::CubemapArray);
    assert_eq!(TextureType::from_raw(9), TextureType::Invalid);
}

#[test]
fn test_component_type_values() {
    assert_eq!(ComponentType::from_raw(0), ComponentType::Invalid);
    assert_eq!(ComponentType::from_raw(1), ComponentType::SNorm);
    assert_eq!(ComponentType::from_raw(2), ComponentType::UNorm);
    assert_eq!(ComponentType::from_raw(3), ComponentType::SInt);
    assert_eq!(ComponentType::from_raw(4), ComponentType::UInt);
    assert_eq!(ComponentType::from_raw(7), ComponentType::Float);
}

#[test]
fn test_swizzle_source_values() {
    assert_eq!(SwizzleSource::from_raw(0), SwizzleSource::Zero);
    assert_eq!(SwizzleSource::from_raw(2), SwizzleSource::R);
    assert_eq!(SwizzleSource::from_raw(3), SwizzleSource::G);
    assert_eq!(SwizzleSource::from_raw(4), SwizzleSource::B);
    assert_eq!(SwizzleSource::from_raw(5), SwizzleSource::A);
    assert_eq!(SwizzleSource::from_raw(7), SwizzleSource::OneFloat);
    assert_eq!(SwizzleSource::from_raw(1), SwizzleSource::Invalid);
}

#[test]
fn test_tic_header_version_values() {
    assert_eq!(TicHeaderVersion::from_raw(0), TicHeaderVersion::OneDBuffer);
    assert_eq!(TicHeaderVersion::from_raw(2), TicHeaderVersion::Pitch);
    assert_eq!(TicHeaderVersion::from_raw(3), TicHeaderVersion::BlockLinear);
    assert_eq!(TicHeaderVersion::from_raw(5), TicHeaderVersion::Invalid);
}

#[test]
fn test_wrap_mode_values() {
    assert_eq!(WrapMode::from_raw(0), WrapMode::Wrap);
    assert_eq!(WrapMode::from_raw(1), WrapMode::Mirror);
    assert_eq!(WrapMode::from_raw(2), WrapMode::ClampToEdge);
    assert_eq!(WrapMode::from_raw(3), WrapMode::Border);
    assert_eq!(WrapMode::from_raw(4), WrapMode::Clamp);
    assert_eq!(WrapMode::from_raw(7), WrapMode::MirrorOnceClampOgl);
}

#[test]
fn test_texture_filter_values() {
    assert_eq!(TextureFilter::from_raw(0), TextureFilter::Invalid);
    assert_eq!(TextureFilter::from_raw(1), TextureFilter::Nearest);
    assert_eq!(TextureFilter::from_raw(2), TextureFilter::Linear);
    assert_eq!(TextureFilter::from_raw(3), TextureFilter::Invalid);
}

#[test]
fn test_mipmap_filter_values() {
    assert_eq!(MipmapFilter::from_raw(0), MipmapFilter::Invalid);
    assert_eq!(MipmapFilter::from_raw(1), MipmapFilter::None);
    assert_eq!(MipmapFilter::from_raw(2), MipmapFilter::Nearest);
    assert_eq!(MipmapFilter::from_raw(3), MipmapFilter::Linear);
}

#[test]
fn test_depth_compare_func_values() {
    assert_eq!(DepthCompareFunc::from_raw(0), DepthCompareFunc::Never);
    assert_eq!(DepthCompareFunc::from_raw(1), DepthCompareFunc::Less);
    assert_eq!(DepthCompareFunc::from_raw(3), DepthCompareFunc::LessEqual);
    assert_eq!(DepthCompareFunc::from_raw(7), DepthCompareFunc::Always);
}

#[test]
fn test_texture_descriptor_basic() {
    // Build a basic 2D RGBA8 texture descriptor.
    let mut words = [0u32; 8];
    // word0: format=0x1D(A8B8G8R8), r_type=UNorm(2), g_type=UNorm(2), b_type=UNorm(2),
    //        a_type=UNorm(2), xyzw swizzle = R(2),G(3),B(4),A(5)
    words[0] = 0x1D
        | (2 << 7)   // r_type = UNorm
        | (2 << 10)  // g_type = UNorm
        | (2 << 13)  // b_type = UNorm
        | (2 << 16)  // a_type = UNorm
        | (2 << 19)  // x_source = R
        | (3 << 22)  // y_source = G
        | (4 << 25)  // z_source = B
        | (5 << 28); // w_source = A
                     // word1: addr_low
    words[1] = 0x0010_0000;
    // word2: addr_high[15:0]=0x0001, header_version=BlockLinear(3) at bits[23:21]
    words[2] = 0x0001 | (3 << 21);
    // word3: max_mip_level=5 at bits[31:28]
    words[3] = 5 << 28;
    // word4: width=1279(+1=1280) at [15:0], texture_type=Texture2D(1) at [26:23]
    words[4] = 1279 | (1 << 23);
    // word5: height=719(+1=720) at [15:0], depth=0(+1=1) at [29:16], normalized=1 at [31]
    words[5] = 719 | (1 << 31);

    let desc = TextureDescriptor::from_words(&words);
    assert_eq!(desc.format, TextureFormat::A8B8G8R8);
    assert_eq!(desc.r_type, ComponentType::UNorm);
    assert_eq!(desc.g_type, ComponentType::UNorm);
    assert_eq!(desc.x_source, SwizzleSource::R);
    assert_eq!(desc.w_source, SwizzleSource::A);
    assert_eq!(desc.address, 0x0001_0010_0000);
    assert_eq!(desc.header_version, TicHeaderVersion::BlockLinear);
    assert_eq!(desc.texture_type, TextureType::Texture2D);
    assert_eq!(desc.width, 1280);
    assert_eq!(desc.height, 720);
    assert_eq!(desc.depth, 1);
    assert_eq!(desc.max_mip_level, 5);
    assert_eq!(desc.block_height, 0);
    assert_eq!(desc.block_depth, 0);
    assert!(!desc.srgb_conversion);
    assert!(desc.normalized_coords);
}

#[test]
fn test_texture_descriptor_block_height_depth() {
    let mut words = [0u32; 8];
    words[0] = 0x1D; // A8B8G8R8
                     // word3: block_height=3 at bits[5:3], block_depth=2 at bits[8:6], max_mip=0
    words[3] = (3 << 3) | (2 << 6);
    let desc = TextureDescriptor::from_words(&words);
    assert_eq!(desc.block_height, 3);
    assert_eq!(desc.block_depth, 2);
}

#[test]
fn test_texture_descriptor_srgb_3d() {
    let mut words = [0u32; 8];
    words[0] = 0x1D; // A8B8G8R8, all other fields zero
                     // word4: srgb_conversion=1 at bit[22], texture_type=Texture3D(2) at [26:23], width=63(+1=64)
    words[4] = 63 | (1 << 22) | (2 << 23);
    // word5: height=63(+1=64), depth=31(+1=32) at [29:16]
    words[5] = 63 | (31 << 16);

    let desc = TextureDescriptor::from_words(&words);
    assert!(desc.srgb_conversion);
    assert_eq!(desc.texture_type, TextureType::Texture3D);
    assert_eq!(desc.width, 64);
    assert_eq!(desc.height, 64);
    assert_eq!(desc.depth, 32);
    assert!(!desc.normalized_coords);
}

#[test]
fn test_texture_descriptor_buffer() {
    let mut words = [0u32; 8];
    words[0] = 0x09; // R32
                     // word2: header_version=OneDBuffer(0) — already zero
                     // word4: texture_type=Buffer1D(6) at [26:23], width=255(+1=256)
    words[4] = 255 | (6 << 23);
    words[5] = 0; // height=0+1=1, depth=0+1=1

    let desc = TextureDescriptor::from_words(&words);
    assert_eq!(desc.format, TextureFormat::R32);
    assert_eq!(desc.header_version, TicHeaderVersion::OneDBuffer);
    assert_eq!(desc.texture_type, TextureType::Buffer1D);
    assert_eq!(desc.width, 256);
    assert_eq!(desc.height, 1);
    assert_eq!(desc.depth, 1);
}

#[test]
fn test_sampler_descriptor_basic() {
    let mut words = [0u32; 8];
    // word0: wrap_u=Wrap(0), wrap_v=ClampToEdge(2) at [5:3], wrap_p=Mirror(1) at [8:6]
    words[0] = 0 | (2 << 3) | (1 << 6);
    // word1: mag=Linear(2) at [1:0], min=Linear(2) at [5:4], mipmap=Linear(3) at [7:6]
    //        mip_lod_bias=0 at [24:12]
    words[1] = 2 | (2 << 4) | (3 << 6);
    // word2: min_lod=0, max_lod=3072 (=12.0*256) at [23:12]
    words[2] = 0 | (3072 << 12);
    // border color = [1.0, 0.5, 0.0, 1.0]
    words[4] = f32::to_bits(1.0);
    words[5] = f32::to_bits(0.5);
    words[6] = f32::to_bits(0.0);
    words[7] = f32::to_bits(1.0);

    let desc = SamplerDescriptor::from_words(&words);
    assert_eq!(desc.wrap_u, WrapMode::Wrap);
    assert_eq!(desc.wrap_v, WrapMode::ClampToEdge);
    assert_eq!(desc.wrap_p, WrapMode::Mirror);
    assert!(!desc.depth_compare_enabled);
    assert_eq!(desc.mag_filter, TextureFilter::Linear);
    assert_eq!(desc.min_filter, TextureFilter::Linear);
    assert_eq!(desc.mipmap_filter, MipmapFilter::Linear);
    assert!((desc.min_lod - 0.0).abs() < f32::EPSILON);
    assert!((desc.max_lod - 12.0).abs() < 0.01);
    assert!((desc.mip_lod_bias - 0.0).abs() < f32::EPSILON);
    assert_eq!(desc.border_color[0], 1.0);
    assert_eq!(desc.border_color[1], 0.5);
}

#[test]
fn test_sampler_descriptor_depth_compare() {
    let mut words = [0u32; 8];
    // word0: wrap_u=Border(3), depth_compare_enabled=1 at bit[9],
    //        depth_compare_func=Less(1) at [12:10], max_anisotropy=3 at [22:20]
    words[0] = 3 | (1 << 9) | (1 << 10) | (3 << 20);
    // word1: mag=Nearest(1), min=Nearest(1) at [5:4], mipmap=None(1) at [7:6],
    //        mip_lod_bias: -1.0 → -256 as 13-bit signed → 0x1F00 at [24:12]
    let bias_raw = ((-256i32) as u32) & 0x1FFF; // 13-bit
    words[1] = 1 | (1 << 4) | (1 << 6) | (bias_raw << 12);

    let desc = SamplerDescriptor::from_words(&words);
    assert!(desc.depth_compare_enabled);
    assert_eq!(desc.depth_compare_func, DepthCompareFunc::Less);
    assert_eq!(desc.max_anisotropy, 3);
    assert_eq!(desc.mag_filter, TextureFilter::Nearest);
    assert_eq!(desc.min_filter, TextureFilter::Nearest);
    assert_eq!(desc.mipmap_filter, MipmapFilter::None);
    assert!((desc.mip_lod_bias - (-1.0)).abs() < 0.01);
}

#[test]
fn test_sampler_anisotropy_multiplier() {
    let mut words = [0u32; 8];

    // anisotropy = 0 → 1x
    words[0] = 0;
    assert_eq!(
        SamplerDescriptor::from_words(&words).anisotropy_multiplier(),
        1
    );

    // anisotropy = 1 → 2x
    words[0] = 1 << 20;
    assert_eq!(
        SamplerDescriptor::from_words(&words).anisotropy_multiplier(),
        2
    );

    // anisotropy = 2 → 4x
    words[0] = 2 << 20;
    assert_eq!(
        SamplerDescriptor::from_words(&words).anisotropy_multiplier(),
        4
    );

    // anisotropy = 3 → 8x
    words[0] = 3 << 20;
    assert_eq!(
        SamplerDescriptor::from_words(&words).anisotropy_multiplier(),
        8
    );

    // anisotropy = 4 → 16x
    words[0] = 4 << 20;
    assert_eq!(
        SamplerDescriptor::from_words(&words).anisotropy_multiplier(),
        16
    );
}

#[test]
fn test_tex_header_pool_address() {
    let mut engine = Maxwell3D::new();
    let base = TEX_HEADER_POOL_BASE as usize;
    engine.regs[base] = 0x0002; // addr_high
    engine.regs[base + 1] = 0x4000; // addr_low
    engine.regs[base + 2] = 1024; // limit

    assert_eq!(engine.tex_header_pool_address(), 0x0002_0000_4000);
    assert_eq!(engine.tex_header_pool_limit(), 1024);
}

#[test]
fn test_tex_sampler_pool_address() {
    let mut engine = Maxwell3D::new();
    let base = TEX_SAMPLER_POOL_BASE as usize;
    engine.regs[base] = 0x0003; // addr_high
    engine.regs[base + 1] = 0x8000; // addr_low
    engine.regs[base + 2] = 512; // limit

    assert_eq!(engine.tex_sampler_pool_address(), 0x0003_0000_8000);
    assert_eq!(engine.tex_sampler_pool_limit(), 512);
}

#[test]
fn test_draw_captures_tex_pools() {
    let mut engine = Maxwell3D::new();

    // Set up TIC pool.
    let tic = TEX_HEADER_POOL_BASE as usize;
    engine.regs[tic] = 0x0001;
    engine.regs[tic + 1] = 0x2000;
    engine.regs[tic + 2] = 256;

    // Set up TSC pool.
    let tsc = TEX_SAMPLER_POOL_BASE as usize;
    engine.regs[tsc] = 0x0001;
    engine.regs[tsc + 1] = 0x3000;
    engine.regs[tsc + 2] = 128;

    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    assert_eq!(draws[0].tex_header_pool_addr, 0x0001_0000_2000);
    assert_eq!(draws[0].tex_header_pool_limit, 256);
    assert_eq!(draws[0].tex_sampler_pool_addr, 0x0001_0000_3000);
    assert_eq!(draws[0].tex_sampler_pool_limit, 128);
}

// ── Multi-viewport / multi-scissor tests ────────────────────────────

#[test]
fn test_viewport_default() {
    let vp = ViewportInfo::default();
    assert_eq!(vp.x, 0.0);
    assert_eq!(vp.y, 0.0);
    assert_eq!(vp.width, 0.0);
    assert_eq!(vp.height, 0.0);
    assert_eq!(vp.depth_near, 0.0);
    assert_eq!(vp.depth_far, 0.0);
}

#[test]
fn test_scissor_default() {
    let sc = ScissorInfo::default();
    assert!(!sc.enabled);
    assert_eq!(sc.min_x, 0);
    assert_eq!(sc.max_x, 0);
    assert_eq!(sc.min_y, 0);
    assert_eq!(sc.max_y, 0);
}

#[test]
fn test_viewport_info_nonzero_index() {
    let mut engine = Maxwell3D::new();

    // Set viewport 3 only.
    let vp3_base = VP_TRANSFORM_BASE + 3 * VP_TRANSFORM_STRIDE;
    engine.write_reg(vp3_base, f32::to_bits(400.0)); // scale_x
    engine.write_reg(vp3_base + 1, f32::to_bits(-300.0)); // scale_y
    engine.write_reg(vp3_base + 2, f32::to_bits(0.5)); // scale_z
    engine.write_reg(vp3_base + 3, f32::to_bits(400.0)); // translate_x
    engine.write_reg(vp3_base + 4, f32::to_bits(300.0)); // translate_y
    engine.write_reg(vp3_base + 5, f32::to_bits(0.5)); // translate_z

    let vp3 = engine.viewport_info(3);
    assert_eq!(vp3.width, 800.0);
    assert_eq!(vp3.height, 600.0);

    // Viewport 0 should still be all zeros.
    let vp0 = engine.viewport_info(0);
    assert_eq!(vp0.width, 0.0);
    assert_eq!(vp0.height, 0.0);
}

#[test]
fn test_scissor_info_nonzero_index() {
    let mut engine = Maxwell3D::new();

    // Set scissor 5 only.
    let sc5_base = SCISSOR_BASE + 5 * SCISSOR_STRIDE;
    engine.write_reg(sc5_base, 1); // enable
    engine.write_reg(sc5_base + 1, 100 | (500 << 16)); // min_x=100, max_x=500
    engine.write_reg(sc5_base + 2, 50 | (400 << 16)); // min_y=50, max_y=400

    let sc5 = engine.scissor_info(5);
    assert!(sc5.enabled);
    assert_eq!(sc5.min_x, 100);
    assert_eq!(sc5.max_x, 500);
    assert_eq!(sc5.min_y, 50);
    assert_eq!(sc5.max_y, 400);
}

#[test]
fn test_draw_captures_all_viewports() {
    let mut engine = Maxwell3D::new();

    // Set viewport 0.
    let vp0_base = VP_TRANSFORM_BASE;
    engine.write_reg(vp0_base, f32::to_bits(640.0));
    engine.write_reg(vp0_base + 1, f32::to_bits(-360.0));
    engine.write_reg(vp0_base + 2, f32::to_bits(0.5));
    engine.write_reg(vp0_base + 3, f32::to_bits(640.0));
    engine.write_reg(vp0_base + 4, f32::to_bits(360.0));
    engine.write_reg(vp0_base + 5, f32::to_bits(0.5));

    // Set viewport 5.
    let vp5_base = VP_TRANSFORM_BASE + 5 * VP_TRANSFORM_STRIDE;
    engine.write_reg(vp5_base, f32::to_bits(200.0));
    engine.write_reg(vp5_base + 1, f32::to_bits(-100.0));
    engine.write_reg(vp5_base + 2, f32::to_bits(1.0));
    engine.write_reg(vp5_base + 3, f32::to_bits(200.0));
    engine.write_reg(vp5_base + 4, f32::to_bits(100.0));
    engine.write_reg(vp5_base + 5, f32::to_bits(1.0));

    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws[0].viewports[0].width, 1280.0);
    assert_eq!(draws[0].viewports[0].height, 720.0);
    assert_eq!(draws[0].viewports[5].width, 400.0);
    assert_eq!(draws[0].viewports[5].height, 200.0);
    // Unset viewport should be zero.
    assert_eq!(draws[0].viewports[10].width, 0.0);
}

#[test]
fn test_draw_captures_all_scissors() {
    let mut engine = Maxwell3D::new();

    // Enable scissor 0.
    let sc0_base = SCISSOR_BASE;
    engine.write_reg(sc0_base, 1);
    engine.write_reg(sc0_base + 1, 0 | (1920 << 16));
    engine.write_reg(sc0_base + 2, 0 | (1080 << 16));

    // Enable scissor 7.
    let sc7_base = SCISSOR_BASE + 7 * SCISSOR_STRIDE;
    engine.write_reg(sc7_base, 1);
    engine.write_reg(sc7_base + 1, 10 | (200 << 16));
    engine.write_reg(sc7_base + 2, 20 | (300 << 16));

    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert!(draws[0].scissors[0].enabled);
    assert_eq!(draws[0].scissors[0].max_x, 1920);
    assert!(draws[0].scissors[7].enabled);
    assert_eq!(draws[0].scissors[7].min_x, 10);
    // Unset scissor should be disabled.
    assert!(!draws[0].scissors[3].enabled);
}

#[test]
fn test_draw_viewport_array_all_indices() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);
    let draws = engine.take_draw_calls();
    // All 16 viewports should be accessible.
    assert_eq!(draws[0].viewports.len(), NUM_VIEWPORTS);
    for vp in &draws[0].viewports {
        assert_eq!(vp.width, 0.0);
    }
}

#[test]
fn test_draw_scissor_array_all_indices() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);
    let draws = engine.take_draw_calls();
    // All 16 scissors should be accessible.
    assert_eq!(draws[0].scissors.len(), NUM_VIEWPORTS);
    for sc in &draws[0].scissors {
        assert!(!sc.enabled);
    }
}

// ── Instance / DrawMode tests ────────────────────────────────────────

#[test]
fn test_instance_id_from_raw() {
    // bits[27:26] = 0 → First
    assert_eq!(InstanceId::from_raw(0x0000_0000), InstanceId::First);
    // bits[27:26] = 1 → Subsequent
    assert_eq!(InstanceId::from_raw(0x0400_0000), InstanceId::Subsequent);
    // bits[27:26] = 2 → Unchanged
    assert_eq!(InstanceId::from_raw(0x0800_0000), InstanceId::Unchanged);
    // bits[27:26] = 3 → Unchanged (fallback)
    assert_eq!(InstanceId::from_raw(0x0C00_0000), InstanceId::Unchanged);
}

#[test]
fn test_draw_begin_parses_instance_id() {
    let mut engine = Maxwell3D::new();
    // Topology = TriangleStrip(5), instance_id = Subsequent (bits[27:26]=1).
    let value = 5 | (1 << 26);
    engine.write_reg(DRAW_BEGIN, value);
    let draw_state = engine.draw_manager_state();
    assert_eq!(draw_state.topology, PrimitiveTopology::TriangleStrip);
    assert_eq!(draw_state.draw_mode, dm::DrawMode::Instance);
}

#[test]
fn test_draw_manager_state_tracks_live_draw_owner() {
    let mut engine = Maxwell3D::new();
    let subsequent = PrimitiveTopology::TriangleStrip as u32 | (1 << 26);
    engine.write_reg(DRAW_BEGIN, subsequent);
    engine.write_reg(DRAW_INLINE_INDEX, 0x4433_2211);

    let draw_state = engine.draw_manager_state();
    assert_eq!(draw_state.topology, PrimitiveTopology::TriangleStrip);
    assert_eq!(draw_state.draw_mode, dm::DrawMode::InlineIndex);
    assert_eq!(draw_state.instance_count, 1);
    assert_eq!(
        draw_state.inline_index_draw_indexes,
        0x4433_2211u32.to_le_bytes()
    );
}

#[test]
fn test_with_draw_manager_keeps_stable_allocation_and_restores_owner() {
    let mut engine = Maxwell3D::new();
    let initial_address = engine.draw_manager() as *const dm::DrawManager as usize;

    let active_address = engine.with_draw_manager(|draw_manager, maxwell3d| {
        assert!(maxwell3d.draw_manager.is_none());
        draw_manager as *mut dm::DrawManager as usize
    });
    assert_eq!(active_address, initial_address);
    assert_eq!(
        engine.draw_manager() as *const dm::DrawManager as usize,
        initial_address
    );

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine.with_draw_manager(|_, _| panic!("test callback unwind"));
    }));
    assert!(unwind.is_err());
    assert_eq!(
        engine.draw_manager() as *const dm::DrawManager as usize,
        initial_address
    );
}

#[test]
fn test_active_draw_manager_state_is_visible_during_rasterizer_callback() {
    let mut engine = Maxwell3D::new();
    let idle_state = engine.draw_manager_state().clone();
    let mut active_state = idle_state.clone();
    active_state.topology = PrimitiveTopology::Triangles;
    active_state.draw_indexed = true;
    active_state.index_buffer.count = 6;
    active_state.index_buffer.format = IndexFormat::UnsignedShort;

    engine.with_active_draw_manager_state(&active_state, |engine| {
        let visible_state = engine.draw_manager_state();
        assert_eq!(visible_state.topology, PrimitiveTopology::Triangles);
        assert!(visible_state.draw_indexed);
        assert_eq!(visible_state.index_buffer.count, 6);
        assert_eq!(
            visible_state.index_buffer.format,
            IndexFormat::UnsignedShort
        );
    });

    let restored_state = engine.draw_manager_state();
    assert_eq!(restored_state.topology, idle_state.topology);
    assert_eq!(restored_state.draw_indexed, idle_state.draw_indexed);
    assert_eq!(
        restored_state.index_buffer.count,
        idle_state.index_buffer.count
    );
    assert_eq!(
        restored_state.index_buffer.format,
        idle_state.index_buffer.format
    );

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine.with_active_draw_manager_state(&active_state, |_| {
            panic!("test callback unwind");
        });
    }));
    assert!(unwind.is_err());
    assert!(engine.active_draw_manager_state.is_none());
    assert_eq!(engine.draw_manager_state().topology, idle_state.topology);
}

#[test]
fn test_general_draw_has_instance_count_one() {
    let mut engine = Maxwell3D::new();
    // Plain draw: topology=Triangles(4), instance_id=First (bits[27:26]=0).
    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    assert_eq!(draws[0].instance_count, 1);
}

#[test]
fn test_instanced_draw_accumulates() {
    let mut engine = Maxwell3D::new();
    let subsequent = 4 | (1 << 26); // Triangles + Subsequent

    // 3 × Subsequent BEGIN+END → no DrawCalls yet.
    for _ in 0..3 {
        engine.write_reg(DRAW_BEGIN, subsequent);
        engine.write_reg(DRAW_END, 0);
    }

    let draws = engine.take_draw_calls();
    assert!(draws.is_empty());
    assert_eq!(engine.draw_manager_state().instance_count, 3);
}

#[test]
fn test_instanced_draw_flushes_on_first() {
    let mut engine = Maxwell3D::new();
    let subsequent = 4 | (1 << 26);

    // 3 Subsequent draws.
    for _ in 0..3 {
        engine.write_reg(DRAW_BEGIN, subsequent);
        engine.write_reg(DRAW_END, 0);
    }
    assert!(engine.take_draw_calls().is_empty());

    // BEGIN(First) flushes the previous batch.
    engine.write_reg(DRAW_BEGIN, 4); // First (bits[27:26]=0)
    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    assert_eq!(draws[0].instance_count, 4);
}

#[test]
fn test_instance_count_resets_after_flush() {
    let mut engine = Maxwell3D::new();
    let subsequent = 4 | (1 << 26);

    // Accumulate 2 instances.
    for _ in 0..2 {
        engine.write_reg(DRAW_BEGIN, subsequent);
        engine.write_reg(DRAW_END, 0);
    }
    assert_eq!(engine.draw_manager_state().instance_count, 2);

    // Flush via First.
    engine.write_reg(DRAW_BEGIN, 4);
    engine.take_draw_calls(); // discard flush

    // Now a General draw should have instance_count=1.
    engine.write_reg(DRAW_END, 0);
    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    assert_eq!(draws[0].instance_count, 1);
}

#[test]
fn test_draw_captures_base_instance() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(GLOBAL_BASE_INSTANCE_INDEX, 42);
    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws[0].base_instance, 42);
}

#[test]
fn test_draw_captures_base_vertex() {
    let mut engine = Maxwell3D::new();
    // Write a negative base vertex (-10 as u32).
    engine.write_reg(GLOBAL_BASE_VERTEX_INDEX, (-10i32) as u32);
    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws[0].base_vertex, -10);
}

#[test]
fn test_inline_index_accumulates() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(DRAW_BEGIN, 4);

    // Push two inline index values.
    engine.write_reg(DRAW_INLINE_INDEX, 0x0000_0001);
    engine.write_reg(DRAW_INLINE_INDEX, 0x0000_0002);

    let draw_state = engine.draw_manager_state();
    assert_eq!(draw_state.inline_index_draw_indexes.len(), 8);
    assert_eq!(draw_state.draw_mode, dm::DrawMode::InlineIndex);
}

#[test]
fn test_inline_index_draw_end() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_INLINE_INDEX, 0x0000_0000);
    engine.write_reg(DRAW_INLINE_INDEX, 0x0000_0001);
    engine.write_reg(DRAW_INLINE_INDEX, 0x0000_0002);
    engine.write_reg(DRAW_END, 0);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    assert!(draws[0].indexed);
    assert_eq!(draws[0].index_format, IndexFormat::UnsignedInt);
    assert_eq!(draws[0].index_buffer_count, 3);
    assert_eq!(draws[0].inline_index_data.len(), 12);
}

#[test]
fn test_inline_index_clears_after_draw() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_INLINE_INDEX, 0x0000_0001);
    engine.write_reg(DRAW_END, 0);

    // After draw, inline buffer should be empty.
    assert!(engine
        .draw_manager_state()
        .inline_index_draw_indexes
        .is_empty());
}

#[test]
fn test_inline_index_keeps_draw_mode_owned_by_draw_manager() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(DRAW_BEGIN, 4);
    engine.write_reg(DRAW_INLINE_INDEX, 0x0000_0001);
    engine.write_reg(DRAW_END, 0);

    assert_eq!(
        engine.draw_manager_state().draw_mode,
        dm::DrawMode::InlineIndex
    );
}

// ── Report Semaphore tests ───────────────────────────────────────────

#[test]
fn test_report_operation_from_raw() {
    assert_eq!(ReportOperation::from_raw(0), ReportOperation::Release);
    assert_eq!(ReportOperation::from_raw(1), ReportOperation::Acquire);
    assert_eq!(ReportOperation::from_raw(2), ReportOperation::ReportOnly);
    assert_eq!(ReportOperation::from_raw(3), ReportOperation::Trap);
    // Bits above [1:0] are ignored for operation extraction.
    assert_eq!(
        ReportOperation::from_raw(0xFFFF_FF00),
        ReportOperation::Release
    );
}

#[test]
fn test_report_semaphore_address() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0x0000_0001); // addr_high
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0xABCD_0000); // addr_low

    assert_eq!(engine.report_semaphore_address(), 0x0001_ABCD_0000);
}

#[test]
fn test_report_semaphore_short_query() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0); // addr_high
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x1000); // addr_low
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0xDEAD_BEEF); // payload

    // Trigger: Release(0) + short_query=1 (bit 28).
    let query = 0 | (1 << 28);
    engine.write_reg(REPORT_SEMAPHORE_TRIGGER, query);

    assert_eq!(engine.pending_semaphore_writes.len(), 1);
    let pw = &engine.pending_semaphore_writes[0];
    assert_eq!(pw.gpu_va, 0x1000);
    assert_eq!(pw.data.len(), 4);
    assert_eq!(pw.data, 0xDEAD_BEEFu32.to_le_bytes());
}

#[test]
fn test_report_semaphore_long_query() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x2000);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0x42);

    // Trigger: Release(0) + short_query=0.
    engine.write_reg(REPORT_SEMAPHORE_TRIGGER, 0);

    assert_eq!(engine.pending_semaphore_writes.len(), 1);
    let pw = &engine.pending_semaphore_writes[0];
    assert_eq!(pw.gpu_va, 0x2000);
    assert_eq!(pw.data.len(), 16);
    // First 8 bytes: payload as u64.
    assert_eq!(&pw.data[0..8], &(0x42u64).to_le_bytes());
    // Last 8 bytes: zero timestamp when no GPU tick getter is installed.
    assert_eq!(&pw.data[8..16], &0u64.to_le_bytes());
}

#[test]
fn test_report_semaphore_long_query_uses_gpu_ticks_in_fallback() {
    let mut engine = Maxwell3D::new();
    engine.set_gpu_ticks_getter(Arc::new(|| 0x1122_3344_5566_7788));
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x2100);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0x7B);
    engine.write_reg(REPORT_SEMAPHORE_TRIGGER, 0);

    let pw = &engine.pending_semaphore_writes[0];
    assert_eq!(&pw.data[0..8], &(0x7Bu64).to_le_bytes());
    assert_eq!(&pw.data[8..16], &0x1122_3344_5566_7788u64.to_le_bytes());
}

#[test]
fn test_report_semaphore_payload_value() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x3000);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0x1234_5678);

    // Short query Release.
    engine.write_reg(REPORT_SEMAPHORE_TRIGGER, 1 << 28);

    let pw = &engine.pending_semaphore_writes[0];
    let payload = u32::from_le_bytes(pw.data[0..4].try_into().unwrap());
    assert_eq!(payload, 0x1234_5678);
}

#[test]
fn test_report_semaphore_query_bitfields_match_upstream_layout() {
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
    let mut engine = Maxwell3D::new();
    engine.bind_rasterizer(&mut rasterizer);
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x3400);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0xCAFE_BABE);

    let report = 0x15;
    let subreport = 0x5;
    let query = 0 | (subreport << 5) | (report << 23) | (1 << 28);
    engine.write_reg(REPORT_SEMAPHORE_TRIGGER, query);

    let calls = calls.lock().unwrap();
    assert_eq!(calls.query_calls.len(), 1);
    assert_eq!(
        calls.query_calls[0],
        (
            0x3400,
            report,
            QueryPropertiesFlags::IS_A_FENCE,
            0xCAFE_BABE,
            subreport
        )
    );
}

#[test]
fn test_report_semaphore_acquire_stops_like_upstream_unimplemented_msg() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x4000);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0xFF);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Acquire = operation 1.
        engine.write_reg(REPORT_SEMAPHORE_TRIGGER, 1);
    }));

    assert!(result.is_err());
}

#[test]
fn test_report_semaphore_trap_stops_like_upstream_unimplemented_msg() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x4100);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0xFF);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Trap = operation 3.
        engine.write_reg(REPORT_SEMAPHORE_TRIGGER, 3);
    }));

    assert!(result.is_err());
}

#[test]
fn test_report_semaphore_no_trigger_no_write() {
    let mut engine = Maxwell3D::new();
    // Write addr and payload but NOT the trigger word.
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x5000);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0xFF);

    assert!(engine.pending_semaphore_writes.is_empty());
}

#[test]
fn test_report_semaphore_drains_on_execute() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x6000);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0xAA);

    // Two short-query releases.
    engine.write_reg(REPORT_SEMAPHORE_TRIGGER, 1 << 28);
    engine.write_reg(REPORT_SEMAPHORE_TRIGGER, 1 << 28);
    assert_eq!(engine.pending_semaphore_writes.len(), 2);

    let noop_reader = |_addr: u64, _buf: &mut [u8]| {};
    let writes = engine.execute_pending(&noop_reader);
    assert_eq!(writes.len(), 2);

    // Second call should be empty.
    let writes2 = engine.execute_pending(&noop_reader);
    assert!(writes2.is_empty());
}

#[test]
fn test_report_semaphore_query_forwards_to_rasterizer_without_engine_side_write() {
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let mut rasterizer = TestRasterizer::new(Arc::clone(&calls));
    let memory_manager = Arc::new(parking_lot::Mutex::new(
        crate::memory_manager::MemoryManager::default(),
    ));
    memory_manager.lock().map(0x7000, 0x8000, 0x1000, 0, false);

    let writes = Arc::new(Mutex::new(Vec::<(u64, Vec<u8>)>::new()));
    let writes_cb = Arc::clone(&writes);

    let mut engine = Maxwell3D::new();
    engine.bind_rasterizer(&rasterizer);
    engine.set_memory_manager(Arc::clone(&memory_manager));
    engine.set_guest_memory_writer(Arc::new(move |cpu_addr, data| {
        writes_cb.lock().unwrap().push((cpu_addr, data.to_vec()));
    }));

    engine.write_reg(REPORT_SEMAPHORE_BASE, 0);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x7000);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0x1122_3344);
    engine.write_reg(REPORT_SEMAPHORE_TRIGGER, 1 << 28);

    let calls = calls.lock().unwrap();
    assert_eq!(calls.query_writes.len(), 1);
    assert_eq!(calls.query_writes[0].0, 0x7000);
    assert_eq!(
        calls.query_writes[0].1,
        0x1122_3344u32.to_le_bytes().to_vec()
    );
    drop(calls);

    let writes = writes.lock().unwrap();
    assert!(writes.is_empty());
    assert!(engine.pending_semaphore_writes.is_empty());

    let _ = &mut rasterizer;
}

#[test]
fn report_semaphore_fallback_uses_memory_manager_owner_without_guest_writer() {
    let device_memory =
        Arc::new(crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default());
    let backing = vec![0u8; 0x1000];
    device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
    device_memory.smmu_map_with_cpu_backing(
        0x8000,
        backing.as_ptr(),
        0x5000,
        backing.len(),
        1,
        true,
    );
    let memory_manager = Arc::new(parking_lot::Mutex::new(
        crate::memory_manager::MemoryManager::new_with_geometry_and_device_memory(
            1,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        ),
    ));
    memory_manager.lock().map(0x7000, 0x8000, 0x1000, 0, false);

    let mut engine = Maxwell3D::new();
    engine.set_memory_manager(memory_manager);
    engine.write_reg(REPORT_SEMAPHORE_BASE, 0);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 1, 0x7000);
    engine.write_reg(REPORT_SEMAPHORE_BASE + 2, 0x1122_3344);
    engine.write_reg(REPORT_SEMAPHORE_TRIGGER, 1 << 28);

    assert!(engine.pending_semaphore_writes.is_empty());
    assert_eq!(&backing[..4], &0x1122_3344u32.to_le_bytes());
}

// ── MME macro integration tests ─────────────────────────────────────

#[test]
fn test_load_mme_upload() {
    let mut engine = Maxwell3D::new();

    // Set upload pointer to offset 5.
    engine.write_reg(LOAD_MME_INSTRUCTION_PTR, 5);
    assert_eq!(engine.regs[LOAD_MME_INSTRUCTION_PTR as usize], 5);

    // ProcessMethodCall handles LOAD_MME_INSTRUCTION with AddCode at the
    // current pointer. Upstream's ProcessMacroUpload auto-increment helper
    // is not used on this path.
    engine.write_reg(LOAD_MME_INSTRUCTION, 0xAAAA);
    assert_eq!(engine.regs[LOAD_MME_INSTRUCTION_PTR as usize], 5);

    engine.write_reg(LOAD_MME_INSTRUCTION, 0xBBBB);
    assert_eq!(engine.regs[LOAD_MME_INSTRUCTION_PTR as usize], 5);
}

#[test]
fn test_refresh_parameters_updates_dirty_macro_segments() {
    let backing = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
    let mut engine = new_descriptor_owner_backed_engine(&backing, 0x1000);
    engine.macro_params = vec![0, 0];
    engine.macro_segments.push((0x1000, 2));
    engine.current_macro_dirty = true;

    engine.refresh_parameters();

    assert_eq!(engine.macro_params, vec![0x4433_2211, 0x8877_6655]);
    assert!(engine.any_parameters_dirty());
}

#[test]
fn read_gpu_block_uses_memory_manager_owner_without_guest_callback() {
    let device_memory = std::sync::Arc::new(
        crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default(),
    );
    let backing = vec![0x11, 0x22, 0x33, 0x44];
    device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
    device_memory.smmu_map_with_cpu_backing(
        0x2000,
        backing.as_ptr(),
        0x8000,
        backing.len(),
        1,
        true,
    );

    let memory_manager = std::sync::Arc::new(parking_lot::Mutex::new(
        crate::memory_manager::MemoryManager::new_with_geometry_and_device_memory(
            1,
            std::sync::Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        ),
    ));
    memory_manager.lock().map(0x1000, 0x2000, 4, 0, false);

    let mut engine = Maxwell3D::new();
    engine.set_memory_manager(memory_manager);

    let mut output = [0u8; 4];
    assert!(engine.read_gpu_block(0x1000, &mut output));
    assert_eq!(output, [0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn test_call_macro_keeps_params_alive_for_refresh_parameters() {
    let gpu = crate::gpu::Gpu::new(false, false);
    gpu.set_guest_memory_reader(std::sync::Arc::new(|addr, output| {
        let backing = [0xEF, 0xBE, 0xAD, 0xDE];
        let start = (addr - 0x2000) as usize;
        output.copy_from_slice(&backing[start..start + output.len()]);
        true
    }));

    let memory_manager = std::sync::Arc::new(parking_lot::Mutex::new(
        crate::memory_manager::MemoryManager::default(),
    ));
    memory_manager.lock().map(0x1000, 0x2000, 4, 0, false);

    let mut engine = Maxwell3D::new();
    engine.set_memory_manager(std::sync::Arc::clone(&memory_manager));
    let gpu_ptr = &gpu as *const crate::gpu::Gpu as usize;
    engine.set_guest_memory_reader(std::sync::Arc::new(move |addr, output| unsafe {
        let gpu = &*(gpu_ptr as *const crate::gpu::Gpu);
        let _ = gpu.read_guest_memory(addr, output);
    }));

    let exit_nop = 0b1 | (0b001 << 4) | (1 << 7) | (1 << 8);
    let nop = 0b1 | (0b001 << 4) | (1 << 8);
    engine.macro_positions[0] = 0x100;
    engine.macro_engine.add_code(0x100, exit_nop);
    engine.macro_engine.add_code(0x100, nop);
    engine.macro_params = vec![0];
    engine.macro_params.reserve(63);
    let macro_params_capacity = engine.macro_params.capacity();
    engine.macro_segments.push((0x1000, 1));
    engine.current_macro_dirty = true;

    engine.call_macro_method(MACRO_REGISTERS_START);

    assert!(engine.macro_params.is_empty());
    assert_eq!(engine.macro_params.capacity(), macro_params_capacity);
    assert!(engine.macro_segments.is_empty());
    assert!(!engine.any_parameters_dirty());
}

#[test]
fn test_load_mme_bind() {
    let mut engine = Maxwell3D::new();

    // Set bind pointer to slot 0.
    engine.write_reg(LOAD_MME_START_ADDR_PTR, 0);

    // Bind slot 0 → start offset 10, slot 1 → start offset 20.
    engine.write_reg(LOAD_MME_START_ADDR, 10);
    assert_eq!(engine.regs[LOAD_MME_START_ADDR_PTR as usize], 1);

    engine.write_reg(LOAD_MME_START_ADDR, 20);
    assert_eq!(engine.regs[LOAD_MME_START_ADDR_PTR as usize], 2);
}

#[test]
fn test_initialize_register_defaults_matches_upstream_boot_values() {
    let engine = Maxwell3D::new();

    let viewport_0 = VIEWPORT_BASE as usize;
    let viewport_15 = VIEWPORT_BASE as usize + 15 * VIEWPORT_STRIDE as usize;
    assert_eq!(engine.regs[viewport_0 + 2], f32::to_bits(0.0));
    assert_eq!(engine.regs[viewport_0 + 3], f32::to_bits(1.0));
    assert_eq!(engine.regs[viewport_15 + 2], f32::to_bits(0.0));
    assert_eq!(engine.regs[viewport_15 + 3], f32::to_bits(1.0));
    assert_eq!(engine.regs[VP_TRANSFORM_BASE as usize + 6], 0x6420);
    assert_eq!(engine.regs[DEPTH_TEST_FUNC as usize], 0x207);
    assert_eq!(engine.regs[STENCIL_TWO_SIDE_ENABLE as usize], 1);
    assert_eq!(engine.regs[STENCIL_FRONT_FUNC_MASK as usize], 0xFFFF_FFFF);
    assert_eq!(engine.regs[STENCIL_BACK_MASK as usize], 0xFFFF_FFFF);
    assert_eq!(engine.regs[POINT_SIZE as usize], f32::to_bits(1.0));
    assert_eq!(engine.regs[COLOR_MASK_BASE as usize], 0x1111);
    assert_eq!(engine.regs[VERTEX_ATTRIB_BASE as usize] & (1 << 6), 1 << 6);
    assert_eq!(engine.regs[RASTERIZE_ENABLE as usize], 1);
    assert_eq!(engine.regs[COLOR_TARGET_MRT_ENABLE as usize], 1);
    assert_eq!(engine.regs[FRAMEBUFFER_SRGB as usize], 1);
    assert_eq!(engine.regs[LINE_WIDTH_ALIASED as usize], f32::to_bits(1.0));
    assert_eq!(engine.regs[LINE_WIDTH_SMOOTH as usize], f32::to_bits(1.0));
    assert_eq!(engine.regs[POLYGON_MODE_FRONT as usize], 0x1B02);
    assert_eq!(engine.regs[POLYGON_MODE_BACK as usize], 0x1B02);
    assert_eq!(engine.shadow_state[DEPTH_TEST_FUNC as usize], 0x207);
    assert_eq!(engine.shadow_state[COLOR_MASK_BASE as usize], 0x1111);
}

#[test]
fn test_gpu_dirty_flags_do_not_change_dma_parameter_dirty_state() {
    let mut engine = Maxwell3D::new();

    assert!(engine.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize]);
    assert!(engine.dirty.flags[dirty_flags::flags::SHADERS as usize]);

    engine.dirty.flags.fill(false);
    engine.interface_state.current_dirty = false;

    <Maxwell3D as dm::Maxwell3DAccess>::set_dirty_flag(
        &mut engine,
        dirty_flags::flags::INDEX_BUFFER,
    );

    assert!(engine.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize]);
    assert!(!engine.dirty.flags[dirty_flags::flags::SHADERS as usize]);
    assert!(!engine.interface_state.current_dirty);
}

#[test]
fn maxwell_draw_view_live_clears_engine_dirty_flag() {
    let mut engine = Maxwell3D::new();
    engine.dirty.flags.fill(false);
    let flag = crate::renderer_opengl::gl_state_tracker::dirty::VIEWPORTS;
    engine.dirty.flags[flag as usize] = true;

    let draw_state = dm::DrawState::default();
    let mut view = dm::Maxwell3DDrawView::live(&draw_state, false, &mut engine);
    assert!(view.dirty_flags()[flag as usize]);

    view.clear_dirty_flag(flag);
    drop(view);

    assert!(!engine.dirty.flags[flag as usize]);
}

#[test]
fn maxwell_draw_view_persists_logic_op_register_mutation() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(LOGIC_OP, 1);

    let draw_state = dm::DrawState::default();
    let mut view = dm::Maxwell3DDrawView::live(&draw_state, false, &mut engine);
    view.set_logic_op_enabled(false);
    drop(view);

    assert!(!engine.logic_op_info().enabled);
    assert_eq!(engine.regs[LOGIC_OP as usize], 0);
}

#[test]
fn maxwell_access_delegates_fixed_pipeline_registers_to_live_engine() {
    let mut engine = Maxwell3D::new();
    engine.write_reg(PROVOKING_VERTEX, 1);
    engine.write_reg(DEPTH_BOUNDS_ENABLE, 1);
    engine.write_reg(DEPTH_BOUNDS_BASE, 0.25_f32.to_bits());
    engine.write_reg(DEPTH_BOUNDS_BASE + 1, 0.75_f32.to_bits());
    engine.write_reg(MANDATED_EARLY_Z, 1);

    let access: &dyn dm::Maxwell3DAccess = &engine;
    assert!(access.provoking_vertex_last());
    assert!(access.depth_bounds_enable());
    assert_eq!(access.depth_bounds(), [0.25, 0.75]);
    assert!(access.mandated_early_z());
}

#[test]
fn test_process_dirty_registers_marks_flags_from_dirty_tables() {
    let mut engine = Maxwell3D::new();
    let method = 0x123usize;

    engine.dirty.flags.fill(false);
    engine.dirty.tables[0][method] = dirty_flags::flags::INDEX_BUFFER;
    engine.dirty.tables[1][method] = dirty_flags::flags::SHADERS;

    // Upstream ProcessDirtyRegisters marks both table owners even when
    // the value written is identical to the current register value.
    engine.write_reg(method as u32, 0);
    assert!(engine.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize]);
    assert!(engine.dirty.flags[dirty_flags::flags::SHADERS as usize]);

    engine.dirty.flags.fill(false);
    engine.write_reg(method as u32, 0xCAFE_BABE);
    assert!(engine.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize]);
    assert!(engine.dirty.flags[dirty_flags::flags::SHADERS as usize]);
}

#[test]
fn dirty_tables_start_unowned_until_rasterizer_initialization() {
    let engine = Maxwell3D::new();
    assert!(engine.dirty_tables().iter().all(|table| table
        .iter()
        .all(|entry| *entry == dirty_flags::flags::NULL_ENTRY)));
}

#[test]
fn test_call_method_marks_flags_from_dirty_tables() {
    let mut engine = Maxwell3D::new();
    let method = 0x124usize;

    engine.dirty.flags.fill(false);
    engine.dirty.tables[0][method] = dirty_flags::flags::INDEX_BUFFER;
    engine.dirty.tables[1][method] = dirty_flags::flags::SHADERS;

    engine.call_method(method as u32, 0, true);
    assert!(engine.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize]);
    assert!(engine.dirty.flags[dirty_flags::flags::SHADERS as usize]);

    engine.dirty.flags.fill(false);
    engine.call_method(method as u32, 0xFEED_FACE, true);
    assert!(engine.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize]);
    assert!(engine.dirty.flags[dirty_flags::flags::SHADERS as usize]);
}

#[test]
fn test_macro_call_triggers_execution() {
    let mut engine = Maxwell3D::new();

    // Upload a minimal macro through the real MME upload path. Exit has a
    // delay slot, so the executable program is two words long.
    let exit_nop = 0b1 | (0b001 << 4) | (1 << 7) | (1 << 8);
    let nop = 0b1 | (0b001 << 4) | (1 << 8);
    let code = [exit_nop, nop];

    // Upload code at offset 0.
    engine.write_reg(LOAD_MME_INSTRUCTION_PTR, 0);
    for &word in &code {
        engine.write_reg(LOAD_MME_INSTRUCTION, word);
    }

    // Bind slot 0 → offset 0.
    engine.write_reg(LOAD_MME_START_ADDR_PTR, 0);
    engine.write_reg(LOAD_MME_START_ADDR, 0);

    // Invoke macro at slot 0 (method MACRO_METHODS_START) with param 0xDEAD.
    engine.write_reg(MACRO_METHODS_START, 0xDEAD);
    engine.macro_params.reserve(63);
    let macro_params_capacity = engine.macro_params.capacity();
    engine.flush_macro();

    assert!(engine.macro_params.is_empty());
    assert_eq!(engine.macro_params.capacity(), macro_params_capacity);
}

#[test]
fn test_macro_writes_registers() {
    let mut engine = Maxwell3D::new();

    let exit_nop = 0b1 | (0b001 << 4) | (1 << 7) | (1 << 8);
    let nop = 0b1 | (0b001 << 4) | (1 << 8);
    let code = [exit_nop, nop];

    engine.write_reg(LOAD_MME_INSTRUCTION_PTR, 0);
    for &word in &code {
        engine.write_reg(LOAD_MME_INSTRUCTION, word);
    }
    engine.write_reg(LOAD_MME_START_ADDR_PTR, 0);
    engine.write_reg(LOAD_MME_START_ADDR, 0);

    engine.call_multi_method(MACRO_METHODS_START, &[0xAA], 1, 1);

    assert!(engine.macro_params.is_empty());
}

#[test]
fn test_macro_slot_calculation() {
    let mut engine = Maxwell3D::new();

    let exit_nop = 0b1 | (0b001 << 4) | (1 << 7) | (1 << 8);
    let nop = 0b1 | (0b001 << 4) | (1 << 8);
    let code = [exit_nop, nop];
    engine.write_reg(LOAD_MME_INSTRUCTION_PTR, 0);
    for &word in &code {
        engine.write_reg(LOAD_MME_INSTRUCTION, word);
    }

    // Bind slot 5 → offset 0.
    engine.write_reg(LOAD_MME_START_ADDR_PTR, 5);
    engine.write_reg(LOAD_MME_START_ADDR, 0);

    // Method for slot 5 = MACRO_METHODS_START + 5*2 = 0x380A.
    engine.write_reg(MACRO_METHODS_START + 5 * 2, 0);
    engine.flush_macro();

    // Slot = ((0x380A - 0x3800) >> 1) % 128 = (0xA >> 1) % 128 = 5.
    // Macro should have executed slot 5.
    // (We can't directly check which slot ran, but the macro writes r2=42.)
    // Since we can't inspect interpreter registers from here, just verify
    // no panic occurred. The macro has no send, so no register writes.
}

// ── Descriptor table integration tests ───────────────────────────────

#[test]
fn test_sampler_binding_register() {
    let mut engine = Maxwell3D::new();
    assert_eq!(engine.regs[SAMPLER_BINDING as usize], 0);

    engine.write_reg(SAMPLER_BINDING, 1);
    assert_eq!(engine.regs[SAMPLER_BINDING as usize], 1);

    engine.write_reg(SAMPLER_BINDING, 0);
    assert_eq!(engine.regs[SAMPLER_BINDING as usize], 0);
}

#[test]
fn test_tex_header_pool_address_reconstruction() {
    let mut engine = Maxwell3D::new();
    let base = TEX_HEADER_POOL_BASE as usize;
    engine.regs[base] = 0x0005;
    engine.regs[base + 1] = 0xABCD_0000;
    assert_eq!(engine.tex_header_pool_address(), 0x0005_ABCD_0000);
}

#[test]
fn test_tex_sampler_pool_address_reconstruction() {
    let mut engine = Maxwell3D::new();
    let base = TEX_SAMPLER_POOL_BASE as usize;
    engine.regs[base] = 0x0003;
    engine.regs[base + 1] = 0x1234_0000;
    assert_eq!(engine.tex_sampler_pool_address(), 0x0003_1234_0000);
}

#[test]
fn test_decode_texture_handle_independent() {
    let engine = Maxwell3D::new();
    // Default is Independently (0).
    let handle: u32 = (0x0AB << 20) | 0x1_2345;
    let (tic_id, tsc_id) = engine.decode_texture_handle(handle);
    assert_eq!(tic_id, 0x1_2345); // 20-bit
    assert_eq!(tsc_id, 0x0AB); // 12-bit
}

#[test]
fn test_decode_texture_handle_linked() {
    let mut engine = Maxwell3D::new();
    engine.regs[SAMPLER_BINDING as usize] = SamplerBinding::ViaHeaderBinding as u32;

    let handle = 42u32;
    let (tic_id, tsc_id) = engine.decode_texture_handle(handle);
    assert_eq!(tic_id, 42);
    assert_eq!(tsc_id, 42);
}

#[test]
fn test_get_tic_entry() {
    let mut backing = vec![0u8; 0x1000];

    // Set up TIC pool: address = 0x1_0000, limit = 10.
    let tic_pool_addr = 0x1_0000u64;
    let base = TEX_HEADER_POOL_BASE as usize;

    // Build a TIC entry for A8B8G8R8 UNorm at index 3.
    // word0: format=0x08(A8B8G8R8), component types UNorm(2), swizzle RGBA.
    let mut raw = [0u8; 32];
    let word0: u32 = 0x08
        | (2 << 7)   // r_type = UNorm
        | (2 << 10)  // g_type = UNorm
        | (2 << 13)  // b_type = UNorm
        | (2 << 16)  // a_type = UNorm
        | (2 << 19)  // x_source = R
        | (3 << 22)  // y_source = G
        | (4 << 25)  // z_source = B
        | (5 << 28); // w_source = A
    raw[0..4].copy_from_slice(&word0.to_le_bytes());
    let offset = 3 * 32;
    backing[offset..offset + 32].copy_from_slice(&raw);

    let mut engine = new_descriptor_owner_backed_engine(&backing, tic_pool_addr);
    engine.regs[base] = 0;
    engine.regs[base + 1] = tic_pool_addr as u32;
    engine.regs[base + 2] = 10;
    let desc = engine.get_tic_entry(3);
    assert_eq!(
        desc.format(),
        crate::textures::texture::TextureFormat::A8B8G8R8 as u32
    );

    // Maxwell3D does not cache these direct reads upstream.
    backing[offset..offset + 4].copy_from_slice(&0x1Du32.to_le_bytes());
    let updated = engine.get_tic_entry(3);
    assert_eq!(
        updated.format(),
        crate::textures::texture::TextureFormat::R8 as u32
    );
}

#[test]
fn test_get_tsc_entry() {
    let mut backing = vec![0u8; 0x1000];

    // Set up TSC pool: address = 0x2_0000, limit = 5.
    let tsc_pool_addr = 0x2_0000u64;
    let base = TEX_SAMPLER_POOL_BASE as usize;

    // Build a TSC entry at index 1.
    // word0: wrap_u=Wrap(0), wrap_v=ClampToEdge(2), wrap_p=Mirror(1).
    // word1: mag=Linear(2), min=Linear(2).
    let mut raw = [0u8; 32];
    let word0: u32 = 0 | (2 << 3) | (1 << 6);
    raw[0..4].copy_from_slice(&word0.to_le_bytes());
    let word1: u32 = 2 | (2 << 4); // mag=Linear(2), min=Linear(2)
    raw[4..8].copy_from_slice(&word1.to_le_bytes());
    let offset = 32;
    backing[offset..offset + 32].copy_from_slice(&raw);

    let mut engine = new_descriptor_owner_backed_engine(&backing, tsc_pool_addr);
    engine.regs[base] = 0;
    engine.regs[base + 1] = tsc_pool_addr as u32;
    engine.regs[base + 2] = 5;
    let desc = engine.get_tsc_entry(1);
    assert_eq!(desc.wrap_u(), WrapMode::Wrap as u32);
    assert_eq!(desc.wrap_v(), WrapMode::ClampToEdge as u32);
    assert_eq!(desc.wrap_p(), WrapMode::Mirror as u32);
}

#[test]
fn test_draw_call_captures_sampler_binding() {
    let mut engine = Maxwell3D::new();

    // Default → Independently.
    engine.write_reg(DRAW_BEGIN, 0); // Topology = Points.
    engine.write_reg(DRAW_END, 0);
    let draws = engine.take_draw_calls();
    assert_eq!(draws[0].sampler_binding, SamplerBinding::Independently);

    // Set ViaHeaderBinding.
    engine.write_reg(SAMPLER_BINDING, 1);
    engine.write_reg(DRAW_BEGIN, 0);
    engine.write_reg(DRAW_END, 0);
    let draws = engine.take_draw_calls();
    assert_eq!(draws[0].sampler_binding, SamplerBinding::ViaHeaderBinding);
}

// ── EngineInterface (call_method / call_multi_method) tests ──────────

#[test]
fn test_call_method_writes_register() {
    let mut engine = Maxwell3D::new();
    engine.call_method(0x100, 0xBEEF, true);
    assert_eq!(engine.regs[0x100], 0xBEEF);
}

#[test]
fn test_call_method_cb_data_increments_offset() {
    let mut engine = Maxwell3D::new();
    // Set up const buffer config: address and offset.
    engine.call_method(CB_CONFIG_BASE, 0x1000, true); // size
    engine.call_method(CB_CONFIG_BASE + 1, 0, true); // addr_high
    engine.call_method(CB_CONFIG_BASE + 2, 0x8000, true); // addr_low
    engine.call_method(CB_CONFIG_BASE + 3, 0, true); // offset = 0

    // Write to CB_DATA — should increment offset by 4 each time.
    engine.call_method(CB_DATA_BASE, 0x1111, true);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 4);
    engine.call_method(CB_DATA_BASE, 0x2222, true);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 8);
}

#[test]
fn test_call_multi_method_cb_data_batch() {
    let mut engine = Maxwell3D::new();
    // Set up const buffer config.
    engine.call_method(CB_CONFIG_BASE, 0x1000, true);
    engine.call_method(CB_CONFIG_BASE + 1, 0, true);
    engine.call_method(CB_CONFIG_BASE + 2, 0x8000, true);
    engine.call_method(CB_CONFIG_BASE + 3, 0, true);

    // Multi-write 4 words to CB_DATA.
    let data = [0x1111u32, 0x2222, 0x3333, 0x4444];
    engine.call_multi_method(CB_DATA_BASE, &data, 4, 4);
    // Offset should advance by 4*4 = 16 bytes.
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 16);
}

#[test]
fn test_call_multi_method_inline_indices_use_upstream_bulk_shadow_path() {
    let mut engine = Maxwell3D::new();
    let data = [0x4433_2211, 0x8877_6655];

    engine.call_multi_method(
        DRAW_INLINE_INDEX,
        &data,
        data.len() as u32,
        data.len() as u32,
    );

    assert_eq!(engine.shadow_state[DRAW_INLINE_INDEX as usize], data[1]);
    assert_eq!(engine.regs[DRAW_INLINE_INDEX as usize], data[1]);
    assert_eq!(
        engine.draw_manager_state().inline_index_draw_indexes,
        bytemuck::cast_slice::<u32, u8>(&data)
    );
}

#[test]
fn test_call_multi_method_inline_indices_replay_each_shadowed_word() {
    let mut engine = Maxwell3D::new();
    engine.shadow_state[SHADOW_RAM_CONTROL as usize] = ShadowRamControl::Replay as u32;
    engine.shadow_state[DRAW_INLINE_INDEX as usize] = 0x4433_2211;

    engine.call_multi_method(DRAW_INLINE_INDEX, &[0xAAAA_AAAA, 0xBBBB_BBBB], 2, 2);

    let word = 0x4433_2211u32.to_le_bytes();
    assert_eq!(
        engine.draw_manager_state().inline_index_draw_indexes,
        [word, word].concat()
    );
}

#[test]
fn draw_manager_instance_arithmetic_wraps_like_upstream_u32() {
    let mut engine = Maxwell3D::new();
    engine.with_draw_manager(|draw_manager, this| {
        draw_manager.draw_state.instance_count = u32::MAX;
        draw_manager.process_method_call(INDEX_BUFFER32_SUBSEQUENT, 0, this);
        assert_eq!(draw_manager.draw_state.instance_count, 0);

        draw_manager.draw_state.instance_count = 0;
        draw_manager.draw_array_instanced(PrimitiveTopology::Triangles, 0, 3, true, this);
        assert_eq!(draw_manager.draw_state.base_instance, u32::MAX);
        assert_eq!(draw_manager.draw_state.instance_count, 1);

        draw_manager.draw_state.draw_mode = dm::DrawMode::Instance;
        draw_manager.draw_state.instance_count = u32::MAX;
        draw_manager.draw_deferred(this);
        assert_eq!(draw_manager.draw_state.instance_count, 0);
    });
}

#[test]
fn test_cb_header_writes_do_not_trigger_cb_data_path() {
    let mut engine = Maxwell3D::new();

    engine.call_method(CB_CONFIG_BASE, 0x200, true);
    engine.call_method(CB_CONFIG_BASE + 1, 0x1234, true);
    engine.call_method(CB_CONFIG_BASE + 2, 0x5678, true);
    engine.call_method(CB_CONFIG_BASE + 3, 0x40, true);

    assert_eq!(engine.regs[CB_CONFIG_BASE as usize], 0x200);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 1) as usize], 0x1234);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 2) as usize], 0x5678);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 0x40);
}

#[test]
fn test_hle_clear_const_buffer_sets_cb_and_resets_offset() {
    let mut engine = Maxwell3D::new();
    // parameters[2] is the vec4 count upstream (macro.cpp passes
    // parameters[2] * 4 as the u32-count amount). parameters[2]=4 means
    // 4 vec4 entries = 16 u32 = 64 bytes written.
    engine.hle_clear_const_buffer(0x5F00, &mut [0x12, 0x3456, 4], &[0; 0x7000]);

    assert_eq!(engine.regs[CB_CONFIG_BASE as usize], 0x5F00);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 1) as usize], 0x12);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 2) as usize], 0x3456);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 64);
}

#[test]
fn test_hle_d7333d26e0a93ede_sets_const_buffer_from_shadow_scratch() {
    let mut engine = Maxwell3D::new();
    engine.regs[SHADOW_SCRATCH_BASE as usize + 43] = 0x1234_5678;
    engine.regs[SHADOW_SCRATCH_BASE as usize + 48] = 0x6000;

    engine.hle_d7333d26e0a93ede(&mut [1]);

    assert_eq!(engine.regs[CB_CONFIG_BASE as usize], 0x6000);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 1) as usize], 0x12);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 2) as usize], 0x3456_7800);
}

#[test]
fn test_hle_c713c83d8f63ccf3_sets_const_buffer_from_shadow_scratch() {
    let mut engine = Maxwell3D::new();
    engine.regs[SHADOW_SCRATCH_BASE as usize + 24] = 0x89AB_CDEF;

    engine.hle_c713c83d8f63ccf3(&mut [0xC000_0010]);

    assert_eq!(engine.regs[CB_CONFIG_BASE as usize], 0x7000);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 1) as usize], 0x89);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 2) as usize], 0xABCD_EF00);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 0x40);
}

#[test]
fn test_hle_set_raster_bounding_box_sets_raw_and_pad_bits() {
    let mut engine = Maxwell3D::new();
    engine.regs[CONSERVATIVE_RASTER_ENABLE as usize] = 0xF3;
    engine.regs[SHADOW_SCRATCH_BASE as usize + 52] = 0xA5;

    engine.hle_set_raster_bounding_box(&mut [0x1234_5678]);

    assert_eq!(
        engine.regs[RASTER_BOUNDING_BOX as usize],
        (0x1234_5678 & 0xFFFF_F00F) | ((0xA5 & 0xF3 & 0xFF) << 4)
    );
}

#[test]
fn test_hle_multi_layer_clear_uses_rt_depth() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);
    let rt_index = 2usize;
    engine.regs[RT_BASE as usize + rt_index * RT_STRIDE as usize + RT_OFF_DEPTH as usize] = 3;

    engine.hle_multi_layer_clear(&mut [(rt_index as u32) << 6]);

    assert_eq!(engine.regs[CLEAR_SURFACE as usize], (rt_index as u32) << 6);
    assert_eq!(calls.lock().unwrap().clear_layers, vec![3]);
}

#[test]
fn test_hle_multi_layer_clear_preserves_zero_rt_depth() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);

    engine.hle_multi_layer_clear(&mut [0]);

    assert_eq!(calls.lock().unwrap().clear_layers, vec![0]);
}

#[test]
fn test_hle_bind_shader_sets_pipeline_offset_and_cb_bind() {
    let mut engine = Maxwell3D::new();
    engine.dirty.flags.fill(false);

    engine.hle_bind_shader(&mut [1, 0xAAAA, 0x240, 0, 0x1234_5600]);

    let pipeline_base = (PIPELINE_BASE + PIPELINE_STRIDE) as usize;
    assert_eq!(engine.regs[pipeline_base + 1], 0x240);
    assert!(engine.dirty.flags[dirty_flags::flags::SHADERS as usize]);
    assert_eq!(engine.regs[SHADOW_SCRATCH_BASE as usize + 29], 0xAAAA);
    assert_eq!(engine.regs[SHADOW_SCRATCH_BASE as usize + 35], 0x240);
    assert_eq!(engine.regs[CB_CONFIG_BASE as usize], 0x10000);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 1) as usize], 0x12);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 2) as usize], 0x3456_0000);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 0);
    assert_eq!(engine.regs[(CB_BIND_BASE + 4) as usize], 0x11);
    assert!(engine.cb_bindings[0][1].enabled);
    assert_eq!(engine.cb_bindings[0][1].address, 0x12_3456_0000);
    assert_eq!(engine.cb_bindings[0][1].size, 0x10000);
}

#[test]
fn test_hle_bind_shader_early_return_preserves_pipeline_offset() {
    let mut engine = Maxwell3D::new();
    engine.regs[SHADOW_SCRATCH_BASE as usize + 29] = 0xAAAA;
    engine.dirty.flags.fill(false);

    engine.hle_bind_shader(&mut [1, 0xAAAA, 0x240, 0, 0x1234_5600]);

    let pipeline_base = (PIPELINE_BASE + PIPELINE_STRIDE) as usize;
    assert_eq!(engine.regs[pipeline_base + 1], 0);
    assert!(!engine.dirty.flags[dirty_flags::flags::SHADERS as usize]);
}

#[test]
fn test_process_cb_multi_data_writes_through_memory_manager() {
    let mut engine = Maxwell3D::new();
    let device_memory =
        Arc::new(crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default());
    let backing = vec![0u8; 0x1000];
    device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
    device_memory.smmu_map_with_cpu_backing(
        0x9000_0000,
        backing.as_ptr(),
        0x4000,
        backing.len(),
        1,
        true,
    );
    let memory_manager = Arc::new(parking_lot::Mutex::new(
        crate::memory_manager::MemoryManager::new_with_geometry_and_device_memory(
            1,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        ),
    ));
    memory_manager
        .lock()
        .map(0x10000, 0x9000_0000, 0x1000, 0, false);
    let writes = Arc::new(Mutex::new(0usize));
    let writes_clone = Arc::clone(&writes);
    engine.set_memory_manager(Arc::clone(&memory_manager));
    engine.set_guest_memory_writer(Arc::new(move |addr, bytes| {
        let _ = (addr, bytes);
        *writes_clone.lock().unwrap() += 1;
    }));

    engine.call_method(CB_CONFIG_BASE, 0x100, true);
    engine.call_method(CB_CONFIG_BASE + 1, 0, true);
    engine.call_method(CB_CONFIG_BASE + 2, 0x10000, true);
    engine.call_method(CB_CONFIG_BASE + 3, 0, true);
    engine.call_multi_method(CB_DATA_BASE, &[0x11223344, 0x55667788], 2, 2);
    memory_manager.lock().flush_caching();

    assert_eq!(
        &backing[..8],
        &[0x44, 0x33, 0x22, 0x11, 0x88, 0x77, 0x66, 0x55]
    );
    assert_eq!(*writes.lock().unwrap(), 0);
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 8);
}

#[test]
fn process_cb_multi_data_uses_memory_manager_owner_without_guest_writer() {
    let mut engine = Maxwell3D::new();
    let device_memory =
        Arc::new(crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default());
    let backing = vec![0u8; 0x1000];
    device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
    device_memory.smmu_map_with_cpu_backing(
        0x9000_0000,
        backing.as_ptr(),
        0x4000,
        backing.len(),
        1,
        true,
    );
    let memory_manager = Arc::new(parking_lot::Mutex::new(
        crate::memory_manager::MemoryManager::new_with_geometry_and_device_memory(
            1,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        ),
    ));
    memory_manager
        .lock()
        .map(0x10000, 0x9000_0000, 0x1000, 0, false);
    engine.set_memory_manager(Arc::clone(&memory_manager));

    engine.call_method(CB_CONFIG_BASE, 0x100, true);
    engine.call_method(CB_CONFIG_BASE + 1, 0, true);
    engine.call_method(CB_CONFIG_BASE + 2, 0x10000, true);
    engine.call_method(CB_CONFIG_BASE + 3, 0, true);
    engine.call_multi_method(CB_DATA_BASE, &[0x11223344, 0x55667788], 2, 2);
    memory_manager.lock().flush_caching();

    assert_eq!(
        &backing[..8],
        &[0x44, 0x33, 0x22, 0x11, 0x88, 0x77, 0x66, 0x55]
    );
    assert_eq!(engine.regs[(CB_CONFIG_BASE + 3) as usize], 8);
}

#[test]
fn test_hle_clear_memory_sets_upload_regs_and_launches_dma() {
    let mut engine = Maxwell3D::new();
    let mut zero_memory = Vec::new();
    engine.hle_clear_memory(&mut [0x44, 0x5566, 0x20], &mut zero_memory);

    assert_eq!(engine.regs[UPLOAD_REGS_BASE], 0x20);
    assert_eq!(engine.regs[UPLOAD_REGS_BASE + 1], 1);
    assert_eq!(engine.regs[UPLOAD_REGS_BASE + 2], 0x44);
    assert_eq!(engine.regs[UPLOAD_REGS_BASE + 3], 0x5566);
    assert_eq!(engine.regs[LAUNCH_DMA as usize], 0x1011);
    assert_eq!(zero_memory.len(), 8);
}

#[test]
fn transform_feedback_buffer_info_decodes_live_register_layout() {
    let mut engine = Maxwell3D::new();
    let base =
        TRANSFORM_FEEDBACK_BUFFERS_BASE as usize + 2 * TRANSFORM_FEEDBACK_BUFFER_STRIDE as usize;
    engine.regs[base] = 1;
    engine.regs[base + 1] = 0x12;
    engine.regs[base + 2] = 0x3456_7800;
    engine.regs[base + 3] = 0x400;
    engine.regs[base + TRANSFORM_FEEDBACK_BUFFER_START_OFFSET as usize] = 0x20;

    assert_eq!(
        engine.transform_feedback_buffer_info(2),
        TransformFeedbackBufferInfo {
            enable: 1,
            address: 0x12_3456_7800,
            size: 0x400,
            start_offset: 0x20,
        }
    );
}

#[test]
fn transform_feedback_buffer_info_preserves_signed_fields() {
    let mut engine = Maxwell3D::new();
    let base = TRANSFORM_FEEDBACK_BUFFERS_BASE as usize;
    engine.regs[base + 3] = 0xffff_fff0;
    engine.regs[base + TRANSFORM_FEEDBACK_BUFFER_START_OFFSET as usize] = 0xffff_ffe0;

    let info = engine.transform_feedback_buffer_info(0);

    assert_eq!(info.size, -16);
    assert_eq!(info.start_offset, -32);
}

#[test]
fn test_hle_transform_feedback_setup_uploads_stride_and_registers_tfb() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);
    engine.regs[TRANSFORM_FEEDBACK_CONTROLS_BASE as usize + 2] = 0x30;
    for index in 0..4usize {
        let base = TRANSFORM_FEEDBACK_BUFFERS_BASE as usize
            + index * TRANSFORM_FEEDBACK_BUFFER_STRIDE as usize;
        engine.regs[base + TRANSFORM_FEEDBACK_BUFFER_START_OFFSET as usize] = 0x7FFF;
    }

    engine.hle_transform_feedback_setup(&mut [0x12, 0x3456_7800]);

    assert_eq!(engine.regs[TRANSFORM_FEEDBACK_ENABLED as usize], 1);
    for index in 0..4usize {
        let base = TRANSFORM_FEEDBACK_BUFFERS_BASE as usize
            + index * TRANSFORM_FEEDBACK_BUFFER_STRIDE as usize;
        assert_eq!(
            engine.regs[base + TRANSFORM_FEEDBACK_BUFFER_START_OFFSET as usize],
            0
        );
    }
    assert_eq!(engine.regs[UPLOAD_REGS_BASE], 4);
    assert_eq!(engine.regs[UPLOAD_REGS_BASE + 1], 1);
    assert_eq!(engine.regs[UPLOAD_REGS_BASE + 2], 0x12);
    assert_eq!(engine.regs[UPLOAD_REGS_BASE + 3], 0x3456_7800);
    assert_eq!(engine.regs[LAUNCH_DMA as usize], 0x1011);
    assert_eq!(
        calls.lock().unwrap().transform_feedback,
        vec![0x12_3456_7800]
    );
}

#[test]
fn test_hle_draw_arrays_indirect_fallback_emits_instanced_draw() {
    let mut engine = Maxwell3D::new();
    engine.regs[0xD1B] = 0xFF;

    engine.hle_draw_arrays_indirect(
        true,
        &mut [PrimitiveTopology::Triangles as u32, 6, 0x22, 4, 7],
    );

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    let draw = &draws[0];
    assert_eq!(draw.topology, PrimitiveTopology::Triangles);
    assert!(!draw.indexed);
    assert_eq!(draw.vertex_first, 4);
    assert_eq!(draw.vertex_count, 6);
    assert_eq!(draw.base_instance, 7);
    assert_eq!(draw.instance_count, 0x22);
    assert_eq!(engine.regs[GLOBAL_BASE_INSTANCE_INDEX as usize], 0);
    assert_eq!(engine.engine_state, EngineHint::None);
    assert!(engine.replace_table.is_empty());
}

#[test]
fn test_hle_draw_indexed_indirect_fallback_emits_instanced_draw_and_resets_base_regs() {
    let mut engine = Maxwell3D::new();
    engine.regs[0xD1B] = 0x07;

    engine.hle_draw_indexed_indirect(
        true,
        &mut [PrimitiveTopology::Triangles as u32, 12, 0x03, 5, 9, 11],
    );

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    let draw = &draws[0];
    assert_eq!(draw.topology, PrimitiveTopology::Triangles);
    assert!(draw.indexed);
    assert_eq!(draw.index_buffer_first, 5);
    assert_eq!(draw.index_buffer_count, 12);
    assert_eq!(draw.base_vertex, 9);
    assert_eq!(draw.base_instance, 11);
    assert_eq!(draw.instance_count, 3);
    assert_eq!(engine.regs[VERTEX_ID_BASE as usize], 0);
    assert_eq!(engine.regs[GLOBAL_BASE_VERTEX_INDEX as usize], 0);
    assert_eq!(engine.regs[GLOBAL_BASE_INSTANCE_INDEX as usize], 0);
    assert_eq!(engine.engine_state, EngineHint::None);
    assert!(engine.replace_table.is_empty());
}

#[test]
fn direct_indexed_draw_does_not_contaminate_following_array_draw() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);

    engine.with_draw_manager(|draw_manager, this| {
        draw_manager.draw_index(PrimitiveTopology::Triangles, 0, 36, 0, 0, 1, this);
        assert!(!draw_manager.get_draw_state().draw_indexed);
        draw_manager.draw_array(PrimitiveTopology::Triangles, 0, 18, 0, 1, this);
        assert!(!draw_manager.get_draw_state().draw_indexed);
    });

    let calls = calls.lock().unwrap();
    assert_eq!(
        calls
            .draws
            .iter()
            .map(|(_, indexed, _)| *indexed)
            .collect::<Vec<_>>(),
        [true, false]
    );
    assert!(!calls.draw_states[0].draw_indexed);
    assert!(!calls.draw_states[1].draw_indexed);
}

#[test]
fn test_hle_multi_draw_indexed_indirect_count_fallback_emits_draw_sequence() {
    let mut engine = Maxwell3D::new();
    // The upstream fallback writes DrawID through the guest's currently
    // configured constant buffer and asserts this configuration is valid.
    engine.regs[CB_CONFIG_BASE as usize] = 0x1000;
    engine.regs[(CB_CONFIG_BASE + 1) as usize] = 0;
    engine.regs[(CB_CONFIG_BASE + 2) as usize] = 0x1000;

    engine.hle_multi_draw_indexed_indirect_count(&mut [
        0,
        2,
        PrimitiveTopology::Quads as u32,
        0,
        2,
        6,
        17,
        100,
        3,
        5,
        12,
        34,
        200,
        7,
        9,
    ]);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 2);
    assert_eq!(draws[0].index_buffer_count, 6);
    assert_eq!(draws[0].index_buffer_first, 100);
    assert_eq!(draws[0].base_vertex, 3);
    assert_eq!(draws[0].base_instance, 5);
    assert_eq!(draws[0].instance_count, 17);
    assert_eq!(draws[1].index_buffer_count, 12);
    assert_eq!(draws[1].index_buffer_first, 200);
    assert_eq!(draws[1].base_vertex, 7);
    assert_eq!(draws[1].base_instance, 9);
    assert_eq!(draws[1].instance_count, 34);
    assert_eq!(engine.regs[VERTEX_ID_BASE as usize], 0);
    assert_eq!(engine.engine_state, EngineHint::None);
    assert!(engine.replace_table.is_empty());
}

#[test]
fn test_hle_multi_draw_indexed_indirect_count_uses_upstream_indirect_state() {
    let mut engine = Maxwell3D::new();
    engine.macro_addresses = vec![0, 0, 0, 0, 0xAA00, 0xBB00];

    engine.hle_multi_draw_indexed_indirect_count(&mut [
        0,
        4,
        PrimitiveTopology::Triangles as u32,
        1,
        4,
    ]);

    let params = engine.draw_manager().get_indirect_params();
    assert!(params.is_indexed);
    assert!(params.include_count);
    assert!(!params.is_byte_count);
    assert_eq!(params.count_start_address, 0xAA00);
    assert_eq!(params.indirect_start_address, 0xBB00);
    assert_eq!(params.buffer_size, 4 * 6 * std::mem::size_of::<u32>());
    assert_eq!(params.max_draw_counts, 4);
    assert_eq!(params.stride, 6 * std::mem::size_of::<u32>());
    assert!(engine.take_draw_calls().is_empty());
    assert_eq!(engine.engine_state, EngineHint::None);
    assert!(engine.replace_table.is_empty());
}

#[test]
fn test_hle_draw_indirect_byte_count_fallback_emits_draw_array() {
    let mut engine = Maxwell3D::new();

    engine.hle_draw_indirect_byte_count(&mut [PrimitiveTopology::TriangleStrip as u32, 4, 16]);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    let draw = &draws[0];
    assert_eq!(draw.topology, PrimitiveTopology::TriangleStrip);
    assert!(!draw.indexed);
    assert_eq!(draw.vertex_first, 0);
    assert_eq!(draw.vertex_count, 4);
    assert_eq!(draw.instance_count, 1);
    assert_eq!(engine.regs[DRAW_AUTO_STRIDE as usize], 4);
    assert_eq!(engine.regs[DRAW_AUTO_BYTE_COUNT as usize], 16);
}

#[test]
fn test_hle_draw_indirect_byte_count_uses_transform_feedback_indirect_path() {
    let mut engine = Maxwell3D::new();
    engine.macro_addresses = vec![0, 0, 0xAA04];
    let calls = Arc::new(Mutex::new(RasterizerCalls {
        has_draw_transform_feedback: true,
        ..Default::default()
    }));
    let rasterizer = TestRasterizer::new(Arc::clone(&calls));
    engine.bind_rasterizer(&rasterizer);

    engine.hle_draw_indirect_byte_count(&mut [PrimitiveTopology::TriangleStrip as u32, 12, 0x3456]);

    let params = engine.draw_manager().get_indirect_params();
    assert!(params.is_byte_count);
    assert!(!params.is_indexed);
    assert!(!params.include_count);
    assert_eq!(params.indirect_start_address, 0xAA04);
    assert_eq!(params.buffer_size, std::mem::size_of::<u32>());
    assert_eq!(params.max_draw_counts, 1);
    assert_eq!(params.stride, 12);
    assert_eq!(
        engine.regs[DRAW_BEGIN as usize],
        PrimitiveTopology::TriangleStrip as u32
    );
    assert_eq!(engine.regs[DRAW_AUTO_STRIDE as usize], 12);
    assert_eq!(engine.regs[DRAW_AUTO_BYTE_COUNT as usize], 0x3456);
    assert_eq!(
        engine.draw_manager().get_draw_state().topology,
        PrimitiveTopology::TriangleStrip
    );
}

#[test]
fn test_process_inline_upload_multi_calls_bound_rasterizer() {
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(Arc::clone(&calls));
    let memory_manager = Arc::new(parking_lot::Mutex::new(
        crate::memory_manager::MemoryManager::new_with_geometry(1, 32, 0x1_0000_0000, 16, 12),
    ));

    let mut engine = Maxwell3D::new();
    engine.set_memory_manager(Arc::clone(&memory_manager));
    engine.bind_rasterizer(&rasterizer);
    engine.regs[UPLOAD_REGS_BASE as usize] = 8;
    engine.regs[(UPLOAD_REGS_BASE + 1) as usize] = 1;
    engine.regs[(UPLOAD_REGS_BASE + 2) as usize] = 0;
    engine.regs[(UPLOAD_REGS_BASE + 3) as usize] = 0x2000;
    engine.regs[(UPLOAD_REGS_BASE + 4) as usize] = 8;
    engine.regs[LAUNCH_DMA as usize] = 0x1011;
    let upload_regs = engine.upload_registers();
    engine
        .upload_state
        .process_exec(&upload_regs, engine.launch_dma_is_linear());

    engine.process_inline_upload_multi(&[0x1122_3344, 0x5566_7788]);

    assert_eq!(
        calls.lock().unwrap().inline_to_memory,
        vec![(
            0x2000,
            8,
            vec![0x44, 0x33, 0x22, 0x11, 0x88, 0x77, 0x66, 0x55]
        )]
    );

    calls.lock().unwrap().inline_to_memory.clear();
    engine
        .upload_state
        .process_exec(&upload_regs, engine.launch_dma_is_linear());
    engine.process_inline_upload_word(0x1122_3344, false);
    engine.process_inline_upload_word(0x5566_7788, true);

    assert_eq!(
        calls.lock().unwrap().inline_to_memory,
        vec![(
            0x2000,
            8,
            vec![0x44, 0x33, 0x22, 0x11, 0x88, 0x77, 0x66, 0x55]
        )]
    );
}

#[test]
fn inline_upload_linear_forwards_through_the_bound_rasterizer() {
    let device_memory =
        Arc::new(crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager::default());
    let backing = vec![0u8; 0x1000];
    device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
    device_memory.smmu_map_with_cpu_backing(
        0x8000,
        backing.as_ptr(),
        0x6000,
        backing.len(),
        1,
        true,
    );
    let memory_manager = Arc::new(parking_lot::Mutex::new(
        crate::memory_manager::MemoryManager::new_with_geometry_and_device_memory(
            1,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        ),
    ));
    memory_manager.lock().map(0x2000, 0x8000, 0x1000, 0, false);

    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(Arc::clone(&calls));
    let mut engine = Maxwell3D::new_with_memory_manager(memory_manager);
    engine.bind_rasterizer(&rasterizer);
    engine.regs[UPLOAD_REGS_BASE as usize] = 8;
    engine.regs[(UPLOAD_REGS_BASE + 1) as usize] = 1;
    engine.regs[(UPLOAD_REGS_BASE + 2) as usize] = 0;
    engine.regs[(UPLOAD_REGS_BASE + 3) as usize] = 0x2000;
    engine.regs[(UPLOAD_REGS_BASE + 4) as usize] = 8;
    engine.regs[LAUNCH_DMA as usize] = 0x1011;
    let upload_regs = engine.upload_registers();
    engine
        .upload_state
        .process_exec(&upload_regs, engine.launch_dma_is_linear());

    engine.process_inline_upload_multi(&[0x1122_3344, 0x5566_7788]);

    assert_eq!(
        calls.lock().unwrap().inline_to_memory,
        vec![(
            0x2000,
            8,
            [0x1122_3344u32.to_ne_bytes(), 0x5566_7788u32.to_ne_bytes()].concat(),
        )]
    );
}

#[test]
fn test_call_multi_method_default_iterates() {
    let mut engine = Maxwell3D::new();
    // Writing to a generic register (not CB_DATA) via call_multi_method
    // should iterate call_method for each value.
    let data = [0xAAAAu32, 0xBBBB, 0xCCCC];
    // Use a non-special register.
    engine.call_multi_method(0x100, &data, 3, 3);
    // Last value written wins since all go to same register.
    assert_eq!(engine.regs[0x100], 0xCCCC);
}

#[test]
fn test_call_method_firmware_call4() {
    let mut engine = Maxwell3D::new();
    assert_eq!(FALCON4, reg_index!(0x2310));
    // Writing to falcon[4] should set shadow_scratch[0] to 1.
    engine.call_method(FALCON4, 0, true);
    assert_eq!(engine.regs[SHADOW_SCRATCH_BASE as usize], 1);
}

#[test]
fn test_call_method_shadow_ram_track() {
    let mut engine = Maxwell3D::new();
    // Enable shadow RAM tracking.
    engine.call_method(SHADOW_RAM_CONTROL, 1, true); // Track
                                                     // Write a value through call_method.
    engine.call_method(0x200, 0xDEAD, true);
    // Shadow state should have the tracked value.
    assert_eq!(engine.shadow_state[0x200], 0xDEAD);
    // Regs should also have the value.
    assert_eq!(engine.regs[0x200], 0xDEAD);
}

#[test]
fn test_call_method_shadow_ram_replay() {
    let mut engine = Maxwell3D::new();
    // First, track a value.
    engine.call_method(SHADOW_RAM_CONTROL, 1, true); // Track
    engine.call_method(0x200, 0xAAAA, true);
    assert_eq!(engine.shadow_state[0x200], 0xAAAA);

    // Switch to Replay mode.
    engine.call_method(SHADOW_RAM_CONTROL, 3, true); // Replay
                                                     // Write a different value — should use shadow state instead.
    engine.call_method(0x200, 0xBBBB, true);
    // Regs should have the shadow value (0xAAAA), not the written value.
    assert_eq!(engine.regs[0x200], 0xAAAA);
}

#[test]
fn test_execution_mask_covers_key_methods() {
    let engine = Maxwell3D::new();
    // Key methods should be marked as executable.
    assert!(engine.interface_state.execution_mask[DRAW_END as usize]);
    assert!(engine.interface_state.execution_mask[DRAW_BEGIN as usize]);
    assert!(engine.interface_state.execution_mask[CLEAR_SURFACE as usize]);
    assert!(engine.interface_state.execution_mask[CB_DATA_BASE as usize]);
    assert_eq!(REPORT_SEMAPHORE_BASE, 0x6C0);
    assert_eq!(REPORT_SEMAPHORE_QUERY, 0x6C3);
    assert_eq!(RENDER_ENABLE_BASE, 0x554);
    assert_eq!(RENDER_ENABLE_MODE, 0x556);
    assert_eq!(CB_BIND_TRIGGER_0, 0x904);
    assert_eq!(CB_BIND_TRIGGER_1, 0x90C);
    assert_eq!(CB_BIND_TRIGGER_2, 0x914);
    assert_eq!(CB_BIND_TRIGGER_3, 0x91C);
    assert_eq!(CB_BIND_TRIGGER_4, 0x924);
    assert!(engine.interface_state.execution_mask[CB_BIND_TRIGGER_0 as usize]);
    assert!(engine.interface_state.execution_mask[SYNC_INFO as usize]);
    assert!(engine.interface_state.execution_mask[LAUNCH_DMA as usize]);
    // Generic register should NOT be executable.
    assert!(!engine.interface_state.execution_mask[0x100]);
}

#[test]
fn test_method_sink_deferred_writes() {
    let mut engine = Maxwell3D::new();
    // Push method sink entries directly (simulating DmaPusher deferral).
    engine.interface_state.method_sink.push((0x200, 0x1111));
    engine.interface_state.method_sink.push((0x201, 0x2222));

    // Consume sink should apply the writes.
    engine.consume_sink();
    assert_eq!(engine.regs[0x200], 0x1111);
    assert_eq!(engine.regs[0x201], 0x2222);
    assert!(engine.interface_state.method_sink.is_empty());
}

#[test]
fn test_method_sink_consume_retains_capacity_like_upstream_clear() {
    let mut engine = Maxwell3D::new();
    engine.interface_state.method_sink.reserve(64);
    let capacity = engine.interface_state.method_sink.capacity();
    engine.interface_state.method_sink.push((0x200, 0x1111));

    engine.consume_sink();

    assert!(engine.interface_state.method_sink.is_empty());
    assert_eq!(engine.interface_state.method_sink.capacity(), capacity);
}

#[test]
fn test_call_method_query_condition_always_render() {
    let mut engine = Maxwell3D::new();
    // Set render_enable_override to AlwaysRender (1).
    engine.call_method(RENDER_ENABLE_OVERRIDE, 1, true);
    // Trigger query condition evaluation.
    engine.call_method(RENDER_ENABLE_MODE, 0, true);
    assert!(engine.should_execute());
}

#[test]
fn test_call_method_query_condition_never_render() {
    let mut engine = Maxwell3D::new();
    // Set render_enable_override to NeverRender (2).
    engine.call_method(RENDER_ENABLE_OVERRIDE, 2, true);
    engine.call_method(RENDER_ENABLE_MODE, 0, true);
    assert!(!engine.should_execute());
}

#[test]
fn test_call_method_query_condition_uses_rasterizer_acceleration() {
    let calls = Arc::new(Mutex::new(RasterizerCalls {
        accelerate_conditional_rendering: true,
        ..Default::default()
    }));
    let mut engine = Maxwell3D::new();
    let rasterizer = TestRasterizer::new(Arc::clone(&calls));
    engine.bind_rasterizer(&rasterizer);

    engine.call_method(RENDER_ENABLE_OVERRIDE, 2, true);
    engine.call_method(RENDER_ENABLE_MODE, 0, true);

    assert!(engine.should_execute());
}

#[test]
fn test_call_method_query_condition_if_equal_reads_compare_block() {
    let gpu = crate::gpu::Gpu::new(false, false);
    gpu.set_guest_memory_reader(std::sync::Arc::new(|addr, output| {
        let mut backing = [0u8; 24];
        backing[0..4].copy_from_slice(&5u32.to_le_bytes());
        backing[4..8].copy_from_slice(&7u32.to_le_bytes());
        backing[16..20].copy_from_slice(&5u32.to_le_bytes());
        backing[20..24].copy_from_slice(&7u32.to_le_bytes());
        let start = (addr - 0x3000) as usize;
        output.copy_from_slice(&backing[start..start + output.len()]);
        true
    }));

    let memory_manager = std::sync::Arc::new(parking_lot::Mutex::new(
        crate::memory_manager::MemoryManager::default(),
    ));
    memory_manager.lock().map(0x2000, 0x3000, 24, 0, false);

    let mut engine = Maxwell3D::new();
    engine.set_memory_manager(std::sync::Arc::clone(&memory_manager));
    let gpu_ptr = &gpu as *const crate::gpu::Gpu as usize;
    engine.set_guest_memory_reader(std::sync::Arc::new(move |addr, output| unsafe {
        let gpu = &*(gpu_ptr as *const crate::gpu::Gpu);
        let _ = gpu.read_guest_memory(addr, output);
    }));

    engine.call_method(RENDER_ENABLE_BASE, 0, true);
    engine.call_method(RENDER_ENABLE_BASE + 1, 0x2000, true);
    engine.call_method(RENDER_ENABLE_OVERRIDE, 0, true);
    engine.call_method(RENDER_ENABLE_MODE, 3, true);

    assert!(engine.should_execute());
}

#[test]
fn test_call_method_cb_bind() {
    let mut engine = Maxwell3D::new();
    // Set up const buffer config.
    engine.call_method(CB_CONFIG_BASE, 0x800, true); // size
    engine.call_method(CB_CONFIG_BASE + 1, 0x1, true); // addr_high
    engine.call_method(CB_CONFIG_BASE + 2, 0x2000, true); // addr_low

    // Trigger CB_BIND for stage 0, slot 2, valid.
    // raw_config: valid=1, slot=2 => (2 << 4) | 1 = 0x21
    let bind_base = (CB_BIND_BASE + 0 * CB_BIND_STRIDE) as usize;
    engine.regs[bind_base + 4] = 0x21;
    engine.call_method(CB_BIND_TRIGGER_0, 0x21, true);

    let binding = engine.cb_bindings[0][2];
    assert!(binding.enabled);
    assert_eq!(binding.address, 0x1_0000_2000);
    assert_eq!(binding.size, 0x800);
}

#[test]
fn test_call_method_sync_point() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);
    engine.call_method(SYNC_INFO, 42, true);
    assert_eq!(engine.regs[SYNC_INFO as usize], 42);
    assert_eq!(calls.lock().unwrap().signal_sync_point, vec![42]);
}

#[test]
fn test_call_method_counter_reset_maps_clear_reports_to_query_types() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);

    for clear_report in 0..=5 {
        engine.call_method(CLEAR_REPORT_VALUE, clear_report, true);
    }

    assert_eq!(
        calls.lock().unwrap().reset_counter,
        vec![
            QueryType::Payload as u32,
            QueryType::ZPassPixelCount64 as u32,
            QueryType::StreamingPrimitivesSucceeded as u32,
            QueryType::PrimitivesGenerated as u32,
            QueryType::VtgPrimitivesOut as u32,
            QueryType::Payload as u32,
        ]
    );
}

#[test]
fn test_wait_for_idle_calls_rasterizer() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);
    engine.call_method(WAIT_FOR_IDLE, 0, true);
    assert_eq!(calls.lock().unwrap().wait_for_idle, 1);
}

#[test]
fn test_draw_texture_trigger_calls_rasterizer() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);

    engine.call_method(DRAW_TEXTURE_SRC_Y0, 0x1234, true);

    assert_eq!(calls.lock().unwrap().draw_texture, 1);
}

#[test]
fn test_draw_texture_trigger_populates_draw_texture_state_from_live_registers() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls);
    engine.bind_rasterizer(&rasterizer);

    let base = DRAW_TEXTURE_BASE as usize;
    engine.regs[base + DRAW_TEXTURE_DST_X0_OFFSET] = 4096;
    engine.regs[base + DRAW_TEXTURE_DST_Y0_OFFSET] = 8192;
    engine.regs[base + DRAW_TEXTURE_DST_WIDTH_OFFSET] = 12288;
    engine.regs[base + DRAW_TEXTURE_DST_HEIGHT_OFFSET] = 4096;
    engine.regs[base + DRAW_TEXTURE_DX_DU_LOW_OFFSET] = 0x4000_0000;
    engine.regs[base + DRAW_TEXTURE_DX_DU_HIGH_OFFSET] = 0;
    engine.regs[base + DRAW_TEXTURE_DY_DV_LOW_OFFSET] = 0x8000_0000;
    engine.regs[base + DRAW_TEXTURE_DY_DV_HIGH_OFFSET] = 0;
    engine.regs[base + DRAW_TEXTURE_SRC_SAMPLER_OFFSET] = 7;
    engine.regs[base + DRAW_TEXTURE_SRC_TEXTURE_OFFSET] = 9;
    engine.regs[base + DRAW_TEXTURE_SRC_X0_OFFSET] = 2048;
    engine.regs[base + DRAW_TEXTURE_SRC_Y0_OFFSET] = 1024;
    engine.regs[SURFACE_CLIP_BASE as usize + SURFACE_CLIP_HEIGHT_OFFSET] = 100 << 16;
    engine.regs[WINDOW_ORIGIN as usize] = 1;

    engine.call_method(DRAW_TEXTURE_SRC_Y0, 1024, true);

    let state = engine.draw_manager().get_draw_texture_state();
    assert_eq!(state.dst_x0, 1.0);
    assert_eq!(state.dst_y0, 98.0);
    assert_eq!(state.dst_x1, 4.0);
    assert_eq!(state.dst_y1, 99.0);
    assert_eq!(state.src_x0, 0.5);
    assert_eq!(state.src_y0, 0.25);
    assert_eq!(state.src_x1, 1.25);
    assert_eq!(state.src_y1, 0.75);
    assert_eq!(state.src_sampler, 7);
    assert_eq!(state.src_texture, 9);
}

#[test]
fn test_draw_end_dispatches_draw_state_to_rasterizer_with_program_addresses() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);

    // Configure shader_program_region = 0x1_0000_0000 and enable
    // VertexB (slot 1) at offset 0x100 plus Fragment (slot 5) at 0x500.
    engine.write_reg(PROGRAM_REGION_HIGH, 1); // high = 1 → base = 0x1_0000_0000
    engine.write_reg(PROGRAM_REGION_LOW, 0);

    let vb_base = PIPELINE_BASE + 1 * PIPELINE_STRIDE;
    engine.write_reg(vb_base, 1 | (1 << 4));
    engine.write_reg(vb_base + 1, 0x100);

    let frag_base = PIPELINE_BASE + 5 * PIPELINE_STRIDE;
    engine.write_reg(frag_base, 1 | (5 << 4));
    engine.write_reg(frag_base + 1, 0x500);

    // Trigger a non-indexed draw via DRAW_BEGIN/DRAW_END.
    engine.write_reg(DRAW_BEGIN, 4); // Triangles
    engine.write_reg(DRAW_END, 0);

    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.draws.len(),
        1,
        "DRAW_END should trigger exactly one rasterizer.draw call"
    );
    let (instance_count, indexed, addrs) = calls.draws[0];
    assert_eq!(instance_count, 1);
    assert!(!indexed);
    // Slots 0/2/3/4 disabled (zero); 1 = VertexB at base+0x100; 5 = Fragment at base+0x500.
    assert_eq!(addrs[0], 0);
    assert_eq!(addrs[1], 0x1_0000_0100);
    assert_eq!(addrs[2], 0);
    assert_eq!(addrs[3], 0);
    assert_eq!(addrs[4], 0);
    assert_eq!(addrs[5], 0x1_0000_0500);
}

#[test]
fn test_cb_bind_forwards_to_rasterizer() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);

    engine.call_method(CB_CONFIG_BASE, 0x800, true);
    engine.call_method(CB_CONFIG_BASE + 1, 0x1, true);
    engine.call_method(CB_CONFIG_BASE + 2, 0x2000, true);
    let bind_base = (CB_BIND_BASE + 0 * CB_BIND_STRIDE) as usize;
    engine.regs[bind_base + 4] = 0x21;
    engine.call_method(CB_BIND_TRIGGER_0, 0x21, true);

    assert_eq!(
        calls.lock().unwrap().bound_uniforms,
        vec![(0, 2, 0x1_0000_2000, 0x800)]
    );
}

#[test]
fn test_draw_index_small_matches_upstream_immediate_indexed_draw() {
    let mut engine = Maxwell3D::new();
    engine.regs[(IB_BASE + IB_OFF_FORMAT) as usize] = 2;
    engine.regs[GLOBAL_BASE_VERTEX_INDEX as usize] = 7;
    engine.regs[GLOBAL_BASE_INSTANCE_INDEX as usize] = 3;

    let argument = (4u32 << 28) | (100u32 << 16) | 12u32;
    engine.call_method(INDEX_BUFFER32_FIRST, argument, true);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 1);
    let draw = &draws[0];
    assert!(draw.indexed);
    assert_eq!(draw.topology, PrimitiveTopology::Triangles);
    assert_eq!(draw.index_buffer_first, 12);
    assert_eq!(draw.index_buffer_count, 100);
    assert_eq!(draw.base_vertex, 7);
    assert_eq!(draw.base_instance, 3);
    assert_eq!(draw.instance_count, 1);
}

#[test]
fn test_draw_state_snapshots_vertex_streams_and_limits_for_buffer_cache() {
    let mut engine = Maxwell3D::new();
    let calls = Arc::new(Mutex::new(RasterizerCalls::default()));
    let rasterizer = TestRasterizer::new(calls.clone());
    engine.bind_rasterizer(&rasterizer);

    let stream_base = VERTEX_STREAM_BASE as usize;
    engine.regs[stream_base] = (1 << 12) | 0x20;
    engine.regs[stream_base + 1] = 0x1;
    engine.regs[stream_base + 2] = 0x2000;
    engine.regs[stream_base + 3] = 5;
    engine.regs[VERTEX_STREAM_INSTANCE_BASE as usize] = 1;
    let limit_base = VERTEX_STREAM_LIMIT_BASE as usize;
    engine.regs[limit_base] = 0x1;
    engine.regs[limit_base + 1] = 0x2FFF;

    engine.write_reg(VB_FIRST, 0);
    engine.write_reg(VB_COUNT, 4);
    engine.write_reg(DRAW_BEGIN, PrimitiveTopology::Triangles as u32);
    engine.write_reg(DRAW_END, 0);

    let calls = calls.lock().unwrap();
    assert_eq!(calls.draw_states.len(), 1);
    assert_eq!(calls.draw_registers.len(), 1);
    let registers = &calls.draw_registers[0];
    assert_eq!(registers.vertex_streams[0].address, 0x1_0000_2000);
    assert_eq!(registers.vertex_streams[0].stride, 0x20);
    assert_eq!(registers.vertex_streams[0].frequency, 5);
    assert!(registers.vertex_streams[0].enabled);
    assert_eq!(registers.vertex_stream_instances[0], 1);
    assert_eq!(registers.vertex_stream_limits[0].address, 0x1_0000_2FFF);
}

#[test]
fn test_vertex_array_instance_methods_match_upstream_base_instance_progression() {
    let mut engine = Maxwell3D::new();

    let encoded = (4u32 << 28) | (20u32 << 16) | 10u32;
    engine.call_method(VERTEX_ARRAY_INSTANCE_FIRST, encoded, true);
    engine.call_method(VERTEX_ARRAY_INSTANCE_SUBSEQUENT, encoded, true);

    let draws = engine.take_draw_calls();
    assert_eq!(draws.len(), 2);
    assert_eq!(draws[0].topology, PrimitiveTopology::Triangles);
    assert_eq!(draws[0].vertex_first, 10);
    assert_eq!(draws[0].vertex_count, 20);
    assert_eq!(draws[0].indexed, false);
    assert_eq!(draws[0].base_instance, 0);
    assert_eq!(draws[0].instance_count, 1);
    assert_eq!(draws[1].base_instance, 1);
    assert_eq!(draws[1].instance_count, 1);
}

#[test]
fn test_call_method_report_semaphore() {
    let mut engine = Maxwell3D::new();
    // Set up report semaphore: address, payload.
    engine.call_method(REPORT_SEMAPHORE_BASE, 0, true); // addr_high
    engine.call_method(REPORT_SEMAPHORE_BASE + 1, 0x5000, true); // addr_low
    engine.call_method(REPORT_SEMAPHORE_BASE + 2, 0xCAFE, true); // payload
                                                                 // Trigger: Release (0), short query (bit 28 set).
    let query_val = (1 << 28) | 0; // short_query=1, operation=Release
    engine.call_method(REPORT_SEMAPHORE_QUERY, query_val, true);

    let writes = engine.execute_pending(&|_, _| {});
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].gpu_va, 0x5000);
    // Short query: 4 bytes (u32 payload).
    assert_eq!(writes[0].data.len(), 4);
    let payload = u32::from_le_bytes(writes[0].data[..4].try_into().unwrap());
    assert_eq!(payload, 0xCAFE);
}
