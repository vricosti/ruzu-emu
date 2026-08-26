// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::buffer_cache::buffer_cache::BufferCache;
use crate::buffer_cache::buffer_cache_base::{
    BufferCacheParams, GpuMemoryAccess, StagingBufferRef, TestBuffer, TestBufferCacheRuntime,
};
use crate::buffer_cache::word_manager::DeviceTracker;
use crate::engines::kepler_compute::{ConstBufferConfig, KeplerCompute};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::memory_manager::MemoryManager;
use crate::test_support::GpuAccuracyGuard;
use common::settings_enums::GpuAccuracy;

#[test]
fn amd_logic_op_workaround_matches_upstream() {
    let integer_attribs = [crate::engines::maxwell_3d::VertexAttribInfo::default()];
    assert!(effective_logic_op_enabled(false, true, &integer_attribs));

    let mut float_attribs = integer_attribs;
    float_attribs[0].attrib_type = VertexAttribType::Float;
    assert!(!effective_logic_op_enabled(true, true, &float_attribs));

    assert!(effective_logic_op_enabled(true, false, &float_attribs));
    assert!(!effective_logic_op_enabled(false, false, &integer_attribs));
}

#[test]
fn amd_logic_op_workaround_persists_the_register_value() {
    let draw_state = DrawState::default();
    let mut registers = crate::engines::draw_manager::Maxwell3DDrawRegisters::default();
    registers.logic_op.enabled = true;
    registers.vertex_attribs[0].attrib_type = VertexAttribType::Float;
    let mut view = Maxwell3DDrawView::with_register_snapshot(&draw_state, false, registers);

    let logic_op = apply_amd_logic_op_workaround(&mut view, true);

    assert!(!logic_op.enabled);
    assert!(!view.logic_op().enabled);
}

#[test]
fn test_rasterizer_retains_one_stable_state_tracker_owner() {
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rasterizer = RasterizerOpenGL::new_for_test(syncpoints);
    let referenced = unsafe { rasterizer.state_tracker.as_mut() } as *mut StateTracker;
    let owned = rasterizer
        .owned_state_tracker
        .as_deref_mut()
        .expect("test rasterizer state tracker owner") as *mut StateTracker;

    assert_eq!(referenced, owned);
}

#[test]
fn get_flush_area_uses_the_boxed_cache_objects() {
    let syncpoints = Arc::new(SyncpointManager::new());
    let rasterizer = RasterizerOpenGL::new_for_test(syncpoints);

    let area = RasterizerInterface::get_flush_area(&rasterizer, 0x4444_4123, 0x2345);

    assert_eq!(area.start_address, 0x4444_4000);
    assert_eq!(area.end_address, 0x4444_7000);
    assert!(area.preemptive);
}

#[test]
fn cache_invalidation_eagerly_removes_shaders_like_upstream() {
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rasterizer = RasterizerOpenGL::new_for_test(syncpoints);
    rasterizer.shader_cache.register(
        Box::new(crate::shader_cache::ShaderInfo {
            unique_hash: 0x1234,
            size_bytes: 0x40,
        }),
        0x4000,
        0x40,
    );

    assert!(rasterizer.shader_cache.try_get(0x4000).is_some());

    RasterizerInterface::on_cache_invalidation(&mut rasterizer, 0x4000, 4);

    assert!(rasterizer.shader_cache.try_get(0x4000).is_none());
}

#[test]
fn clip_enable_preserves_upstream_mask_and_dirty_order_without_a_state_change() {
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rasterizer = RasterizerOpenGL::new_for_test(syncpoints);
    rasterizer.last_clip_distance_mask = 0b0101;

    let draw_state = DrawState::default();
    let mut registers = crate::engines::draw_manager::Maxwell3DDrawRegisters::default();
    registers.user_clip_enable_raw = 0b0101;
    registers.dirty_flags[GlDirty::CLIP_DISTANCES as usize] = true;
    let mut view = Maxwell3DDrawView::with_register_snapshot(&draw_state, false, registers);

    // The effective mask is unchanged, so Eden clears the dirty bit and
    // returns before issuing any OpenGL call.
    rasterizer.sync_clip_enabled(&mut view, 0b1111);

    assert!(!view.dirty_flag(GlDirty::CLIP_DISTANCES));
    assert_eq!(rasterizer.last_clip_distance_mask, 0b0101);
}

