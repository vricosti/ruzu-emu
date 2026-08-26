// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/macro.h` and `video_core/macro.cpp`.
//!
//! Defines the macro instruction set, opcode decoding, and the `MacroEngine`
//! base that manages macro code upload, caching, and execution dispatch.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

mod macro_hle {
    // SPDX-FileCopyrightText: 2025 ruzu contributors
    // SPDX-License-Identifier: GPL-2.0-or-later

    //! Port of the HLE macro declarations and implementations in
    //! `video_core/macro.h` and `video_core/macro.cpp`.
    //!
    //! High-Level Emulation (HLE) of known macro programs. When a macro's hash
    //! matches a known program, the HLE implementation is used instead of
    //! interpreting/JIT-compiling the macro code, providing significant speedup.

    use super::AnyCachedMacro;
    use crate::dirty_flags;
    use crate::engines::engine_interface::EngineInterface;
    use crate::engines::maxwell_3d::*;

    // ── Known HLE program hashes ─────────────────────────────────────────────────

    // These are the Common::HashValue(code) values of known upstream macro programs.
    // Port of upstream `HLE_MACRO_LIST` in `macro.cpp`.

    const HASH_DRAW_ARRAYS_INDIRECT: u64 = 0x0D61FC9FAAC9FCAD;
    const HASH_DRAW_ARRAYS_INDIRECT_EXT: u64 = 0x8A4D173EB99A8603;
    const HASH_DRAW_INDEXED_INDIRECT: u64 = 0x771BB18C62444DA0;
    const HASH_DRAW_INDEXED_INDIRECT_EXT: u64 = 0x0217920100488FF7;
    const HASH_MULTI_DRAW_INDEXED_INDIRECT_COUNT: u64 = 0x3F5E74B9C9A50164;
    const HASH_MULTI_LAYER_CLEAR: u64 = 0xEAD26C3E2109B06B;
    const HASH_C713C83D8F63CCF3: u64 = 0xC713C83D8F63CCF3;
    const HASH_D7333D26E0A93EDE: u64 = 0xD7333D26E0A93EDE;
    const HASH_BIND_SHADER: u64 = 0xEB29B2A09AA06D38;
    const HASH_SET_RASTER_BOUNDING_BOX: u64 = 0xDB1341DBEB4C8AF7;
    const HASH_CLEAR_CONST_BUFFER_5F00: u64 = 0x6C97861D891EDF7E;
    const HASH_CLEAR_CONST_BUFFER_7000: u64 = 0xD246FDDF3A6173D7;
    const HASH_CLEAR_MEMORY: u64 = 0xEE4D0004BEC8ECF4;
    const HASH_TRANSFORM_FEEDBACK_SETUP: u64 = 0xFC0CF27F5FFAA661;
    const HASH_DRAW_INDIRECT_BYTE_COUNT: u64 = 0xB5F74EDB717278EC;

    static CLEAR_CONST_BUFFER_ZEROES: [u32; 0x7000] = [0; 0x7000];

    // ── HLE Macro Implementations ────────────────────────────────────────────────

    /// HLE: DrawArraysIndirect (non-extended).
    ///
    /// Port of `HLE_DrawArraysIndirect<false>`.
    pub(super) struct HleDrawArraysIndirect {
        pub(super) extended: bool,
    }

