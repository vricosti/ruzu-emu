// SPDX-FileCopyrightText: Copyright 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/nvnflinger/display.h

use std::sync::{Arc, Mutex};

use super::buffer_item_consumer::BufferItemConsumer;
use super::hwc_layer::{LayerBlending, DEFAULT_LAYER_STACK_MASK};

/// A single layer within the display stack.
pub struct Layer {
    pub buffer_item_consumer: Arc<BufferItemConsumer>,
    pub consumer_id: i32,
    pub blending: LayerBlending,
    pub visible: bool,
    pub z_index: i32,
    pub is_overlay: bool,
    pub layer_stack_mask: u32,
}

impl Layer {
    pub fn new(buffer_item_consumer: Arc<BufferItemConsumer>, consumer_id: i32) -> Self {
        Self {
            buffer_item_consumer,
            consumer_id,
            blending: LayerBlending::None,
            visible: true,
            z_index: 0,
            is_overlay: false,
            layer_stack_mask: DEFAULT_LAYER_STACK_MASK,
        }
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        self.buffer_item_consumer.abandon();
    }
}

/// A stack of layers, searchable by consumer_id.
pub struct LayerStack {
    pub layers: Vec<Arc<Mutex<Layer>>>,
}

impl LayerStack {
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    pub fn find_layer(&self, consumer_id: i32) -> Option<Arc<Mutex<Layer>>> {
        for layer in &self.layers {
            if layer.lock().unwrap().consumer_id == consumer_id {
                return Some(Arc::clone(layer));
            }
        }
        None
    }

    pub fn has_layers(&self) -> bool {
        !self.layers.is_empty()
    }
}

impl Default for LayerStack {
    fn default() -> Self {
        Self::new()
    }
}

/// A display with an ID and a layer stack.
pub struct Display {
    pub id: u64,
    pub stack: LayerStack,
}

impl Display {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            stack: LayerStack::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::service::nvnflinger::buffer_queue_consumer::BufferQueueConsumer;
    use crate::hle::service::nvnflinger::buffer_queue_core::BufferQueueCore;

    #[test]
    fn layer_stack_returns_mutable_shared_layer() {
        let consumer = Arc::new(BufferQueueConsumer::new(BufferQueueCore::new()));
        let layer = Arc::new(Mutex::new(Layer::new(
            Arc::new(BufferItemConsumer::new(consumer)),
            7,
        )));
        let mut stack = LayerStack::new();
        stack.layers.push(Arc::clone(&layer));

        let found = stack.find_layer(7).unwrap();
        found.lock().unwrap().visible = false;
        found.lock().unwrap().blending = LayerBlending::Coverage;

        let guard = layer.lock().unwrap();
        assert!(!guard.visible);
        assert_eq!(guard.blending, LayerBlending::Coverage);
    }

    #[test]
    fn layer_defaults_match_upstream() {
        let consumer = Arc::new(BufferQueueConsumer::new(BufferQueueCore::new()));
        let layer = Layer::new(Arc::new(BufferItemConsumer::new(consumer)), 7);

        assert!(layer.visible);
        assert_eq!(layer.blending, LayerBlending::None);
        assert_eq!(layer.z_index, 0);
        assert!(!layer.is_overlay);
        assert_eq!(layer.layer_stack_mask, DEFAULT_LAYER_STACK_MASK);
    }
}
