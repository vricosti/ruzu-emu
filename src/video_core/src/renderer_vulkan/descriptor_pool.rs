// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `vk_descriptor_pool.h` / `vk_descriptor_pool.cpp`.
//!
//! Banked descriptor set allocation pool. Manages multiple VkDescriptorPools
//! organized into banks by descriptor type requirements.

use std::ptr::NonNull;
use std::sync::{Arc, Mutex, RwLock};

use ash::vk;
use shader_recompiler::shader_info::{HasCount, Info as ShaderInfo};

use super::master_semaphore::MasterSemaphore;
use super::resource_pool::ResourcePool;
use super::scheduler::Scheduler;
use crate::vulkan_common::vulkan_device::{Device, DeviceReference};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Prefer small grow rates to avoid saturating the descriptor pool with
/// barely used pipelines.
///
/// Port of `SETS_GROW_RATE` from `vk_descriptor_pool.cpp`.
const SETS_GROW_RATE: usize = 16;

/// Score difference threshold for bank reuse.
///
/// Port of `SCORE_THRESHOLD` from `vk_descriptor_pool.cpp`.
const SCORE_THRESHOLD: i32 = 3;

// ---------------------------------------------------------------------------
// DescriptorBankInfo
// ---------------------------------------------------------------------------

/// Descriptor type counts for a descriptor bank.
///
/// Port of `DescriptorBankInfo` from `vk_descriptor_pool.h`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DescriptorBankInfo {
    /// Number of uniform buffer descriptors.
    pub uniform_buffers: u32,
    /// Number of storage buffer descriptors.
    pub storage_buffers: u32,
    /// Number of texture buffer descriptors.
    pub texture_buffers: u32,
    /// Number of image buffer descriptors.
    pub image_buffers: u32,
    /// Number of texture descriptors.
    pub textures: u32,
    /// Number of image descriptors.
    pub images: u32,
    /// Total number of descriptors (score).
    pub score: i32,
}

impl DescriptorBankInfo {
    /// Port of `DescriptorBankInfo::IsSuperset`.
    ///
    /// Returns true if this bank can satisfy the given subset's requirements.
    pub fn is_superset(&self, subset: &DescriptorBankInfo) -> bool {
        self.uniform_buffers >= subset.uniform_buffers
            && self.storage_buffers >= subset.storage_buffers
            && self.texture_buffers >= subset.texture_buffers
            && self.image_buffers >= subset.image_buffers
            && self.textures >= subset.textures
            && self.images >= subset.images
    }
}

// ---------------------------------------------------------------------------
// DescriptorBank
// ---------------------------------------------------------------------------

/// A bank of descriptor pools with a specific descriptor type configuration.
///
/// Port of `DescriptorBank` from `vk_descriptor_pool.cpp`.
struct DescriptorBank {
    info: DescriptorBankInfo,
    pools: Vec<vk::DescriptorPool>,
}

/// Port of the file-local `Accumulate` helper.
fn accumulate<T: HasCount>(descriptors: &[T]) -> u32 {
    descriptors.iter().fold(0, |count, descriptor| {
        count.wrapping_add(descriptor.descriptor_count())
    })
}

/// Port of the file-local `MakeBankInfo` helper.
fn make_bank_info(infos: &[ShaderInfo]) -> DescriptorBankInfo {
    let mut bank = DescriptorBankInfo::default();
    for info in infos {
        bank.uniform_buffers = bank
            .uniform_buffers
            .wrapping_add(accumulate(&info.constant_buffer_descriptors));
        bank.storage_buffers = bank
            .storage_buffers
            .wrapping_add(accumulate(&info.storage_buffers_descriptors));
        bank.texture_buffers = bank
            .texture_buffers
            .wrapping_add(accumulate(&info.texture_buffer_descriptors));
        bank.image_buffers = bank
            .image_buffers
            .wrapping_add(accumulate(&info.image_buffer_descriptors));
        bank.textures = bank
            .textures
            .wrapping_add(accumulate(&info.texture_descriptors));
        bank.images = bank
            .images
            .wrapping_add(accumulate(&info.image_descriptors));
    }
    bank.score = bank
        .uniform_buffers
        .wrapping_add(bank.storage_buffers)
        .wrapping_add(bank.texture_buffers)
        .wrapping_add(bank.image_buffers)
        .wrapping_add(bank.textures)
        .wrapping_add(bank.images) as i32;
    bank
}

// ---------------------------------------------------------------------------
// Helper: AllocatePool
// ---------------------------------------------------------------------------