    impl HleDrawArraysIndirect {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_draw_arrays_indirect(self.extended, parameters) };
        }
    }

    /// HLE: DrawIndexedIndirect.
    ///
    /// Port of `HLE_DrawIndexedIndirect<extended>`.
    pub(super) struct HleDrawIndexedIndirect {
        pub(super) extended: bool,
    }

    impl HleDrawIndexedIndirect {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_draw_indexed_indirect(self.extended, parameters) };
        }
    }

    /// HLE: MultiLayerClear.
    ///
    /// Port of `HLE_MultiLayerClear`.
    pub(super) struct HleMultiLayerClear;

    impl HleMultiLayerClear {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_multi_layer_clear(parameters) };
        }
    }

    /// HLE: MultiDrawIndexedIndirectCount.
    ///
    /// Port of `HLE_MultiDrawIndexedIndirectCount`.
    pub(super) struct HleMultiDrawIndexedIndirectCount;

    impl HleMultiDrawIndexedIndirectCount {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_multi_draw_indexed_indirect_count(parameters) };
        }
    }

    /// HLE: DrawIndirectByteCount.
    ///
    /// Port of `HLE_DrawIndirectByteCount`.
    pub(super) struct HleDrawIndirectByteCount;

    impl HleDrawIndirectByteCount {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_draw_indirect_byte_count(parameters) };
        }
    }

    /// HLE: C713C83D8F63CCF3 — const buffer setup.
    ///
    /// Port of `HLE_C713C83D8F63CCF3`.
    pub(super) struct HleC713C83d8f63Ccf3;

    impl HleC713C83d8f63Ccf3 {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_c713c83d8f63ccf3(parameters) };
        }
    }

    /// HLE: D7333D26E0A93EDE — const buffer address setup.
    ///
    /// Port of `HLE_D7333D26E0A93EDE`.
    pub(super) struct HleD7333d26e0a93Ede;

    impl HleD7333d26e0a93Ede {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_d7333d26e0a93ede(parameters) };
        }
    }

    /// HLE: BindShader.
    ///
    /// Port of `HLE_BindShader`.
    pub(super) struct HleBindShader;

    impl HleBindShader {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_bind_shader(parameters) };
        }
    }

    /// HLE: SetRasterBoundingBox.
    ///
    /// Port of `HLE_SetRasterBoundingBox`.
    pub(super) struct HleSetRasterBoundingBox;

    impl HleSetRasterBoundingBox {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_set_raster_bounding_box(parameters) };
        }
    }

    /// HLE: ClearConstBuffer.
    ///
    /// Port of `HLE_ClearConstBuffer<base_size>`.
    pub(super) struct HleClearConstBuffer {
        pub(super) base_size: usize,
    }

    impl HleClearConstBuffer {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe {
                (&mut *maxwell3d).hle_clear_const_buffer(
                    self.base_size,
                    parameters,
                    &CLEAR_CONST_BUFFER_ZEROES,
                )
            };
        }
    }

    /// HLE: ClearMemory.
    ///
    /// Port of `HLE_ClearMemory`.
    pub(super) struct HleClearMemory {
        pub(super) zero_memory: Vec<u32>,
    }

    impl HleClearMemory {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_clear_memory(parameters, &mut self.zero_memory) };
        }
    }

    /// HLE: TransformFeedbackSetup.
    ///
    /// Port of `HLE_TransformFeedbackSetup`.
    pub(super) struct HleTransformFeedbackSetup;

    impl HleTransformFeedbackSetup {
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            unsafe { (&mut *maxwell3d).hle_transform_feedback_setup(parameters) };
        }
    }

    // ── HLE Macro Registry ──────────────────────────────────────────────────────

    /// Look up and instantiate an HLE program by its hash.
    ///
    /// Port of the free `GetHLEProgram` function in `macro.cpp`.
    pub(super) fn get_hle_program(hash: u64) -> Option<AnyCachedMacro> {
        match hash {
            HASH_DRAW_ARRAYS_INDIRECT => {
                Some(AnyCachedMacro::DrawArraysIndirect(HleDrawArraysIndirect {
                    extended: false,
                }))
            }
            HASH_DRAW_ARRAYS_INDIRECT_EXT => {
                Some(AnyCachedMacro::DrawArraysIndirect(HleDrawArraysIndirect {
                    extended: true,
                }))
            }
            HASH_DRAW_INDEXED_INDIRECT => Some(AnyCachedMacro::DrawIndexedIndirect(
                HleDrawIndexedIndirect { extended: false },
            )),
            HASH_DRAW_INDEXED_INDIRECT_EXT => Some(AnyCachedMacro::DrawIndexedIndirect(
                HleDrawIndexedIndirect { extended: true },
            )),
            HASH_MULTI_DRAW_INDEXED_INDIRECT_COUNT => Some(
                AnyCachedMacro::MultiDrawIndexedIndirectCount(HleMultiDrawIndexedIndirectCount),
            ),
            HASH_MULTI_LAYER_CLEAR => Some(AnyCachedMacro::MultiLayerClear(HleMultiLayerClear)),
            HASH_C713C83D8F63CCF3 => Some(AnyCachedMacro::C713C83d8f63Ccf3(HleC713C83d8f63Ccf3)),
            HASH_D7333D26E0A93EDE => Some(AnyCachedMacro::D7333d26e0a93Ede(HleD7333d26e0a93Ede)),
            HASH_BIND_SHADER => Some(AnyCachedMacro::BindShader(HleBindShader)),
            HASH_SET_RASTER_BOUNDING_BOX => Some(AnyCachedMacro::SetRasterBoundingBox(
                HleSetRasterBoundingBox,
            )),
            HASH_CLEAR_CONST_BUFFER_5F00 => {
                Some(AnyCachedMacro::ClearConstBuffer(HleClearConstBuffer {
                    base_size: 0x5F00,
                }))
            }
            HASH_CLEAR_CONST_BUFFER_7000 => {
                Some(AnyCachedMacro::ClearConstBuffer(HleClearConstBuffer {
                    base_size: 0x7000,
                }))
            }
            HASH_CLEAR_MEMORY => Some(AnyCachedMacro::ClearMemory(HleClearMemory {
                zero_memory: Vec::new(),
            })),
            HASH_TRANSFORM_FEEDBACK_SETUP => Some(AnyCachedMacro::TransformFeedbackSetup(
                HleTransformFeedbackSetup,
            )),
            HASH_DRAW_INDIRECT_BYTE_COUNT => Some(AnyCachedMacro::DrawIndirectByteCount(
                HleDrawIndirectByteCount,
            )),
            _ => None,
        }
    }

    impl Maxwell3D {
        pub(crate) fn hle_multi_layer_clear(&mut self, parameters: &mut [u32]) {
            self.refresh_parameters_impl(parameters);
            crate::r#macro::assert_fail_soft(parameters.len() == 1, || {
                format!(
                    "HLE_MultiLayerClear expected one parameter, got {}",
                    parameters.len()
                )
            });
            let clear_surface = parameters[0];
            let rt_index = ((clear_surface >> 6) & 0xF) as usize;
            let layer = (clear_surface >> 10) & 0xFFFF;
            crate::r#macro::assert_fail_soft(layer == 0, || {
                format!("HLE_MultiLayerClear expected layer zero, got {layer}")
            });
            let rt_depth_reg =
                RT_BASE as usize + rt_index * RT_STRIDE as usize + RT_OFF_DEPTH as usize;
            let num_layers = self.regs[rt_depth_reg];
            self.regs[CLEAR_SURFACE as usize] = clear_surface;
            self.with_draw_manager(|draw_manager, this| {
                draw_manager.clear(num_layers, this);
            });
        }

        pub(crate) fn hle_c713c83d8f63ccf3(&mut self, parameters: &mut [u32]) {
            self.refresh_parameters_impl(parameters);
            let offset = (parameters[0] & 0x3FFF_FFFF) << 2;
            let address = self.regs[SHADOW_SCRATCH_BASE as usize + 24];
            self.regs[CB_CONFIG_BASE as usize] = 0x7000;
            self.regs[CB_CONFIG_BASE as usize + 1] = (address >> 24) & 0xFF;
            self.regs[CB_CONFIG_BASE as usize + 2] = address << 8;
            self.regs[CB_CONFIG_BASE as usize + 3] = offset;
        }

        pub(crate) fn hle_set_raster_bounding_box(&mut self, parameters: &mut [u32]) {
            self.refresh_parameters_impl(parameters);
            let raster_mode = parameters[0];
            let raster_enabled = self.regs[CONSERVATIVE_RASTER_ENABLE as usize];
            let scratch_data = self.regs[SHADOW_SCRATCH_BASE as usize + 52];
            let pad = (scratch_data & raster_enabled) & 0xFF;
            self.regs[RASTER_BOUNDING_BOX as usize] = (raster_mode & 0xFFFF_F00F) | (pad << 4);
        }

        pub(crate) fn hle_clear_const_buffer(
            &mut self,
            base_size: usize,
            parameters: &mut [u32],
            zeroes: &[u32; 0x7000],
        ) {
            self.refresh_parameters_impl(parameters);
            self.regs[CB_CONFIG_BASE as usize] = base_size as u32;
            self.regs[CB_CONFIG_BASE as usize + 1] = parameters[0];
            self.regs[CB_CONFIG_BASE as usize + 2] = parameters[1];
            self.regs[CB_CONFIG_BASE as usize + 3] = 0;
            // Upstream passes `parameters[2] * 4` as the u32-count amount to
            // HLE_ClearConstBuffer::Execute (macro.cpp). `parameters[2]` is a vec4
            // (16-byte) entry count; multiply by 4 to get u32 count.
            let u32_count = parameters[2].wrapping_mul(4) as usize;
            self.process_cb_multi_data(&zeroes[..u32_count]);
        }

        pub(crate) fn hle_d7333d26e0a93ede(&mut self, parameters: &mut [u32]) {
            self.refresh_parameters_impl(parameters);
            let index = parameters[0] as usize;
            let address = self.regs[SHADOW_SCRATCH_BASE as usize + 42 + index];
            let size = self.regs[SHADOW_SCRATCH_BASE as usize + 47 + index];
            self.regs[CB_CONFIG_BASE as usize] = size;
            self.regs[CB_CONFIG_BASE as usize + 1] = (address >> 24) & 0xFF;
            self.regs[CB_CONFIG_BASE as usize + 2] = address << 8;
        }

        pub(crate) fn hle_bind_shader(&mut self, parameters: &mut [u32]) {
            self.refresh_parameters_impl(parameters);
            let index = parameters[0] as usize;
            if parameters[1].wrapping_sub(self.regs[SHADOW_SCRATCH_BASE as usize + 28 + index]) == 0
            {
                return;
            }

            let pipeline_base = (PIPELINE_BASE + ((index as u32) & 0xF) * PIPELINE_STRIDE) as usize;
            self.regs[pipeline_base + 1] = parameters[2];
            self.dirty.flags[dirty_flags::flags::SHADERS as usize] = true;
            self.regs[SHADOW_SCRATCH_BASE as usize + 28 + index] = parameters[1];
            self.regs[SHADOW_SCRATCH_BASE as usize + 34 + index] = parameters[2];

            let address = parameters[4];
            self.regs[CB_CONFIG_BASE as usize] = 0x10000;
            self.regs[CB_CONFIG_BASE as usize + 1] = (address >> 24) & 0xFF;
            self.regs[CB_CONFIG_BASE as usize + 2] = address << 8;
            self.regs[CB_CONFIG_BASE as usize + 3] = 0;

            let bind_group_id = (parameters[3] & 0x7F) as usize;
            let bind_base = (CB_BIND_BASE + bind_group_id as u32 * CB_BIND_STRIDE) as usize;
            self.regs[bind_base + 4] = 0x11;
            self.process_cb_bind(bind_group_id);
        }

        pub(crate) fn hle_clear_memory(
            &mut self,
            parameters: &mut [u32],
            zero_memory: &mut Vec<u32>,
        ) {
            self.refresh_parameters_impl(parameters);
            let needed_memory = (parameters[2] / std::mem::size_of::<u32>() as u32) as usize;
            if needed_memory > zero_memory.len() {
                zero_memory.resize(needed_memory, 0);
            }
            self.regs[UPLOAD_REGS_BASE] = parameters[2];
            self.regs[UPLOAD_REGS_BASE + 1] = 1;
            self.regs[UPLOAD_REGS_BASE + 2] = parameters[0];
            self.regs[UPLOAD_REGS_BASE + 3] = parameters[1];
            <Self as EngineInterface>::call_method(self, LAUNCH_DMA, 0x1011, true);
            <Self as EngineInterface>::call_multi_method(
                self,
                INLINE_DATA,
                &zero_memory[..needed_memory],
                needed_memory as u32,
                needed_memory as u32,
            );
        }

        pub(crate) fn hle_transform_feedback_setup(&mut self, parameters: &mut [u32]) {
            self.refresh_parameters_impl(parameters);
            self.regs[TRANSFORM_FEEDBACK_ENABLED as usize] = 1;
            for index in 0..4usize {
                let base = TRANSFORM_FEEDBACK_BUFFERS_BASE as usize
                    + index * TRANSFORM_FEEDBACK_BUFFER_STRIDE as usize;
                self.regs[base + TRANSFORM_FEEDBACK_BUFFER_START_OFFSET as usize] = 0;
            }

            self.regs[UPLOAD_REGS_BASE] = 4;
            self.regs[UPLOAD_REGS_BASE + 1] = 1;
            self.regs[UPLOAD_REGS_BASE + 2] = parameters[0];
            self.regs[UPLOAD_REGS_BASE + 3] = parameters[1];
            <Self as EngineInterface>::call_method(self, LAUNCH_DMA, 0x1011, true);
            let stride = self.regs[TRANSFORM_FEEDBACK_CONTROLS_BASE as usize + 2];
            <Self as EngineInterface>::call_method(self, INLINE_DATA, stride, true);

            let address = ((self.regs[UPLOAD_REGS_BASE + 2] as u64) << 32)
                | self.regs[UPLOAD_REGS_BASE + 3] as u64;
            let _ = self.with_rasterizer_mut(|rasterizer| {
                rasterizer.register_transform_feedback(address);
            });
        }

        pub(crate) fn hle_draw_arrays_indirect(&mut self, extended: bool, parameters: &mut [u32]) {
            let topology = PrimitiveTopology::from_raw(parameters[0]);
            if self.any_parameters_dirty() && topology.is_hle_safe() {
                let indirect_start_address = self.get_macro_address(1);
                let params = self.draw_manager_mut().get_indirect_params_mut();
                params.is_byte_count = false;
                params.is_indexed = false;
                params.include_count = false;
                params.count_start_address = 0;
                params.indirect_start_address = indirect_start_address;
                params.buffer_size = 4 * std::mem::size_of::<u32>();
                params.max_draw_counts = 1;
                params.stride = 0;

                if extended {
                    self.engine_state = EngineHint::OnHleMacro;
                    self.set_hle_replacement_attribute_type(
                        0,
                        0x640,
                        HleReplacementAttributeType::BaseInstance,
                    );
                }
                self.with_draw_manager(|draw_manager, this| {
                    draw_manager.draw_array_indirect(topology, this);
                });
                if extended {
                    self.engine_state = EngineHint::None;
                    self.replace_table.clear();
                }
                return;
            }

            self.refresh_parameters_impl(parameters);
            let topology = PrimitiveTopology::from_raw(parameters[0]);
            let instance_count = self.regs[0xD1B] & parameters[2];
            let vertex_first = parameters[3];
            let vertex_count = parameters[1];
            let base_instance = parameters[4];

            let required_vertices = (vertex_first as usize).wrapping_add(vertex_count as usize);
            if !topology.is_hle_safe() && (self.max_current_vertices() as usize) < required_vertices
            {
                crate::r#macro::assert_fail_soft(false, || {
                    "HLE_DrawArraysIndirect: faulty unsafe-topology draw".to_owned()
                });
                if extended {
                    self.engine_state = EngineHint::None;
                    self.replace_table.clear();
                }
                return;
            }

            if extended {
                self.regs[GLOBAL_BASE_INSTANCE_INDEX as usize] = base_instance;
                self.engine_state = EngineHint::OnHleMacro;
                self.set_hle_replacement_attribute_type(
                    0,
                    0x640,
                    HleReplacementAttributeType::BaseInstance,
                );
            }

            self.with_draw_manager(|draw_manager, this| {
                draw_manager.draw_array(
                    topology,
                    vertex_first,
                    vertex_count,
                    base_instance,
                    instance_count,
                    this,
                );
            });

            if extended {
                self.regs[GLOBAL_BASE_INSTANCE_INDEX as usize] = 0;
                self.engine_state = EngineHint::None;
                self.replace_table.clear();
            }
        }

        pub(crate) fn hle_draw_indexed_indirect(&mut self, extended: bool, parameters: &mut [u32]) {
            let topology = PrimitiveTopology::from_raw(parameters[0]);
            if self.any_parameters_dirty() && topology.is_hle_safe() {
                let estimate = self.estimate_index_buffer_size() as u32;
                let indirect_start_address = self.get_macro_address(1);
                let element_base = parameters[4];
                let base_instance = parameters[5];
                self.regs[VERTEX_ID_BASE as usize] = element_base;
                self.regs[GLOBAL_BASE_VERTEX_INDEX as usize] = element_base;
                self.regs[GLOBAL_BASE_INSTANCE_INDEX as usize] = base_instance;
                self.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize] = true;
                if extended {
                    self.engine_state = EngineHint::OnHleMacro;
                    self.set_hle_replacement_attribute_type(
                        0,
                        0x640,
                        HleReplacementAttributeType::BaseVertex,
                    );
                    self.set_hle_replacement_attribute_type(
                        0,
                        0x644,
                        HleReplacementAttributeType::BaseInstance,
                    );
                }
                let params = self.draw_manager_mut().get_indirect_params_mut();
                params.is_byte_count = false;
                params.is_indexed = true;
                params.include_count = false;
                params.count_start_address = 0;
                params.indirect_start_address = indirect_start_address;
                params.buffer_size = 5 * std::mem::size_of::<u32>();
                params.max_draw_counts = 1;
                params.stride = 0;
                self.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize] = true;
                self.with_draw_manager(|draw_manager, this| {
                    draw_manager.draw_indexed_indirect(topology, 0, estimate, this);
                });
                self.regs[VERTEX_ID_BASE as usize] = 0;
                self.regs[GLOBAL_BASE_VERTEX_INDEX as usize] = 0;
                self.regs[GLOBAL_BASE_INSTANCE_INDEX as usize] = 0;
                if extended {
                    self.engine_state = EngineHint::None;
                    self.replace_table.clear();
                }
                return;
            }

            self.refresh_parameters_impl(parameters);
            let topology = PrimitiveTopology::from_raw(parameters[0]);
            let instance_count = self.regs[0xD1B] & parameters[2];
            let index_first = parameters[3];
            let index_count = parameters[1];
            let element_base = parameters[4];
            let base_instance = parameters[5];

            self.regs[VERTEX_ID_BASE as usize] = element_base;
            self.regs[GLOBAL_BASE_VERTEX_INDEX as usize] = element_base;
            self.regs[GLOBAL_BASE_INSTANCE_INDEX as usize] = base_instance;
            self.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize] = true;

            if extended {
                self.engine_state = EngineHint::OnHleMacro;
                self.set_hle_replacement_attribute_type(
                    0,
                    0x640,
                    HleReplacementAttributeType::BaseVertex,
                );
                self.set_hle_replacement_attribute_type(
                    0,
                    0x644,
                    HleReplacementAttributeType::BaseInstance,
                );
            }

            self.with_draw_manager(|draw_manager, this| {
                draw_manager.draw_index(
                    topology,
                    index_first,
                    index_count,
                    element_base,
                    base_instance,
                    instance_count,
                    this,
                );
            });

            self.regs[VERTEX_ID_BASE as usize] = 0;
            self.regs[GLOBAL_BASE_VERTEX_INDEX as usize] = 0;
            self.regs[GLOBAL_BASE_INSTANCE_INDEX as usize] = 0;
            if extended {
                self.engine_state = EngineHint::None;
                self.replace_table.clear();
            }
        }

        pub(crate) fn hle_multi_draw_indexed_indirect_count(&mut self, parameters: &mut [u32]) {
            let topology = PrimitiveTopology::from_raw(parameters[2]);
            if topology.is_hle_safe() {
                let start_indirect = parameters[0];
                let end_indirect = parameters[1];
                if start_indirect >= end_indirect {
                    return;
                }
                let padding = parameters[3];
                let indirect_words = 5u32.wrapping_add(padding);
                let stride = indirect_words.wrapping_mul(std::mem::size_of::<u32>() as u32);
                let draw_count = (end_indirect - start_indirect) as usize;
                let estimate = self.estimate_index_buffer_size() as u32;
                let count_start_address = self.get_macro_address(4);
                let indirect_start_address = self.get_macro_address(5);
                self.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize] = true;
                let params = self.draw_manager_mut().get_indirect_params_mut();
                params.is_byte_count = false;
                params.is_indexed = true;
                params.include_count = true;
                params.count_start_address = count_start_address;
                params.indirect_start_address = indirect_start_address;
                params.buffer_size = (stride as usize).wrapping_mul(draw_count);
                params.max_draw_counts = draw_count;
                params.stride = stride as usize;
                self.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize] = true;
                self.engine_state = EngineHint::OnHleMacro;
                self.set_hle_replacement_attribute_type(
                    0,
                    0x640,
                    HleReplacementAttributeType::BaseVertex,
                );
                self.set_hle_replacement_attribute_type(
                    0,
                    0x644,
                    HleReplacementAttributeType::BaseInstance,
                );
                self.set_hle_replacement_attribute_type(
                    0,
                    0x648,
                    HleReplacementAttributeType::DrawId,
                );
                self.with_draw_manager(|draw_manager, this| {
                    draw_manager.draw_indexed_indirect(topology, 0, estimate, this);
                });
                self.engine_state = EngineHint::None;
                self.replace_table.clear();
                return;
            }

            self.refresh_parameters_impl(parameters);
            let topology = PrimitiveTopology::from_raw(parameters[2]);
            let start_indirect = parameters[0] as usize;
            let end_indirect = parameters[1] as usize;
            if start_indirect >= end_indirect {
                self.regs[VERTEX_ID_BASE as usize] = 0;
                self.engine_state = EngineHint::None;
                self.replace_table.clear();
                return;
            }
            let indirect_words = 5u32.wrapping_add(parameters[3]) as usize;
            let max_draws = parameters[4] as usize;
            let first_draw = start_indirect;
            let effective_draws = end_indirect - start_indirect;
            let last_draw = start_indirect + effective_draws.min(max_draws);

            for index in first_draw..last_draw {
                let base = index * indirect_words + 5;
                let index_count = parameters[base];
                let instance_count = parameters[base + 1];
                let index_first = parameters[base + 2];
                let base_vertex = parameters[base + 3];
                let base_instance = parameters[base + 4];

                self.regs[VERTEX_ID_BASE as usize] = base_vertex;
                self.engine_state = EngineHint::OnHleMacro;
                self.set_hle_replacement_attribute_type(
                    0,
                    0x640,
                    HleReplacementAttributeType::BaseVertex,
                );
                self.set_hle_replacement_attribute_type(
                    0,
                    0x644,
                    HleReplacementAttributeType::BaseInstance,
                );
                <Self as EngineInterface>::call_method(self, 0x8E3, 0x648, true);
                <Self as EngineInterface>::call_method(self, 0x8E4, index as u32, true);
                self.dirty.flags[dirty_flags::flags::INDEX_BUFFER as usize] = true;

                self.with_draw_manager(|draw_manager, this| {
                    draw_manager.draw_index(
                        topology,
                        index_first,
                        index_count,
                        base_vertex,
                        base_instance,
                        instance_count,
                        this,
                    );
                });
            }

            self.regs[VERTEX_ID_BASE as usize] = 0;
            self.engine_state = EngineHint::None;
            self.replace_table.clear();
        }

        pub(crate) fn hle_draw_indirect_byte_count(&mut self, parameters: &mut [u32]) {
            let force = self
                .with_rasterizer_mut(|rasterizer| rasterizer.has_draw_transform_feedback())
                .unwrap_or(false);
            if force {
                let topology = PrimitiveTopology::from_raw(parameters[0] & 0xFFFF);
                let indirect_start_address = self.get_macro_address(2);
                let params = self.draw_manager_mut().get_indirect_params_mut();
                params.is_byte_count = true;
                params.is_indexed = false;
                params.include_count = false;
                params.count_start_address = 0;
                params.indirect_start_address = indirect_start_address;
                params.buffer_size = std::mem::size_of::<u32>();
                params.max_draw_counts = 1;
                params.stride = parameters[1] as usize;
                self.regs[DRAW_BEGIN as usize] = parameters[0];
                self.regs[DRAW_AUTO_STRIDE as usize] = parameters[1];
                self.regs[DRAW_AUTO_BYTE_COUNT as usize] = parameters[2];
                self.with_draw_manager(|draw_manager, this| {
                    draw_manager.draw_array_indirect(topology, this);
                });
                return;
            }

            self.refresh_parameters_impl(parameters);
            self.regs[DRAW_BEGIN as usize] = parameters[0];
            self.regs[DRAW_AUTO_STRIDE as usize] = parameters[1];
            self.regs[DRAW_AUTO_BYTE_COUNT as usize] = parameters[2];
            let topology = PrimitiveTopology::from_raw(self.regs[DRAW_BEGIN as usize]);
            self.with_draw_manager(|draw_manager, this| {
                draw_manager.draw_array(topology, 0, parameters[2] / parameters[1], 0, 1, this);
            });
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn hle_macro_switch_contains_every_upstream_hash() {
            let hashes = [
                HASH_DRAW_ARRAYS_INDIRECT,
                HASH_DRAW_ARRAYS_INDIRECT_EXT,
                HASH_DRAW_INDEXED_INDIRECT,
                HASH_DRAW_INDEXED_INDIRECT_EXT,
                HASH_MULTI_DRAW_INDEXED_INDIRECT_COUNT,
                HASH_MULTI_LAYER_CLEAR,
                HASH_C713C83D8F63CCF3,
                HASH_D7333D26E0A93EDE,
                HASH_BIND_SHADER,
                HASH_SET_RASTER_BOUNDING_BOX,
                HASH_CLEAR_CONST_BUFFER_5F00,
                HASH_CLEAR_CONST_BUFFER_7000,
                HASH_CLEAR_MEMORY,
                HASH_TRANSFORM_FEEDBACK_SETUP,
                HASH_DRAW_INDIRECT_BYTE_COUNT,
            ];
            for hash in hashes {
                assert!(get_hle_program(hash).is_some(), "missing {hash:#018x}");
            }
        }

        #[test]
        fn hle_macro_unknown_hash_returns_none() {
            assert!(get_hle_program(0xDEADBEEF).is_none());
        }
    }
}
mod macro_interpreter {
    // SPDX-FileCopyrightText: 2025 ruzu contributors
    // SPDX-License-Identifier: GPL-2.0-or-later

