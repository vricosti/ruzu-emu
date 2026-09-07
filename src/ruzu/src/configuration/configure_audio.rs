// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_audio.cpp`
// (`ConfigureAudio`), whose widget tree lives in `configure_audio.ui`.
//
// A single "Audio" group: output engine, output device, input device, sound
// output mode, volume slider, and the two mute toggles.
//
// The device lists are refreshed whenever the engine changes
// (`ConfigureAudio::UpdateAudioDevices`), because each sink enumerates its own
// devices.

use std::rc::Rc;

use gtk::prelude::*;

use common::settings_enums::AudioEngine;

use super::configure_dialog::Page;
use super::shared_translation as tr;
use super::shared_widget as w;

/// The device entry meaning "let the sink pick" — upstream inserts `"auto"`
/// as the first row of both device combos.
const AUTO_DEVICE: &str = "auto";

/// Build the Audio tab — upstream `ConfigureAudio`.
pub fn page(runtime_lock: bool) -> Page {
    let configuring_global = common::settings::is_configuring_global();
    let (scroller, column) = w::page();

    let (group, content) = w::group("Audio");

    // Output engine. Upstream inserts `auto`, then every compiled-in sink in
    // `AudioCore::Sink::GetSinkIDs()` order.
    let engines = Rc::new(audio_engines());
    let engine_labels: Vec<String> = engines
        .iter()
        .map(|engine| engine.canonicalize().to_string())
        .collect();
    let engine_refs: Vec<&str> = engine_labels.iter().map(String::as_str).collect();
    let engine_index = engines
        .iter()
        .position(|engine| engine == common::settings::values().sink_id.get_value())
        .unwrap_or(0) as u32;
    let (engine_row, engine) = w::combo_row("Output Engine:", &engine_refs, engine_index);
    content.append(&engine_row);

    let initial_engine = engines
        .get(engine_index as usize)
        .copied()
        .unwrap_or(AudioEngine::Auto);
    let output_device_value = common::settings::values()
        .audio_output_device_id
        .get_value()
        .clone();
    let output_devices = audio_devices(initial_engine, false);
    let output_refs: Vec<&str> = output_devices.iter().map(String::as_str).collect();
    let (output_row, output_device) = w::combo_row(
        "Output Device:",
        &output_refs,
        selected_device(&output_devices, &output_device_value),
    );
    content.append(&output_row);

    let input_device_value = common::settings::values()
        .audio_input_device_id
        .get_value()
        .clone();
    let input_devices = audio_devices(initial_engine, true);
    let input_refs: Vec<&str> = input_devices.iter().map(String::as_str).collect();
    let (input_row, input_device) = w::combo_row(
        "Input Device:",
        &input_refs,
        selected_device(&input_devices, &input_device_value),
    );
    content.append(&input_row);

    let mode_value = *common::settings::values().sound_index.get_value();
    let (mode_row, mode) = w::combo_row(
        "Sound Output Mode:",
        &tr::labels(tr::AUDIO_MODE),
        tr::index_of(tr::AUDIO_MODE, &mode_value),
    );
    content.append(&mode_row);

    let volume_value = *common::settings::values().volume.get_value();
    let (volume_row, volume, _) = w::percent_slider_row("Volume:", volume_value as f64, 0.0, 200.0);
    content.append(&volume_row);

    let mute = w::check_row(
        "Mute audio",
        *common::settings::values().audio_muted.get_value(),
    );
    mute.set_visible(configuring_global);
    content.append(&mute);

    let mute_background = w::check_row(
        "Mute audio when in background",
        crate::uisettings::with(|v| *v.mute_when_in_background.get_value()),
    );
    mute_background.set_visible(configuring_global);
    content.append(&mute_background);

    // Eden uses Widget's runtime sensitivity for all rows, but retains
    // dedicated unconditional serializers for the engine and device lists.
    engine_row.set_sensitive(
        w::SettingEditPolicy::new(
            &common::settings::values().sink_id,
            runtime_lock,
            configuring_global,
        )
        .sensitive,
    );
    output_row.set_sensitive(
        w::SettingEditPolicy::new(
            &common::settings::values().audio_output_device_id,
            runtime_lock,
            configuring_global,
        )
        .sensitive,
    );
    input_row.set_sensitive(
        w::SettingEditPolicy::new(
            &common::settings::values().audio_input_device_id,
            runtime_lock,
            configuring_global,
        )
        .sensitive,
    );
    let mode_policy = w::SettingEditPolicy::new(
        &common::settings::values().sound_index,
        runtime_lock,
        configuring_global,
    );
    mode_row.set_sensitive(mode_policy.sensitive);
    let volume_policy = w::SettingEditPolicy::new(
        &common::settings::values().volume,
        runtime_lock,
        configuring_global,
    );
    volume_row.set_sensitive(volume_policy.sensitive);

    column.append(&group);

    // Upstream clears and re-enumerates both device combos when the engine
    // changes. Clearing selects the leading `auto` entry for each list.
    let engines_for_devices = Rc::clone(&engines);
    let output_device_for_engine = output_device.clone();
    let input_device_for_engine = input_device.clone();
    engine.connect_selected_notify(move |engine| {
        let sink_id = engines_for_devices
            .get(engine.selected() as usize)
            .copied()
            .unwrap_or(AudioEngine::Auto);
        set_devices(&output_device_for_engine, audio_devices(sink_id, false), 0);
        set_devices(&input_device_for_engine, audio_devices(sink_id, true), 0);
    });

    Page::new("Audio", scroller, move || {
        let sink_id = engines
            .get(engine.selected() as usize)
            .copied()
            .unwrap_or(AudioEngine::Auto);
        let output_name = combo_text(&output_device).unwrap_or_else(|| AUTO_DEVICE.to_string());
        let input_name = combo_text(&input_device).unwrap_or_else(|| AUTO_DEVICE.to_string());
        let mode_value = tr::value_at(tr::AUDIO_MODE, mode.selected());
        let volume_value = volume.value() as u8;
        let muted = mute.is_active();
        let muted_background = mute_background.is_active();

        {
            let mut values = common::settings::values_mut();
            values.sink_id.set_value(sink_id);
            values.audio_output_device_id.set_value(output_name);
            values.audio_input_device_id.set_value(input_name);
            mode_policy.apply(&mut values.sound_index, mode_value);
            volume_policy.apply(&mut values.volume, volume_value);
            if configuring_global {
                values.audio_muted.set_value(muted);
            }
        }
        if configuring_global {
            crate::uisettings::with_mut(|v| v.mute_when_in_background.set_value(muted_background));
        }
    })
}