#[test]
fn graphics_uniform_buffer_notifications_match_upstream() {
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rasterizer = RasterizerOpenGL::new_for_test(syncpoints);
    let mut channel = crate::control::channel_state::ChannelState::new(1);
    channel.maxwell_3d = Some(Box::new(crate::engines::maxwell_3d::Maxwell3D::new()));
    let gpu_memory = Arc::new(parking_lot::Mutex::new(MemoryManager::new(1)));
    gpu_memory
        .lock()
        .map(0x1234_5000, 0x1234_5000, 0x1000, 0, false);
    channel.memory_manager = Some(gpu_memory);
    rasterizer.initialize_channel(&mut channel);
    rasterizer.bind_channel(&mut channel);

    RasterizerInterface::bind_graphics_uniform_buffer(&mut rasterizer, 2, 3, 0x1234_5000, 0x400);
    let binding = rasterizer
        .buffer_cache
        .current_channel_state()
        .expect("bound buffer-cache channel")
        .uniform_buffers[2][3];
    assert_eq!(binding.device_addr, 0x1234_5000);
    assert_eq!(binding.size, 0x400);

    RasterizerInterface::disable_graphics_uniform_buffer(&mut rasterizer, 2, 3);
    let binding = rasterizer
        .buffer_cache
        .current_channel_state()
        .expect("bound buffer-cache channel")
        .uniform_buffers[2][3];
    let null_binding = crate::buffer_cache::buffer_cache_base::NULL_BINDING;
    assert_eq!(binding.device_addr, null_binding.device_addr);
    assert_eq!(binding.size, null_binding.size);
    assert_eq!(binding.buffer_id, null_binding.buffer_id);
}

fn query_memory_manager() -> (
    Vec<u8>,
    Arc<parking_lot::Mutex<crate::memory_manager::MemoryManager>>,
) {
    let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
    let mut backing = vec![0u8; 0x10000];
    device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
    device_memory.smmu_map_with_cpu_backing(
        0x9000_1000,
        backing.as_mut_ptr(),
        0x4000_0000,
        backing.len(),
        5,
        true,
    );

    let mut mm = MemoryManager::new_with_geometry_and_device_memory(
        0,
        Arc::clone(&device_memory),
        32,
        0x1_0000_0000,
        16,
        12,
    );
    mm.map(0x1000, 0x9000_1000, 0x10000, 0, false);
    let mm = Arc::new(parking_lot::Mutex::new(mm));
    (backing, mm)
}

fn install_query_memory_manager(rast: &mut RasterizerOpenGL) -> Vec<u8> {
    let (backing, mm) = query_memory_manager();
    let mut channel = crate::control::channel_state::ChannelState::new(1);
    channel.program_id = 0xCAFE;
    channel.memory_manager = Some(Arc::clone(&mm));
    rast.channel_memory_manager = Some(Arc::clone(&mm));
    rast.query_cache.create_channel(&channel);
    rast.query_cache.bind_to_channel(channel.bind_id);
    backing
}

struct DummyTracker;

impl DeviceTracker for DummyTracker {
    fn update_pages_cached_batch(&self, _ranges: &[(u64, usize)], _delta: i32) {}
}

struct TestParams;

impl BufferCacheParams for TestParams {
    type Runtime = TestBufferCacheRuntime;
    type Buffer = TestBuffer;
    type AsyncBuffer = StagingBufferRef;

    const IS_OPENGL: bool = false;
    const HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS: bool = false;
    const HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT: bool = true;
    const NEEDS_BIND_UNIFORM_INDEX: bool = false;
    const NEEDS_BIND_STORAGE_INDEX: bool = false;
    const USE_MEMORY_MAPS: bool = false;
    const SEPARATE_IMAGE_BUFFER_BINDINGS: bool = false;
    const USE_MEMORY_MAPS_FOR_UPLOADS: bool = false;
}