    //! Port of `MacroInterpreterImpl` in `video_core/macro.h` and
    //! `video_core/macro.cpp`.
    //!
    //! Software interpreter for Maxwell macro programs. This is the fallback
    //! execution backend when JIT compilation is unavailable or disabled.

    use super::{
        assert_fail_soft, AluOperation, BranchCondition, MethodAddress, Opcode, Operation,
        ResultOperation, NUM_MACRO_REGISTERS,
    };
    use crate::engines::engine_interface::EngineInterface;
    use crate::engines::maxwell_3d::Maxwell3D;
    // ── MacroInterpreterImpl ─────────────────────────────────────────────────────

    /// A software-interpreted macro program.
    ///
    /// Port of `MacroInterpreterImpl` from `video_core/macro.h` and
    /// `video_core/macro.cpp`.
    ///
    /// This struct holds the macro code and executes it instruction by instruction,
    /// maintaining register state, carry flag, and method address register.
    pub struct MacroInterpreterImpl {
        /// The macro code words.
        code: Vec<u32>,

        /// Current program counter (byte offset).
        pc: u32,

        /// Program counter to execute at after the delay slot.
        delayed_pc: Option<u32>,

        /// General purpose macro registers.
        registers: [u32; NUM_MACRO_REGISTERS],

        /// Method address register with auto-increment.
        method_address: MethodAddress,

        /// Input parameters of the current macro.
        parameters: Vec<u32>,

        /// Index of the next parameter that will be fetched by the 'parm' instruction.
        next_parameter_index: usize,

        /// Carry flag from ALU operations.
        carry_flag: bool,
    }

    impl MacroInterpreterImpl {
        /// Create a new interpreter for the given macro code.
        ///
        /// Port of `MacroInterpreterImpl::MacroInterpreterImpl`.
        pub fn new(code: Vec<u32>) -> Self {
            Self {
                code,
                pc: 0,
                delayed_pc: None,
                registers: [0; NUM_MACRO_REGISTERS],
                method_address: MethodAddress::new(0),
                parameters: Vec::new(),
                next_parameter_index: 0,
                carry_flag: false,
            }
        }

        #[cfg(test)]
        pub(crate) fn registers_for_test(&self) -> [u32; NUM_MACRO_REGISTERS] {
            self.registers
        }

        /// Reset the execution engine state, zeroing registers, etc.
        ///
        /// Port of `MacroInterpreterImpl::Reset`.
        fn reset(&mut self) {
            self.registers = [0; NUM_MACRO_REGISTERS];
            self.pc = 0;
            self.delayed_pc = None;
            self.method_address = MethodAddress::new(0);
            // The next parameter index starts at 1, because $r1 already has the
            // value of the first parameter.
            self.next_parameter_index = 1;
            self.carry_flag = false;
        }

        /// Execute a single macro instruction at the current PC.
        /// Returns whether the interpreter should keep running.
        ///
        /// Port of `MacroInterpreterImpl::Step`.
        fn step(&mut self, maxwell3d: *mut Maxwell3D, is_delay_slot: bool) -> bool {
            let base_address = self.pc;
            let opcode = self.get_opcode();
            self.pc += 4;

            // Update the program counter if we were delayed
            if let Some(delayed) = self.delayed_pc.take() {
                assert_fail_soft(is_delay_slot, || {
                    "MacroInterpreter delayed_pc set outside a delay slot".to_owned()
                });
                self.pc = delayed;
            }

            match opcode.operation() {
                Operation::Alu => {
                    let result = self.get_alu_result(
                        opcode.alu_operation(),
                        self.get_register(opcode.src_a()),
                        self.get_register(opcode.src_b()),
                    );
                    self.process_result(maxwell3d, opcode.result_operation(), opcode.dst(), result);
                }
                Operation::AddImmediate => {
                    let result = self
                        .get_register(opcode.src_a())
                        .wrapping_add(opcode.immediate() as u32);
                    self.process_result(maxwell3d, opcode.result_operation(), opcode.dst(), result);
                }
                Operation::ExtractInsert => {
                    let mut dst = self.get_register(opcode.src_a());
                    let src = self.get_register(opcode.src_b());

                    let extracted = (src >> opcode.bf_src_bit()) & opcode.get_bitfield_mask();
                    dst &= !(opcode.get_bitfield_mask() << opcode.bf_dst_bit());
                    dst |= extracted << opcode.bf_dst_bit();
                    self.process_result(maxwell3d, opcode.result_operation(), opcode.dst(), dst);
                }
                Operation::ExtractShiftLeftImmediate => {
                    let dst_val = self.get_register(opcode.src_a());
                    let src = self.get_register(opcode.src_b());

                    let result =
                        ((src >> dst_val) & opcode.get_bitfield_mask()) << opcode.bf_dst_bit();
                    self.process_result(maxwell3d, opcode.result_operation(), opcode.dst(), result);
                }
                Operation::ExtractShiftLeftRegister => {
                    let dst_val = self.get_register(opcode.src_a());
                    let src = self.get_register(opcode.src_b());

                    let result =
                        ((src >> opcode.bf_src_bit()) & opcode.get_bitfield_mask()) << dst_val;
                    self.process_result(maxwell3d, opcode.result_operation(), opcode.dst(), result);
                }
                Operation::Read => {
                    let method = self
                        .get_register(opcode.src_a())
                        .wrapping_add(opcode.immediate() as u32);
                    let result = self.read(maxwell3d, method);
                    self.process_result(maxwell3d, opcode.result_operation(), opcode.dst(), result);
                }
                Operation::Branch => {
                    assert_fail_soft(!is_delay_slot, || {
                        "Executing a macro branch in a delay slot is not valid".to_owned()
                    });
                    let value = self.get_register(opcode.src_a());
                    let taken = self.evaluate_branch_condition(opcode.branch_condition(), value);
                    if taken {
                        // Ignore the delay slot if the branch has the annul bit.
                        if opcode.branch_annul() {
                            self.pc = (base_address as i32 + opcode.get_branch_target()) as u32;
                            return true;
                        }

                        self.delayed_pc =
                            Some((base_address as i32 + opcode.get_branch_target()) as u32);
                        // Execute one more instruction due to the delay slot.
                        return self.step(maxwell3d, true);
                    }
                }
                Operation::Unused => {
                    assert_fail_soft(false, || {
                        format!(
                            "MacroInterpreter: unimplemented operation at PC=0x{base_address:x}"
                        )
                    });
                }
            }

            // An instruction with the Exit flag will not actually
            // cause an exit if it's executed inside a delay slot.
            if opcode.is_exit() && !is_delay_slot {
                // Exit has a delay slot, execute the next instruction
                self.step(maxwell3d, true);
                return false;
            }

            true
        }

        /// Calculate the result of an ALU operation.
        ///
        /// Port of `MacroInterpreterImpl::GetALUResult`.
        fn get_alu_result(&mut self, operation: AluOperation, src_a: u32, src_b: u32) -> u32 {
            match operation {
                AluOperation::Add => {
                    let result = src_a as u64 + src_b as u64;
                    self.carry_flag = result > 0xFFFFFFFF;
                    result as u32
                }
                AluOperation::AddWithCarry => {
                    let carry = if self.carry_flag { 1u64 } else { 0u64 };
                    let result = src_a as u64 + src_b as u64 + carry;
                    self.carry_flag = result > 0xFFFFFFFF;
                    result as u32
                }
                AluOperation::Subtract => {
                    let result = (src_a as u64).wrapping_sub(src_b as u64);
                    self.carry_flag = result < 0x100000000;
                    result as u32
                }
                AluOperation::SubtractWithBorrow => {
                    let borrow = if self.carry_flag { 0u64 } else { 1u64 };
                    let result = (src_a as u64)
                        .wrapping_sub(src_b as u64)
                        .wrapping_sub(borrow);
                    self.carry_flag = result < 0x100000000;
                    result as u32
                }
                AluOperation::Xor => src_a ^ src_b,
                AluOperation::Or => src_a | src_b,
                AluOperation::And => src_a & src_b,
                AluOperation::AndNot => src_a & !src_b,
                AluOperation::Nand => !(src_a & src_b),
                AluOperation::Invalid => {
                    assert_fail_soft(false, || "Unimplemented macro ALU operation".to_owned());
                    0
                }
            }
        }

        /// Perform the result operation on the input result.
        ///
        /// Port of `MacroInterpreterImpl::ProcessResult`.
        fn process_result(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            operation: ResultOperation,
            reg: u32,
            result: u32,
        ) {
            match operation {
                ResultOperation::IgnoreAndFetch => {
                    // Fetch parameter and ignore result.
                    let param = self.fetch_parameter();
                    self.set_register(reg, param);
                }
                ResultOperation::Move => {
                    // Move result.
                    self.set_register(reg, result);
                }
                ResultOperation::MoveAndSetMethod => {
                    // Move result and use as Method Address.
                    self.set_register(reg, result);
                    self.set_method_address(result);
                }
                ResultOperation::FetchAndSend => {
                    // Fetch parameter and send result.
                    let param = self.fetch_parameter();
                    self.set_register(reg, param);
                    self.send(maxwell3d, result);
                }
                ResultOperation::MoveAndSend => {
                    // Move and send result.
                    self.set_register(reg, result);
                    self.send(maxwell3d, result);
                }
                ResultOperation::FetchAndSetMethod => {
                    // Fetch parameter and use result as Method Address.
                    let param = self.fetch_parameter();
                    self.set_register(reg, param);
                    self.set_method_address(result);
                }
                ResultOperation::MoveAndSetMethodFetchAndSend => {
                    // Move result and use as Method Address, then fetch and send parameter.
                    self.set_register(reg, result);
                    self.set_method_address(result);
                    let param = self.fetch_parameter();
                    self.send(maxwell3d, param);
                }
                ResultOperation::MoveAndSetMethodSend => {
                    // Move result and use as Method Address, then send bits 12:17 of result.
                    self.set_register(reg, result);
                    self.set_method_address(result);
                    self.send(maxwell3d, (result >> 12) & 0b111111);
                }
            }
        }

        /// Evaluate branch condition.
        ///
        /// Port of `MacroInterpreterImpl::EvaluateBranchCondition`.
        fn evaluate_branch_condition(&self, cond: BranchCondition, value: u32) -> bool {
            match cond {
                BranchCondition::Zero => value == 0,
                BranchCondition::NotZero => value != 0,
            }
        }

        /// Read an opcode at the current program counter.
        ///
        /// Port of `MacroInterpreterImpl::GetOpcode`.
        fn get_opcode(&self) -> Opcode {
            assert!(self.pc % 4 == 0, "PC not aligned: 0x{:x}", self.pc);
            let index = (self.pc / 4) as usize;
            assert!(
                index < self.code.len(),
                "PC out of bounds: 0x{:x} (code size: {})",
                self.pc,
                self.code.len()
            );
            Opcode::new(self.code[index])
        }

        /// Returns the specified register's value. Register 0 always returns 0.
        ///
        /// Port of `MacroInterpreterImpl::GetRegister`.
        fn get_register(&self, register_id: u32) -> u32 {
            self.registers[register_id as usize]
        }

        /// Set a register value. Register 0 writes are ignored.
        ///
        /// Port of `MacroInterpreterImpl::SetRegister`.
        fn set_register(&mut self, register_id: u32, value: u32) {
            // Register 0 is hardwired as the zero register.
            if register_id == 0 {
                return;
            }
            self.registers[register_id as usize] = value;
        }

        /// Set the method address register.
        ///
        /// Port of `MacroInterpreterImpl::SetMethodAddress`.
        fn set_method_address(&mut self, address: u32) {
            self.method_address = MethodAddress::new(address);
        }

        /// Send a value to the GPU via the current method address.
        ///
        /// Port of `MacroInterpreterImpl::Send`.
        fn send(&mut self, maxwell3d: *mut Maxwell3D, value: u32) {
            let address = self.method_address.address();
            unsafe { (&mut *maxwell3d).call_method(address, value, true) };
            // Increment the method address by the method increment.
            let new_addr = address.wrapping_add(self.method_address.increment());
            self.method_address.set_address(new_addr);
        }

        /// Read a GPU register.
        ///
        /// Port of `MacroInterpreterImpl::Read`.
        fn read(&self, maxwell3d: *mut Maxwell3D, method: u32) -> u32 {
            unsafe { (&*maxwell3d).get_register_value(method) }
        }

        /// Fetch the next parameter.
        ///
        /// Port of `MacroInterpreterImpl::FetchParameter`.
        fn fetch_parameter(&mut self) -> u32 {
            assert!(
                self.next_parameter_index < self.parameters.len(),
                "Macro parameter index out of bounds: {} >= {}",
                self.next_parameter_index,
                self.parameters.len()
            );
            let value = self.parameters[self.next_parameter_index];
            self.next_parameter_index += 1;
            value
        }
    }

    impl MacroInterpreterImpl {
        /// Execute the macro with the given parameters.
        ///
        /// Port of `MacroInterpreterImpl::Execute`.
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            self.reset();

            self.registers[1] = parameters[0];
            self.parameters.resize(parameters.len(), 0);
            self.parameters.copy_from_slice(parameters);
            // Execute the code until we hit an exit condition.
            let mut keep_executing = true;
            while keep_executing {
                keep_executing = self.step(maxwell3d, false);
            }
            // Assert that the macro used all the input parameters
            assert_fail_soft(self.next_parameter_index == self.parameters.len(), || {
                format!(
                    "Macro did not consume all parameters: used {}, total {}",
                    self.next_parameter_index,
                    self.parameters.len()
                )
            });
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn add_immediate(
            dst: u32,
            src_a: u32,
            immediate: i32,
            result: ResultOperation,
            exit: bool,
        ) -> u32 {
            Operation::AddImmediate as u32
                | ((result as u32) << 4)
                | ((exit as u32) << 7)
                | (dst << 8)
                | (src_a << 11)
                | (((immediate as u32) & 0x3ffff) << 14)
        }

