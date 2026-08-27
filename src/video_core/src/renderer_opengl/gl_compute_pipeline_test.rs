// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::engines::kepler_compute::{ConstBufferConfig, KeplerCompute, LaunchParams};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use shader_recompiler::shader_info::{
    ImageBufferDescriptor, ImageDescriptor, ImageFormat, StorageBufferDescriptor,
    TextureBufferDescriptor, TextureDescriptor, TextureType,
};

#[test]
fn pipeline_programs_and_fence_use_upstream_raii_owners() {
    let pipeline = ComputePipeline::new_for_test(Info::default(), false, 0);
    assert_eq!(pipeline.source_program.handle, 0);
    assert_eq!(pipeline.assembly_program.handle, 0);
    assert!(pipeline.built_fence.handle.is_null());
}

#[test]
fn compute_texture_bindings_keep_upstream_static_vector_capacities() {
    let bindings = ComputeTextureBindings::default();
    assert_eq!(
        bindings.views.capacity(),
        (MAX_TEXTURES + MAX_IMAGES) as usize
    );
    assert_eq!(bindings.samplers.capacity(), MAX_TEXTURES as usize);
}

#[test]
#[should_panic(expected = "image-view bindings exceed Eden's static_vector capacity")]
fn compute_texture_bindings_do_not_spill_views_past_upstream_capacity() {
    let mut bindings = ComputeTextureBindings::default();
    for _ in 0..=(MAX_TEXTURES + MAX_IMAGES) {
        bindings.push_view(ImageViewInOut::default());
    }
}

#[test]
#[should_panic(expected = "sampler bindings exceed Eden's static_vector capacity")]
fn compute_texture_bindings_do_not_spill_samplers_past_upstream_capacity() {
    let mut bindings = ComputeTextureBindings::default();
    for index in 0..=MAX_TEXTURES {
        bindings.push_sampler(SamplerId { index });
    }
}

#[test]
fn compute_pipeline_key_hash() {
    let key = ComputePipelineKey {
        unique_hash: 0x1234,
        shared_memory_size: 1024,
        workgroup_size: [32, 1, 1],
    };
    let h1 = key.hash_key();
    let h2 = key.hash_key();
    assert_eq!(h1, h2);
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (&key as *const ComputePipelineKey).cast::<u8>(),
            std::mem::size_of::<ComputePipelineKey>(),
        )
    };
    assert_eq!(h1, city_hash64(bytes));

    let key2 = ComputePipelineKey {
        unique_hash: 0x5678,
        shared_memory_size: 1024,
        workgroup_size: [32, 1, 1],
    };
    assert_ne!(key.hash_key(), key2.hash_key());
}

#[test]
fn compute_pipeline_key_size() {
    assert_eq!(
        std::mem::size_of::<ComputePipelineKey>(),
        8 + 4 + 12 // u64 + u32 + 3*u32
    );
    assert_eq!(std::mem::offset_of!(ComputePipelineKey, unique_hash), 0);
    assert_eq!(
        std::mem::offset_of!(ComputePipelineKey, shared_memory_size),
        8
    );
    assert_eq!(std::mem::offset_of!(ComputePipelineKey, workgroup_size), 12);
}

#[test]
fn set_engine_replaces_current_compute_engine_state() {
    let first_memory = Arc::new(parking_lot::Mutex::new(MemoryManager::new(0)));
    let second_memory = Arc::new(parking_lot::Mutex::new(MemoryManager::new(1)));
    let mut first = Box::new(KeplerCompute::new(Arc::clone(&first_memory)));
    first.launch_description.linked_tsc = false;
    let mut second = Box::new(KeplerCompute::new(Arc::clone(&second_memory)));
    second.launch_description.linked_tsc = true;

    let mut pipeline = ComputePipeline::new_for_test(Info::default(), false, 0);
    pipeline.set_engine(NonNull::from(first.as_mut()), Arc::clone(&first_memory));
    assert!(
        !unsafe { pipeline.kepler_compute.unwrap().as_ref() }
            .launch_description()
            .linked_tsc
    );
    assert!(Arc::ptr_eq(
        pipeline.gpu_memory.as_ref().unwrap(),
        &first_memory
    ));

    pipeline.set_engine(NonNull::from(second.as_mut()), Arc::clone(&second_memory));
    assert!(
        unsafe { pipeline.kepler_compute.unwrap().as_ref() }
            .launch_description()
            .linked_tsc
    );
    assert!(Arc::ptr_eq(
        pipeline.gpu_memory.as_ref().unwrap(),
        &second_memory
    ));
}

#[test]
fn descriptor_sync_regs_come_from_live_compute_engine() {
    let memory = Arc::new(parking_lot::Mutex::new(MemoryManager::new(0)));
    let mut engine = KeplerCompute::new(memory);
    engine.launch_description.linked_tsc = true;
    engine.call_method(0x557, 0, true);
    engine.call_method(0x558, 0x3000, true);
    engine.call_method(0x559, 1, true);
    engine.call_method(0x55D, 0, true);
    engine.call_method(0x55E, 0x4000, true);
    engine.call_method(0x55F, 6, true);

    let regs = ComputePipeline::descriptor_sync_regs(&engine);

    assert!(regs.linked_tsc);
    assert_eq!(regs.tic_addr, 0x4000);
    assert_eq!(regs.tic_limit, 6);
    assert_eq!(regs.tsc_addr, 0x3000);
    assert_eq!(regs.tsc_limit, 1);
}

