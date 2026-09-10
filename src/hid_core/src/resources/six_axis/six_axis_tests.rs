// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::resources::{
    applet_resource::AppletResource, shared_memory_holder::KSharedMemoryBacking,
};
use parking_lot::Mutex;
use std::{any::Any, sync::Arc};

struct Backing;
impl KSharedMemoryBacking for Backing {
    fn create(&self, size: usize) -> Option<(*mut u8, Arc<dyn Any + Send + Sync>)> {
        // Explicit u64 alignment for SharedMemoryFormat, unlike a byte allocation.
        let mut bytes = vec![0u64; size.div_ceil(8)].into_boxed_slice();
        let ptr = bytes.as_mut_ptr().cast();
        Some((ptr, Arc::new(bytes)))
    }
}

#[test]
fn motion_publication_respects_styles_gates_and_disabled_defaults() {
    // Keep global settings changes out of other concurrently executing tests.
    const CHILD: &str = "RUZU_TEST_SIXAXIS_PUBLICATION";
    if std::env::var_os(CHILD).is_none() {
        assert!(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "resources::six_axis::six_axis::tests::motion_publication_respects_styles_gates_and_disabled_defaults"])
            .env(CHILD, "1").status().unwrap().success());
        return;
    }
    const ARUID: u64 = 0x51;
    let hid = HIDCore::new();
    let mut resource = AppletResource::new();
    resource.set_shared_memory_backing(Arc::new(Backing));
    assert!(resource
        .register_applet_resource_user_id(ARUID, true)
        .is_success());
    assert!(resource.create_applet_resource(ARUID).is_success());
    let resource = Arc::new(Mutex::new(resource));
    let mut six_axis = SixAxis::new(&hid);
    six_axis.activation.set_applet_resource(resource.clone());
    common::settings::values_mut()
        .motion_enabled
        .set_value(false);
    let styles = [
        NpadStyleIndex::Fullkey,
        NpadStyleIndex::JoyconDual,
        NpadStyleIndex::JoyconLeft,
        NpadStyleIndex::JoyconRight,
        NpadStyleIndex::Pokeball,
    ];
    for (index, style) in styles.into_iter().enumerate() {
        let device = hid.get_emulated_controller_by_index(index);
        let mut device = device.lock();
        device.set_npad_style_index(style);
        device.connect(false);
    }
    let handheld = hid.get_emulated_controller(NpadIdType::Handheld);
    handheld
        .lock()
        .set_npad_style_index(NpadStyleIndex::Handheld);
    handheld.lock().connect(false);
    six_axis.on_update();
    assert_eq!(
        six_axis.controller_data[0]
            .sixaxis_fullkey_state
            .sampling_number,
        0
    );
    six_axis.activation.activate();
    resource.lock().enable_six_axis_sensor(ARUID, false);
    six_axis.on_update();
    assert_eq!(
        six_axis.controller_data[0]
            .sixaxis_fullkey_state
            .sampling_number,
        0
    );
    resource.lock().enable_six_axis_sensor(ARUID, true);
    for sampling in 1..=3 {
        six_axis.on_update();
        let resource = resource.lock();
        let shared = resource.get_shared_memory_format(ARUID).unwrap();
        for (index, expected) in [(0, 0), (1, 2), (2, 4), (3, 5), (4, 0), (8, 1)] {
            let memory = &shared.npad.npad_entry[index].internal_state;
            let lifos = [
                &memory.sixaxis_fullkey_lifo,
                &memory.sixaxis_handheld_lifo,
                &memory.sixaxis_dual_left_lifo,
                &memory.sixaxis_dual_right_lifo,
                &memory.sixaxis_left_lifo,
                &memory.sixaxis_right_lifo,
            ];
            for (slot, lifo) in lifos.into_iter().enumerate() {
                let state = &lifo.lifo.read_current_entry().state;
                let excluded = if index == 8 { slot == 0 } else { slot == 1 };
                assert_eq!(state.sampling_number, if excluded { 0 } else { sampling });
                let active = slot == expected || (index == 1 && slot == 3);
                assert_eq!(state.attribute.is_connected(), active);
                assert_eq!(
                    state.delta_time,
                    if active {
                        if index == 4 {
                            15_000_000
                        } else {
                            5_000_000
                        }
                    } else {
                        0
                    }
                );
                assert_eq!(state.accel.z, if active { -1.0 } else { 0.0 });
                assert_eq!(state.orientation[0].x, if active { 1.0 } else { 0.0 });
                assert_eq!(state._reserved, [0; 4]);
            }
        }
    }
    common::settings::values_mut()
        .motion_enabled
        .set_value(true);
    six_axis.controller_data[0].sixaxis_sensor_enabled = false;
    six_axis.on_update();
    assert_eq!(
        six_axis.controller_data[0].sixaxis_fullkey_state.accel.z,
        -1.0
    );
    // Other controllers now publish actual (initially zero) frontend samples.
    assert_eq!(
        six_axis.controller_data[1].sixaxis_dual_left_state.accel.z,
        0.0
    );
    assert!(!six_axis.controller_data[1].sixaxis_at_rest);
}