        fn branch(src_a: u32, immediate: i32, condition: BranchCondition, annul: bool) -> u32 {
            Operation::Branch as u32
                | ((condition as u32) << 4)
                | ((annul as u32) << 5)
                | (src_a << 11)
                | (((immediate as u32) & 0x3ffff) << 14)
        }

        fn extract_shift(
            operation: Operation,
            dst: u32,
            src_a: u32,
            src_b: u32,
            src_bit: u32,
            size: u32,
            dst_bit: u32,
            exit: bool,
        ) -> u32 {
            operation as u32
                | ((ResultOperation::Move as u32) << 4)
                | ((exit as u32) << 7)
                | (dst << 8)
                | (src_a << 11)
                | (src_b << 14)
                | (src_bit << 17)
                | (size << 22)
                | (dst_bit << 27)
        }

        fn delay_nop() -> u32 {
            add_immediate(0, 0, 0, ResultOperation::Move, false)
        }

        #[test]
        fn interpreter_basic_exit() {
            // Encode an instruction that just exits immediately:
            // operation = AddImmediate (1), result_operation = Move (1),
            // is_exit = 1 (bit 7), dst = 1 (bits 10:8), src_a = 0 (bits 13:11),
            // immediate = 0 (bits 31:14)
            //
            // raw = operation(1) | result_op(1 << 4) | is_exit(1 << 7) | dst(1 << 8)
            let exit_nop = 0b1 | (0b001 << 4) | (1 << 7) | (1 << 8);
            // The exit instruction has a delay slot, so we need a second NOP
            let nop = 0b1 | (0b001 << 4) | (1 << 8); // AddImmediate, Move, dst=1

            let code = vec![exit_nop, nop];
            let mut program = MacroInterpreterImpl::new(code);
            // Execute with 2 parameters (minimum: $r1 = params[0], plus one for fetch)
            // Actually, with Move result operations, no parameters are fetched.
            // We only need 1 parameter since $r1 is set from params[0].
            // But the exit delay slot also runs, so we need the parameter count to match.
            // With Move operations, next_parameter_index stays at 1.
            program.execute(std::ptr::null_mut(), &mut [0x42], 0);
        }

        #[test]
        fn interpreter_execute_retains_parameter_capacity_like_upstream_resize() {
            let exit_nop = 0b1 | (0b001 << 4) | (1 << 7) | (1 << 8);
            let nop = 0b1 | (0b001 << 4) | (1 << 8);
            let mut program = MacroInterpreterImpl::new(vec![exit_nop, nop]);
            program.parameters = Vec::with_capacity(64);

            program.execute(std::ptr::null_mut(), &mut [0x42], 0);

            assert!(program.parameters.capacity() >= 64);
        }

        #[test]
        fn alu_add() {
            let mut interp = MacroInterpreterImpl::new(vec![]);
            let result = interp.get_alu_result(AluOperation::Add, 5, 3);
            assert_eq!(result, 8);
            assert!(!interp.carry_flag);
        }

        #[test]
        fn alu_add_overflow() {
            let mut interp = MacroInterpreterImpl::new(vec![]);
            let result = interp.get_alu_result(AluOperation::Add, 0xFFFFFFFF, 2);
            assert_eq!(result, 1);
            assert!(interp.carry_flag);
        }

        #[test]
        fn alu_subtract() {
            let mut interp = MacroInterpreterImpl::new(vec![]);
            let result = interp.get_alu_result(AluOperation::Subtract, 10, 3);
            assert_eq!(result, 7);
            assert!(interp.carry_flag); // No borrow
        }

        #[test]
        fn alu_subtract_underflow_wraps_and_clears_carry() {
            let mut interp = MacroInterpreterImpl::new(vec![]);
            let result = interp.get_alu_result(AluOperation::Subtract, 3, 5);
            assert_eq!(result, u32::MAX - 1);
            assert!(!interp.carry_flag);
        }

        #[test]
        fn alu_subtract_with_borrow_uses_the_upstream_interpreter_carry_convention() {
            let mut interp = MacroInterpreterImpl::new(vec![]);
            interp.carry_flag = false;
            assert_eq!(
                interp.get_alu_result(AluOperation::SubtractWithBorrow, 10, 3),
                6
            );
            assert!(interp.carry_flag);
        }

        #[test]
        fn alu_bitwise() {
            let mut interp = MacroInterpreterImpl::new(vec![]);
            assert_eq!(
                interp.get_alu_result(AluOperation::Xor, 0xFF00, 0x0FF0),
                0xF0F0
            );
            assert_eq!(
                interp.get_alu_result(AluOperation::Or, 0xFF00, 0x0FF0),
                0xFFF0
            );
            assert_eq!(
                interp.get_alu_result(AluOperation::And, 0xFF00, 0x0FF0),
                0x0F00
            );
            assert_eq!(
                interp.get_alu_result(AluOperation::AndNot, 0xFF00, 0x0FF0),
                0xF000
            );
            assert_eq!(
                interp.get_alu_result(AluOperation::Nand, 0xFF00, 0x0FF0),
                !0x0F00u32
            );
        }

        #[test]
        fn register_zero_is_hardwired() {
            let mut interp = MacroInterpreterImpl::new(vec![]);
            interp.set_register(0, 42);
            assert_eq!(interp.get_register(0), 0);
            interp.set_register(1, 42);
            assert_eq!(interp.get_register(1), 42);
        }

        #[test]
        fn branch_condition() {
            let interp = MacroInterpreterImpl::new(vec![]);
            assert!(interp.evaluate_branch_condition(BranchCondition::Zero, 0));
            assert!(!interp.evaluate_branch_condition(BranchCondition::Zero, 1));
            assert!(!interp.evaluate_branch_condition(BranchCondition::NotZero, 0));
            assert!(interp.evaluate_branch_condition(BranchCondition::NotZero, 1));
        }

        #[test]
        fn interpreter_fetch_and_method_result_operations_match_upstream() {
            let method = 0x200 | (1 << 12);
            let code = vec![
                add_immediate(
                    2,
                    0,
                    method,
                    ResultOperation::MoveAndSetMethodFetchAndSend,
                    false,
                ),
                add_immediate(3, 0, 0x300, ResultOperation::FetchAndSetMethod, false),
                add_immediate(
                    4,
                    0,
                    0x500 | (5 << 12),
                    ResultOperation::MoveAndSetMethodSend,
                    true,
                ),
                delay_nop(),
            ];
            let mut maxwell3d = Maxwell3D::new();
            let mut interpreter = MacroInterpreterImpl::new(code);

            interpreter.execute(std::ptr::from_mut(&mut maxwell3d), &mut [0, 0xaa, 0xbb], 0);

            assert_eq!(interpreter.get_register(2), method as u32);
            assert_eq!(interpreter.get_register(3), 0xbb);
            assert_eq!(interpreter.get_register(4), 0x5500);
            assert_eq!(maxwell3d.get_register_value(0x200), 0xaa);
            assert_eq!(maxwell3d.get_register_value(0x500), 5);
            assert_eq!(interpreter.method_address.address(), 0x505);
        }

        #[test]
        fn interpreter_extract_shift_variants_match_upstream() {
            let code = vec![
                add_immediate(2, 0, 4, ResultOperation::Move, false),
                add_immediate(3, 0, 0xabcd, ResultOperation::Move, false),
                extract_shift(
                    Operation::ExtractShiftLeftImmediate,
                    4,
                    2,
                    3,
                    0,
                    8,
                    4,
                    false,
                ),
                add_immediate(2, 0, 8, ResultOperation::Move, false),
                extract_shift(Operation::ExtractShiftLeftRegister, 5, 2, 3, 4, 8, 0, true),
                delay_nop(),
            ];
            let mut interpreter = MacroInterpreterImpl::new(code);

            interpreter.execute(std::ptr::null_mut(), &mut [0], 0);

            assert_eq!(interpreter.get_register(4), 0xbc0);
            assert_eq!(interpreter.get_register(5), 0xbc00);
        }

        #[test]
        fn interpreter_annulled_loop_repeats_until_zero() {
            let code = vec![
                add_immediate(2, 1, 0, ResultOperation::Move, false),
                add_immediate(2, 2, -1, ResultOperation::Move, false),
                branch(2, -1, BranchCondition::NotZero, true),
                add_immediate(3, 2, 7, ResultOperation::Move, true),
                delay_nop(),
            ];
            let mut interpreter = MacroInterpreterImpl::new(code);

            interpreter.execute(std::ptr::null_mut(), &mut [3], 0);

            assert_eq!(interpreter.get_register(2), 0);
            assert_eq!(interpreter.get_register(3), 7);
        }
    }
}
#[cfg(target_arch = "x86_64")]
mod macro_jit_x64 {
    // SPDX-FileCopyrightText: 2025 ruzu contributors
    // SPDX-License-Identifier: GPL-2.0-or-later

    //! x86-64 native compiler for Maxwell macro programs.
    //!
    //! Port of `MacroJITx64Impl` in current upstream `video_core/macro.cpp`.
    //! Upstream can address `Maxwell3D::regs` by C++ field offset. Rust's
    //! `Maxwell3D` is not a stable-layout type, so JIT state carries the stable
    //! address of its boxed register array. Register reads remain native indexed
    //! loads; no Rust callback is introduced on that path.

    use std::mem::offset_of;

    use rxbyak::{
        byte_ptr, dword_ptr, qword_ptr, CodeAssembler, JmpType, Label, Reg, RegExp, EAX, ECX, R10,
        R10D, R11, R12, R14, R14D, R15, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP,
    };

    #[cfg(target_os = "windows")]
    use rxbyak::R8;

    use super::{
        AluOperation, BranchCondition, Opcode, Operation, ResultOperation, NUM_MACRO_REGISTERS,
    };
    use crate::engines::engine_interface::EngineInterface;
    use crate::engines::maxwell_3d::Maxwell3D;

    /// Upstream `MAX_CODE_SIZE`.
    const MAX_CODE_SIZE: usize = 0x10000;

    const STATE: Reg = RBX;
    const RESULT: Reg = R10D;
    const MAX_PARAMETER: Reg = R11;
    const PARAMETERS: Reg = R12;
    const METHOD_ADDRESS: Reg = R14D;
    const BRANCH_HOLDER: Reg = R15;

    #[cfg(not(target_os = "windows"))]
    const ABI_PARAM1: Reg = RDI;
    #[cfg(not(target_os = "windows"))]
    const ABI_PARAM2: Reg = RSI;
    #[cfg(not(target_os = "windows"))]
    const ABI_PARAM3: Reg = RDX;

    #[cfg(target_os = "windows")]
    const ABI_PARAM1: Reg = rxbyak::RCX;
    #[cfg(target_os = "windows")]
    const ABI_PARAM2: Reg = RDX;
    #[cfg(target_os = "windows")]
    const ABI_PARAM3: Reg = R8;

    #[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
    type ProgramType = unsafe extern "sysv64" fn(*mut JitState, *const u32, *const u32);
    #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
    type ProgramType = unsafe extern "win64" fn(*mut JitState, *const u32, *const u32);

    /// Port of upstream `MacroJITx64Impl::JITState`.
    #[repr(C)]
    struct JitState {
        maxwell3d: *mut Maxwell3D,
        /// Stable pointer to `Maxwell3D::regs`, replacing upstream's C++ member
        /// offset load while preserving the same native indexed read.
        register_array: *const u32,
        registers: [u32; NUM_MACRO_REGISTERS],
        carry_flag: u32,
    }

    const _: () = assert!(offset_of!(JitState, maxwell3d) == 0);

    /// Port of upstream `MacroJITx64Impl::OptimizerState`.
    #[derive(Debug, Clone, Copy, Default)]
    struct OptimizerState {
        can_skip_carry: bool,
        has_delayed_pc: bool,
        zero_reg_skip: bool,
        skip_dummy_addimmediate: bool,
        optimize_for_method_move: bool,
        enable_asserts: bool,
    }

    #[cfg(not(target_os = "windows"))]
    unsafe extern "sysv64" fn macro_jit_send_thunk(
        maxwell3d: *mut Maxwell3D,
        method_address: u32,
        value: u32,
    ) {
        (&mut *maxwell3d).call_method(method_address & 0xfff, value, true);
    }

    #[cfg(target_os = "windows")]
    unsafe extern "win64" fn macro_jit_send_thunk(
        maxwell3d: *mut Maxwell3D,
        method_address: u32,
        value: u32,
    ) {
        (&mut *maxwell3d).call_method(method_address & 0xfff, value, true);
    }

    #[cfg(not(target_os = "windows"))]
    unsafe extern "sysv64" fn macro_jit_error_thunk(parameter: usize, max_parameter: usize) {
        log::error!(
            "Macro JIT: invalid parameter access {parameter:#x} ({:#x} is the last parameter)",
            max_parameter.wrapping_sub(std::mem::size_of::<u32>())
        );
    }

    #[cfg(target_os = "windows")]
    unsafe extern "win64" fn macro_jit_error_thunk(parameter: usize, max_parameter: usize) {
        log::error!(
            "Macro JIT: invalid parameter access {parameter:#x} ({:#x} is the last parameter)",
            max_parameter.wrapping_sub(std::mem::size_of::<u32>())
        );
    }

    /// Port of upstream `MacroJITx64Impl`.
    pub(crate) struct MacroJitX64Impl {
        assembler: CodeAssembler,
        code: Vec<u32>,
        optimizer: OptimizerState,
        next_opcode: Option<Opcode>,
        labels: Vec<Label>,
        delay_skip: Vec<Label>,
        end_of_code: Label,
        is_delay_slot: bool,
        pc: usize,
        program: Option<ProgramType>,
    }

    // Upstream owns compiled macros on the serialized GPU thread. The executable
    // mapping moves with that owner and is not accessed concurrently.
    unsafe impl Send for MacroJitX64Impl {}