#[test]
fn compute_pipeline_info_state_matches_upstream_constructor_metadata() {
    let mut info = Info::default();
    info.constant_buffer_used_sizes[0] = 0x10;
    info.constant_buffer_used_sizes[7] = 0x80;
    info.constant_buffer_used_sizes[8] = 0x90;
    info.texture_buffer_descriptors
        .push(TextureBufferDescriptor {
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 2,
            size_shift: 2,
        });
    info.texture_buffer_descriptors
        .push(TextureBufferDescriptor {
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 3,
            size_shift: 2,
        });
    info.image_buffer_descriptors.push(ImageBufferDescriptor {
        format: ImageFormat::R32Uint,
        is_written: false,
        is_read: true,
        is_integer: true,
        cbuf_index: 0,
        cbuf_offset: 0,
        count: 4,
        size_shift: 2,
    });
    info.texture_descriptors.push(TextureDescriptor {
        texture_type: TextureType::Color2D,
        is_depth: false,
        is_multisample: false,
        is_integer: false,
        has_secondary: false,
        cbuf_index: 0,
        cbuf_offset: 0,
        shift_left: 0,
        secondary_cbuf_index: 0,
        secondary_cbuf_offset: 0,
        secondary_shift_left: 0,
        count: 6,
        size_shift: 2,
    });
    info.image_descriptors.push(ImageDescriptor {
        texture_type: TextureType::Color2D,
        format: ImageFormat::R32Uint,
        is_written: false,
        is_read: true,
        is_integer: true,
        cbuf_index: 0,
        cbuf_offset: 0,
        count: 7,
        size_shift: 2,
    });
    info.storage_buffers_descriptors
        .push(StorageBufferDescriptor {
            cbuf_index: 0,
            cbuf_offset: 0x20,
            count: 2,
            is_written: true,
        });
    info.uses_local_memory = true;

    let glsl_state = ComputePipeline::info_state(&info, false, 0);
    assert_eq!(
        glsl_state.uniform_buffer_sizes,
        [0x10, 0, 0, 0, 0, 0, 0, 0x80]
    );
    assert_eq!(glsl_state.num_texture_buffers, 5);
    assert_eq!(glsl_state.num_image_buffers, 4);
    assert!(glsl_state.use_storage_buffers);
    assert!(!glsl_state.writes_global_memory);
    assert!(glsl_state.uses_local_memory);

    let glasm_state_without_capacity = ComputePipeline::info_state(&info, true, 2);
    assert!(!glasm_state_without_capacity.use_storage_buffers);
    assert!(glasm_state_without_capacity.writes_global_memory);

    let glasm_state_with_capacity = ComputePipeline::info_state(&info, true, 3);
    assert!(glasm_state_with_capacity.use_storage_buffers);
    assert!(!glasm_state_with_capacity.writes_global_memory);
}

#[test]
fn descriptor_capacity_assertions_are_fail_soft_like_eden() {
    let mut info = Info::default();
    info.texture_descriptors.push(TextureDescriptor {
        texture_type: TextureType::Color2D,
        is_depth: false,
        is_multisample: false,
        is_integer: false,
        has_secondary: false,
        cbuf_index: 0,
        cbuf_offset: 0,
        shift_left: 0,
        secondary_cbuf_index: 0,
        secondary_cbuf_offset: 0,
        secondary_shift_left: 0,
        count: MAX_TEXTURES + 1,
        size_shift: 2,
    });
    info.image_descriptors.push(ImageDescriptor {
        texture_type: TextureType::Color2D,
        format: ImageFormat::R32Uint,
        is_written: false,
        is_read: true,
        is_integer: true,
        cbuf_index: 0,
        cbuf_offset: 0,
        count: MAX_IMAGES + 1,
        size_shift: 2,
    });

    let state = ComputePipeline::info_state(&info, false, 0);
    assert_eq!(state.num_texture_buffers, 0);
    assert_eq!(state.num_image_buffers, 0);
}

