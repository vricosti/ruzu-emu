// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `video_core/renderer_opengl/gl_shader_context.h`.

use std::mem::ManuallyDrop;
use std::sync::Arc;

use ruzu_core::frontend::graphics_context::GraphicsContext;
use shader_recompiler::frontend::control_flow::FlowBlock;
use shader_recompiler::ir::basic_block::Block;
use shader_recompiler::ir::instruction::Inst;
use shader_recompiler::object_pool::ObjectPool;

/// Frontend-owned factory used by each shader worker to create its shared GL context.
pub type SharedContextFactory =
    Arc<dyn Fn() -> Box<dyn GraphicsContext + Send> + Send + Sync + 'static>;

/// Shader object pools reused by one compilation thread.
pub struct ShaderPools {
    pub inst: ObjectPool<Inst>,
    pub block: ObjectPool<Block>,
    pub flow_block: ObjectPool<FlowBlock>,
}

impl ShaderPools {
    pub fn new() -> Self {
        Self {
            inst: ObjectPool::new(8192),
            block: ObjectPool::new(32),
            flow_block: ObjectPool::new(32),
        }
    }

    /// Upstream releases the flow, IR block, then instruction pools.
    pub fn release_contents(&mut self) {
        self.flow_block.release_contents();
        self.block.release_contents();
        self.inst.release_contents();
    }
}

impl Default for ShaderPools {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ShaderPools {
    fn drop(&mut self) {
        // C++ destroys members in reverse declaration order. Empty the owned
        // objects in that same dependency order before Rust subsequently
        // releases the now-empty pool allocations in declaration order.
        self.release_contents();
    }
}

/// Per-worker shared OpenGL context and shader allocation pools.
pub struct Context {
    gl_context: Box<dyn GraphicsContext + Send>,
    pub pools: ManuallyDrop<ShaderPools>,
}

impl Context {
    pub fn new(factory: &SharedContextFactory) -> Self {
        let mut gl_context = factory();
        gl_context.make_current();
        Self {
            gl_context,
            pools: ManuallyDrop::new(ShaderPools::new()),
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // C++ field destruction is reverse declaration order: pools are
        // released while the shared context is current, then Scoped releases
        // the context, and finally the context object itself is destroyed.
        unsafe {
            ManuallyDrop::drop(&mut self.pools);
        }
        self.gl_context.done_current();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct RecordingContext {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl GraphicsContext for RecordingContext {
        fn make_current(&mut self) {
            self.events.lock().unwrap().push("make_current");
        }

        fn done_current(&mut self) {
            self.events.lock().unwrap().push("done_current");
        }
    }

    impl Drop for RecordingContext {
        fn drop(&mut self) {
            self.events.lock().unwrap().push("drop_context");
        }
    }

    #[test]
    fn context_releases_current_scope_before_destroying_gl_context() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let factory_events = Arc::clone(&events);
        let factory: SharedContextFactory = Arc::new(move || {
            Box::new(RecordingContext {
                events: Arc::clone(&factory_events),
            })
        });

        drop(Context::new(&factory));

        assert_eq!(
            events.lock().unwrap().as_slice(),
            &["make_current", "done_current", "drop_context"]
        );
    }
}
