// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! MSL source-emission context.
//!
//! This is the native-MSL counterpart of `glsl_emit_context.{h,cpp}`. It owns
//! source construction only; Maxwell translation and IR optimization remain
//! in their existing frontend passes.

use crate::stage::Stage;

use super::{MslBindingLayout, MslError, MslShaderArtifact, MslShaderSource};

pub struct MslEmitContext {
    stage: Stage,
    source: String,
}

impl MslEmitContext {
    pub fn new(stage: Stage) -> Result<Self, MslError> {
        match stage {
            Stage::VertexA => return Err(MslError::UnmergedVertexA),
            Stage::VertexB | Stage::Fragment => {}
            Stage::TessellationControl
            | Stage::TessellationEval
            | Stage::Geometry
            | Stage::Compute => return Err(MslError::UnsupportedStage(stage)),
        }
        let mut source = String::from("#include <metal_stdlib>\nusing namespace metal;\n\n");
        match stage {
            Stage::VertexB => source.push_str(concat!(
                "struct MslVertexOut {\n",
                "    float4 position [[position]];\n",
                "};\n\n",
                "vertex MslVertexOut main0() {\n",
                "    MslVertexOut output = {};\n",
                "    output.position = float4(0.0f);\n",
            )),
            Stage::Fragment => source.push_str("fragment void main0() {\n"),
            _ => unreachable!("stage was validated above"),
        }
        Ok(Self { stage, source })
    }

    pub fn finish(mut self) -> MslShaderArtifact {
        if self.stage == Stage::VertexB {
            self.source.push_str("    return output;\n");
        }
        self.source.push_str("}\n");
        MslShaderArtifact {
            source: MslShaderSource {
                source: self.source,
                stage: self.stage,
            },
            bindings: MslBindingLayout::default(),
            entry_point: "main0".to_owned(),
        }
    }
}
