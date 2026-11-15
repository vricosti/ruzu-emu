// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GLASM emit context.
//!
//! Maps to upstream `backend/glasm/glasm_emit_context.h` and
//! `glasm_emit_context.cpp`.

use crate::backend::bindings::Bindings;
use crate::ir;
use crate::profile::Profile;
use crate::runtime_info::RuntimeInfo;
use crate::stage::Stage;

use super::reg_alloc::RegAlloc;
use super::PROGRAM_LOCAL_PARAMETER_STORAGE_BUFFER_BASE;

/// GLASM emission context.
///
/// Accumulates NV_gpu_program assembly text and manages register allocation.
pub struct EmitContext<'a> {
    pub code: String,
    pub reg_alloc: RegAlloc,
    pub profile: &'a Profile,
    pub runtime_info: &'a RuntimeInfo,
    pub stage: Stage,
    pub stage_name: &'static str,
    pub attrib_name: &'static str,
    pub texture_buffer_bindings: Vec<u32>,
    pub image_buffer_bindings: Vec<u32>,
    pub texture_bindings: Vec<u32>,
    pub image_bindings: Vec<u32>,
    pub num_safety_loop_vars: u32,
    pub uses_y_direction: bool,
}

impl<'a> EmitContext<'a> {
    /// Create a new GLASM emit context from a program and bindings.
    ///
    /// Matches upstream `EmitContext::EmitContext(...)`.
    pub fn new(
        program: &ir::Program,
        bindings: &mut Bindings,
        profile: &'a Profile,
        runtime_info: &'a RuntimeInfo,
    ) -> Self {
        let mut ctx = Self {
            code: String::new(),
            reg_alloc: RegAlloc::new(),
            profile,
            runtime_info,
            stage: program.stage.into(),
            stage_name: "invalid",
            attrib_name: "invalid",
            texture_buffer_bindings: Vec::new(),
            image_buffer_bindings: Vec::new(),
            texture_bindings: Vec::new(),
            image_bindings: Vec::new(),
            num_safety_loop_vars: 0,
            uses_y_direction: false,
        };

        // Set stage names matching upstream
        match program.stage {
            ir::types::ShaderStage::VertexB => {
                ctx.stage_name = "vertex";
                ctx.attrib_name = "vertex";
            }
            ir::types::ShaderStage::TessellationControl => {
                ctx.stage_name = "primitive";
                ctx.attrib_name = "primitive";
            }
            ir::types::ShaderStage::TessellationEval => {
                ctx.stage_name = "primitive";
                ctx.attrib_name = "primitive";
            }
            ir::types::ShaderStage::Geometry => {
                ctx.stage_name = "primitive";
                ctx.attrib_name = "vertex";
            }
            ir::types::ShaderStage::Fragment => {
                ctx.stage_name = "fragment";
                ctx.attrib_name = "fragment";
            }
            ir::types::ShaderStage::Compute => {
                ctx.stage_name = "invocation";
                ctx.attrib_name = "invocation";
            }
            ir::types::ShaderStage::VertexA => {
                // Upstream merges VertexA into VertexB during translation
                // (`MergeDualVertexPrograms`). The backend should never see
                // VertexA reaching emission.
                unreachable!("VertexA must be merged into VertexB before GLASM emission");
            }
        }

        // Upstream maps descriptor order, rather than the guest cbuf index,
        // onto the sequential GL program-buffer namespace.
        for (cbuf_index, desc) in program.info.constant_buffer_descriptors.iter().enumerate() {
            assert_eq!(desc.count, 1, "GLASM constant-buffer descriptor array");
            ctx.add_line(&format!(
                "CBUFFER c{}[]={{program.buffer[{}]}};",
                desc.index, cbuf_index
            ));
        }

        let mut ssbo_index = 0;
        for desc in &program.info.storage_buffers_descriptors {
            assert_eq!(desc.count, 1, "GLASM storage-buffer descriptor array");
            if runtime_info.glasm_use_storage_buffers {
                ctx.add_line(&format!(
                    "STORAGE ssbo{}[]={{program.storage[{}]}};",
                    ssbo_index, bindings.storage_buffer
                ));
                bindings.storage_buffer += 1;
                ssbo_index += 1;
            }
        }
        if !runtime_info.glasm_use_storage_buffers
            && !program.info.storage_buffers_descriptors.is_empty()
        {
            let index = program.info.storage_buffers_descriptors.len() as u32
                + PROGRAM_LOCAL_PARAMETER_STORAGE_BUFFER_BASE;
            ctx.add_line(&format!(
                "PARAM c[{}]={{program.local[0..{}]}};",
                index,
                index - 1
            ));
        }

        // Emit fragment output declarations
        if matches!(program.stage, ir::types::ShaderStage::Fragment) {
            ctx.add_line("OUTPUT frag_color0=result.color;");
        }

        for desc in &program.info.image_buffer_descriptors {
            ctx.image_buffer_bindings.push(bindings.image);
            bindings.image += desc.count;
        }
        for desc in &program.info.image_descriptors {
            ctx.image_bindings.push(bindings.image);
            bindings.image += desc.count;
        }
        for desc in &program.info.texture_buffer_descriptors {
            ctx.texture_buffer_bindings.push(bindings.texture);
            bindings.texture += desc.count;
        }
        for desc in &program.info.texture_descriptors {
            ctx.texture_bindings.push(bindings.texture);
            bindings.texture += desc.count;
        }

        ctx
    }