#[test]
fn configure_buffer_state_follows_upstream_compute_order() {
    let mut info = Info::default();
    info.constant_buffer_mask = 0b101;
    info.constant_buffer_used_sizes[0] = 0x20;
    info.constant_buffer_used_sizes[2] = 0x40;
    info.storage_buffers_descriptors
        .push(StorageBufferDescriptor {
            cbuf_index: 0,
            cbuf_offset: 0x100,
            count: 1,
            is_written: false,
        });
    info.storage_buffers_descriptors
        .push(StorageBufferDescriptor {
            cbuf_index: 2,
            cbuf_offset: 0x200,
            count: 1,
            is_written: true,
        });

    let pipeline = ComputePipeline::new_for_test(info, false, 0);
    let tracker = MaxwellDeviceMemoryManager::default();
    let staging_pool =
        crate::renderer_opengl::gl_staging_buffer_pool::make_shared_staging_buffer_pool();
    let runtime =
        crate::renderer_opengl::gl_buffer_cache::BufferCacheRuntime::new_for_test(staging_pool);
    let mut buffer_cache = OpenGLBufferCache::new(&tracker, runtime);
    let mut channel = crate::control::channel_state::ChannelState::new(1);
    let mut kepler_compute =
        KeplerCompute::new(Arc::new(parking_lot::Mutex::new(MemoryManager::new(0))));
    kepler_compute.launch_description.const_buffer_enable_mask = 0b101;
    kepler_compute.launch_description.const_buffers[0] = ConstBufferConfig {
        address: 0x1_0000,
        size: 0x1000,
    };
    kepler_compute.launch_description.const_buffers[2] = ConstBufferConfig {
        address: 0x2_0000,
        size: 0x1000,
    };
    channel.kepler_compute = Some(Box::new(kepler_compute));
    buffer_cache.create_channel(&channel);
    buffer_cache.bind_to_channel(channel.bind_id);
    {
        let cs = buffer_cache.current_channel_state_mut().unwrap();
        cs.enabled_compute_storage_buffers = 0xFFFF;
        cs.written_compute_storage_buffers = 0xFFFF;
    }

    pipeline.configure_buffer_state(&mut buffer_cache);

    let cs = buffer_cache.current_channel_state().unwrap();
    assert_eq!(cs.enabled_compute_uniform_buffer_mask, 0b101);
    // SAFETY: `pipeline` still owns the configured size array in this scope.
    assert_eq!(
        *unsafe { cs.compute_uniform_buffer_sizes.unwrap().as_ref() },
        [0x20, 0, 0x40, 0, 0, 0, 0, 0]
    );
    assert_eq!(cs.enabled_compute_storage_buffers, 0b11);
    assert_eq!(cs.written_compute_storage_buffers, 0b10);
}

#[test]
fn collect_texture_handles_follows_upstream_compute_order_and_pairs() {
    let mut info = Info::default();
    info.texture_buffer_descriptors
        .push(TextureBufferDescriptor {
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count: 1,
            size_shift: 2,
        });
    info.image_buffer_descriptors.push(ImageBufferDescriptor {
        format: ImageFormat::R32Uint,
        is_written: false,
        is_read: true,
        is_integer: true,
        cbuf_index: 0,
        cbuf_offset: 4,
        count: 1,
        size_shift: 2,
    });
    info.texture_descriptors.push(TextureDescriptor {
        texture_type: TextureType::Color2D,
        is_depth: false,
        is_multisample: false,
        is_integer: false,
        has_secondary: true,
        cbuf_index: 0,
        cbuf_offset: 8,
        shift_left: 0,
        secondary_cbuf_index: 1,
        secondary_cbuf_offset: 0,
        secondary_shift_left: 20,
        count: 2,
        size_shift: 2,
    });
    info.image_descriptors.push(ImageDescriptor {
        texture_type: TextureType::Color2D,
        format: ImageFormat::R32Uint,
        is_written: true,
        is_read: false,
        is_integer: true,
        cbuf_index: 0,
        cbuf_offset: 16,
        count: 1,
        size_shift: 2,
    });

    let mut qmd = LaunchParams::default();
    qmd.linked_tsc = false;
    qmd.const_buffer_enable_mask = 0b11;
    qmd.const_buffers[0] = ConstBufferConfig {
        address: 0x1000,
        size: 0x100,
    };
    qmd.const_buffers[1] = ConstBufferConfig {
        address: 0x2000,
        size: 0x100,
    };
    let events = std::cell::RefCell::new(Vec::new());
    let handles = ComputePipeline::collect_texture_bindings(
        &info,
        &qmd,
        |addr| {
            events.borrow_mut().push(("read", addr));
            match addr {
                0x1000 => 0x0000_0011,
                0x1004 => 0xABC0_0022,
                0x1008 => 0x0000_0033,
                0x100c => 0x0000_0044,
                0x1010 => 0xDEF0_0055,
                0x2000 => 0x0000_0007,
                0x2004 => 0x0000_0008,
                _ => panic!("unexpected read at 0x{addr:X}"),
            }
        },
        |index| {
            events.borrow_mut().push(("sampler", index as u64));
            crate::texture_cache::types::NULL_SAMPLER_ID
        },
    );

    assert_eq!(handles.samplers.len(), 2);
    assert_eq!(
        handles
            .views
            .iter()
            .map(|view| view.index)
            .collect::<Vec<_>>(),
        vec![0x11, 0x22, 0x33, 0x44, 0x55]
    );
    assert_eq!(
        handles
            .views
            .iter()
            .map(|view| view.blacklist)
            .collect::<Vec<_>>(),
        vec![false, false, false, false, true]
    );
    assert_eq!(
        *events.borrow(),
        vec![
            ("read", 0x1000),
            ("read", 0x1004),
            ("read", 0x1008),
            ("read", 0x2000),
            ("sampler", 7),
            ("read", 0x100c),
            ("read", 0x2004),
            ("sampler", 8),
            ("read", 0x1010),
        ]
    );
}
