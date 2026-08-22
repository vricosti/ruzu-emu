// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal dirty-state ownership.
//!
//! This is the Metal counterpart of Eden's backend state tracker lifecycle.
//! Metal currently emits its native dynamic state for every draw. It also
//! reuses Vulkan's upstream-faithful `FixedPipelineState`, so its register
//! tables must include every dirty flag consumed by that shared state.

use crate::control::channel_state::ChannelState;
use crate::renderer_vulkan::state_tracker::setup_pipeline_state_dirty_tables;

#[derive(Default)]
pub struct MetalStateTracker {
    bound_channel_id: Option<i32>,
}

impl MetalStateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Metal counterpart of Eden `StateTracker::SetupTables`.
    pub fn setup_tables(&mut self, channel: &mut ChannelState) {
        let Some(maxwell_3d) = channel.maxwell_3d.as_mut() else {
            return;
        };
        setup_pipeline_state_dirty_tables(maxwell_3d.dirty_tables_mut());
    }

    /// Metal counterpart of Eden `StateTracker::ChangeChannel`.
    pub fn change_channel(&mut self, channel: &ChannelState) {
        self.bound_channel_id = Some(channel.bind_id);
    }

    /// Metal counterpart of Eden `StateTracker::InvalidateState`.
    pub fn invalidate_state(&self, channel: &mut ChannelState) {
        if self.bound_channel_id != Some(channel.bind_id) {
            return;
        }
        if let Some(maxwell_3d) = channel.maxwell_3d.as_mut() {
            maxwell_3d.dirty_flags_mut().fill(true);
        }
    }

    pub fn release_channel(&mut self, channel_id: i32) {
        if self.bound_channel_id == Some(channel_id) {
            self.bound_channel_id = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dirty_flags::flags;
    use crate::engines::draw_manager::Maxwell3DAccess;
    use crate::engines::maxwell_3d::{
        Maxwell3D, BLEND_BASE, BLEND_PER_TARGET_BASE, RT_BASE, VERTEX_ATTRIB_BASE,
    };
    use crate::renderer_vulkan::state_tracker::dirty;

    #[test]
    fn installs_common_tables_and_invalidates_bound_channel() {
        let mut tracker = MetalStateTracker::new();
        let mut channel = ChannelState::new(7);
        channel.maxwell_3d = Some(Box::new(Maxwell3D::new()));
        channel
            .maxwell_3d
            .as_mut()
            .unwrap()
            .dirty_flags_mut()
            .fill(false);

        tracker.setup_tables(&mut channel);
        let tables = channel.maxwell_3d.as_ref().unwrap().dirty_tables();
        assert_eq!(tables[0][RT_BASE as usize], flags::COLOR_BUFFER0);
        assert_eq!(tables[1][RT_BASE as usize], flags::RENDER_TARGETS);
        assert_eq!(tables[0][BLEND_BASE as usize], dirty::BLENDING);
        assert_eq!(tables[1][BLEND_BASE as usize], dirty::BLEND_EQUATIONS);
        assert_eq!(
            tables[0][BLEND_PER_TARGET_BASE as usize],
            dirty::BLENDING
        );
        assert_eq!(
            tables[0][VERTEX_ATTRIB_BASE as usize],
            dirty::VERTEX_ATTRIBUTE_0
        );
        assert_eq!(
            tables[1][VERTEX_ATTRIB_BASE as usize],
            dirty::VERTEX_INPUT
        );

        tracker.change_channel(&channel);
        tracker.invalidate_state(&mut channel);
        assert!(channel
            .maxwell_3d
            .as_ref()
            .unwrap()
            .dirty_flags()
            .iter()
            .all(|dirty| *dirty));

        tracker.release_channel(channel.bind_id);
    }
}