struct TestGpuMemory;

impl GpuMemoryAccess for TestGpuMemory {
    fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
        Some(0x100000 + gpu_addr)
    }

    fn read_u64(&self, gpu_addr: u64) -> Option<u64> {
        match gpu_addr {
            0x1020 => Some(0x5008),
            0x1028 => Some(0x30),
            _ => None,
        }
    }

    fn read_u32(&self, gpu_addr: u64) -> Option<u32> {
        match gpu_addr {
            0x1028 => Some(0x30),
            _ => None,
        }
    }

    fn is_within_gpu_address_range(&self, _gpu_addr: u64) -> bool {
        true
    }

    fn max_continuous_range(&self, _gpu_addr: u64, size: u64) -> u64 {
        size
    }

    fn get_memory_layout_size(&self, _gpu_addr: u64) -> u64 {
        0x1000
    }
}

#[test]
fn channel_bound_compute_engine_feeds_compute_storage_buffer_binding() {
    let memory = Arc::new(parking_lot::Mutex::new(MemoryManager::new(0)));
    let mut kepler_compute = KeplerCompute::new(memory);
    kepler_compute.launch_description.const_buffer_enable_mask = 1;
    kepler_compute.launch_description.const_buffers[0] = ConstBufferConfig {
        address: 0x1000,
        size: 0x100,
    };

    let tracker = DummyTracker;
    let mut cache =
        BufferCache::<TestParams, DummyTracker>::new(&tracker, TestBufferCacheRuntime::default());
    let mut channel = crate::control::channel_state::ChannelState::new(1);
    channel.kepler_compute = Some(Box::new(kepler_compute));
    cache.create_channel(&channel);
    cache.bind_to_channel(channel.bind_id);
    cache.set_gpu_memory(Box::new(TestGpuMemory));

    cache.bind_compute_storage_buffer(0, 0, 0x20, true);

    let binding = cache
        .current_channel_state()
        .unwrap()
        .compute_storage_buffers[0];
    assert_eq!(binding.device_addr, 0x105000);
    assert_eq!(binding.size, 0x38);
}

#[test]
fn device_memory_adapter_forwards_download_writes_to_guest_owner() {
    let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let writes_for_callback = Arc::clone(&writes);
    let adapter = DeviceMemoryAccessAdapter {
        device_reader: Arc::new(|_, out| {
            out.fill(0);
            true
        }),
        guest_writer: Some(Arc::new(move |addr, data| {
            writes_for_callback
                .lock()
                .unwrap()
                .push((addr, data.to_vec()));
        })),
    };

    crate::buffer_cache::buffer_cache_base::DeviceMemoryAccess::write_block_unsafe(
        &adapter,
        0x1234,
        &[1, 2, 3, 4],
    );

    assert_eq!(
        writes.lock().unwrap().as_slice(),
        &[(0x1234, vec![1, 2, 3, 4])]
    );
}

#[test]
fn query_fence_defers_guest_write_until_release() {
    let _gpu_accuracy = GpuAccuracyGuard::set(GpuAccuracy::High);
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rast = RasterizerOpenGL::new_for_test(syncpoints);
    let backing = install_query_memory_manager(&mut rast);
    rast.set_gpu_ticks_getter(Arc::new(|| 0));

    rast.query(0x1000, 0, QueryPropertiesFlags::IS_A_FENCE, 0x1234_5678, 0);

    assert_eq!(&backing[0..4], &[0; 4]);

    rast.release_fences(true);

    assert_eq!(&backing[0..4], &0x1234_5678u32.to_le_bytes());
}