/// Allocate a new VkDescriptorPool for the given bank.
///
/// Port of `AllocatePool` from `vk_descriptor_pool.cpp`.
fn allocate_pool(
    device: &Device,
    bank: &mut DescriptorBank,
) -> Result<vk::DescriptorPool, vk::Result> {
    let logical = device.get_logical();
    let sets_per_pool = device.get_sets_per_pool();
    let mut pool_sizes = Vec::with_capacity(6);
    let info = &bank.info;

    let add = |pool_sizes: &mut Vec<vk::DescriptorPoolSize>, ty: vk::DescriptorType, count: u32| {
        if count > 0 {
            pool_sizes.push(vk::DescriptorPoolSize {
                ty,
                descriptor_count: count.wrapping_mul(sets_per_pool),
            });
        }
    };

    add(
        &mut pool_sizes,
        vk::DescriptorType::UNIFORM_BUFFER,
        info.uniform_buffers,
    );
    add(
        &mut pool_sizes,
        vk::DescriptorType::STORAGE_BUFFER,
        info.storage_buffers,
    );
    add(
        &mut pool_sizes,
        vk::DescriptorType::UNIFORM_TEXEL_BUFFER,
        info.texture_buffers,
    );
    add(
        &mut pool_sizes,
        vk::DescriptorType::STORAGE_TEXEL_BUFFER,
        info.image_buffers,
    );
    add(
        &mut pool_sizes,
        vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
        info.textures,
    );
    add(
        &mut pool_sizes,
        vk::DescriptorType::STORAGE_IMAGE,
        info.images,
    );

    let pool_ci = vk::DescriptorPoolCreateInfo::builder()
        .max_sets(sets_per_pool)
        .pool_sizes(&pool_sizes)
        .build();

    let pool = unsafe { logical.create_descriptor_pool(&pool_ci, None)? };
    bank.pools.push(pool);
    Ok(pool)
}

struct DescriptorAllocatorState {
    resource_pool: ResourcePool,
    sets: Vec<Vec<vk::DescriptorSet>>,
}

/// Stable non-owning counterpart of the `this` pointer captured by Eden's
/// deferred scheduler commands.
#[derive(Clone, Copy)]
pub(crate) struct DescriptorAllocatorReference(NonNull<DescriptorAllocator>);

impl DescriptorAllocatorReference {
    pub(crate) fn commit(self) -> Result<vk::DescriptorSet, vk::Result> {
        unsafe { self.0.as_ref().commit() }
    }
}

// SAFETY: descriptor allocators live in stable renderer-owned objects. The
// scheduler drains its commands before those owners are destroyed, and the
// mutable allocator state is protected by its mutex.
unsafe impl Send for DescriptorAllocatorReference {}
unsafe impl Sync for DescriptorAllocatorReference {}

/// Port of upstream `DescriptorAllocator`.
///
/// Each pipeline owns one allocator. The allocator reserves descriptor sets
/// in groups of `SETS_GROW_RATE` and tags every set with the scheduler tick
/// that references it.
pub struct DescriptorAllocator {
    device: DeviceReference,
    bank: NonNull<Mutex<DescriptorBank>>,
    layout: vk::DescriptorSetLayout,
    state: Mutex<DescriptorAllocatorState>,
}

// SAFETY: Eden's move-only allocator is transferred with its owning pipeline
// or compute pass. Shared access from deferred commands is synchronized by
// `state`, while access to a shared descriptor bank is synchronized by the
// bank mutex.
unsafe impl Send for DescriptorAllocator {}
unsafe impl Sync for DescriptorAllocator {}

impl DescriptorAllocator {
    fn new(
        device: &Device,
        master_semaphore: Arc<MasterSemaphore>,
        bank: NonNull<Mutex<DescriptorBank>>,
        layout: vk::DescriptorSetLayout,
    ) -> Self {
        Self {
            device: DeviceReference::new(device),
            bank,
            layout,
            state: Mutex::new(DescriptorAllocatorState {
                resource_pool: ResourcePool::new(master_semaphore, SETS_GROW_RATE),
                sets: Vec::new(),
            }),
        }
    }

    pub(crate) fn reference(&self) -> DescriptorAllocatorReference {
        DescriptorAllocatorReference(NonNull::from(self))
    }

