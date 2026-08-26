// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GLSL emit context.
//!
//! Maps to upstream `backend/glsl/glsl_emit_context.h` and
//! `glsl_emit_context.cpp`.

use crate::backend::bindings::Bindings;
use crate::ir;
use crate::ir::attribute::Attribute;
use crate::profile::Profile;
use crate::runtime_info::{AttributeType, RuntimeInfo};
use crate::shader_info::{ImageFormat, Interpolation, TextureType};
use crate::stage::Stage;

use super::var_alloc::{glsl_type_str, GlslVarType, VarAlloc};

/// Per-generic-output element info.
#[derive(Debug, Clone, Default)]
pub struct GenericElementInfo {
    pub name: String,
    pub first_element: u32,
    pub num_components: u32,
}

/// Texture/image definition with binding index and count.
#[derive(Debug, Clone, Copy)]
pub struct TextureImageDefinition {
    pub binding: u32,
    pub count: u32,
    pub is_multisample: bool,
}

/// GLSL emission context.
pub struct EmitContext<'a> {
    pub header: String,
    pub code: String,
    pub var_alloc: VarAlloc,
    pub profile: &'a Profile,
    pub runtime_info: &'a RuntimeInfo,
    pub stage: Stage,
    pub stage_name: &'static str,
    pub position_name: &'static str,
    pub texture_buffers: Vec<TextureImageDefinition>,
    pub image_buffers: Vec<TextureImageDefinition>,
    pub textures: Vec<TextureImageDefinition>,
    pub images: Vec<TextureImageDefinition>,
    pub output_generics: [[GenericElementInfo; 4]; 32],
    pub num_safety_loop_vars: u32,
    pub uses_y_direction: bool,
    pub uses_cc_carry: bool,
    pub uses_geometry_passthrough: bool,
}

fn sampler_type(texture_type: TextureType, is_depth: bool, is_multisample: bool) -> &'static str {
    match (texture_type, is_depth, is_multisample) {
        (TextureType::Color1D, false, _) => "sampler1D",
        (TextureType::ColorArray1D, false, _) => "sampler1DArray",
        (TextureType::Color2D, false, false) => "sampler2D",
        (TextureType::Color2D, false, true) => "sampler2DMS",
        (TextureType::ColorArray2D, false, false) => "sampler2DArray",
        (TextureType::ColorArray2D, false, true) => "sampler2DMSArray",
        (TextureType::Color3D, false, _) => "sampler3D",
        (TextureType::ColorCube, false, _) => "samplerCube",
        (TextureType::ColorArrayCube, false, _) => "samplerCubeArray",
        (TextureType::Buffer, false, _) => "samplerBuffer",
        (TextureType::Color2DRect, false, _) => "sampler2D",
        (TextureType::Color1D, true, _) => "sampler1DShadow",
        (TextureType::ColorArray1D, true, _) => "sampler1DArrayShadow",
        (TextureType::Color2D, true, _) => "sampler2DShadow",
        (TextureType::ColorArray2D, true, _) => "sampler2DArrayShadow",
        (TextureType::ColorCube, true, _) => "samplerCubeShadow",
        (TextureType::ColorArrayCube, true, _) => "samplerCubeArrayShadow",
        (TextureType::Color2DRect, true, _) => "sampler2DShadow",
        _ => "sampler2D",
    }
}

fn image_type(texture_type: TextureType) -> &'static str {
    match texture_type {
        TextureType::Color1D => "uimage1D",
        TextureType::ColorArray1D => "uimage1DArray",
        TextureType::Color2D => "uimage2D",
        TextureType::ColorArray2D => "uimage2DArray",
        TextureType::Color3D => "uimage3D",
        TextureType::ColorCube => "uimageCube",
        TextureType::ColorArrayCube => "uimageCubeArray",
        TextureType::Buffer => "uimageBuffer",
        TextureType::Color2DRect => "uimage2DRect",
    }
}

fn image_format_string(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Typeless => "",
        ImageFormat::R8Uint => ",r8ui",
        ImageFormat::R8Sint => ",r8i",
        ImageFormat::R16Uint => ",r16ui",
        ImageFormat::R16Sint => ",r16i",
        ImageFormat::R32Uint => ",r32ui",
        ImageFormat::R32G32Uint => ",rg32ui",
        ImageFormat::R32G32B32A32Uint => ",rgba32ui",
    }
}

