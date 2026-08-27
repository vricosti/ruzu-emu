// SPDX-FileCopyrightText: Copyright 2022 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `vk_turbo_mode.h` / `vk_turbo_mode.cpp`.

#[cfg(not(target_os = "android"))]
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(not(target_os = "android"))]
use ash::vk;

#[cfg(not(target_os = "android"))]
use super::renderer_vulkan::create_device;
#[cfg(not(target_os = "android"))]
use super::shader_util::build_shader;
#[cfg(not(target_os = "android"))]
use crate::host_shaders::spirv_shaders::VULKAN_TURBO_MODE_COMP_SPV;
#[cfg(not(target_os = "android"))]
use crate::vulkan_common::vulkan_device::Device;
#[cfg(not(target_os = "android"))]
use crate::vulkan_common::vulkan_memory_allocator::{
    AllocatedBuffer, MemoryAllocator, MemoryUsage,
};
use crate::vulkan_common::vulkan_wrapper::{Instance, VulkanError};

#[cfg(all(target_os = "android", target_arch = "aarch64"))]
#[link(name = "adrenotools")]
unsafe extern "C" {
    fn adrenotools_set_turbo(enable: bool);
}

#[cfg(not(target_os = "android"))]
const TURBO_BUFFER_SIZE: u64 = 2 * 1024 * 1024;
const IDLE_TIMEOUT: Duration = Duration::from_millis(100);

struct TurboState {
    submission_time: Mutex<Instant>,
    submission_cv: Condvar,
    stop_requested: AtomicBool,
}

#[cfg(not(target_os = "android"))]
struct TurboResources {
    allocator: MemoryAllocator,
    device: Device,
    buffer: Option<AllocatedBuffer>,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_set: vk::DescriptorSet,
    shader: vk::ShaderModule,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    fence: vk::Fence,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
}

// All Vulkan handles in this owner belong to its dedicated logical device and
// are moved to, then exclusively used by, the turbo thread.
#[cfg(not(target_os = "android"))]
unsafe impl Send for TurboResources {}

#[cfg(not(target_os = "android"))]
impl TurboResources {
    fn new(instance: &Instance) -> Result<Self, VulkanError> {
        let device = create_device(instance, vk::SurfaceKHR::null())?;
        let allocator = MemoryAllocator::new(&device);
        Ok(Self {
            allocator,
            device,
            buffer: None,
            descriptor_pool: vk::DescriptorPool::null(),
            descriptor_set_layout: vk::DescriptorSetLayout::null(),
            descriptor_set: vk::DescriptorSet::null(),
            shader: vk::ShaderModule::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            fence: vk::Fence::null(),
            command_pool: vk::CommandPool::null(),
            command_buffer: vk::CommandBuffer::null(),
        })
    }

