// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `vk_master_semaphore.h` / `vk_master_semaphore.cpp`.
//!
//! Master timeline semaphore that tracks GPU tick progress and manages
//! fence-based submission synchronization.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use ash::vk;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of pre-allocated fences for the fence fallback path.
///
/// Port of `FENCE_RESERVE_SIZE` from `vk_master_semaphore.cpp`.
const FENCE_RESERVE_SIZE: usize = 8;

/// Wait stage mask for queue submissions.
///
/// Port of `wait_stage_mask` from `vk_master_semaphore.cpp`.
const WAIT_STAGE_MASK: vk::PipelineStageFlags = vk::PipelineStageFlags::from_raw(
    vk::PipelineStageFlags::VERTEX_SHADER.as_raw()
        | vk::PipelineStageFlags::FRAGMENT_SHADER.as_raw()
        | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT.as_raw(),
);

struct FenceWaitState {
    device: ash::Device,
    gpu_tick: Arc<AtomicU64>,
    wait_queue: Mutex<VecDeque<(u64, vk::Fence)>>,
    free_queue: Mutex<VecDeque<vk::Fence>>,
    wait_cv: Condvar,
    progress_cv: Condvar,
    stop_requested: Arc<AtomicBool>,
}

// ---------------------------------------------------------------------------
// MasterSemaphore
// ---------------------------------------------------------------------------

/// Port of `MasterSemaphore` class.
///
/// Tracks the logical tick (CPU-side counter) and GPU tick (last known
/// completed work), using either timeline semaphores or fence fallback.
pub struct MasterSemaphore {
    device: ash::Device,

    /// Timeline semaphore handle (when supported, otherwise null).
    semaphore: vk::Semaphore,

    /// Whether the device supports timeline semaphores.
    has_timeline: bool,

    synchronization2_core: bool,
    synchronization2_khr: Option<ash::extensions::khr::Synchronization2>,

    /// Current known GPU tick.
    gpu_tick: Arc<AtomicU64>,

    /// Current logical tick (CPU-side, monotonically increasing).
    current_tick: AtomicU64,

    fence_wait_state: Option<Arc<FenceWaitState>>,
    stop_requested: Arc<AtomicBool>,
    debug_thread: Option<JoinHandle<()>>,
    wait_thread: Option<JoinHandle<()>>,

    /// Graphics queue for submissions.
    graphics_queue: vk::Queue,
}

// Safety: MasterSemaphore is designed to be shared across threads via Arc.
// The Vulkan handles are only used through thread-safe atomic operations
// and mutex-protected sections, matching the upstream C++ thread safety model.
unsafe impl Send for MasterSemaphore {}
unsafe impl Sync for MasterSemaphore {}