#[test]
fn must_flush_region_matches_upstream_cache_order_and_accuracy_gate() {
    let texture_queries = std::cell::Cell::new(0);
    assert!(RasterizerOpenGL::must_flush_region_with(
        false,
        || true,
        || {
            texture_queries.set(texture_queries.get() + 1);
            false
        },
    ));
    assert_eq!(texture_queries.get(), 0);

    assert!(!RasterizerOpenGL::must_flush_region_with(
        false,
        || false,
        || {
            texture_queries.set(texture_queries.get() + 1);
            true
        },
    ));
    assert_eq!(texture_queries.get(), 0);

    assert!(RasterizerOpenGL::must_flush_region_with(
        true,
        || false,
        || {
            texture_queries.set(texture_queries.get() + 1);
            true
        },
    ));
    assert_eq!(texture_queries.get(), 1);
}

#[test]
fn signal_reference_accumulates_buffer_ranges_without_queuing_a_fence() {
    let _gpu_accuracy = GpuAccuracyGuard::set(GpuAccuracy::Low);
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rast = RasterizerOpenGL::new_for_test(syncpoints);
    rast.buffer_cache
        .test_add_uncommitted_gpu_modified_range(0x1000, 0x1000);

    assert_eq!(rast.fence_manager.queued_fence_count(), 0);
    assert!(!rast
        .buffer_cache
        .test_uncommitted_gpu_modified_ranges_empty());
    assert_eq!(
        rast.buffer_cache.test_committed_gpu_modified_range_count(),
        0
    );

    rast.signal_reference();

    assert_eq!(rast.fence_manager.queued_fence_count(), 0);
    assert!(rast
        .buffer_cache
        .test_uncommitted_gpu_modified_ranges_empty());
    assert_eq!(
        rast.buffer_cache.test_committed_gpu_modified_range_count(),
        1
    );
}

#[test]
fn release_fences_pops_async_flushes_for_stubbed_fence() {
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rast = RasterizerOpenGL::new_for_test(syncpoints);

    rast.buffer_cache.test_push_async_flush_buffer();
    assert!(rast.buffer_cache.should_wait_async_flushes());

    rast.signal_fence(Box::new(|| {}));
    rast.release_fences(true);

    assert!(!rast.buffer_cache.should_wait_async_flushes());
}