    fn initialize(&mut self) -> Result<(), VulkanError> {
        let dld = self.device.get_logical();
        let buffer_info = vk::BufferCreateInfo::builder()
            .size(TURBO_BUFFER_SIZE)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        self.buffer = Some(
            self.allocator
                .create_buffer(&buffer_info, MemoryUsage::DeviceLocal)?,
        );

        let pool_sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::STORAGE_BUFFER,
            descriptor_count: 1,
        }];
        let descriptor_pool_info = vk::DescriptorPoolCreateInfo::builder()
            .max_sets(1)
            .pool_sizes(&pool_sizes)
            .build();
        self.descriptor_pool = unsafe {
            dld.create_descriptor_pool(&descriptor_pool_info, None)
                .map_err(VulkanError::new)?
        };

        let layout_bindings = [vk::DescriptorSetLayoutBinding {
            binding: 0,
            descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
            descriptor_count: 1,
            stage_flags: vk::ShaderStageFlags::COMPUTE,
            p_immutable_samplers: std::ptr::null(),
        }];
        let descriptor_set_layout_info = vk::DescriptorSetLayoutCreateInfo::builder()
            .bindings(&layout_bindings)
            .build();
        self.descriptor_set_layout = unsafe {
            dld.create_descriptor_set_layout(&descriptor_set_layout_info, None)
                .map_err(VulkanError::new)?
        };

        let set_layouts = [self.descriptor_set_layout];
        let descriptor_set_info = vk::DescriptorSetAllocateInfo::builder()
            .descriptor_pool(self.descriptor_pool)
            .set_layouts(&set_layouts)
            .build();
        self.descriptor_set = unsafe {
            dld.allocate_descriptor_sets(&descriptor_set_info)
                .map_err(VulkanError::new)?[0]
        };

        self.shader = build_shader(dld, VULKAN_TURBO_MODE_COMP_SPV).map_err(VulkanError::new)?;

        let pipeline_layout_info = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&set_layouts)
            .build();
        self.pipeline_layout = unsafe {
            dld.create_pipeline_layout(&pipeline_layout_info, None)
                .map_err(VulkanError::new)?
        };

        let entry_name = CString::new("main").unwrap();
        let stage = vk::PipelineShaderStageCreateInfo::builder()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(self.shader)
            .name(&entry_name)
            .build();
        let pipeline_info = [vk::ComputePipelineCreateInfo::builder()
            .stage(stage)
            .layout(self.pipeline_layout)
            .build()];
        self.pipeline = unsafe {
            dld.create_compute_pipelines(vk::PipelineCache::null(), &pipeline_info, None)
                .map_err(|(_, result)| VulkanError::new(result))?[0]
        };

        self.fence = unsafe {
            dld.create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(VulkanError::new)?
        };
        let command_pool_info = vk::CommandPoolCreateInfo::builder()
            .flags(
                vk::CommandPoolCreateFlags::TRANSIENT
                    | vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
            )
            .queue_family_index(self.device.get_graphics_family())
            .build();
        self.command_pool = unsafe {
            dld.create_command_pool(&command_pool_info, None)
                .map_err(VulkanError::new)?
        };
        let command_buffer_info = vk::CommandBufferAllocateInfo::builder()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1)
            .build();
        self.command_buffer = unsafe {
            dld.allocate_command_buffers(&command_buffer_info)
                .map_err(VulkanError::new)?[0]
        };
        Ok(())
    }

    fn dispatch(&self) -> Result<(), vk::Result> {
        let dld = self.device.get_logical();
        let buffer = self
            .buffer
            .as_ref()
            .expect("turbo buffer must be initialized")
            .handle();
        unsafe {
            dld.reset_fences(&[self.fence])?;

            let descriptor_buffer_info = [vk::DescriptorBufferInfo {
                buffer,
                offset: 0,
                range: vk::WHOLE_SIZE,
            }];
            let descriptor_write = [vk::WriteDescriptorSet::builder()
                .dst_set(self.descriptor_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&descriptor_buffer_info)
                .build()];
            dld.update_descriptor_sets(&descriptor_write, &[]);

            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();
            dld.begin_command_buffer(self.command_buffer, &begin_info)?;
            dld.cmd_fill_buffer(self.command_buffer, buffer, 0, vk::WHOLE_SIZE, 0);
            dld.cmd_bind_descriptor_sets(
                self.command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );
            dld.cmd_bind_pipeline(
                self.command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline,
            );
            dld.cmd_dispatch(self.command_buffer, 64, 64, 1);
            dld.end_command_buffer(self.command_buffer)?;

            let command_buffers = [self.command_buffer];
            let submit_info = [vk::SubmitInfo::builder()
                .command_buffers(&command_buffers)
                .build()];
            dld.queue_submit(self.device.get_graphics_queue(), &submit_info, self.fence)?;
            dld.wait_for_fences(&[self.fence], true, u64::MAX)?;
        }
        Ok(())
    }
}

#[cfg(not(target_os = "android"))]
impl Drop for TurboResources {
    fn drop(&mut self) {
        let dld = self.device.get_logical();
        unsafe {
            if self.command_pool != vk::CommandPool::null() {
                dld.destroy_command_pool(self.command_pool, None);
            }
            if self.fence != vk::Fence::null() {
                dld.destroy_fence(self.fence, None);
            }
            if self.pipeline != vk::Pipeline::null() {
                dld.destroy_pipeline(self.pipeline, None);
            }
            if self.pipeline_layout != vk::PipelineLayout::null() {
                dld.destroy_pipeline_layout(self.pipeline_layout, None);
            }
            if self.shader != vk::ShaderModule::null() {
                dld.destroy_shader_module(self.shader, None);
            }
            if self.descriptor_set_layout != vk::DescriptorSetLayout::null() {
                dld.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            }
            if self.descriptor_pool != vk::DescriptorPool::null() {
                dld.destroy_descriptor_pool(self.descriptor_pool, None);
            }
        }
        self.buffer.take();
    }
}