impl MasterSemaphore {
    /// Port of `MasterSemaphore::MasterSemaphore`.
    ///
    /// Creates a new master semaphore. If `has_timeline` is true, creates
    /// a timeline semaphore; otherwise sets up fence-based fallback.
    pub fn new(
        device: ash::Device,
        graphics_queue: vk::Queue,
        has_timeline: bool,
        synchronization2_core: bool,
        synchronization2_khr: Option<ash::extensions::khr::Synchronization2>,
    ) -> Result<Self, vk::Result> {
        let semaphore = if has_timeline {
            let mut type_ci = vk::SemaphoreTypeCreateInfo {
                s_type: vk::StructureType::SEMAPHORE_TYPE_CREATE_INFO,
                p_next: std::ptr::null(),
                semaphore_type: vk::SemaphoreType::TIMELINE,
                initial_value: 0,
            };
            let ci = vk::SemaphoreCreateInfo {
                s_type: vk::StructureType::SEMAPHORE_CREATE_INFO,
                p_next: &mut type_ci as *mut _ as *mut std::ffi::c_void,
                flags: vk::SemaphoreCreateFlags::empty(),
            };
            unsafe { device.create_semaphore(&ci, None)? }
        } else {
            vk::Semaphore::null()
        };

        let free_fences = if !has_timeline {
            let mut fences = VecDeque::with_capacity(FENCE_RESERVE_SIZE);
            let fence_ci = vk::FenceCreateInfo::builder().build();
            for _ in 0..FENCE_RESERVE_SIZE {
                let fence = match unsafe { device.create_fence(&fence_ci, None) } {
                    Ok(fence) => fence,
                    Err(error) => {
                        for fence in fences {
                            unsafe {
                                device.destroy_fence(fence, None);
                            }
                        }
                        return Err(error);
                    }
                };
                fences.push_back(fence);
            }
            fences
        } else {
            VecDeque::new()
        };

        let gpu_tick = Arc::new(AtomicU64::new(0));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let fence_wait_state = (!has_timeline).then(|| {
            Arc::new(FenceWaitState {
                device: device.clone(),
                gpu_tick: Arc::clone(&gpu_tick),
                wait_queue: Mutex::new(VecDeque::new()),
                free_queue: Mutex::new(free_fences),
                wait_cv: Condvar::new(),
                progress_cv: Condvar::new(),
                stop_requested: Arc::clone(&stop_requested),
            })
        });
        let wait_thread = if let Some(state) = fence_wait_state.as_ref() {
            let thread_state = Arc::clone(state);
            match std::thread::Builder::new()
                .name("VulkanFenceWait".into())
                .spawn(move || Self::wait_thread(thread_state))
            {
                Ok(thread) => Some(thread),
                Err(error) => {
                    log::error!("Failed to spawn Vulkan fence wait thread: {error}");
                    let free_queue = state.free_queue.lock().unwrap();
                    for fence in free_queue.iter() {
                        unsafe {
                            device.destroy_fence(*fence, None);
                        }
                    }
                    return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
                }
            }
        } else {
            None
        };
        let debug_thread = if has_timeline && *common::settings::values().renderer_debug.get_value()
        {
            let debug_device = device.clone();
            let debug_stop = Arc::clone(&stop_requested);
            match std::thread::Builder::new()
                .name("VulkanTimelineDebugWait".into())
                .spawn(move || {
                    let mut counter = 0;
                    while !debug_stop.load(Ordering::Acquire) {
                        let semaphores = [semaphore];
                        let values = [counter];
                        let wait_info = vk::SemaphoreWaitInfo::builder()
                            .semaphores(&semaphores)
                            .values(&values)
                            .build();
                        match unsafe { debug_device.wait_semaphores(&wait_info, 10_000_000) } {
                            Ok(()) => counter += 1,
                            Err(vk::Result::TIMEOUT) => {}
                            Err(error) => {
                                log::error!(
                                    "Vulkan timeline debug wait failed at value {counter}: {error:?}"
                                );
                                std::process::abort();
                            }
                        }
                    }
                })
            {
                Ok(thread) => Some(thread),
                Err(error) => {
                    log::error!("Failed to spawn Vulkan timeline debug wait thread: {error}");
                    unsafe {
                        device.destroy_semaphore(semaphore, None);
                    }
                    return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
                }
            }
        } else {
            None
        };

        Ok(MasterSemaphore {
            device,
            semaphore,
            has_timeline,
            synchronization2_core,
            synchronization2_khr,
            gpu_tick,
            current_tick: AtomicU64::new(1),
            fence_wait_state,
            stop_requested,
            debug_thread,
            wait_thread,
            graphics_queue,
        })
    }

    /// Returns the current logical tick.
    /// Port of `MasterSemaphore::CurrentTick`.
    pub fn current_tick(&self) -> u64 {
        self.current_tick.load(Ordering::Acquire)
    }

    /// Returns the last known GPU tick.
    /// Port of `MasterSemaphore::KnownGpuTick`.
    pub fn known_gpu_tick(&self) -> u64 {
        self.gpu_tick.load(Ordering::Acquire)
    }