#[test]
fn signal_fence_triggers_invalidate_gpu_cache_callback() {
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rast = RasterizerOpenGL::new_for_test(syncpoints);
    let hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let hits_cb = Arc::clone(&hits);
    rast.set_invalidate_gpu_cache_callback(Arc::new(move || {
        hits_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));

    rast.signal_fence(Box::new(|| {}));

    assert_eq!(hits.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn signal_fence_executes_callback_immediately_outside_gpu_high_mode() {
    let _gpu_accuracy = GpuAccuracyGuard::set(GpuAccuracy::Low);
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rast = RasterizerOpenGL::new_for_test(syncpoints);
    let hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let hits_cb = Arc::clone(&hits);

    rast.signal_fence(Box::new(move || {
        hits_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));

    assert_eq!(hits.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn query_non_fence_payload_fallback_writes_immediately_and_preserves_payload() {
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rast = RasterizerOpenGL::new_for_test(syncpoints);
    let backing = install_query_memory_manager(&mut rast);
    rast.set_gpu_ticks_getter(Arc::new(|| 0));

    rast.query(
        0x3000,
        crate::query_cache::types::QueryType::Payload as u32,
        QueryPropertiesFlags::empty(),
        0xCAFE_BABE,
        0,
    );

    assert_eq!(&backing[0x2000..0x2004], &0xCAFE_BABEu32.to_le_bytes());
}

#[test]
fn query_has_timeout_payload_fallback_writes_immediately_and_preserves_payload() {
    let (backing, mm) = query_memory_manager();
    let _gpu_accuracy = GpuAccuracyGuard::set(GpuAccuracy::High);

    RasterizerOpenGL::make_query_fallback_operation(
        mm,
        0x4000,
        true,
        0xABCD_EF01,
        Some(Arc::new(|| 0x0123_4567_89AB_CDEF)),
    )();

    assert_eq!(&backing[0x3000..0x3008], &0xABCD_EF01u64.to_le_bytes());
    assert_eq!(
        &backing[0x3008..0x3010],
        &0x0123_4567_89AB_CDEFu64.to_le_bytes()
    );
}

#[test]
fn query_fallback_non_payload_fence_writes_one_after_release() {
    let _gpu_accuracy = GpuAccuracyGuard::set(GpuAccuracy::High);
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rast = RasterizerOpenGL::new_for_test(syncpoints);
    let backing = install_query_memory_manager(&mut rast);

    rast.query(
        0x5000,
        crate::query_cache::types::QueryType::VerticesGenerated as u32,
        QueryPropertiesFlags::IS_A_FENCE,
        0xDEAD_BEEF,
        0,
    );

    assert_eq!(&backing[0x4000..0x4004], &[0; 4]);

    rast.release_fences(true);

    assert_eq!(&backing[0x4000..0x4004], &1u32.to_le_bytes());
}

#[test]
fn tick_frame_resets_queued_commands_like_upstream() {
    let syncpoints = Arc::new(SyncpointManager::new());
    let mut rast = RasterizerOpenGL::new_for_test(syncpoints);
    rast.num_queued_commands = 7;

    rast.tick_frame();

    assert_eq!(rast.num_queued_commands, 0);
}

#[test]
fn clip_control_depth_matches_maxwell_depth_mode() {
    assert_eq!(clip_control_depth(DepthMode::ZeroToOne), gl::ZERO_TO_ONE);
    assert_eq!(
        clip_control_depth(DepthMode::MinusOneToOne),
        gl::NEGATIVE_ONE_TO_ONE
    );
}

#[test]
fn clip_control_origin_matches_upstream_flip_rules() {
    assert_eq!(clip_control_origin(false, 1.0), gl::LOWER_LEFT);
    assert_eq!(clip_control_origin(false, -1.0), gl::UPPER_LEFT);
    assert_eq!(clip_control_origin(true, 1.0), gl::UPPER_LEFT);
    assert_eq!(clip_control_origin(true, -1.0), gl::LOWER_LEFT);
}

#[test]
fn viewport_front_face_matches_upstream_flip_rules() {
    assert_eq!(
        viewport_front_face_to_gl(FrontFace::CCW, false, 1.0),
        gl::CW
    );
    assert_eq!(
        viewport_front_face_to_gl(FrontFace::CCW, false, -1.0),
        gl::CCW
    );
    assert_eq!(
        viewport_front_face_to_gl(FrontFace::CCW, true, 1.0),
        gl::CCW
    );
    assert_eq!(
        viewport_front_face_to_gl(FrontFace::CCW, true, -1.0),
        gl::CW
    );
}

#[test]
fn viewport_scale_matches_upstream_rounding_rules() {
    assert_eq!(scale_viewport_value(10.0, 2.0), 20.0);
    assert_eq!(scale_viewport_value(10.25, 0.5), 5.0);
    assert_eq!(scale_viewport_value(-10.25, 0.5), -5.0);
}

#[test]
fn viewport_zero_substitution_preserves_negative_extents() {
    assert_eq!(nonzero_viewport_extent(0.0), 1.0);
    assert_eq!(nonzero_viewport_extent(-3.0), -3.0);
    assert_eq!(nonzero_viewport_extent(4.0), 4.0);
}

#[test]
fn scissor_scaling_matches_upstream_downscale_accumulator() {
    assert_eq!(scale_scissor_value(0, 1, 1), 0);
    assert_eq!(scale_scissor_value(1, 1, 1), 1);
    assert_eq!(scale_scissor_value(2, 1, 1), 1);
    assert_eq!(scale_scissor_value(3, 1, 1), 2);
    assert_eq!(scale_scissor_value(11, 2, 0), 22);
}

#[test]
fn viewport_swizzle_components_match_upstream_bitfields() {
    let base = crate::renderer_opengl::maxwell_to_gl::viewport_swizzle(0);
    assert_eq!(
        viewport_swizzle_components(0x3210),
        [base, base + 1, base + 2, base + 3]
    );
}