    impl MacroJitX64Impl {
        pub(crate) fn new(code: Vec<u32>) -> Self {
            let mut assembler = CodeAssembler::new(MAX_CODE_SIZE)
                .expect("MacroJITx64 must allocate its upstream-sized code buffer");
            let labels = (0..MAX_CODE_SIZE)
                .map(|_| assembler.create_label())
                .collect();
            let delay_skip = (0..MAX_CODE_SIZE)
                .map(|_| assembler.create_label())
                .collect();
            let end_of_code = assembler.create_label();
            let mut jit = Self {
                assembler,
                code,
                optimizer: OptimizerState::default(),
                next_opcode: None,
                labels,
                delay_skip,
                end_of_code,
                is_delay_slot: false,
                pc: 0,
                program: None,
            };
            jit.compile()
                .expect("MacroJITx64 must compile valid uploaded macro code");
            jit.program = Some(unsafe { jit.assembler.get_code::<ProgramType>() });
            jit
        }

        /// Port of `MacroJITx64Impl::Optimizer_ScanFlags`.
        fn optimizer_scan_flags(&mut self) {
            self.optimizer.can_skip_carry = true;
            self.optimizer.has_delayed_pc = false;
            for &raw_op in &self.code {
                let opcode = Opcode::new(raw_op);
                if opcode.operation() == Operation::Alu
                    && matches!(
                        opcode.alu_operation(),
                        AluOperation::AddWithCarry | AluOperation::SubtractWithBorrow
                    )
                {
                    self.optimizer.can_skip_carry = false;
                }
                if opcode.operation() == Operation::Branch && !opcode.branch_annul() {
                    self.optimizer.has_delayed_pc = true;
                }
            }
        }

        fn state_offset(field: usize) -> i32 {
            i32::try_from(field).expect("JIT state offset must fit x86 displacement")
        }

        fn registers_offset(index: u32) -> i32 {
            Self::state_offset(offset_of!(JitState, registers) + index as usize * size_of::<u32>())
        }

        fn emit_prologue(&mut self) -> rxbyak::Result<()> {
            #[cfg(not(target_os = "windows"))]
            let callee_saved = [RBX, RBP, R12, rxbyak::R13, R14, R15];
            #[cfg(target_os = "windows")]
            let callee_saved = [RBX, RBP, RSI, RDI, R12, rxbyak::R13, R14, R15];
            for register in callee_saved {
                self.assembler.push(register)?;
            }
            self.assembler.sub(RSP, 8i32)?;
            self.assembler.mov(STATE, ABI_PARAM1)?;
            self.assembler.mov(PARAMETERS, ABI_PARAM2)?;
            self.assembler.mov(MAX_PARAMETER, ABI_PARAM3)?;
            self.assembler.xor_(RESULT, RESULT)?;
            self.assembler.xor_(METHOD_ADDRESS, METHOD_ADDRESS)?;
            self.assembler.xor_(BRANCH_HOLDER, BRANCH_HOLDER)?;
            let first_parameter = self.compile_fetch_parameter()?;
            self.assembler.mov(
                dword_ptr(RegExp::from(STATE) + Self::registers_offset(1)),
                first_parameter,
            )?;
            Ok(())
        }

        fn emit_epilogue(&mut self) -> rxbyak::Result<()> {
            self.assembler.add(RSP, 8i32)?;
            #[cfg(not(target_os = "windows"))]
            let callee_saved = [RBX, RBP, R12, rxbyak::R13, R14, R15];
            #[cfg(target_os = "windows")]
            let callee_saved = [RBX, RBP, RSI, RDI, R12, rxbyak::R13, R14, R15];
            for register in callee_saved.into_iter().rev() {
                self.assembler.pop(register)?;
            }
            self.assembler.ret()
        }

        fn push_persistent_caller_saved(&mut self) -> rxbyak::Result<()> {
            self.assembler.push(R10)?;
            self.assembler.push(R11)?;
            #[cfg(target_os = "windows")]
            self.assembler.sub(RSP, 32i32)?;
            Ok(())
        }

        fn pop_persistent_caller_saved(&mut self) -> rxbyak::Result<()> {
            #[cfg(target_os = "windows")]
            self.assembler.add(RSP, 32i32)?;
            self.assembler.pop(R11)?;
            self.assembler.pop(R10)
        }

        fn emit_far_call(&mut self, function: usize) -> rxbyak::Result<()> {
            self.assembler.mov(RAX, function as i64)?;
            self.assembler.call_reg(RAX)
        }

        /// Port of `MacroJITx64Impl::Compile`.
        fn compile(&mut self) -> rxbyak::Result<()> {
            self.emit_prologue()?;
            self.optimizer.zero_reg_skip = true;
            self.optimizer.skip_dummy_addimmediate = true;
            self.optimizer.optimize_for_method_move = true;
            self.optimizer.enable_asserts = false;
            self.optimizer_scan_flags();

            for index in 0..self.code.len() {
                self.next_opcode = self.code.get(index + 1).copied().map(Opcode::new);
                self.pc = index;
                self.compile_next_instruction()?;
            }
            self.assembler.bind(&self.end_of_code)?;
            self.emit_epilogue()?;
            self.assembler.ready()
        }

        /// Port of `MacroJITx64Impl::Compile_NextInstruction`.
        fn compile_next_instruction(&mut self) -> rxbyak::Result<bool> {
            let opcode = self.get_opcode();
            self.assembler.bind(&self.labels[self.pc])?;
            match opcode.operation() {
                Operation::Alu => self.compile_alu(opcode)?,
                Operation::AddImmediate => self.compile_add_immediate(opcode)?,
                Operation::ExtractInsert => self.compile_extract_insert(opcode)?,
                Operation::ExtractShiftLeftImmediate => {
                    self.compile_extract_shift_left_immediate(opcode)?
                }
                Operation::ExtractShiftLeftRegister => {
                    self.compile_extract_shift_left_register(opcode)?
                }
                Operation::Read => self.compile_read(opcode)?,
                Operation::Branch => self.compile_branch(opcode)?,
                Operation::Unused => log::warn!("Unimplemented macro opcode Unused"),
            }

            if self.optimizer.has_delayed_pc {
                if opcode.is_exit() {
                    self.assembler.lea_label(RAX, &self.end_of_code)?;
                    self.assembler.test(BRANCH_HOLDER, BRANCH_HOLDER)?;
                    self.assembler.cmove(BRANCH_HOLDER, RAX)?;
                    self.assembler
                        .je(&self.labels[self.pc + 1], JmpType::Near)?;
                } else {
                    let no_delay_slot = self.assembler.create_label();
                    self.assembler.test(BRANCH_HOLDER, BRANCH_HOLDER)?;
                    self.assembler.je(&no_delay_slot, JmpType::Near)?;
                    self.assembler.mov(RAX, BRANCH_HOLDER)?;
                    self.assembler.xor_(BRANCH_HOLDER, BRANCH_HOLDER)?;
                    self.assembler.jmp_reg(RAX)?;
                    self.assembler.bind(&no_delay_slot)?;
                }
                self.assembler.bind(&self.delay_skip[self.pc])?;
                if opcode.is_exit() {
                    return Ok(false);
                }
            } else {
                self.assembler.test(BRANCH_HOLDER, BRANCH_HOLDER)?;
                self.assembler.jne(&self.end_of_code, JmpType::Near)?;
                if opcode.is_exit() {
                    self.assembler.inc(BRANCH_HOLDER)?;
                    return Ok(false);
                }
            }
            Ok(true)
        }

        /// Port of `MacroJITx64Impl::Compile_ALU`.
        fn compile_alu(&mut self, opcode: Opcode) -> rxbyak::Result<()> {
            let is_a_zero = opcode.src_a() == 0;
            let is_b_zero = opcode.src_b() == 0;
            let valid_operation = !is_a_zero && !is_b_zero;
            let has_zero_register = is_a_zero || is_b_zero;
            let no_zero_reg_skip = matches!(
                opcode.alu_operation(),
                AluOperation::AddWithCarry | AluOperation::SubtractWithBorrow
            );
            let mut src_a = RESULT;
            let mut src_b = EAX;
            if !self.optimizer.zero_reg_skip || no_zero_reg_skip {
                src_a = self.compile_get_register(opcode.src_a(), RESULT)?;
                src_b = self.compile_get_register(opcode.src_b(), EAX)?;
            } else {
                if !is_a_zero {
                    src_a = self.compile_get_register(opcode.src_a(), RESULT)?;
                }
                if !is_b_zero {
                    src_b = self.compile_get_register(opcode.src_b(), EAX)?;
                }
            }
            let mut has_emitted = false;
            match opcode.alu_operation() {
                AluOperation::Add => {
                    if !self.optimizer.zero_reg_skip || valid_operation {
                        self.assembler.add(src_a, src_b)?;
                    }
                    if !self.optimizer.can_skip_carry {
                        self.assembler.setc(byte_ptr(
                            RegExp::from(STATE)
                                + Self::state_offset(offset_of!(JitState, carry_flag)),
                        ))?;
                    }
                }
                AluOperation::AddWithCarry => {
                    self.assembler.bt_imm(
                        dword_ptr(
                            RegExp::from(STATE)
                                + Self::state_offset(offset_of!(JitState, carry_flag)),
                        ),
                        0,
                    )?;
                    self.assembler.adc(src_a, src_b)?;
                    self.assembler.setc(byte_ptr(
                        RegExp::from(STATE) + Self::state_offset(offset_of!(JitState, carry_flag)),
                    ))?;
                }
                AluOperation::Subtract => {
                    if !self.optimizer.zero_reg_skip || valid_operation {
                        self.assembler.sub(src_a, src_b)?;
                        has_emitted = true;
                    }
                    if !self.optimizer.can_skip_carry && has_emitted {
                        self.assembler.setc(byte_ptr(
                            RegExp::from(STATE)
                                + Self::state_offset(offset_of!(JitState, carry_flag)),
                        ))?;
                    }
                }
                AluOperation::SubtractWithBorrow => {
                    self.assembler.bt_imm(
                        dword_ptr(
                            RegExp::from(STATE)
                                + Self::state_offset(offset_of!(JitState, carry_flag)),
                        ),
                        0,
                    )?;
                    self.assembler.sbb(src_a, src_b)?;
                    self.assembler.setc(byte_ptr(
                        RegExp::from(STATE) + Self::state_offset(offset_of!(JitState, carry_flag)),
                    ))?;
                }
                AluOperation::Xor => {
                    if !self.optimizer.zero_reg_skip || valid_operation {
                        self.assembler.xor_(src_a, src_b)?;
                    }
                }
                AluOperation::Or => {
                    if !self.optimizer.zero_reg_skip || valid_operation {
                        self.assembler.or_(src_a, src_b)?;
                    }
                }
                AluOperation::And => {
                    if !self.optimizer.zero_reg_skip || !has_zero_register {
                        self.assembler.and_(src_a, src_b)?;
                    }
                }
                AluOperation::AndNot => {
                    if !self.optimizer.zero_reg_skip || !is_a_zero {
                        self.assembler.not_(src_b)?;
                        self.assembler.and_(src_a, src_b)?;
                    }
                }
                AluOperation::Nand => {
                    if !self.optimizer.zero_reg_skip || !is_a_zero {
                        self.assembler.and_(src_a, src_b)?;
                        self.assembler.not_(src_a)?;
                    }
                }
                AluOperation::Invalid => log::warn!("Unimplemented ALU operation"),
            }
            self.compile_process_result(opcode.result_operation(), opcode.dst())
        }

        /// Port of `MacroJITx64Impl::Compile_AddImmediate`.
        fn compile_add_immediate(&mut self, opcode: Opcode) -> rxbyak::Result<()> {
            if self.optimizer.skip_dummy_addimmediate
                && opcode.result_operation() == ResultOperation::Move
                && opcode.dst() == 0
            {
                return Ok(());
            }
            if self.optimizer.optimize_for_method_move
                && opcode.result_operation() == ResultOperation::MoveAndSetMethod
                && self.next_opcode.is_some_and(|next| {
                    next.result_operation() == ResultOperation::MoveAndSetMethod
                        && opcode.dst() == next.dst()
                })
            {
                return Ok(());
            }
            self.compile_register_plus_immediate(opcode)?;
            self.compile_process_result(opcode.result_operation(), opcode.dst())
        }

        fn compile_register_plus_immediate(&mut self, opcode: Opcode) -> rxbyak::Result<()> {
            let immediate = opcode.immediate();
            if self.optimizer.zero_reg_skip && opcode.src_a() == 0 {
                if immediate == 0 {
                    self.assembler.xor_(RESULT, RESULT)?;
                } else {
                    self.assembler.mov(RESULT, immediate)?;
                }
            } else {
                let result = self.compile_get_register(opcode.src_a(), RESULT)?;
                if immediate > 2 {
                    self.assembler.add(result, immediate)?;
                } else if immediate == 1 {
                    self.assembler.inc(result)?;
                } else if immediate < 0 {
                    self.assembler.sub(result, immediate.wrapping_neg())?;
                }
            }
            Ok(())
        }

        /// Port of `MacroJITx64Impl::Compile_ExtractInsert`.
        fn compile_extract_insert(&mut self, opcode: Opcode) -> rxbyak::Result<()> {
            let dst = self.compile_get_register(opcode.src_a(), RESULT)?;
            let src = self.compile_get_register(opcode.src_b(), EAX)?;
            let mask = !(opcode.get_bitfield_mask() << opcode.bf_dst_bit());
            self.assembler.and_(dst, mask as i32)?;
            self.assembler.shr(src, opcode.bf_src_bit() as u8)?;
            self.assembler
                .and_(src, opcode.get_bitfield_mask() as i32)?;
            self.assembler.shl(src, opcode.bf_dst_bit() as u8)?;
            self.assembler.or_(dst, src)?;
            self.compile_process_result(opcode.result_operation(), opcode.dst())
        }

        /// Port of `MacroJITx64Impl::Compile_ExtractShiftLeftImmediate`.
        fn compile_extract_shift_left_immediate(&mut self, opcode: Opcode) -> rxbyak::Result<()> {
            self.compile_get_register(opcode.src_a(), ECX)?;
            let src = self.compile_get_register(opcode.src_b(), RESULT)?;
            self.assembler.shr_cl(src)?;
            self.assembler
                .and_(src, opcode.get_bitfield_mask() as i32)?;
            self.assembler.shl(src, opcode.bf_dst_bit() as u8)?;
            self.compile_process_result(opcode.result_operation(), opcode.dst())
        }

        /// Port of `MacroJITx64Impl::Compile_ExtractShiftLeftRegister`.
        fn compile_extract_shift_left_register(&mut self, opcode: Opcode) -> rxbyak::Result<()> {
            self.compile_get_register(opcode.src_a(), ECX)?;
            let src = self.compile_get_register(opcode.src_b(), RESULT)?;
            self.assembler.shr(src, opcode.bf_src_bit() as u8)?;
            self.assembler
                .and_(src, opcode.get_bitfield_mask() as i32)?;
            self.assembler.shl_cl(src)?;
            self.compile_process_result(opcode.result_operation(), opcode.dst())
        }