    /// Returns true when a tick has been completed by the GPU.
    /// Port of `MasterSemaphore::IsFree`.
    pub fn is_free(&self, tick: u64) -> bool {
        self.known_gpu_tick() >= tick
    }

    /// Advance to the next logical tick and return the old one.
    /// Port of `MasterSemaphore::NextTick`.
    pub fn next_tick(&self) -> u64 {
        self.current_tick.fetch_add(1, Ordering::Release)
    }

    /// Refresh the known GPU tick from the timeline semaphore counter.
    /// Port of `MasterSemaphore::Refresh`.
    pub fn refresh(&self) {
        if self.semaphore == vk::Semaphore::null() {
            // If we don't support timeline semaphores, there's nothing to refresh
            return;
        }

        loop {
            let this_tick = self.gpu_tick.load(Ordering::Acquire);
            let counter = unsafe {
                self.device
                    .get_semaphore_counter_value(self.semaphore)
                    .expect("Failed to query the Vulkan timeline semaphore counter")
            };
            if counter < this_tick {
                return;
            }
            match self.gpu_tick.compare_exchange_weak(
                this_tick,
                counter,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(_) => continue,
            }
        }
    }

    /// Wait for a tick to be completed on the GPU.
    /// Port of `MasterSemaphore::Wait`.
    pub fn wait(&self, tick: u64) {
        if self.semaphore == vk::Semaphore::null() {
            let state = self
                .fence_wait_state
                .as_ref()
                .expect("fence wait state must exist without timeline semaphores");
            if self.gpu_tick.load(Ordering::Acquire) >= tick {
                return;
            }
            let mut free_queue = state.free_queue.lock().unwrap();
            while self.gpu_tick.load(Ordering::Acquire) < tick {
                free_queue = state.progress_cv.wait(free_queue).unwrap();
            }
            return;
        }

        // No need to wait if the GPU is ahead of the tick
        if self.is_free(tick) {
            return;
        }

        // Update the GPU tick and try again
        self.refresh();
        if self.is_free(tick) {
            return;
        }

        // Fallback to a regular timeline semaphore wait
        let semaphores = [self.semaphore];
        let values = [tick];
        let wait_info = vk::SemaphoreWaitInfo::builder()
            .semaphores(&semaphores)
            .values(&values)
            .build();

        loop {
            let result = unsafe { self.device.wait_semaphores(&wait_info, u64::MAX) };
            match result {
                Ok(_) => break,
                Err(vk::Result::TIMEOUT) => continue,
                Err(error) => panic!("MasterSemaphore timeline wait failed: {error:?}"),
            }
        }

        self.refresh();
    }

    /// Submit the device graphics queue with timeline semaphore signaling.
    /// Port of `MasterSemaphore::SubmitQueue`.
    pub fn submit_queue(
        &self,
        cmdbuf: vk::CommandBuffer,
        upload_cmdbuf: vk::CommandBuffer,
        signal_semaphore: vk::Semaphore,
        wait_semaphore: vk::Semaphore,
        host_tick: u64,
    ) -> vk::Result {
        if self.has_timeline {
            self.submit_queue_timeline(
                cmdbuf,
                upload_cmdbuf,
                signal_semaphore,
                wait_semaphore,
                host_tick,
            )
        } else {
            self.submit_queue_fence(
                cmdbuf,
                upload_cmdbuf,
                signal_semaphore,
                wait_semaphore,
                host_tick,
            )
        }
    }