fn image_access_qualifier(is_written: bool, is_read: bool) -> &'static str {
    match (is_written, is_read) {
        (true, false) => "writeonly ",
        (false, true) => "readonly ",
        _ => "",
    }
}

fn interp_decorator(interp: Interpolation) -> &'static str {
    match interp {
        Interpolation::Smooth => "",
        Interpolation::Flat => "flat ",
        Interpolation::NoPerspective => "noperspective ",
    }
}

fn swizzle(offset: u32) -> &'static str {
    match (offset % 16) / 4 {
        0 => "x",
        1 => "y",
        2 => "z",
        _ => "w",
    }
}

fn stores_per_vertex_attributes(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::VertexA | Stage::VertexB | Stage::Geometry | Stage::TessellationEval
    )
}

impl<'a> EmitContext<'a> {
    pub fn new(
        program: &ir::Program,
        bindings: &mut Bindings,
        profile: &'a Profile,
        runtime_info: &'a RuntimeInfo,
    ) -> Self {
        let mut ctx = Self {
            header: String::new(),
            code: String::new(),
            var_alloc: VarAlloc::new(),
            profile,
            runtime_info,
            stage: program.stage.into(),
            stage_name: "invalid",
            position_name: "gl_Position",
            texture_buffers: Vec::new(),
            image_buffers: Vec::new(),
            textures: Vec::new(),
            images: Vec::new(),
            output_generics: Default::default(),
            num_safety_loop_vars: 0,
            uses_y_direction: false,
            uses_cc_carry: false,
            uses_geometry_passthrough: program.is_geometry_passthrough
                && profile.support_geometry_shader_passthrough,
        };

        match program.stage {
            ir::types::ShaderStage::VertexB => {
                ctx.stage_name = "vs";
            }
            ir::types::ShaderStage::TessellationControl => {
                ctx.stage_name = "tcs";
            }
            ir::types::ShaderStage::TessellationEval => {
                ctx.stage_name = "tes";
            }
            ir::types::ShaderStage::Geometry => {
                ctx.stage_name = "gs";
            }
            ir::types::ShaderStage::Fragment => {
                ctx.stage_name = "fs";
                ctx.position_name = "gl_FragCoord";
            }
            ir::types::ShaderStage::Compute => {
                ctx.stage_name = "cs";
            }
            ir::types::ShaderStage::VertexA => {
                unreachable!("VertexA must be merged into VertexB before GLSL emission");
            }
        }

        ctx.setup_extensions(program);
        ctx.setup_out_per_vertex(program);
        ctx.setup_in_per_vertex(program);
        ctx.define_generic_inputs(program);
        ctx.define_patches(program);
        ctx.define_generic_outputs(program);
        ctx.define_fragment_outputs(program);
        ctx.define_context_uniforms(program);
        ctx.define_constant_buffers(bindings, program);
        ctx.define_storage_buffers(bindings, program);
        ctx.setup_images(bindings, program);
        ctx.setup_textures(bindings, program);
        ctx.define_helper_functions(program);

        ctx
    }

    fn setup_extensions(&mut self, program: &ir::Program) {
        self.header
            .push_str("#extension GL_ARB_separate_shader_objects : enable\n");
        if program.info.uses_shadow_lod && self.profile.support_gl_texture_shadow_lod {
            self.header
                .push_str("#extension GL_EXT_texture_shadow_lod : enable\n");
        }
        if program.info.uses_int64 && self.profile.support_int64 {
            self.header
                .push_str("#extension GL_ARB_gpu_shader_int64 : enable\n");
        }
    }