    /// Append a line of GLASM assembly to the output.
    pub fn add_line(&mut self, line: &str) {
        self.code.push_str(line);
        self.code.push('\n');
    }

    /// Append formatted text to the output followed by a newline.
    pub fn add_fmt(&mut self, text: String) {
        self.code.push_str(&text);
        self.code.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::ShaderStage;
    use crate::ir::Program;
    use crate::shader_info::{
        ConstantBufferDescriptor, ImageBufferDescriptor, ImageDescriptor, ImageFormat,
        StorageBufferDescriptor, TextureBufferDescriptor, TextureDescriptor, TextureType,
    };

    fn image_buffer(count: u32) -> ImageBufferDescriptor {
        ImageBufferDescriptor {
            handle_program: u32::MAX,
            format: ImageFormat::Typeless,
            is_written: false,
            is_read: true,
            is_integer: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            count,
            size_shift: 0,
        }
    }

    fn image(count: u32) -> ImageDescriptor {
        ImageDescriptor {
            handle_program: u32::MAX,
            texture_type: TextureType::Color2D,
            format: ImageFormat::Typeless,
            is_written: false,
            is_read: true,
            is_integer: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            count,
            size_shift: 0,
        }
    }

    fn texture_buffer(count: u32) -> TextureBufferDescriptor {
        TextureBufferDescriptor {
            handle_program: u32::MAX,
            has_secondary: false,
            cbuf_index: 0,
            cbuf_offset: 0,
            shift_left: 0,
            secondary_cbuf_index: 0,
            secondary_cbuf_offset: 0,
            secondary_shift_left: 0,
            count,
            size_shift: 0,
        }
    }

    fn texture(count: u32) -> TextureDescriptor {
        TextureDescriptor {
            handle_program: u32::MAX,
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
            count,
            size_shift: 0,
        }
    }

    #[test]
    fn descriptor_bindings_follow_upstream_order_and_counts() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.info.image_buffer_descriptors = vec![image_buffer(2)];
        program.info.image_descriptors = vec![image(3), image(1)];
        program.info.texture_buffer_descriptors = vec![texture_buffer(4)];
        program.info.texture_descriptors = vec![texture(2), texture(1)];
        let mut bindings = Bindings {
            image: 7,
            texture: 11,
            ..Bindings::default()
        };
        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();

        let context = EmitContext::new(&program, &mut bindings, &profile, &runtime_info);

        assert_eq!(context.image_buffer_bindings, vec![7]);
        assert_eq!(context.image_bindings, vec![9, 12]);
        assert_eq!(context.texture_buffer_bindings, vec![11]);
        assert_eq!(context.texture_bindings, vec![15, 17]);
        assert_eq!(bindings.image, 13);
        assert_eq!(bindings.texture, 18);
    }

    #[test]
    fn cbuffer_and_storage_declarations_match_upstream_namespaces() {
        let mut program = Program::new(ShaderStage::Fragment);
        program.info.constant_buffer_descriptors = vec![
            ConstantBufferDescriptor { index: 5, count: 1 },
            ConstantBufferDescriptor { index: 2, count: 1 },
        ];
        program.info.storage_buffers_descriptors = vec![StorageBufferDescriptor {
            cbuf_index: 0,
            cbuf_offset: 0,
            count: 1,
            is_written: false, ..Default::default() }];
        let mut bindings = Bindings::default();
        let profile = Profile::default();
        let mut runtime_info = RuntimeInfo::default();
        runtime_info.glasm_use_storage_buffers = true;

        let context = EmitContext::new(&program, &mut bindings, &profile, &runtime_info);

        assert!(context.code.contains("CBUFFER c5[]={program.buffer[0]};"));
        assert!(context.code.contains("CBUFFER c2[]={program.buffer[1]};"));
        assert!(context
            .code
            .contains("STORAGE ssbo0[]={program.storage[0]};"));
        assert_eq!(bindings.storage_buffer, 1);
    }
}