    /// Port of `DescriptorAllocator::Commit`.
    pub fn commit(&self) -> Result<vk::DescriptorSet, vk::Result> {
        let mut state = self.state.lock().unwrap();
        let device = self.device;
        let bank = self.bank;
        let layout = self.layout;
        let DescriptorAllocatorState {
            resource_pool,
            sets,
        } = &mut *state;
        let mut allocate =
            |begin: usize, end: usize| Self::allocate(device.get(), bank, layout, sets, begin, end);
        let index = resource_pool.try_commit_resource(&mut allocate)?;
        Ok(sets[index / SETS_GROW_RATE][index % SETS_GROW_RATE])
    }

    /// Port of `DescriptorAllocator::Allocate`.
    fn allocate(
        device: &Device,
        bank: NonNull<Mutex<DescriptorBank>>,
        layout: vk::DescriptorSetLayout,
        sets: &mut Vec<Vec<vk::DescriptorSet>>,
        begin: usize,
        end: usize,
    ) -> Result<(), vk::Result> {
        sets.push(Self::allocate_descriptors(
            device,
            bank,
            layout,
            end - begin,
        )?);
        Ok(())
    }

    /// Port of `DescriptorAllocator::AllocateDescriptors`.
    fn allocate_descriptors(
        device: &Device,
        mut bank: NonNull<Mutex<DescriptorBank>>,
        layout: vk::DescriptorSetLayout,
        count: usize,
    ) -> Result<Vec<vk::DescriptorSet>, vk::Result> {
        let logical = device.get_logical();
        let layouts = vec![layout; count];
        let mut bank = unsafe { bank.as_mut() }.lock().unwrap();
        let mut allocate_info = vk::DescriptorSetAllocateInfo::builder()
            .descriptor_pool(*bank.pools.last().expect("descriptor bank has no pool"))
            .set_layouts(&layouts)
            .build();
        match unsafe { logical.allocate_descriptor_sets(&allocate_info) } {
            Ok(sets) => Ok(sets),
            Err(vk::Result::ERROR_OUT_OF_POOL_MEMORY) => {
                let pool = allocate_pool(device, &mut bank)?;
                allocate_info.descriptor_pool = pool;
                unsafe { logical.allocate_descriptor_sets(&allocate_info) }
            }
            Err(error) => Err(error),
        }
    }
}

// ---------------------------------------------------------------------------
// DescriptorPool
// ---------------------------------------------------------------------------

/// Banked descriptor pool manager.
///
/// Port of `DescriptorPool` from `vk_descriptor_pool.h`.
///
/// Manages multiple descriptor banks, each containing VkDescriptorPools
/// configured for specific descriptor type requirements. Banks are reused
/// when their descriptor counts are close enough (within SCORE_THRESHOLD).
pub struct DescriptorPool {
    // Upstream's bank pools are RAII wrappers. Ruzu stores raw ash handles,
    // so their owner retains this lightweight reference for destruction.
    device: DeviceReference,
    banks_mutex: RwLock<BanksState>,
}

struct BanksState {
    bank_infos: Vec<DescriptorBankInfo>,
    banks: Vec<Box<Mutex<DescriptorBank>>>,
}

impl DescriptorPool {
    /// Port of `DescriptorPool::DescriptorPool`.
    pub fn new(device: &Device, _scheduler: &mut Scheduler) -> Self {
        DescriptorPool {
            device: DeviceReference::new(device),
            banks_mutex: RwLock::new(BanksState {
                bank_infos: Vec::new(),
                banks: Vec::new(),
            }),
        }
    }

    /// Port of `DescriptorPool::Allocator`.
    pub fn allocator(
        &self,
        device: &Device,
        scheduler: &Scheduler,
        layout: vk::DescriptorSetLayout,
        info: &DescriptorBankInfo,
    ) -> Result<DescriptorAllocator, vk::Result> {
        let bank = self.bank(device, info)?;
        Ok(DescriptorAllocator::new(
            device,
            Arc::clone(scheduler.get_master_semaphore()),
            bank,
            layout,
        ))
    }

    /// Port of `DescriptorPool::Allocator(..., span<const Shader::Info>)`.
    pub fn allocator_for_infos(
        &self,
        device: &Device,
        scheduler: &Scheduler,
        layout: vk::DescriptorSetLayout,
        infos: &[ShaderInfo],
    ) -> Result<DescriptorAllocator, vk::Result> {
        let bank = make_bank_info(infos);
        self.allocator(device, scheduler, layout, &bank)
    }

    /// Port of `DescriptorPool::Allocator(..., const Shader::Info&)`.
    pub fn allocator_for_info(
        &self,
        device: &Device,
        scheduler: &Scheduler,
        layout: vk::DescriptorSetLayout,
        info: &ShaderInfo,
    ) -> Result<DescriptorAllocator, vk::Result> {
        self.allocator(
            device,
            scheduler,
            layout,
            &make_bank_info(std::slice::from_ref(info)),
        )
    }