    fn setup_textures(&mut self, bindings: &mut Bindings, program: &ir::Program) {
        for desc in &program.info.texture_buffer_descriptors {
            let binding = bindings.texture;
            self.texture_buffers.push(TextureImageDefinition {
                binding,
                count: desc.count,
                is_multisample: false,
            });
            let array_decorator = if desc.count > 1 {
                format!("[{}]", desc.count)
            } else {
                String::new()
            };
            self.header.push_str(&format!(
                "layout(binding={}) uniform samplerBuffer tex{}{};\n",
                binding, binding, array_decorator
            ));
            bindings.texture += desc.count;
        }

        // Upstream names sampler variables by their assigned GLSL binding
        // (`tex{binding}`), not by the source TIC cbuf index. TexturePass
        // compacts source descriptors before GLSL emission, so using cbuf_index
        // here creates duplicate declarations and undeclared compact operands.
        for desc in &program.info.texture_descriptors {
            let binding = bindings.texture;
            self.textures.push(TextureImageDefinition {
                binding,
                count: desc.count,
                is_multisample: desc.is_multisample,
            });
            let array_decorator = if desc.count > 1 {
                format!("[{}]", desc.count)
            } else {
                String::new()
            };
            self.header.push_str(&format!(
                "layout(binding={}) uniform {} tex{}{};\n",
                binding,
                sampler_type(desc.texture_type, desc.is_depth, desc.is_multisample),
                binding,
                array_decorator
            ));
            bindings.texture += desc.count;
        }
    }

    fn setup_images(&mut self, bindings: &mut Bindings, program: &ir::Program) {
        for desc in &program.info.image_buffer_descriptors {
            let binding = bindings.image;
            self.image_buffers.push(TextureImageDefinition {
                binding,
                count: desc.count,
                is_multisample: false,
            });
            let array_decorator = if desc.count > 1 {
                format!("[{}]", desc.count)
            } else {
                String::new()
            };
            self.header.push_str(&format!(
                "layout(binding={}{}) uniform {}uimageBuffer img{}{};\n",
                binding,
                image_format_string(desc.format),
                image_access_qualifier(desc.is_written, desc.is_read),
                binding,
                array_decorator
            ));
            bindings.image += desc.count;
        }

        for desc in &program.info.image_descriptors {
            let binding = bindings.image;
            self.images.push(TextureImageDefinition {
                binding,
                count: desc.count,
                is_multisample: false,
            });
            let array_decorator = if desc.count > 1 {
                format!("[{}]", desc.count)
            } else {
                String::new()
            };
            self.header.push_str(&format!(
                "layout(binding={}{})uniform {}{} img{}{};\n",
                binding,
                image_format_string(desc.format),
                image_access_qualifier(desc.is_written, desc.is_read),
                image_type(desc.texture_type),
                binding,
                array_decorator
            ));
            bindings.image += desc.count;
        }
    }

    /// Append a line of GLSL code.
    pub fn add_line(&mut self, line: &str) {
        self.code.push_str(line);
        self.code.push('\n');
    }

    /// Append formatted text to the code followed by a newline.
    pub fn add_fmt(&mut self, text: String) {
        self.code.push_str(&text);
        self.code.push('\n');
    }

    /// Append a line to the header.
    pub fn add_header(&mut self, line: &str) {
        self.header.push_str(line);
        self.header.push('\n');
    }

    pub fn is_input_array(&self) -> bool {
        matches!(
            self.stage,
            Stage::Geometry | Stage::TessellationControl | Stage::TessellationEval
        )
    }