    /// Timeline semaphore submission path.
    /// Port of `MasterSemaphore::SubmitQueueTimeline`.
    fn submit_queue_timeline(
        &self,
        cmdbuf: vk::CommandBuffer,
        upload_cmdbuf: vk::CommandBuffer,
        signal_semaphore: vk::Semaphore,
        wait_semaphore: vk::Semaphore,
        host_tick: u64,
    ) -> vk::Result {
        if self.synchronization2_core || self.synchronization2_khr.is_some() {
            let command_buffer_infos = [
                vk::CommandBufferSubmitInfo::builder()
                    .command_buffer(upload_cmdbuf)
                    .build(),
                vk::CommandBufferSubmitInfo::builder()
                    .command_buffer(cmdbuf)
                    .build(),
            ];
            let mut signal_infos = [
                vk::SemaphoreSubmitInfo::builder()
                    .semaphore(self.semaphore)
                    .value(host_tick)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build(),
                vk::SemaphoreSubmitInfo::default(),
            ];
            let mut num_signal_semaphores = 1;
            if signal_semaphore != vk::Semaphore::null() {
                signal_infos[1] = vk::SemaphoreSubmitInfo::builder()
                    .semaphore(signal_semaphore)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .build();
                num_signal_semaphores = 2;
            }
            let wait_info = vk::SemaphoreSubmitInfo::builder()
                .semaphore(wait_semaphore)
                .stage_mask(vk::PipelineStageFlags2::from_raw(
                    WAIT_STAGE_MASK.as_raw() as u64
                ))
                .build();
            let wait_infos = if wait_semaphore != vk::Semaphore::null() {
                std::slice::from_ref(&wait_info)
            } else {
                &[]
            };
            let submit_info = vk::SubmitInfo2::builder()
                .wait_semaphore_infos(wait_infos)
                .command_buffer_infos(&command_buffer_infos)
                .signal_semaphore_infos(&signal_infos[..num_signal_semaphores])
                .build();
            return unsafe {
                if self.synchronization2_core {
                    self.device.queue_submit2(
                        self.graphics_queue,
                        &[submit_info],
                        vk::Fence::null(),
                    )
                } else {
                    self.synchronization2_khr
                        .as_ref()
                        .expect("KHR synchronization2 dispatch must exist")
                        .queue_submit2(self.graphics_queue, &[submit_info], vk::Fence::null())
                }
                .err()
                .unwrap_or(vk::Result::SUCCESS)
            };
        }

        let num_signal_semaphores = if signal_semaphore != vk::Semaphore::null() {
            2u32
        } else {
            1u32
        };
        let signal_values = [host_tick, 0u64];
        let signal_semaphores = [self.semaphore, signal_semaphore];
        let cmdbuffers = [upload_cmdbuf, cmdbuf];

        let num_wait_semaphores = if wait_semaphore != vk::Semaphore::null() {
            1u32
        } else {
            0u32
        };

        let wait_zero = 0u64;
        let timeline_si = vk::TimelineSemaphoreSubmitInfo {
            s_type: vk::StructureType::TIMELINE_SEMAPHORE_SUBMIT_INFO,
            p_next: std::ptr::null(),
            wait_semaphore_value_count: num_wait_semaphores,
            p_wait_semaphore_values: if num_wait_semaphores != 0 {
                &wait_zero
            } else {
                std::ptr::null()
            },
            signal_semaphore_value_count: num_signal_semaphores,
            p_signal_semaphore_values: signal_values.as_ptr(),
        };

        let submit_info = vk::SubmitInfo {
            s_type: vk::StructureType::SUBMIT_INFO,
            p_next: &timeline_si as *const _ as *const std::ffi::c_void,
            wait_semaphore_count: num_wait_semaphores,
            p_wait_semaphores: if num_wait_semaphores != 0 {
                &wait_semaphore
            } else {
                std::ptr::null()
            },
            p_wait_dst_stage_mask: if num_wait_semaphores != 0 {
                &WAIT_STAGE_MASK
            } else {
                std::ptr::null()
            },
            command_buffer_count: cmdbuffers.len() as u32,
            p_command_buffers: cmdbuffers.as_ptr(),
            signal_semaphore_count: num_signal_semaphores,
            p_signal_semaphores: signal_semaphores.as_ptr(),
        };

        unsafe {
            self.device
                .queue_submit(self.graphics_queue, &[submit_info], vk::Fence::null())
                .err()
                .unwrap_or(vk::Result::SUCCESS)
        }
    }