        /// Port of `MacroJITx64Impl::Compile_Read`.
        fn compile_read(&mut self, opcode: Opcode) -> rxbyak::Result<()> {
            self.compile_register_plus_immediate(opcode)?;
            if self.optimizer.enable_asserts {
                let pass_range_check = self.assembler.create_label();
                self.assembler
                    .cmp(RESULT, crate::engines::maxwell_3d::NUM_REGS as i32)?;
                self.assembler.jb(&pass_range_check, JmpType::Near)?;
                self.assembler.int3()?;
                self.assembler.bind(&pass_range_check)?;
            }
            self.assembler.mov(
                RAX,
                qword_ptr(
                    RegExp::from(STATE) + Self::state_offset(offset_of!(JitState, register_array)),
                ),
            )?;
            self.assembler.mov(
                RESULT,
                dword_ptr(RegExp::from(RAX) + RESULT.cvt64()? * size_of::<u32>() as u8),
            )?;
            self.compile_process_result(opcode.result_operation(), opcode.dst())
        }

        /// Port of `MacroJITx64Impl::Compile_Send`.
        fn compile_send(&mut self, value: Reg) -> rxbyak::Result<()> {
            self.push_persistent_caller_saved()?;
            self.assembler.mov(
                ABI_PARAM1,
                qword_ptr(
                    RegExp::from(STATE) + Self::state_offset(offset_of!(JitState, maxwell3d)),
                ),
            )?;
            self.assembler.mov(ABI_PARAM2.cvt32()?, METHOD_ADDRESS)?;
            self.assembler.mov(ABI_PARAM3.cvt32()?, value)?;
            self.emit_far_call(macro_jit_send_thunk as usize)?;
            self.pop_persistent_caller_saved()?;

            let dont_process = self.assembler.create_label();
            self.assembler.test(METHOD_ADDRESS, 0x3f000i32)?;
            self.assembler.je(&dont_process, JmpType::Near)?;
            self.assembler.mov(ECX, METHOD_ADDRESS)?;
            self.assembler.and_(METHOD_ADDRESS, 0xfffi32)?;
            self.assembler.shr(ECX, 12)?;
            self.assembler.and_(ECX, 0x3fi32)?;
            self.assembler
                .lea(EAX, qword_ptr(RegExp::from(RCX) + R14))?;
            self.assembler.shl(ECX, 12)?;
            self.assembler.or_(EAX, ECX)?;
            self.assembler.mov(METHOD_ADDRESS, EAX)?;
            self.assembler.bind(&dont_process)
        }

        /// Port of `MacroJITx64Impl::Compile_Branch`.
        fn compile_branch(&mut self, opcode: Opcode) -> rxbyak::Result<()> {
            assert!(!self.is_delay_slot, "branch in a delay slot is invalid");
            let jump_address = (self.pc as i32 + opcode.get_branch_target() / 4) as usize;
            assert!(
                jump_address < self.labels.len(),
                "macro branch target out of range"
            );
            let end = self.assembler.create_label();
            let value = self.compile_get_register(opcode.src_a(), EAX)?;
            self.assembler.cmp(value, 0i32)?;
            if self.optimizer.has_delayed_pc {
                match opcode.branch_condition() {
                    BranchCondition::Zero => self.assembler.jne(&end, JmpType::Near)?,
                    BranchCondition::NotZero => self.assembler.je(&end, JmpType::Near)?,
                }
                if opcode.branch_annul() {
                    self.assembler.xor_(BRANCH_HOLDER, BRANCH_HOLDER)?;
                    self.assembler
                        .jmp(&self.labels[jump_address], JmpType::Near)?;
                } else {
                    let handle_post_exit = self.assembler.create_label();
                    let skip = self.assembler.create_label();
                    self.assembler.jmp(&skip, JmpType::Near)?;
                    self.assembler.bind(&handle_post_exit)?;
                    self.assembler.xor_(BRANCH_HOLDER, BRANCH_HOLDER)?;
                    self.assembler
                        .jmp(&self.labels[jump_address], JmpType::Near)?;
                    self.assembler.bind(&skip)?;
                    self.assembler.lea_label(BRANCH_HOLDER, &handle_post_exit)?;
                    self.assembler
                        .jmp(&self.delay_skip[self.pc], JmpType::Near)?;
                }
            } else {
                match opcode.branch_condition() {
                    BranchCondition::Zero => self
                        .assembler
                        .je(&self.labels[jump_address], JmpType::Near)?,
                    BranchCondition::NotZero => self
                        .assembler
                        .jne(&self.labels[jump_address], JmpType::Near)?,
                }
            }
            self.assembler.bind(&end)
        }

        /// Port of `MacroJITx64Impl::Compile_FetchParameter`.
        fn compile_fetch_parameter(&mut self) -> rxbyak::Result<Reg> {
            let parameter_ok = self.assembler.create_label();
            self.assembler.cmp(PARAMETERS, MAX_PARAMETER)?;
            self.assembler.jb(&parameter_ok, JmpType::Near)?;
            self.push_persistent_caller_saved()?;
            self.assembler.mov(ABI_PARAM1, PARAMETERS)?;
            self.assembler.mov(ABI_PARAM2, MAX_PARAMETER)?;
            self.emit_far_call(macro_jit_error_thunk as usize)?;
            self.pop_persistent_caller_saved()?;
            self.assembler.bind(&parameter_ok)?;
            self.assembler
                .mov(EAX, dword_ptr(RegExp::from(PARAMETERS)))?;
            self.assembler.add(PARAMETERS, size_of::<u32>() as i32)?;
            Ok(EAX)
        }

        /// Port of `MacroJITx64Impl::Compile_GetRegister`.
        fn compile_get_register(&mut self, index: u32, dst: Reg) -> rxbyak::Result<Reg> {
            if index == 0 {
                self.assembler.xor_(dst, dst)?;
            } else {
                self.assembler.mov(
                    dst,
                    dword_ptr(RegExp::from(STATE) + Self::registers_offset(index)),
                )?;
            }
            Ok(dst)
        }

        fn compile_set_register(&mut self, index: u32, result: Reg) -> rxbyak::Result<()> {
            if index != 0 {
                self.assembler.mov(
                    dword_ptr(RegExp::from(STATE) + Self::registers_offset(index)),
                    result,
                )?;
            }
            Ok(())
        }

        /// Port of `MacroJITx64Impl::Compile_ProcessResult`.
        fn compile_process_result(
            &mut self,
            operation: ResultOperation,
            register: u32,
        ) -> rxbyak::Result<()> {
            match operation {
                ResultOperation::IgnoreAndFetch => {
                    let parameter = self.compile_fetch_parameter()?;
                    self.compile_set_register(register, parameter)?;
                }
                ResultOperation::Move => self.compile_set_register(register, RESULT)?,
                ResultOperation::MoveAndSetMethod => {
                    self.compile_set_register(register, RESULT)?;
                    self.assembler.mov(METHOD_ADDRESS, RESULT)?;
                }
                ResultOperation::FetchAndSend => {
                    let parameter = self.compile_fetch_parameter()?;
                    self.compile_set_register(register, parameter)?;
                    self.compile_send(RESULT)?;
                }
                ResultOperation::MoveAndSend => {
                    self.compile_set_register(register, RESULT)?;
                    self.compile_send(RESULT)?;
                }
                ResultOperation::FetchAndSetMethod => {
                    let parameter = self.compile_fetch_parameter()?;
                    self.compile_set_register(register, parameter)?;
                    self.assembler.mov(METHOD_ADDRESS, RESULT)?;
                }
                ResultOperation::MoveAndSetMethodFetchAndSend => {
                    self.compile_set_register(register, RESULT)?;
                    self.assembler.mov(METHOD_ADDRESS, RESULT)?;
                    let parameter = self.compile_fetch_parameter()?;
                    self.compile_send(parameter)?;
                }
                ResultOperation::MoveAndSetMethodSend => {
                    self.compile_set_register(register, RESULT)?;
                    self.assembler.mov(METHOD_ADDRESS, RESULT)?;
                    self.assembler.shr(RESULT, 12)?;
                    self.assembler.and_(RESULT, 0b111111i32)?;
                    self.compile_send(RESULT)?;
                }
            }
            Ok(())
        }

        fn get_opcode(&self) -> Opcode {
            assert!(self.pc < self.code.len());
            Opcode::new(self.code[self.pc])
        }

        fn run(&mut self, maxwell3d: *mut Maxwell3D, parameters: &[u32]) -> JitState {
            let register_array = if maxwell3d.is_null() {
                std::ptr::null()
            } else {
                unsafe { (&*maxwell3d).register_array_ptr() }
            };
            self.run_with_register_array(maxwell3d, parameters, register_array)
        }

        fn run_with_register_array(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &[u32],
            register_array: *const u32,
        ) -> JitState {
            let mut state = JitState {
                maxwell3d,
                register_array,
                registers: [0; NUM_MACRO_REGISTERS],
                carry_flag: 0,
            };
            let end = unsafe { parameters.as_ptr().add(parameters.len()) };
            let program = self
                .program
                .expect("MacroJITx64 program must exist after successful compilation");
            unsafe { program(&mut state, parameters.as_ptr(), end) };
            state
        }
    }