/// Port of upstream `TurboMode`.
pub struct TurboMode {
    state: Arc<TurboState>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl TurboMode {
    pub fn new(instance: &Instance) -> Result<Self, VulkanError> {
        #[cfg(not(target_os = "android"))]
        let resources = TurboResources::new(instance)?;
        #[cfg(target_os = "android")]
        let _ = instance;
        let state = Arc::new(TurboState {
            submission_time: Mutex::new(Instant::now()),
            submission_cv: Condvar::new(),
            stop_requested: AtomicBool::new(false),
        });
        let thread_state = Arc::clone(&state);
        let thread_builder = std::thread::Builder::new().name("TurboMode".to_string());
        #[cfg(not(target_os = "android"))]
        let thread_handle = thread_builder
            .spawn(move || Self::run(thread_state, resources))
            .expect("Failed to spawn TurboMode thread");
        #[cfg(target_os = "android")]
        let thread_handle = thread_builder
            .spawn(move || Self::run(thread_state))
            .expect("Failed to spawn TurboMode thread");
        Ok(Self {
            state,
            thread_handle: Some(thread_handle),
        })
    }

    fn run(
        state: Arc<TurboState>,
        #[cfg(not(target_os = "android"))] mut resources: TurboResources,
    ) {
        #[cfg(not(target_os = "android"))]
        if let Err(error) = resources.initialize() {
            log::error!("TurboMode Vulkan initialization failed: {error:?}");
            return;
        }

        while !state.stop_requested.load(Ordering::Acquire) {
            #[cfg(all(target_os = "android", target_arch = "aarch64"))]
            unsafe {
                adrenotools_set_turbo(true);
            }

            #[cfg(not(target_os = "android"))]
            if let Err(error) = resources.dispatch() {
                log::error!("TurboMode Vulkan submission failed: {error:?}");
                return;
            }

            let mut submission_time = state.submission_time.lock().unwrap();
            while !state.stop_requested.load(Ordering::Acquire)
                && Instant::now().saturating_duration_since(*submission_time) > IDLE_TIMEOUT
            {
                submission_time = state.submission_cv.wait(submission_time).unwrap();
            }
        }

        #[cfg(all(target_os = "android", target_arch = "aarch64"))]
        unsafe {
            adrenotools_set_turbo(false);
        }
    }

    pub fn queue_submitted(&self) {
        let mut submission_time = self.state.submission_time.lock().unwrap();
        *submission_time = Instant::now();
        self.state.submission_cv.notify_one();
    }

    pub fn submit_callback(&self) -> Arc<dyn Fn() + Send + Sync> {
        let state = Arc::clone(&self.state);
        Arc::new(move || {
            let mut submission_time = state.submission_time.lock().unwrap();
            *submission_time = Instant::now();
            state.submission_cv.notify_one();
        })
    }
}

impl Drop for TurboMode {
    fn drop(&mut self) {
        self.state.stop_requested.store(true, Ordering::Release);
        self.state.submission_cv.notify_all();
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan_common::vulkan_instance::{create_instance, WindowSystemType};
    use crate::vulkan_common::vulkan_library::open_library;

    #[test]
    fn constants_match_upstream() {
        #[cfg(not(target_os = "android"))]
        assert_eq!(TURBO_BUFFER_SIZE, 2 * 1024 * 1024);
        assert_eq!(IDLE_TIMEOUT, Duration::from_millis(100));
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    #[ignore = "requires a Vulkan-capable host"]
    fn creates_submits_and_destroys_turbo_workload() {
        let entry = open_library().expect("Vulkan loader");
        let instance = create_instance(
            entry,
            vk::API_VERSION_1_1,
            WindowSystemType::Headless,
            false,
        )
        .expect("headless Vulkan instance");
        let turbo_mode = TurboMode::new(&instance).expect("turbo workload resources");
        turbo_mode.queue_submitted();
        std::thread::sleep(Duration::from_millis(20));
        drop(turbo_mode);
    }
}