    /// Port of `DescriptorPool::Bank`.
    fn bank(
        &self,
        device: &Device,
        reqs: &DescriptorBankInfo,
    ) -> Result<NonNull<Mutex<DescriptorBank>>, vk::Result> {
        {
            let state = self.banks_mutex.read().unwrap();
            for (i, bank_info) in state.bank_infos.iter().enumerate() {
                if (bank_info.score - reqs.score).abs() < SCORE_THRESHOLD
                    && bank_info.is_superset(reqs)
                {
                    return Ok(NonNull::from(state.banks[i].as_ref()));
                }
            }
        }

        let mut state = self.banks_mutex.write().unwrap();
        state.bank_infos.push(*reqs);
        let bank = Box::new(Mutex::new(DescriptorBank {
            info: *reqs,
            pools: Vec::new(),
        }));
        state.banks.push(bank);
        let mut bank = NonNull::from(
            state
                .banks
                .last()
                .expect("new descriptor bank disappeared")
                .as_ref(),
        );
        allocate_pool(device, &mut unsafe { bank.as_mut() }.lock().unwrap())?;

        Ok(bank)
    }
}

impl Drop for DescriptorPool {
    fn drop(&mut self) {
        let state = self.banks_mutex.write().unwrap();
        for bank in &state.banks {
            for pool in &bank.lock().unwrap().pools {
                unsafe {
                    self.device
                        .get()
                        .get_logical()
                        .destroy_descriptor_pool(*pool, None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shader_recompiler::shader_info::ConstantBufferDescriptor;

    #[test]
    fn allocator_keeps_an_upstream_device_reference() {
        fn device_reference(allocator: &DescriptorAllocator) -> DeviceReference {
            allocator.device
        }
        fn require_signature(_: fn(&DescriptorAllocator) -> DeviceReference) {}

        require_signature(device_reference);
    }

    #[test]
    fn bank_info_superset() {
        let big = DescriptorBankInfo {
            uniform_buffers: 10,
            storage_buffers: 10,
            texture_buffers: 10,
            image_buffers: 10,
            textures: 10,
            images: 10,
            score: 60,
        };
        let small = DescriptorBankInfo {
            uniform_buffers: 5,
            storage_buffers: 5,
            texture_buffers: 5,
            image_buffers: 5,
            textures: 5,
            images: 5,
            score: 30,
        };
        assert!(big.is_superset(&small));
        assert!(!small.is_superset(&big));
    }

    #[test]
    fn bank_info_superset_checks_image_descriptors() {
        let bank = DescriptorBankInfo {
            image_buffers: 8,
            images: 1,
            score: 9,
            ..DescriptorBankInfo::default()
        };
        let request = DescriptorBankInfo {
            image_buffers: 1,
            images: 2,
            score: 3,
            ..DescriptorBankInfo::default()
        };

        assert!(!bank.is_superset(&request));
    }

    #[test]
    fn constants() {
        assert_eq!(SETS_GROW_RATE, 16);
        assert_eq!(SCORE_THRESHOLD, 3);
    }

    #[test]
    fn make_bank_info_accumulates_all_shader_infos() {
        let mut first = ShaderInfo::default();
        first
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 0, count: 2 });
        let mut second = ShaderInfo::default();
        second
            .constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 1, count: 3 });

        let bank = make_bank_info(&[first, second]);
        assert_eq!(bank.uniform_buffers, 5);
        assert_eq!(bank.score, 5);
    }

    #[test]
    fn accumulate_preserves_unsigned_wrapping() {
        let descriptors = [
            ConstantBufferDescriptor {
                index: 0,
                count: u32::MAX,
            },
            ConstantBufferDescriptor { index: 1, count: 2 },
        ];
        assert_eq!(accumulate(&descriptors), 1);
    }

    #[test]
    fn descriptor_bank_address_survives_outer_vector_growth() {
        let mut banks = vec![Box::new(Mutex::new(DescriptorBank {
            info: DescriptorBankInfo::default(),
            pools: Vec::new(),
        }))];
        let first = NonNull::from(banks[0].as_ref());

        for _ in 0..64 {
            banks.push(Box::new(Mutex::new(DescriptorBank {
                info: DescriptorBankInfo::default(),
                pools: Vec::new(),
            })));
        }

        assert_eq!(first, NonNull::from(banks[0].as_ref()));
    }
}