    fn input_array_decorator(&self) -> &'static str {
        if self.is_input_array() {
            "[]"
        } else {
            ""
        }
    }

    fn output_decorator(&self, invocations: u32) -> String {
        if self.stage == Stage::TessellationControl {
            format!("[{}]", invocations)
        } else {
            String::new()
        }
    }

    fn setup_out_per_vertex(&mut self, program: &ir::Program) {
        if !stores_per_vertex_attributes(self.stage) || self.uses_geometry_passthrough {
            return;
        }
        self.header.push_str("out gl_PerVertex{vec4 gl_Position;");
        if program.info.stores.get(Attribute::PointSize as usize) {
            self.header.push_str("float gl_PointSize;");
        }
        if program.info.stores.clip_distances() {
            self.header.push_str("float gl_ClipDistance[];");
        }
        if program.info.stores.get(Attribute::ViewportIndex as usize)
            && self.profile.support_viewport_index_layer_non_geometry
            && self.stage != Stage::Geometry
        {
            self.header.push_str("int gl_ViewportIndex;");
        }
        self.header.push_str("};\n");
        if program.info.stores.get(Attribute::ViewportIndex as usize)
            && self.stage == Stage::Geometry
        {
            self.header.push_str("out int gl_ViewportIndex;\n");
        }
    }

    fn setup_in_per_vertex(&mut self, program: &ir::Program) {
        if self.stage != Stage::TessellationControl {
            return;
        }
        let loads_position = program
            .info
            .loads
            .any_component(Attribute::PositionX as usize);
        let loads_point_size = program.info.loads.get(Attribute::PointSize as usize);
        let loads_clip_distance = program.info.loads.clip_distances();
        if !(loads_position || loads_point_size || loads_clip_distance) {
            return;
        }
        self.header.push_str("in gl_PerVertex{");
        if loads_position {
            self.header.push_str("vec4 gl_Position;");
        }
        if loads_point_size {
            self.header.push_str("float gl_PointSize;");
        }
        if loads_clip_distance {
            self.header.push_str("float gl_ClipDistance[];");
        }
        self.header.push_str("}gl_in[gl_MaxPatchVertices];\n");
    }

    fn define_generic_inputs(&mut self, program: &ir::Program) {
        for index in 0..32usize {
            if !program.info.loads.generic_any(index)
                || !self.runtime_info.previous_stage_stores.generic_any(index)
            {
                continue;
            }
            self.header.push_str(&format!(
                "layout(location={}){}in vec4 in_attr{}{};\n",
                index,
                interp_decorator(program.info.interpolation[index]),
                index,
                self.input_array_decorator()
            ));
        }
    }

    fn define_patches(&mut self, program: &ir::Program) {
        for (index, used) in program.info.uses_patches.iter().enumerate() {
            if !used {
                continue;
            }
            let qualifier = if self.stage == Stage::TessellationControl {
                "out"
            } else {
                "in"
            };
            self.header.push_str(&format!(
                "layout(location={})patch {} vec4 patch{};\n",
                index, qualifier, index
            ));
        }
    }

    fn define_generic_outputs(&mut self, program: &ir::Program) {
        for index in 0..32usize {
            if program.info.stores.generic_any(index) {
                self.define_generic_output(index, program.invocations);
            }
        }
    }

    fn define_generic_output(&mut self, index: usize, invocations: u32) {
        const SWIZZLE: &str = "xyzw";
        let base_index = Attribute::Generic0X as usize + index * 4;
        let mut element = 0usize;
        while element < 4 {
            let mut definition = format!("layout(location={}", index);
            let remainder = 4 - element;
            let xfb_varying = if base_index + element < self.runtime_info.xfb_count as usize {
                let varying = self.runtime_info.xfb_varyings[base_index + element];
                (varying.components > 0).then_some(varying)
            } else {
                None
            };
            let num_components = xfb_varying
                .map(|varying| varying.components as usize)
                .unwrap_or(remainder);
            if element > 0 {
                definition.push_str(&format!(",component={}", element));
            }
            if let Some(varying) = xfb_varying {
                definition.push_str(&format!(
                    ",xfb_buffer={},xfb_stride={},xfb_offset={}",
                    varying.buffer, varying.stride, varying.offset
                ));
            }
            let component_suffix = &SWIZZLE[element..element + num_components];
            let name = if num_components < 4 || element > 0 {
                format!("out_attr{}_{}", index, component_suffix)
            } else {
                format!("out_attr{}", index)
            };
            let type_name = if num_components == 1 {
                "float".to_string()
            } else {
                format!("vec{}", num_components)
            };
            self.header.push_str(&format!(
                "{})out {} {}{};\n",
                definition,
                type_name,
                name,
                self.output_decorator(invocations)
            ));
            let info = GenericElementInfo {
                name,
                first_element: element as u32,
                num_components: num_components as u32,
            };
            for component in element..element + num_components {
                self.output_generics[index][component] = info.clone();
            }
            element += num_components;
        }
    }

    fn define_fragment_outputs(&mut self, program: &ir::Program) {
        if self.stage != Stage::Fragment {
            return;
        }
        for (render_target, &enabled) in program.info.stores_frag_color.iter().enumerate() {
            if enabled || self.profile.need_declared_frag_colors {
                let type_str = match self.runtime_info.frag_color_types[render_target] {
                    AttributeType::UnsignedInt => "uvec4",
                    AttributeType::SignedInt => "ivec4",
                    _ => "vec4",
                };
                self.header.push_str(&format!(
                    "layout(location={})out {} frag_color{};\n",
                    render_target, type_str, render_target
                ));
            }
        }
    }

    fn define_context_uniforms(&mut self, program: &ir::Program) {
        if program.info.uses_rescaling_uniform {
            self.header
                .push_str("layout(location=0) uniform vec4 scaling;\n");
        }
        if program.info.uses_render_area {
            self.header
                .push_str("layout(location=1) uniform vec4 render_area;\n");
        }
    }

    fn define_constant_buffers(&mut self, bindings: &mut Bindings, program: &ir::Program) {
        for desc in &program.info.constant_buffer_descriptors {
            let cbuf_type = if self.profile.has_gl_cbuf_ftou_bug {
                "uvec4"
            } else {
                "vec4"
            };
            let used_size = program.info.constant_buffer_used_sizes[desc.index as usize];
            let cbuf_used_size = used_size.max(16).div_ceil(16);
            let cbuf_binding_size = if program.info.uses_global_memory {
                0x1000
            } else {
                cbuf_used_size
            };
            self.header.push_str(&format!(
                "layout(std140,binding={}) uniform {}_cbuf_{}{{{} {}_cbuf{}[{}];}};\n",
                bindings.uniform_buffer,
                self.stage_name,
                desc.index,
                cbuf_type,
                self.stage_name,
                desc.index,
                cbuf_binding_size
            ));
            bindings.uniform_buffer += desc.count;
        }
    }

    fn define_storage_buffers(&mut self, bindings: &mut Bindings, program: &ir::Program) {
        let mut index = 0u32;
        for desc in &program.info.storage_buffers_descriptors {
            self.header.push_str(&format!(
                "layout(std430,binding={}) buffer {}_ssbo_{}{{uint {}_ssbo{}[];}};\n",
                bindings.storage_buffer,
                self.stage_name,
                bindings.storage_buffer,
                self.stage_name,
                index
            ));
            bindings.storage_buffer += desc.count;
            index += desc.count;
        }
    }

    fn define_helper_functions(&mut self, program: &ir::Program) {
        self.header.push_str(
            "\n#define ftoi floatBitsToInt\n#define ftou floatBitsToUint\n#define itof intBitsToFloat\n#define utof uintBitsToFloat\n",
        );
        if program.info.uses_atomic_s32_min {
            self.header.push_str(
                "uint CasMinS32(uint op_a,uint op_b){return uint(min(int(op_a),int(op_b)));}",
            );
        }
        if program.info.uses_atomic_s32_max {
            self.header.push_str(
                "uint CasMaxS32(uint op_a,uint op_b){return uint(max(int(op_a),int(op_b)));}",
            );
        }
        if program.info.uses_global_memory && self.profile.support_int64 {
            self.header
                .push_str(&self.define_global_memory_functions(program));
        }
        if program.info.loads_indexed_attributes {
            self.header
                .push_str(&self.define_indexed_attr_load(program));
        }
    }

    fn define_indexed_attr_load(&self, program: &ir::Program) -> String {
        let is_array = self.stage == Stage::Geometry;
        let vertex_arg = if is_array { ",uint vertex" } else { "" };
        let mut func = format!(
            "float IndexedAttrLoad(int offset{}){{int base_index=offset>>2;uint masked_index=uint(base_index)&3u;switch(base_index>>2){{",
            vertex_arg
        );
        if program
            .info
            .loads
            .any_component(Attribute::PositionX as usize)
        {
            let position_idx = if is_array { "gl_in[vertex]." } else { "" };
            func.push_str(&format!(
                "case {}:return {}{}[masked_index];",
                (Attribute::PositionX as u32) >> 2,
                position_idx,
                self.position_name
            ));
        }
        let base_attribute_value = (Attribute::Generic0X as u32) >> 2;
        for index in 0..32u32 {
            if !program.info.loads.generic_any(index as usize) {
                continue;
            }
            let vertex_idx = if is_array { "[vertex]" } else { "" };
            func.push_str(&format!(
                "case {}:return in_attr{}{}[masked_index];",
                base_attribute_value + index,
                index,
                vertex_idx
            ));
        }
        func.push_str("default:return 0.0;}}");
        func
    }

    fn define_global_memory_functions(&self, program: &ir::Program) -> String {
        let mut write_func = "void WriteGlobal32(uint64_t addr,uint data){".to_string();
        let mut write_func_64 = "void WriteGlobal64(uint64_t addr,uvec2 data){".to_string();
        let mut write_func_128 = "void WriteGlobal128(uint64_t addr,uvec4 data){".to_string();
        let mut load_func = "uint LoadGlobal32(uint64_t addr){".to_string();
        let mut load_func_64 = "uvec2 LoadGlobal64(uint64_t addr){".to_string();
        let mut load_func_128 = "uvec4 LoadGlobal128(uint64_t addr){".to_string();

        for (index, ssbo) in program.info.storage_buffers_descriptors.iter().enumerate() {
            let used =
                index < u16::BITS as usize && (program.info.nvn_buffer_used & (1u16 << index)) != 0;
            if !used {
                continue;
            }
            let size_cbuf_offset = ssbo.cbuf_offset + 8;
            let ssbo_addr = format!("ssbo_addr{}", index);
            let cbuf = format!("{}_cbuf{}", self.stage_name, ssbo.cbuf_index);
            let addr_xy = [
                format!(
                    "ftou({}[{}].{})",
                    cbuf,
                    ssbo.cbuf_offset / 16,
                    swizzle(ssbo.cbuf_offset)
                ),
                format!(
                    "ftou({}[{}].{})",
                    cbuf,
                    (ssbo.cbuf_offset + 4) / 16,
                    swizzle(ssbo.cbuf_offset + 4)
                ),
            ];
            let size_xy = [
                format!(
                    "ftou({}[{}].{})",
                    cbuf,
                    size_cbuf_offset / 16,
                    swizzle(size_cbuf_offset)
                ),
                format!(
                    "ftou({}[{}].{})",
                    cbuf,
                    (size_cbuf_offset + 4) / 16,
                    swizzle(size_cbuf_offset + 4)
                ),
            ];
            let align = self.profile.min_ssbo_alignment.max(1) as u32;
            let ssbo_align_mask = !align.wrapping_sub(1);
            let aligned_low_addr = format!("{}&{}", addr_xy[0], ssbo_align_mask);
            let aligned_addr = format!("uvec2({},{})", aligned_low_addr, addr_xy[1]);
            let addr_pack = format!("packUint2x32({})", aligned_addr);
            let addr_statement = format!("uint64_t {}={};", ssbo_addr, addr_pack);
            let size_vec = format!("uvec2({},{})", size_xy[0], size_xy[1]);
            let comparison = format!(
                "if((addr>={})&&(addr<({}+uint64_t({})))){{",
                ssbo_addr, ssbo_addr, size_vec
            );
            let ssbo_name = format!("{}_ssbo{}", self.stage_name, index);

            let define_body = |func: &mut String, return_statement: &str| {
                func.push_str(&addr_statement);
                func.push_str(&comparison);
                func.push_str(
                    &return_statement
                        .replace("{0}", &ssbo_name)
                        .replace("{1}", &ssbo_addr),
                );
            };

            define_body(&mut write_func, "{0}[uint(addr-{1})>>2]=data;return;}");
            define_body(
                &mut write_func_64,
                "{0}[uint(addr-{1})>>2]=data.x;{0}[uint(addr-{1}+4)>>2]=data.y;return;}",
            );
            define_body(
                &mut write_func_128,
                "{0}[uint(addr-{1})>>2]=data.x;{0}[uint(addr-{1}+4)>>2]=data.y;{0}[uint(addr-{1}+8)>>2]=data.z;{0}[uint(addr-{1}+12)>>2]=data.w;return;}",
            );
            define_body(&mut load_func, "return {0}[uint(addr-{1})>>2];}");
            define_body(
                &mut load_func_64,
                "return uvec2({0}[uint(addr-{1})>>2],{0}[uint(addr-{1}+4)>>2]);}",
            );
            define_body(
                &mut load_func_128,
                "return uvec4({0}[uint(addr-{1})>>2],{0}[uint(addr-{1}+4)>>2],{0}[uint(addr-{1}+8)>>2],{0}[uint(addr-{1}+12)>>2]);}",
            );
        }

        write_func.push('}');
        write_func_64.push('}');
        write_func_128.push('}');
        load_func.push_str("return 0u;}");
        load_func_64.push_str("return uvec2(0);}");
        load_func_128.push_str("return uvec4(0);}");
        format!(
            "{}{}{}{}{}{}",
            write_func, write_func_64, write_func_128, load_func, load_func_64, load_func_128
        )
    }

    pub fn define_variables(&self, header: &mut String) {
        for var_type in [
            GlslVarType::U1,
            GlslVarType::F16x2,
            GlslVarType::U32,
            GlslVarType::F32,
            GlslVarType::U64,
            GlslVarType::F64,
            GlslVarType::U32x2,
            GlslVarType::F32x2,
            GlslVarType::U32x3,
            GlslVarType::F32x3,
            GlslVarType::U32x4,
            GlslVarType::F32x4,
            GlslVarType::PrecF32,
            GlslVarType::PrecF64,
        ] {
            let tracker = self.var_alloc.get_use_tracker(var_type);
            let type_name = glsl_type_str(var_type);
            let has_precise_bug = self.stage == Stage::Fragment && self.profile.has_gl_precise_bug;
            let precise = if !has_precise_bug && is_precise_type(var_type) {
                "precise "
            } else {
                ""
            };
            if tracker.uses_temp {
                header.push_str(&format!(
                    "{}{} t{}={}(0);\n",
                    precise,
                    type_name,
                    self.var_alloc.representation_indexed(0, var_type),
                    type_name
                ));
            }
            for index in 0..tracker.num_used {
                header.push_str(&format!(
                    "{}{} {}={}(0);\n",
                    precise,
                    type_name,
                    self.var_alloc
                        .representation_indexed(index as u32, var_type),
                    type_name
                ));
            }
        }
        for index in 0..self.num_safety_loop_vars {
            header.push_str(&format!("int loop{}=0x2000;\n", index));
        }
    }
}

