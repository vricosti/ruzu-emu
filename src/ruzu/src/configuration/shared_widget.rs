// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of the upstream row-building helpers in
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/shared_widget.cpp`
// (`ConfigurationShared::Widget` and its `CreateCheckBox` / `CreateCombobox` /
// `CreateLineEdit` / `CreateSlider` / `CreateSpinBox` builders).
//
// Upstream builds one `ConfigurationShared::Widget` per `Settings::BasicSetting`
// and lays it out as `[label] .......... [control]` inside a `QGroupBox`. This
// module provides the same row shapes in GTK4 so every configuration page can
// be written declaratively, keeping the visual result close to the Qt dialog.
//
// Divergence from upstream: upstream drives widget creation generically off the
// `BasicSetting` type + `Specialization`, because Qt needs runtime type erasure
// to walk the settings registry. The Rust port constructs rows explicitly at
// each call site instead — the concrete `Setting<T>` types are known statically,
// so the generic builder would add indirection without adding capability. The
// row *shapes* below are the same ones upstream produces.

use gtk::prelude::*;

/// Setting-specific edit rules from ConfigurationShared::Widget's constructor
/// and SetupComponent's global serializer. `runtime_lock` means no guest is
/// running, matching the upstream Builder argument (despite its name).
#[derive(Clone, Copy)]
pub struct SettingEditPolicy {
    pub sensitive: bool,
    allow_apply: bool,
}

impl SettingEditPolicy {
    /// Non-switchable Widget settings only exist in global configuration.
    pub fn for_setting<T: Clone>(
        setting: &common::settings_common::Setting<T>,
        runtime_lock: bool,
        configuring_global: bool,
    ) -> Self {
        let allowed = configuring_global && (runtime_lock || setting.runtime_modifiable);
        Self { sensitive: allowed, allow_apply: allowed }
    }

    pub fn apply_setting<T: Clone + PartialOrd>(
        self,
        setting: &mut common::settings_common::Setting<T>,
        value: T,
    ) {
        if self.allow_apply {
            setting.set_value(value);
        }
    }

    pub fn new<T: Clone>(
        setting: &common::settings_common::SwitchableSetting<T>,
        runtime_lock: bool,
        configuring_global: bool,
    ) -> Self {
        let runtime_allowed = runtime_lock || setting.setting.runtime_modifiable;
        Self {
            sensitive: runtime_allowed
                && (!configuring_global || runtime_lock || setting.using_global()),
            allow_apply: runtime_allowed && (!configuring_global || setting.using_global()),
        }
    }

    pub fn apply<T: Clone + PartialOrd>(
        self,
        setting: &mut common::settings_common::SwitchableSetting<T>,
        value: T,
    ) {
        if self.allow_apply {
            setting.set_value(value);
        }
    }
}

#[cfg(test)]
mod setting_edit_tests {
    use super::SettingEditPolicy;
    use common::settings_common::SwitchableSetting;
    use common::settings_enums::Category;

    #[test]
    fn non_switchable_settings_require_global_configuration() {
        let mut setting = common::settings_common::Setting::with_options(
            false, "synthetic", Category::UiGeneral,
            common::settings_common::Specialization::DEFAULT, true, true,
        );
        let custom = SettingEditPolicy::for_setting(&setting, true, false);
        assert!(!custom.sensitive);
        custom.apply_setting(&mut setting, true);
        assert!(!*setting.get_value());
        let global = SettingEditPolicy::for_setting(&setting, false, true);
        assert!(global.sensitive);
        global.apply_setting(&mut setting, true);
        assert!(*setting.get_value());
    }

    #[test]
    fn startup_only_setting_rejects_runtime_writes() {
        let mut setting = SwitchableSetting::new(false, "synthetic", Category::RendererAdvanced);
        let policy = SettingEditPolicy::new(&setting, false, true);
        assert!(!policy.sensitive);
        policy.apply(&mut setting, true);
        assert!(!*setting.get_value());
        let policy = SettingEditPolicy::new(&setting, true, true);
        assert!(policy.sensitive);
        policy.apply(&mut setting, true);
        assert!(*setting.get_value());
    }

    #[test]
    fn runtime_setting_preserves_global_and_custom_ownership() {
        let mut setting = SwitchableSetting::new(false, "synthetic", Category::RendererAdvanced);
        setting.setting.runtime_modifiable = true;
        let global = SettingEditPolicy::new(&setting, false, true);
        assert!(global.sensitive);
        global.apply(&mut setting, true);
        setting.set_global(false);
        let global = SettingEditPolicy::new(&setting, false, true);
        assert!(!global.sensitive);
        global.apply(&mut setting, true);
        assert!(!*setting.get_value());
        let custom = SettingEditPolicy::new(&setting, false, false);
        assert!(custom.sensitive);
        custom.apply(&mut setting, true);
        assert!(*setting.get_value());
        assert!(*setting.get_value_global());
    }
}

/// Left-column width for row labels, in characters. Qt sizes the label column
/// to the widest label in the group; GTK's size groups are per-page, so a fixed
/// request keeps the controls aligned across groups like the Qt dialog does.
const LABEL_COLUMN_CHARS: i32 = 30;

/// Build a titled group — upstream `QGroupBox`.
///
/// Returns `(outer, content)`: append the group's `outer` to the page, and each
/// row to `content`. The title sits above a framed box, matching the flat
/// group style the Qt dialog uses (see the "General" / "Linux" groups in
/// `configure_general.ui`).
pub fn group(title: &str) -> (gtk::Box, gtk::Box) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    outer.set_margin_bottom(8);

    if !title.is_empty() {
        let label = gtk::Label::new(Some(&crate::i18n::tr(title)));
        label.set_xalign(0.0);
        label.set_margin_bottom(2);
        outer.append(&label);
    }

    let frame = gtk::Frame::new(None);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(10);
    content.set_margin_end(10);
    frame.set_child(Some(&content));
    outer.append(&frame);

    (outer, content)
}

/// A page scaffold: a vertically scrolling column of groups.
///
/// Upstream pages are `QWidget`s with a vertical layout plus a trailing
/// `QSpacerItem` that pushes the groups to the top; the `valign: Start` on the
/// column here has the same effect.
pub fn page() -> (gtk::ScrolledWindow, gtk::Box) {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.set_margin_top(10);
    column.set_margin_bottom(10);
    column.set_margin_start(10);
    column.set_margin_end(10);
    column.set_valign(gtk::Align::Start);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .hexpand(true)
        .vexpand(true)
        .child(&column)
        .build();

    (scroller, column)
}

/// `[label] .......... [control]` row — upstream's label + widget pairing.
///
/// The control is right-aligned and takes the remaining width, matching the Qt
/// dialog where combo boxes and line edits fill the right half of the row.
pub fn labeled_row(label: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);

    let name = gtk::Label::new(Some(&crate::i18n::tr(label)));
    name.set_xalign(0.0);
    name.set_width_chars(LABEL_COLUMN_CHARS);
    name.set_max_width_chars(LABEL_COLUMN_CHARS);
    row.append(&name);

    let control = control.as_ref();
    control.set_hexpand(true);
    row.append(control);

    row
}

/// Combo box row — upstream `ConfigurationShared::Widget::CreateCombobox`.
///
/// `active` is the index of the initially selected entry.
pub fn combo_row(label: &str, items: &[&str], active: u32) -> (gtk::Box, gtk::DropDown) {
    let dropdown = combo(items, active);
    (labeled_row(label, &dropdown), dropdown)
}

/// Bare combo box, for rows that need custom placement.
pub fn combo(items: &[&str], active: u32) -> gtk::DropDown {
    let translated: Vec<String> = items.iter().map(|item| crate::i18n::tr(item)).collect();
    let translated_refs: Vec<&str> = translated.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&translated_refs);
    let dropdown = gtk::DropDown::new(Some(model), gtk::Expression::NONE);
    // Guard against an out-of-range stored setting selecting nothing at all;
    // upstream's `setCurrentIndex` silently clamps the same way.
    if (active as usize) < items.len() {
        dropdown.set_selected(active);
    }
    dropdown
}

/// Check box row — upstream `ConfigurationShared::Widget::CreateCheckBox`.
///
/// Upstream check boxes span the full row width with the label to the right of
/// the box, rather than using the `[label] [control]` split.
pub fn check_row(label: &str, active: bool) -> gtk::CheckButton {
    let check = gtk::CheckButton::with_label(&crate::i18n::tr(label));
    check.set_active(active);
    check
}

/// Text entry row — upstream `ConfigurationShared::Widget::CreateLineEdit`.
pub fn entry_row(label: &str, text: &str) -> (gtk::Box, gtk::Entry) {
    let entry = gtk::Entry::new();
    entry.set_text(text);
    (labeled_row(label, &entry), entry)
}

/// Spin-button row — upstream `ConfigurationShared::Widget::CreateSpinBox`.
///
/// `suffix` mirrors the Qt spin box suffix (e.g. `"%"` for the speed limit).
pub fn spin_row(
    label: &str,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    suffix: &str,
) -> (gtk::Box, gtk::SpinButton) {
    let spin = gtk::SpinButton::with_range(min, max, step);
    spin.set_value(value);
    if suffix.is_empty() {
        return (labeled_row(label, &spin), spin);
    }
    let control = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    control.append(&spin);
    control.append(&gtk::Label::new(Some(suffix)));
    (labeled_row(label, &control), spin)
}

/// Slider row with a trailing percentage readout — upstream
/// `ConfigurationShared::Widget::CreateSlider` with `Specialization::Percentage`.
///
/// Returns the row, the scale, and the readout label so callers can keep the
/// two in sync (upstream connects `valueChanged` to update its label likewise).
pub fn percent_slider_row(
    label: &str,
    value: f64,
    min: f64,
    max: f64,
) -> (gtk::Box, gtk::Scale, gtk::Label) {
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, 1.0);
    scale.set_value(value);
    scale.set_draw_value(false);
    scale.set_hexpand(true);

    let readout = gtk::Label::new(Some(&format!("{}%", value as i64)));
    readout.set_width_chars(5);
    readout.set_xalign(1.0);

    // Keep the readout in sync, the same way upstream's slider updates its
    // companion label on `valueChanged`.
    let readout_clone = readout.clone();
    scale.connect_value_changed(move |s| {
        readout_clone.set_text(&format!("{}%", s.value() as i64));
    });

    let holder = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    holder.append(&scale);
    holder.append(&readout);

    (labeled_row(label, &holder), scale, readout)
}

/// Reversed integral slider with a scaled percentage readout — upstream
/// `ConfigurationShared::Widget::CreateSlider` for
/// `RequestType::ReverseSlider`.
///
/// The slider keeps the setting's raw value; only its appearance and feedback
/// are reversed. This matters for FSR, whose raw `25` is presented as
/// `(200 - 25) * 0.5 = 88%` and must still serialize back as `25`.
pub fn reversed_percent_slider_row(
    label: &str,
    value: f64,
    min: f64,
    max: f64,
    multiplier: f64,
) -> (gtk::Box, gtk::Scale, gtk::Label) {
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, 1.0);
    scale.set_value(value);
    scale.set_draw_value(false);
    scale.set_inverted(true);
    scale.set_hexpand(true);

    let readout = gtk::Label::new(Some(&format!(
        "{}%",
        reversed_slider_feedback(value, max, multiplier)
    )));
    readout.set_width_chars(5);
    readout.set_xalign(1.0);

    let readout_clone = readout.clone();
    scale.connect_value_changed(move |scale| {
        readout_clone.set_text(&format!(
            "{}%",
            reversed_slider_feedback(scale.value(), max, multiplier)
        ));
    });

    let holder = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    holder.append(&scale);
    holder.append(&readout);

    (labeled_row(label, &holder), scale, readout)
}

fn reversed_slider_feedback(value: f64, max: f64, multiplier: f64) -> i64 {
    ((max - value) * multiplier + 0.5) as i64
}

/// Path row: an entry plus a `...` browse button — upstream's directory pickers
/// in `configure_filesystem.ui` / `configure_ui.ui`.
pub fn path_row(label: &str, text: &str) -> (gtk::Box, gtk::Entry, gtk::Button) {
    let entry = gtk::Entry::new();
    entry.set_text(text);
    entry.set_hexpand(true);

    let browse = gtk::Button::with_label("...");

    let holder = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    holder.append(&entry);
    holder.append(&browse);

    (labeled_row(label, &holder), entry, browse)
}

/// Give every row the same label-column width, so their controls line up.
///
/// Rows whose label column is a plain [`gtk::Label`] size themselves from
/// [`LABEL_COLUMN_CHARS`], while rows that lead with a check box size to the
/// check box's own text — so the two kinds drift apart and their controls start
/// at different x positions. Qt avoids this because its pages use a
/// `QFormLayout` / grid whose first column is shared; a `SizeGroup` is the GTK
/// equivalent.
///
/// Pass every row of a page, including rows in different groups: upstream
/// aligns the whole page, not each group separately.
pub fn align_label_columns(rows: &[&gtk::Box]) -> gtk::SizeGroup {
    let group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    for row in rows {
        if let Some(label_column) = row.first_child() {
            group.add_widget(&label_column);
        }
    }
    group
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversed_slider_feedback_matches_edens_fsr_presentation() {
        assert_eq!(reversed_slider_feedback(25.0, 200.0, 0.5), 88);
        assert_eq!(reversed_slider_feedback(0.0, 200.0, 0.5), 100);
        assert_eq!(reversed_slider_feedback(200.0, 200.0, 0.5), 0);
    }
}