    impl MacroJitX64Impl {
        /// Port of `MacroJITx64Impl::Execute`.
        pub(super) fn execute(
            &mut self,
            maxwell3d: *mut Maxwell3D,
            parameters: &mut [u32],
            _method: u32,
        ) {
            let _ = self.run(maxwell3d, parameters);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::r#macro::macro_interpreter::MacroInterpreterImpl;

        fn add_immediate(
            dst: u32,
            src_a: u32,
            immediate: i32,
            result: ResultOperation,
            exit: bool,
        ) -> u32 {
            Operation::AddImmediate as u32
                | ((result as u32) << 4)
                | ((exit as u32) << 7)
                | (dst << 8)
                | (src_a << 11)
                | (((immediate as u32) & 0x3ffff) << 14)
        }

        fn alu(
            dst: u32,
            src_a: u32,
            src_b: u32,
            operation: AluOperation,
            result: ResultOperation,
            exit: bool,
        ) -> u32 {
            Operation::Alu as u32
                | ((result as u32) << 4)
                | ((exit as u32) << 7)
                | (dst << 8)
                | (src_a << 11)
                | (src_b << 14)
                | ((operation as u32) << 17)
        }

        fn branch(src_a: u32, immediate: i32, condition: BranchCondition, annul: bool) -> u32 {
            Operation::Branch as u32
                | ((condition as u32) << 4)
                | ((annul as u32) << 5)
                | (src_a << 11)
                | (((immediate as u32) & 0x3ffff) << 14)
        }

        fn extract_insert(
            dst: u32,
            src_a: u32,
            src_b: u32,
            src_bit: u32,
            size: u32,
            dst_bit: u32,
            exit: bool,
        ) -> u32 {
            Operation::ExtractInsert as u32
                | ((ResultOperation::Move as u32) << 4)
                | ((exit as u32) << 7)
                | (dst << 8)
                | (src_a << 11)
                | (src_b << 14)
                | (src_bit << 17)
                | (size << 22)
                | (dst_bit << 27)
        }

        fn read(dst: u32, src_a: u32, immediate: i32, exit: bool) -> u32 {
            Operation::Read as u32
                | ((ResultOperation::Move as u32) << 4)
                | ((exit as u32) << 7)
                | (dst << 8)
                | (src_a << 11)
                | (((immediate as u32) & 0x3ffff) << 14)
        }

        fn extract_shift(
            operation: Operation,
            dst: u32,
            src_a: u32,
            src_b: u32,
            src_bit: u32,
            size: u32,
            dst_bit: u32,
            exit: bool,
        ) -> u32 {
            operation as u32
                | ((ResultOperation::Move as u32) << 4)
                | ((exit as u32) << 7)
                | (dst << 8)
                | (src_a << 11)
                | (src_b << 14)
                | (src_bit << 17)
                | (size << 22)
                | (dst_bit << 27)
        }

        fn assert_jit_matches_interpreter(code: Vec<u32>, parameters: &[u32]) -> JitState {
            let debug_code = code.clone();
            let mut interpreter = MacroInterpreterImpl::new(code.clone());
            let mut interpreter_parameters = parameters.to_vec();
            interpreter.execute(std::ptr::null_mut(), &mut interpreter_parameters, 0);

            let mut jit = MacroJitX64Impl::new(code);
            let state = jit.run(std::ptr::null_mut(), parameters);
            assert_eq!(
                state.registers,
                interpreter.registers_for_test(),
                "JIT/interpreter mismatch for code {:#010x?} and parameters {parameters:#010x?}",
                debug_code
            );
            state
        }

        #[test]
        fn jit_state_layout_matches_upstream_prefix() {
            assert_eq!(offset_of!(JitState, maxwell3d), 0);
            assert_eq!(
                offset_of!(JitState, register_array),
                std::mem::size_of::<*mut Maxwell3D>()
            );
            assert_eq!(
                offset_of!(JitState, registers),
                2 * std::mem::size_of::<*mut Maxwell3D>()
            );
            assert_eq!(offset_of!(JitState, carry_flag), 48);
            assert_eq!(size_of::<JitState>(), 56);
        }

        #[test]
        fn optimizer_scan_flags_matches_upstream() {
            let exit = add_immediate(0, 0, 0, ResultOperation::Move, true);
            let delay = add_immediate(0, 0, 0, ResultOperation::Move, false);
            let jit = MacroJitX64Impl::new(vec![exit, delay]);
            assert!(jit.optimizer.can_skip_carry);
            assert!(!jit.optimizer.has_delayed_pc);
        }

        #[test]
        fn native_add_immediate_executes_exit_delay_slot() {
            let exit = add_immediate(2, 1, 5, ResultOperation::Move, true);
            let delay = add_immediate(3, 2, 7, ResultOperation::Move, false);
            let mut jit = MacroJitX64Impl::new(vec![exit, delay]);
            let state = jit.run(std::ptr::null_mut(), &[42]);
            assert_eq!(state.registers[1], 42);
            assert_eq!(state.registers[2], 47);
            assert_eq!(state.registers[3], 54);
        }

        #[test]
        fn native_alu_carry_chain_matches_interpreter() {
            let code = vec![
                add_immediate(2, 0, -1, ResultOperation::Move, false),
                alu(3, 1, 2, AluOperation::Add, ResultOperation::Move, false),
                alu(
                    4,
                    0,
                    0,
                    AluOperation::AddWithCarry,
                    ResultOperation::Move,
                    true,
                ),
                add_immediate(5, 4, 7, ResultOperation::Move, false),
            ];
            let state = assert_jit_matches_interpreter(code, &[1]);
            assert_eq!(state.registers[3], 0);
            assert_eq!(state.registers[4], 1);
            assert_eq!(state.registers[5], 8);
        }

        #[test]
        fn native_non_borrow_alu_operations_match_interpreter_across_edge_values() {
            let operations = [
                AluOperation::Add,
                AluOperation::AddWithCarry,
                AluOperation::Subtract,
                AluOperation::Xor,
                AluOperation::Or,
                AluOperation::And,
                AluOperation::AndNot,
                AluOperation::Nand,
            ];
            let values = [0, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX];
            for operation in operations {
                for lhs in values {
                    for rhs in values {
                        let code = vec![
                            add_immediate(2, 0, 0, ResultOperation::IgnoreAndFetch, false),
                            alu(3, 1, 2, operation, ResultOperation::Move, true),
                            add_immediate(0, 0, 0, ResultOperation::Move, false),
                        ];
                        assert_jit_matches_interpreter(code, &[lhs, rhs]);
                    }
                }
            }
        }

        #[test]
        fn native_extract_shift_variants_match_interpreter() {
            let code = vec![
                add_immediate(2, 0, 4, ResultOperation::Move, false),
                add_immediate(3, 0, 0xabcd, ResultOperation::Move, false),
                extract_shift(
                    Operation::ExtractShiftLeftImmediate,
                    4,
                    2,
                    3,
                    0,
                    8,
                    4,
                    false,
                ),
                add_immediate(2, 0, 8, ResultOperation::Move, false),
                extract_shift(Operation::ExtractShiftLeftRegister, 5, 2, 3, 4, 8, 0, true),
                add_immediate(0, 0, 0, ResultOperation::Move, false),
            ];
            let state = assert_jit_matches_interpreter(code, &[0]);
            assert_eq!(state.registers[4], 0xbc0);
            assert_eq!(state.registers[5], 0xbc00);
        }

        #[test]
        fn native_taken_branch_executes_delay_slot() {
            let code = vec![
                add_immediate(2, 0, 0, ResultOperation::Move, false),
                branch(2, 3, BranchCondition::Zero, false),
                add_immediate(3, 0, 10, ResultOperation::Move, false),
                add_immediate(3, 0, 99, ResultOperation::Move, false),
                add_immediate(4, 3, 1, ResultOperation::Move, true),
                add_immediate(5, 4, 1, ResultOperation::Move, false),
            ];
            let state = assert_jit_matches_interpreter(code, &[0]);
            assert_eq!(state.registers[3], 10);
            assert_eq!(state.registers[4], 11);
            assert_eq!(state.registers[5], 12);
        }

        #[test]
        fn native_annulled_branch_skips_delay_slot() {
            let code = vec![
                add_immediate(2, 0, 0, ResultOperation::Move, false),
                branch(2, 2, BranchCondition::Zero, true),
                add_immediate(3, 0, 55, ResultOperation::Move, false),
                add_immediate(4, 0, 7, ResultOperation::Move, true),
                add_immediate(5, 4, 1, ResultOperation::Move, false),
            ];
            let state = assert_jit_matches_interpreter(code, &[0]);
            assert_eq!(state.registers[3], 0);
            assert_eq!(state.registers[4], 7);
            assert_eq!(state.registers[5], 8);
        }

        #[test]
        fn native_extract_insert_matches_interpreter() {
            let code = vec![
                add_immediate(2, 0, 0x1234, ResultOperation::Move, false),
                extract_insert(3, 1, 2, 8, 8, 16, true),
                add_immediate(4, 3, 0, ResultOperation::Move, false),
            ];
            let state = assert_jit_matches_interpreter(code, &[0xaaaa_bbbb]);
            assert_eq!(state.registers[3], 0xaa12_bbbb);
            assert_eq!(state.registers[4], 0xaa12_bbbb);
        }

        #[test]
        fn native_read_uses_direct_register_array_load() {
            let code = vec![
                read(2, 0, 7, true),
                add_immediate(3, 2, 1, ResultOperation::Move, false),
            ];
            let mut registers = [0u32; crate::engines::maxwell_3d::NUM_REGS];
            registers[7] = 0x1234_5678;
            let mut jit = MacroJitX64Impl::new(code);
            let state = jit.run_with_register_array(std::ptr::null_mut(), &[0], registers.as_ptr());
            assert_eq!(state.registers[2], 0x1234_5678);
            assert_eq!(state.registers[3], 0x1234_5679);
        }

        #[test]
        fn native_send_uses_maxwell_method_and_increment() {
            let code = vec![
                add_immediate(2, 0, 0x1100, ResultOperation::MoveAndSetMethod, false),
                add_immediate(3, 0, 0x55, ResultOperation::MoveAndSend, false),
                add_immediate(4, 0, 0x66, ResultOperation::MoveAndSend, true),
                add_immediate(0, 0, 0, ResultOperation::Move, false),
            ];
            let mut maxwell3d = Maxwell3D::new();
            let maxwell3d_ptr = std::ptr::from_mut(&mut maxwell3d);
            let mut jit = MacroJitX64Impl::new(code);
            jit.run(maxwell3d_ptr, &[0]);
            assert_eq!(maxwell3d.get_register_value(0x100), 0x55);
            assert_eq!(maxwell3d.get_register_value(0x101), 0x66);
        }
    }
}

use self::macro_hle::{
    get_hle_program, HleBindShader, HleC713C83d8f63Ccf3, HleClearConstBuffer, HleClearMemory,
    HleD7333d26e0a93Ede, HleDrawArraysIndirect, HleDrawIndexedIndirect, HleDrawIndirectByteCount,
    HleMultiDrawIndexedIndirectCount, HleMultiLayerClear, HleSetRasterBoundingBox,
    HleTransformFeedbackSetup,
};
use self::macro_interpreter::MacroInterpreterImpl;
#[cfg(target_arch = "x86_64")]
use self::macro_jit_x64::MacroJitX64Impl;
use crate::engines::maxwell_3d::Maxwell3D;
use common::container_hash::hash_u32_slice;
use common::hash::BuildUnorderedDenseHasher;

// ── Constants ────────────────────────────────────────────────────────────────

/// Number of general-purpose macro registers.
///
/// Port of `Tegra::Macro::NUM_MACRO_REGISTERS`.
pub const NUM_MACRO_REGISTERS: usize = 8;

pub(crate) fn assert_fail_soft(condition: bool, message: impl FnOnce() -> String) {
    if condition {
        return;
    }
    let message = message();
    log::error!("{message}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("{message}");
    }
}

// ── Instruction field enums ──────────────────────────────────────────────────

/// Primary operation encoded in bits [2:0] of the 32-bit opcode.
///
/// Port of `Tegra::Macro::Operation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Operation {
    Alu = 0,
    AddImmediate = 1,
    ExtractInsert = 2,
    ExtractShiftLeftImmediate = 3,
    ExtractShiftLeftRegister = 4,
    Read = 5,
    Unused = 6,
    Branch = 7,
}

impl Operation {
    pub fn from_raw(v: u32) -> Self {
        match v & 0x7 {
            0 => Self::Alu,
            1 => Self::AddImmediate,
            2 => Self::ExtractInsert,
            3 => Self::ExtractShiftLeftImmediate,
            4 => Self::ExtractShiftLeftRegister,
            5 => Self::Read,
            6 => Self::Unused,
            7 => Self::Branch,
            _ => unreachable!(),
        }
    }
}

/// ALU sub-operation encoded in bits [21:17].
///
/// Port of `Tegra::Macro::ALUOperation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AluOperation {
    Add = 0,
    AddWithCarry = 1,
    Subtract = 2,
    SubtractWithBorrow = 3,
    Xor = 8,
    Or = 9,
    And = 10,
    AndNot = 11,
    Nand = 12,
    Invalid = u32::MAX,
}

impl AluOperation {
    pub fn from_raw(v: u32) -> Self {
        match v {
            0 => Self::Add,
            1 => Self::AddWithCarry,
            2 => Self::Subtract,
            3 => Self::SubtractWithBorrow,
            8 => Self::Xor,
            9 => Self::Or,
            10 => Self::And,
            11 => Self::AndNot,
            12 => Self::Nand,
            _ => Self::Invalid,
        }
    }
}

/// Result operation encoded in bits [6:4].
///
/// Port of `Tegra::Macro::ResultOperation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ResultOperation {
    IgnoreAndFetch = 0,
    Move = 1,
    MoveAndSetMethod = 2,
    FetchAndSend = 3,
    MoveAndSend = 4,
    FetchAndSetMethod = 5,
    MoveAndSetMethodFetchAndSend = 6,
    MoveAndSetMethodSend = 7,
}

impl ResultOperation {
    pub fn from_raw(v: u32) -> Self {
        match v & 0x7 {
            0 => Self::IgnoreAndFetch,
            1 => Self::Move,
            2 => Self::MoveAndSetMethod,
            3 => Self::FetchAndSend,
            4 => Self::MoveAndSend,
            5 => Self::FetchAndSetMethod,
            6 => Self::MoveAndSetMethodFetchAndSend,
            7 => Self::MoveAndSetMethodSend,
            _ => unreachable!(),
        }
    }
}

/// Branch condition encoded in bit [4].
///
/// Port of `Tegra::Macro::BranchCondition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BranchCondition {
    Zero = 0,
    NotZero = 1,
}

impl BranchCondition {
    pub fn from_raw(v: u32) -> Self {
        match v & 1 {
            0 => Self::Zero,
            1 => Self::NotZero,
            _ => unreachable!(),
        }
    }
}

// ── Opcode ───────────────────────────────────────────────────────────────────

/// Decoded macro instruction opcode.
///
/// Port of `Tegra::Macro::Opcode` union.
#[derive(Debug, Clone, Copy)]
pub struct Opcode {
    pub raw: u32,
}

impl Opcode {
    pub const fn new(raw: u32) -> Self {
        Self { raw }
    }

    /// bits [2:0]
    pub fn operation(&self) -> Operation {
        Operation::from_raw(self.raw & 0x7)
    }

    /// bits [6:4]
    pub fn result_operation(&self) -> ResultOperation {
        ResultOperation::from_raw((self.raw >> 4) & 0x7)
    }

    /// bit [4]
    pub fn branch_condition(&self) -> BranchCondition {
        BranchCondition::from_raw((self.raw >> 4) & 0x1)
    }

    /// bit [5] — If set on a branch, then the branch doesn't have a delay slot.
    pub fn branch_annul(&self) -> bool {
        (self.raw >> 5) & 1 != 0
    }

    /// bit [7]
    pub fn is_exit(&self) -> bool {
        (self.raw >> 7) & 1 != 0
    }

    /// bits [10:8]
    pub fn dst(&self) -> u32 {
        (self.raw >> 8) & 0x7
    }

    /// bits [13:11]
    pub fn src_a(&self) -> u32 {
        (self.raw >> 11) & 0x7
    }

    /// bits [16:14]
    pub fn src_b(&self) -> u32 {
        (self.raw >> 14) & 0x7
    }

    /// bits [31:14] — signed 18-bit immediate, overlaps src_b and alu_operation.
    pub fn immediate(&self) -> i32 {
        let raw_imm = (self.raw >> 14) as i32;
        // Sign extend from 18 bits
        (raw_imm << 14) >> 14
    }

    /// bits [21:17]
    pub fn alu_operation(&self) -> AluOperation {
        AluOperation::from_raw((self.raw >> 17) & 0x1F)
    }

    /// bits [21:17] — bitfield source bit position
    pub fn bf_src_bit(&self) -> u32 {
        (self.raw >> 17) & 0x1F
    }

    /// bits [26:22] — bitfield size
    pub fn bf_size(&self) -> u32 {
        (self.raw >> 22) & 0x1F
    }

    /// bits [31:27] — bitfield destination bit position
    pub fn bf_dst_bit(&self) -> u32 {
        (self.raw >> 27) & 0x1F
    }

    /// Returns the bitfield mask: `(1 << bf_size) - 1`.
    pub fn get_bitfield_mask(&self) -> u32 {
        (1u32 << self.bf_size()).wrapping_sub(1)
    }

    /// Returns the branch target offset in bytes.
    pub fn get_branch_target(&self) -> i32 {
        self.immediate() * 4 // sizeof(u32)
    }
}

// ── Method Address ───────────────────────────────────────────────────────────

/// Method address register with auto-increment.
///
/// Port of `Tegra::Macro::MethodAddress` union.
#[derive(Debug, Clone, Copy)]
pub struct MethodAddress {
    pub raw: u32,
}

impl MethodAddress {
    pub const fn new(raw: u32) -> Self {
        Self { raw }
    }

    /// bits [11:0]
    pub fn address(&self) -> u32 {
        self.raw & 0xFFF
    }

    /// bits [17:12]
    pub fn increment(&self) -> u32 {
        (self.raw >> 12) & 0x3F
    }

    /// Set the address field.
    pub fn set_address(&mut self, addr: u32) {
        self.raw = (self.raw & !0xFFF) | (addr & 0xFFF);
    }
}

/// Rust counterpart of upstream `AnyCachedMacro` (`std::variant`).
enum AnyCachedMacro {
    DrawArraysIndirect(HleDrawArraysIndirect),
    DrawIndexedIndirect(HleDrawIndexedIndirect),
    MultiDrawIndexedIndirectCount(HleMultiDrawIndexedIndirectCount),
    MultiLayerClear(HleMultiLayerClear),
    C713C83d8f63Ccf3(HleC713C83d8f63Ccf3),
    D7333d26e0a93Ede(HleD7333d26e0a93Ede),
    BindShader(HleBindShader),
    SetRasterBoundingBox(HleSetRasterBoundingBox),
    ClearConstBuffer(HleClearConstBuffer),
    ClearMemory(HleClearMemory),
    TransformFeedbackSetup(HleTransformFeedbackSetup),
    DrawIndirectByteCount(HleDrawIndirectByteCount),
    Interpreter(MacroInterpreterImpl),
    #[cfg(target_arch = "x86_64")]
    Dynamic(Box<MacroJitX64Impl>),
}