    /// Fence-based submission fallback path.
    /// Port of `MasterSemaphore::SubmitQueueFence`.
    fn submit_queue_fence(
        &self,
        cmdbuf: vk::CommandBuffer,
        upload_cmdbuf: vk::CommandBuffer,
        signal_semaphore: vk::Semaphore,
        wait_semaphore: vk::Semaphore,
        host_tick: u64,
    ) -> vk::Result {
        if self.synchronization2_core || self.synchronization2_khr.is_some() {
            let command_buffer_infos = [
                vk::CommandBufferSubmitInfo::builder()
                    .command_buffer(upload_cmdbuf)
                    .build(),
                vk::CommandBufferSubmitInfo::builder()
                    .command_buffer(cmdbuf)
                    .build(),
            ];
            let signal_info = vk::SemaphoreSubmitInfo::builder()
                .semaphore(signal_semaphore)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .build();
            let signal_infos = if signal_semaphore != vk::Semaphore::null() {
                std::slice::from_ref(&signal_info)
            } else {
                &[]
            };
            let wait_info = vk::SemaphoreSubmitInfo::builder()
                .semaphore(wait_semaphore)
                .stage_mask(vk::PipelineStageFlags2::from_raw(
                    WAIT_STAGE_MASK.as_raw() as u64
                ))
                .build();
            let wait_infos = if wait_semaphore != vk::Semaphore::null() {
                std::slice::from_ref(&wait_info)
            } else {
                &[]
            };
            let submit_info = vk::SubmitInfo2::builder()
                .wait_semaphore_infos(wait_infos)
                .command_buffer_infos(&command_buffer_infos)
                .signal_semaphore_infos(signal_infos)
                .build();
            let fence = self.get_free_fence();
            let result = unsafe {
                if self.synchronization2_core {
                    self.device
                        .queue_submit2(self.graphics_queue, &[submit_info], fence)
                } else {
                    self.synchronization2_khr
                        .as_ref()
                        .expect("KHR synchronization2 dispatch must exist")
                        .queue_submit2(self.graphics_queue, &[submit_info], fence)
                }
                .err()
                .unwrap_or(vk::Result::SUCCESS)
            };
            if result == vk::Result::SUCCESS {
                let state = self
                    .fence_wait_state
                    .as_ref()
                    .expect("fence wait state must exist without timeline semaphores");
                state
                    .wait_queue
                    .lock()
                    .unwrap()
                    .push_back((host_tick, fence));
                state.wait_cv.notify_one();
            } else {
                unsafe {
                    self.device.destroy_fence(fence, None);
                }
            }
            return result;
        }

        let num_signal_semaphores = if signal_semaphore != vk::Semaphore::null() {
            1u32
        } else {
            0u32
        };
        let num_wait_semaphores = if wait_semaphore != vk::Semaphore::null() {
            1u32
        } else {
            0u32
        };

        let cmdbuffers = [upload_cmdbuf, cmdbuf];

        let submit_info = vk::SubmitInfo {
            s_type: vk::StructureType::SUBMIT_INFO,
            p_next: std::ptr::null(),
            wait_semaphore_count: num_wait_semaphores,
            p_wait_semaphores: if num_wait_semaphores != 0 {
                &wait_semaphore
            } else {
                std::ptr::null()
            },
            p_wait_dst_stage_mask: if num_wait_semaphores != 0 {
                &WAIT_STAGE_MASK
            } else {
                std::ptr::null()
            },
            command_buffer_count: cmdbuffers.len() as u32,
            p_command_buffers: cmdbuffers.as_ptr(),
            signal_semaphore_count: num_signal_semaphores,
            p_signal_semaphores: if num_signal_semaphores != 0 {
                &signal_semaphore
            } else {
                std::ptr::null()
            },
        };

        let fence = self.get_free_fence();
        let result = unsafe {
            self.device
                .queue_submit(self.graphics_queue, &[submit_info], fence)
                .err()
                .unwrap_or(vk::Result::SUCCESS)
        };

        if result == vk::Result::SUCCESS {
            let state = self
                .fence_wait_state
                .as_ref()
                .expect("fence wait state must exist without timeline semaphores");
            let mut wait_queue = state.wait_queue.lock().unwrap();
            wait_queue.push_back((host_tick, fence));
            state.wait_cv.notify_one();
        } else {
            unsafe {
                self.device.destroy_fence(fence, None);
            }
        }

        result
    }