/// Selected text of a `DropDown` backed by a `StringList`.
fn combo_text(dropdown: &gtk::DropDown) -> Option<String> {
    dropdown
        .model()
        .and_downcast::<gtk::StringList>()
        .and_then(|list| list.string(dropdown.selected()))
        .map(|s| s.to_string())
}

fn audio_engines() -> Vec<AudioEngine> {
    let mut engines = vec![AudioEngine::Auto];
    engines.extend(
        audio_core::sink::sink_details::get_sink_ids()
            .into_iter()
            .filter(|engine| *engine != AudioEngine::Auto),
    );
    engines
}

fn audio_devices(engine: AudioEngine, capture: bool) -> Vec<String> {
    let mut devices = vec![AUTO_DEVICE.to_string()];
    devices.extend(audio_core::sink::sink_details::get_device_list_for_sink(
        engine, capture,
    ));
    devices
}

fn selected_device(devices: &[String], selected: &str) -> u32 {
    devices
        .iter()
        .position(|device| device == selected)
        .unwrap_or(0) as u32
}

fn set_devices(dropdown: &gtk::DropDown, devices: Vec<String>, selected: u32) {
    let device_refs: Vec<&str> = devices.iter().map(String::as_str).collect();
    dropdown.set_model(Some(&gtk::StringList::new(&device_refs)));
    dropdown.set_selected(if (selected as usize) < devices.len() {
        selected
    } else {
        0
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_and_devices_are_startup_only_but_volume_and_mode_are_live() {
        let mut values = common::settings::Values::default();
        assert!(!w::SettingEditPolicy::new(&values.sink_id, false, true).sensitive);
        assert!(!w::SettingEditPolicy::new(&values.audio_output_device_id, false, true).sensitive);
        assert!(!w::SettingEditPolicy::new(&values.audio_input_device_id, false, true).sensitive);
        assert!(w::SettingEditPolicy::new(&values.sink_id, true, true).sensitive);
        assert!(values.audio_muted.runtime_modifiable);
        assert!(crate::uisettings::Values::default().mute_when_in_background.runtime_modifiable);
        let volume = w::SettingEditPolicy::new(&values.volume, false, true);
        assert!(volume.sensitive);
        volume.apply(&mut values.volume, 42);
        assert_eq!(*values.volume.get_value(), 42);
        let mode = w::SettingEditPolicy::new(&values.sound_index, false, true);
        assert!(mode.sensitive);
        mode.apply(
            &mut values.sound_index,
            common::settings_enums::AudioMode::Surround,
        );
        assert_eq!(
            *values.sound_index.get_value(),
            common::settings_enums::AudioMode::Surround
        );
    }

    #[test]
    fn global_volume_apply_does_not_overwrite_active_custom_volume() {
        let mut values = common::settings::Values::default();
        values.volume.set_value(90);
        values.volume.set_global(false);
        values.volume.set_value(30);
        let policy = w::SettingEditPolicy::new(&values.volume, false, true);
        assert!(!policy.sensitive);
        policy.apply(&mut values.volume, 90);
        assert_eq!(*values.volume.get_value(), 30);
        assert_eq!(*values.volume.get_value_global(), 90);
    }

    #[test]
    fn auto_is_the_first_engine() {
        // Upstream's sink list always leads with "auto"; the combo's default
        // selection depends on it.
        assert_eq!(audio_engines()[0], AudioEngine::Auto);
    }

    #[test]
    fn engine_labels_round_trip_through_from_string() {
        for label in audio_engines()
            .into_iter()
            .map(|engine| engine.canonicalize().to_string())
        {
            assert!(
                AudioEngine::from_string(&label).is_some(),
                "engine label {label} is not parseable"
            );
        }
    }

    #[test]
    fn missing_saved_device_falls_back_to_auto() {
        let devices = vec!["auto".to_string(), "speakers".to_string()];
        assert_eq!(selected_device(&devices, "speakers"), 1);
        assert_eq!(selected_device(&devices, "removed device"), 0);
    }
}