impl AnyCachedMacro {
    fn execute(&mut self, maxwell3d: *mut Maxwell3D, parameters: &mut [u32], method: u32) {
        match self {
            Self::DrawArraysIndirect(program) => program.execute(maxwell3d, parameters, method),
            Self::DrawIndexedIndirect(program) => program.execute(maxwell3d, parameters, method),
            Self::MultiDrawIndexedIndirectCount(program) => {
                program.execute(maxwell3d, parameters, method)
            }
            Self::MultiLayerClear(program) => program.execute(maxwell3d, parameters, method),
            Self::C713C83d8f63Ccf3(program) => program.execute(maxwell3d, parameters, method),
            Self::D7333d26e0a93Ede(program) => program.execute(maxwell3d, parameters, method),
            Self::BindShader(program) => program.execute(maxwell3d, parameters, method),
            Self::SetRasterBoundingBox(program) => program.execute(maxwell3d, parameters, method),
            Self::ClearConstBuffer(program) => program.execute(maxwell3d, parameters, method),
            Self::ClearMemory(program) => program.execute(maxwell3d, parameters, method),
            Self::TransformFeedbackSetup(program) => program.execute(maxwell3d, parameters, method),
            Self::DrawIndirectByteCount(program) => program.execute(maxwell3d, parameters, method),
            Self::Interpreter(program) => program.execute(maxwell3d, parameters, method),
            #[cfg(target_arch = "x86_64")]
            Self::Dynamic(program) => program.execute(maxwell3d, parameters, method),
        }
    }

    fn needs_parameter_refresh(&self) -> bool {
        let is_lle = match self {
            Self::Interpreter(_) => true,
            #[cfg(target_arch = "x86_64")]
            Self::Dynamic(_) => true,
            _ => false,
        };
        is_lle || *common::settings::values().disable_macro_hle.get_value()
    }
}

// ── MacroEngine ──────────────────────────────────────────────────────────────

/// Cache info for a single macro method.
///
/// Port of `MacroEngine::CacheInfo`.
struct CacheInfo {
    program: AnyCachedMacro,
    hash: u64,
}

/// Base macro execution engine that manages code upload, caching, and dispatch.
///
/// Port of `Tegra::MacroEngine`.
pub struct MacroEngine {
    macro_cache: HashMap<u32, CacheInfo, BuildUnorderedDenseHasher>,
    uploaded_macro_code: HashMap<u32, Vec<u32>, BuildUnorderedDenseHasher>,
    is_interpreted: bool,
}

impl MacroEngine {
    /// Create a new macro engine.
    ///
    /// Port of `MacroEngine::MacroEngine(bool is_interpreted)`.
    pub fn new(is_interpreted: bool) -> Self {
        Self {
            macro_cache: HashMap::with_hasher(BuildUnorderedDenseHasher),
            uploaded_macro_code: HashMap::with_hasher(BuildUnorderedDenseHasher),
            is_interpreted,
        }
    }

    /// Port of upstream `MacroEngine::Compile`.
    fn compile_backend(is_interpreted: bool, code: &[u32]) -> AnyCachedMacro {
        #[cfg(target_arch = "x86_64")]
        if !is_interpreted {
            return AnyCachedMacro::Dynamic(Box::new(MacroJitX64Impl::new(code.to_vec())));
        }

        AnyCachedMacro::Interpreter(MacroInterpreterImpl::new(code.to_vec()))
    }

    /// Store uploaded macro code word.
    ///
    /// Port of `MacroEngine::AddCode`.
    pub fn add_code(&mut self, method: u32, data: u32) {
        self.uploaded_macro_code
            .entry(method)
            .or_default()
            .push(data);
    }

    /// Clear the code associated with a method.
    ///
    /// Port of `MacroEngine::ClearCode`.
    pub fn clear_code(&mut self, method: u32) {
        self.macro_cache.remove(&method);
        self.uploaded_macro_code.remove(&method);
    }

    /// Compile (if not cached) and execute a macro.
    ///
    /// Port of `MacroEngine::Execute`.
    ///
    pub fn execute<R>(
        &mut self,
        maxwell3d: *mut Maxwell3D,
        method: u32,
        parameters: &mut [u32],
        refresh_parameters: R,
    ) where
        R: FnMut(&mut [u32]),
    {
        let is_interpreted = self.is_interpreted;
        self.execute_with_compiler(
            maxwell3d,
            method,
            parameters,
            refresh_parameters,
            move |code| Self::compile_backend(is_interpreted, code),
        );
    }

    fn execute_with_compiler<F, R>(
        &mut self,
        maxwell3d: *mut Maxwell3D,
        method: u32,
        parameters: &mut [u32],
        mut refresh_parameters: R,
        compile_fn: F,
    ) where
        F: FnOnce(&[u32]) -> AnyCachedMacro,
        R: FnMut(&mut [u32]),
    {
        if let Some(cache_info) = self.macro_cache.get_mut(&method) {
            if cache_info.program.needs_parameter_refresh() {
                refresh_parameters(parameters);
            }
            cache_info.program.execute(maxwell3d, parameters, method);
            return;
        }

        let mid_method = if !self.uploaded_macro_code.contains_key(&method) {
            self.uploaded_macro_code
                .iter()
                .find_map(|(&method_base, code)| {
                    (method >= method_base && (method - method_base) < code.len() as u32)
                        .then_some(method_base)
                })
        } else {
            None
        };
        if !self.uploaded_macro_code.contains_key(&method) && mid_method.is_none() {
            assert_fail_soft(false, || format!("Macro 0x{method:x} was not uploaded"));
            return;
        }

        let code_for_compile = if let Some(method_base) = mid_method {
            let macro_cached = self
                .uploaded_macro_code
                .get(&method_base)
                .expect("mid_method base must exist");
            let rebased_method = (method - method_base) as usize;
            let code = macro_cached[rebased_method..].to_vec();
            self.uploaded_macro_code.insert(method, code.clone());
            code
        } else {
            self.uploaded_macro_code
                .get(&method)
                .expect("method existence checked above")
                .clone()
        };
        let hash = hash_macro_code(&code_for_compile);
        let disable_macro_hle = *common::settings::values().disable_macro_hle.get_value();
        let program = (!disable_macro_hle)
            .then(|| get_hle_program(hash))
            .flatten()
            .unwrap_or_else(|| compile_fn(&code_for_compile));

        let cache_info = CacheInfo { program, hash };

        self.macro_cache.insert(method, cache_info);

        // Execute the newly compiled macro
        let entry = self.macro_cache.get_mut(&method).unwrap();
        if entry.program.needs_parameter_refresh() {
            refresh_parameters(parameters);
        }
        entry.program.execute(maxwell3d, parameters, method);
        if *common::settings::values().dump_macros.get_value() {
            dump(entry.hash, &code_for_compile, true);
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn hash_macro_code(code: &[u32]) -> u64 {
    hash_u32_slice(code) as u64
}

/// Dump macro code to filesystem (debug utility).
///
/// Port of the anonymous `Dump` function from `macro.cpp`.
fn dump(hash: u64, code: &[u32], decompiled: bool) {
    let dump_dir = common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::DumpDir);
    dump_to_directory(
        &dump_dir,
        common::settings::get_current_program_id(),
        hash,
        code,
        decompiled,
    );
}

fn dump_to_directory(dump_dir: &Path, program_id: u64, hash: u64, code: &[u32], decompiled: bool) {
    if !common::fs::fs::create_dir(dump_dir) {
        log::error!("Failed to create dump directory");
        return;
    }
    let variant_suffix = if decompiled { "jit" } else { "raw" };
    let path = dump_dir.join(format!(
        "{program_id:016x}_{hash:016x}_{variant_suffix}.macro"
    ));
    let mut macro_file = match std::fs::File::create(&path) {
        Ok(file) => file,
        Err(error) => {
            log::error!(
                "Unable to open or create file at {}: {}",
                common::fs::fs_util::path_to_utf8_string(&path),
                error
            );
            return;
        }
    };
    if let Err(error) = macro_file.write_all(bytemuck::cast_slice(code)) {
        log::error!(
            "Unable to write macro file at {}: {}",
            common::fs::fs_util::path_to_utf8_string(&path),
            error
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_field_extraction() {
        // Test a known opcode encoding
        let op = Opcode::new(0);
        assert_eq!(op.operation(), Operation::Alu);
        assert_eq!(op.dst(), 0);
        assert_eq!(op.src_a(), 0);
        assert_eq!(op.src_b(), 0);
        assert!(!op.is_exit());
    }

    #[test]
    fn opcode_branch_target() {
        // immediate = 1 (in bits [31:14]), branch target = 1 * 4 = 4
        let op = Opcode::new(1 << 14); // immediate = 1
        assert_eq!(op.get_branch_target(), 4);
    }

    #[test]
    fn method_address_fields() {
        let ma = MethodAddress::new(0x3F_FFF);
        assert_eq!(ma.address(), 0xFFF);
        assert_eq!(ma.increment(), 0x3F);
    }

    #[test]
    fn macro_engine_add_code() {
        let mut engine = MacroEngine::new(true);
        engine.add_code(0x100, 0xDEADBEEF);
        engine.add_code(0x100, 0xCAFEBABE);
        assert_eq!(
            engine.uploaded_macro_code.get(&0x100),
            Some(&vec![0xDEADBEEF, 0xCAFEBABE])
        );
    }

    #[test]
    fn macro_engine_clear_code() {
        let mut engine = MacroEngine::new(true);
        engine.add_code(0x100, 0xDEADBEEF);
        engine.add_code(0x100, 0xCAFEBABE);
        engine.clear_code(0x100);
        assert!(!engine.uploaded_macro_code.contains_key(&0x100));
    }

    #[test]
    fn execute_refreshes_the_parameter_slice_consumed_by_lle() {
        let mut engine = MacroEngine::new(true);
        engine.add_code(
            0x100,
            Operation::AddImmediate as u32
                | ((ResultOperation::IgnoreAndFetch as u32) << 4)
                | (1 << 7)
                | (2 << 8),
        );
        engine.add_code(
            0x100,
            Operation::AddImmediate as u32 | ((ResultOperation::Move as u32) << 4),
        );
        let mut parameters = [0, 0];
        engine.execute_with_compiler(
            std::ptr::null_mut(),
            0x100,
            &mut parameters,
            |parameters| parameters[1] = 0xCAFE_BABE,
            |code| AnyCachedMacro::Interpreter(MacroInterpreterImpl::new(code.to_vec())),
        );

        assert_eq!(parameters, [0, 0xCAFE_BABE]);
        let AnyCachedMacro::Interpreter(program) = &engine.macro_cache[&0x100].program else {
            panic!("the test compiler must install an interpreter");
        };
        assert_eq!(program.registers_for_test()[2], 0xCAFE_BABE);
    }

    #[test]
    fn cached_macro_executes_against_the_current_maxwell_instance() {
        let code = vec![
            Operation::AddImmediate as u32
                | ((ResultOperation::MoveAndSetMethod as u32) << 4)
                | (2 << 8)
                | ((0x1100u32 & 0x3ffff) << 14),
            Operation::AddImmediate as u32
                | ((ResultOperation::MoveAndSend as u32) << 4)
                | (1 << 7)
                | (3 << 8)
                | ((0x55u32 & 0x3ffff) << 14),
            Operation::AddImmediate as u32 | ((ResultOperation::Move as u32) << 4),
        ];
        let mut engine = MacroEngine::new(true);
        for word in code {
            engine.add_code(0x100, word);
        }
        let mut first = Maxwell3D::new();
        let mut second = Maxwell3D::new();

        engine.execute_with_compiler(
            std::ptr::from_mut(&mut first),
            0x100,
            &mut [0],
            |_| {},
            |code| AnyCachedMacro::Interpreter(MacroInterpreterImpl::new(code.to_vec())),
        );
        engine.execute_with_compiler(
            std::ptr::from_mut(&mut second),
            0x100,
            &mut [0],
            |_| {},
            |_| panic!("the cached macro must not be compiled twice"),
        );

        assert_eq!(first.get_register_value(0x100), 0x55);
        assert_eq!(second.get_register_value(0x100), 0x55);
    }

    #[test]
    fn macro_engine_execute_rebases_mid_method_inside_uploaded_blob() {
        let mut engine = MacroEngine::new(true);
        engine.add_code(0x100, 0x11111111);
        engine.add_code(0x100, 0x22222222);
        engine.add_code(0x100, 0x33333333);

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_compile = std::sync::Arc::clone(&captured);
        engine.execute_with_compiler(
            std::ptr::null_mut(),
            0x101,
            &mut [0],
            |_| {},
            move |code| {
                *captured_compile.lock().unwrap() = code.to_vec();
                AnyCachedMacro::Interpreter(MacroInterpreterImpl::new(vec![
                    Operation::AddImmediate as u32
                        | ((ResultOperation::Move as u32) << 4)
                        | (1 << 7),
                    Operation::AddImmediate as u32 | ((ResultOperation::Move as u32) << 4),
                ]))
            },
        );

        assert_eq!(&*captured.lock().unwrap(), &[0x22222222, 0x33333333]);
    }

    #[test]
    fn macro_engine_execute_uses_exact_method_blob_without_contiguous_merge() {
        let mut engine = MacroEngine::new(true);
        engine.add_code(0x100, 0x11111111);
        engine.add_code(0x101, 0x22222222);
        engine.add_code(0x102, 0x33333333);

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_compile = std::sync::Arc::clone(&captured);
        engine.execute_with_compiler(
            std::ptr::null_mut(),
            0x100,
            &mut [0],
            |_| {},
            move |code| {
                *captured_compile.lock().unwrap() = code.to_vec();
                AnyCachedMacro::Interpreter(MacroInterpreterImpl::new(vec![
                    Operation::AddImmediate as u32
                        | ((ResultOperation::Move as u32) << 4)
                        | (1 << 7),
                    Operation::AddImmediate as u32 | ((ResultOperation::Move as u32) << 4),
                ]))
            },
        );

        assert_eq!(&*captured.lock().unwrap(), &[0x11111111]);
    }

    #[test]
    fn hash_macro_code_matches_upstream_hash_range_for_u32_vector() {
        let code = [0x04744351, 0x00708215, 0x00004041, 0x20390021];
        assert_eq!(hash_macro_code(&code), 0x7412B5E8633D2C9B);
    }

    #[test]
    fn macro_dump_uses_eden_filename_and_native_u32_payload() {
        const HOMEBREW_PROGRAM_ID: u64 = 0x0000_0000_4842_5257;
        const HASH: u64 = 0x7412_B5E8_633D_2C9B;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dump_dir =
            std::env::temp_dir().join(format!("ruzu-macro-dump-{}-{unique}", std::process::id()));
        let code = [0x0474_4351, 0x0070_8215];

        dump_to_directory(&dump_dir, HOMEBREW_PROGRAM_ID, HASH, &code, true);

        let path = dump_dir.join("0000000048425257_7412b5e8633d2c9b_jit.macro");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytemuck::cast_slice::<u32, u8>(&code)
        );
        std::fs::remove_dir_all(&dump_dir).unwrap();
    }

    #[test]
    fn num_macro_registers() {
        assert_eq!(NUM_MACRO_REGISTERS, 8);
    }
}