    /// Get a fence from the free pool, or create a new one.
    /// Port of `MasterSemaphore::GetFreeFence`.
    fn get_free_fence(&self) -> vk::Fence {
        let state = self
            .fence_wait_state
            .as_ref()
            .expect("fence wait state must exist without timeline semaphores");
        let mut free_queue = state.free_queue.lock().unwrap();
        if free_queue.is_empty() {
            let fence_ci = vk::FenceCreateInfo::builder().build();
            return unsafe {
                self.device
                    .create_fence(&fence_ci, None)
                    .unwrap_or_else(|error| {
                        log::error!("Failed to grow Vulkan fence pool: {error:?}");
                        std::process::abort();
                    })
            };
        }

        free_queue.pop_back().unwrap()
    }

    /// Port of `MasterSemaphore::WaitThread`.
    fn wait_thread(state: Arc<FenceWaitState>) {
        loop {
            let (host_tick, fence) = {
                let mut wait_queue = state.wait_queue.lock().unwrap();
                while wait_queue.is_empty() && !state.stop_requested.load(Ordering::Acquire) {
                    wait_queue = state.wait_cv.wait(wait_queue).unwrap();
                }
                if state.stop_requested.load(Ordering::Acquire) {
                    return;
                }
                wait_queue
                    .pop_front()
                    .expect("notified fence wait queue must not be empty")
            };

            if let Err(error) = unsafe { state.device.wait_for_fences(&[fence], true, u64::MAX) } {
                log::error!("MasterSemaphore fence wait failed: {error:?}");
                std::process::abort();
            }
            if let Err(error) = unsafe { state.device.reset_fences(&[fence]) } {
                log::error!("MasterSemaphore fence reset failed: {error:?}");
                std::process::abort();
            }
            {
                let mut free_queue = state.free_queue.lock().unwrap();
                free_queue.push_front(fence);
                state.gpu_tick.store(host_tick, Ordering::Release);
            }
            state.progress_cv.notify_one();
        }
    }
}

impl Drop for MasterSemaphore {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::Release);
        if let Some(state) = self.fence_wait_state.as_ref() {
            state.wait_cv.notify_all();
            state.progress_cv.notify_all();
        }
        if let Some(thread) = self.debug_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.wait_thread.take() {
            let _ = thread.join();
        }

        // Clean up timeline semaphore
        if self.semaphore != vk::Semaphore::null() {
            unsafe {
                self.device.destroy_semaphore(self.semaphore, None);
            }
        }

        // Clean up free fences
        if let Some(state) = self.fence_wait_state.as_ref() {
            let free_queue = state.free_queue.lock().unwrap();
            for fence in free_queue.iter() {
                unsafe {
                    self.device.destroy_fence(*fence, None);
                }
            }
            let wait_queue = state.wait_queue.lock().unwrap();
            for &(_, fence) in wait_queue.iter() {
                unsafe {
                    self.device.destroy_fence(fence, None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants() {
        assert_eq!(FENCE_RESERVE_SIZE, 8);
        assert_eq!(
            WAIT_STAGE_MASK,
            vk::PipelineStageFlags::VERTEX_SHADER
                | vk::PipelineStageFlags::FRAGMENT_SHADER
                | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
        );
    }
}