fn is_precise_type(var_type: GlslVarType) -> bool {
    matches!(var_type, GlslVarType::PrecF32 | GlslVarType::PrecF64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_stage_declares_out_per_vertex() {
        let mut program = ir::Program::new(Stage::VertexB);
        program.info.stores.set(Attribute::PointSize as usize, true);
        let mut bindings = Bindings::default();
        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();

        let ctx = EmitContext::new(&program, &mut bindings, &profile, &runtime_info);

        assert!(ctx
            .header
            .contains("out gl_PerVertex{vec4 gl_Position;float gl_PointSize;};"));
    }

    #[test]
    fn generic_inputs_use_interpolation_decorator() {
        let mut program = ir::Program::new(Stage::Fragment);
        program.info.loads.set(Attribute::Generic0X as usize, true);
        program.info.interpolation[0] = Interpolation::Flat;
        let mut runtime_info = RuntimeInfo::default();
        runtime_info
            .previous_stage_stores
            .set(Attribute::Generic0X as usize, true);
        let mut bindings = Bindings::default();
        let profile = Profile::default();

        let ctx = EmitContext::new(&program, &mut bindings, &profile, &runtime_info);

        assert!(ctx
            .header
            .contains("layout(location=0)flat in vec4 in_attr0;"));
    }

    #[test]
    fn generic_outputs_use_transform_feedback_component_splits() {
        let mut program = ir::Program::new(Stage::VertexB);
        program.info.stores.set(Attribute::Generic0X as usize, true);

        let mut runtime_info = RuntimeInfo::default();
        runtime_info.xfb_varyings[Attribute::Generic0X as usize] =
            crate::runtime_info::TransformFeedbackVarying {
                buffer: 1,
                stream: 0,
                stride: 16,
                offset: 0,
                components: 2,
            };
        runtime_info.xfb_varyings[Attribute::Generic0Z as usize] =
            crate::runtime_info::TransformFeedbackVarying {
                buffer: 1,
                stream: 0,
                stride: 16,
                offset: 8,
                components: 2,
            };
        runtime_info.xfb_count = Attribute::Generic0W as u32 + 1;

        let mut bindings = Bindings::default();
        let profile = Profile::default();
        let ctx = EmitContext::new(&program, &mut bindings, &profile, &runtime_info);

        assert!(ctx.header.contains(
            "layout(location=0,xfb_buffer=1,xfb_stride=16,xfb_offset=0)out vec2 out_attr0_xy;"
        ));
        assert!(ctx.header.contains(
            "layout(location=0,component=2,xfb_buffer=1,xfb_stride=16,xfb_offset=8)out vec2 out_attr0_zw;"
        ));
    }

    #[test]
    fn texture_definitions_preserve_multisample_flag_for_image_queries() {
        let mut program = ir::Program::new(Stage::Fragment);
        program
            .info
            .texture_descriptors
            .push(crate::shader_info::TextureDescriptor {
                texture_type: TextureType::Color2D,
                is_depth: false,
                is_multisample: true,
                is_integer: false,
                has_secondary: false,
                cbuf_index: 2,
                cbuf_offset: 0x40,
                shift_left: 0,
                secondary_cbuf_index: 0,
                secondary_cbuf_offset: 0,
                secondary_shift_left: 0,
                count: 1,
                size_shift: 3,
            });
        let mut bindings = Bindings::default();
        let profile = Profile::default();
        let runtime_info = RuntimeInfo::default();

        let ctx = EmitContext::new(&program, &mut bindings, &profile, &runtime_info);

        assert_eq!(ctx.textures.len(), 1);
        assert!(ctx.textures[0].is_multisample);
        assert!(ctx.header.contains("uniform sampler2DMS tex0;"));
    }

    #[test]
    fn global_memory_header_declares_ssbo_and_helpers_when_int64_supported() {
        let mut program = ir::Program::new(Stage::Fragment);
        program.info.uses_int64 = true;
        program.info.uses_global_memory = true;
        program.info.nvn_buffer_used = 1;
        program
            .info
            .constant_buffer_descriptors
            .push(crate::shader_info::ConstantBufferDescriptor { index: 2, count: 1 });
        program.info.storage_buffers_descriptors.push(
            crate::shader_info::StorageBufferDescriptor {
                cbuf_index: 2,
                cbuf_offset: 0x20,
                count: 1,
                is_written: true,
            },
        );
        let mut bindings = Bindings::default();
        let mut profile = Profile::default();
        profile.support_int64 = true;
        profile.min_ssbo_alignment = 0x100;
        let runtime_info = RuntimeInfo::default();

        let ctx = EmitContext::new(&program, &mut bindings, &profile, &runtime_info);

        assert!(ctx
            .header
            .contains("#extension GL_ARB_gpu_shader_int64 : enable"));
        assert!(ctx
            .header
            .contains("layout(std430,binding=0) buffer fs_ssbo_0{uint fs_ssbo0[];}"));
        assert!(ctx.header.contains("uint LoadGlobal32(uint64_t addr){"));
        assert!(ctx
            .header
            .contains("void WriteGlobal32(uint64_t addr,uint data){"));
        assert!(ctx.header.contains("uint64_t ssbo_addr0=packUint2x32"));
    }

    #[test]
    fn shadow_lod_extension_header_matches_upstream_guard() {
        let mut program = ir::Program::new(Stage::Fragment);
        program.info.uses_shadow_lod = true;
        let mut bindings = Bindings::default();
        let mut profile = Profile::default();
        profile.support_gl_texture_shadow_lod = true;
        let runtime_info = RuntimeInfo::default();

        let ctx = EmitContext::new(&program, &mut bindings, &profile, &runtime_info);

        assert!(ctx
            .header
            .contains("#extension GL_EXT_texture_shadow_lod : enable"));
    }
}
