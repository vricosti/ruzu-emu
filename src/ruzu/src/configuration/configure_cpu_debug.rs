// SPDX-License-Identifier: GPL-3.0-or-later
//
// Rust/GTK4 counterpart of
// `/home/vricosti/Dev/emulators/zuyu/src/yuzu/configuration/configure_cpu_debug.cpp`
// (`ConfigureCpuDebug`), whose widget tree lives in `configure_cpu_debug.ui`.
//
// A single "Toggle CPU Optimizations" group: the "For debugging only." warning
// followed by one check box per `cpuopt_*` setting, then the
// "CPU settings are available only when game is not running." note.
//
// Upstream leaves every box ticked by default; unticking one takes effect only
// while "Enable CPU Debugging" is on (see the warning text).

use gtk::prelude::*;

use common::settings::Values;

use super::configure_dialog::Page;
use super::shared_widget as w;

/// Accessor for a plain `Setting<bool>` optimization flag.
type BoolField = fn(&mut Values) -> &mut common::settings_common::Setting<bool>;
/// Accessor for a per-game overridable optimization flag.
type SwitchableField = fn(&mut Values) -> &mut common::settings_common::SwitchableSetting<bool>;

/// The optimization toggles, in `configure_cpu_debug.ui` order.
const BOOL_OPTIONS: &[(&str, BoolField)] = &[
    ("Enable inline page tables", |v| &mut v.cpuopt_page_tables),
    ("Enable block linking", |v| &mut v.cpuopt_block_linking),
    ("Enable return stack buffer", |v| {
        &mut v.cpuopt_return_stack_buffer
    }),
    ("Enable fast dispatcher", |v| &mut v.cpuopt_fast_dispatcher),
    ("Enable context elimination", |v| {
        &mut v.cpuopt_context_elimination
    }),
    ("Enable constant propagation", |v| &mut v.cpuopt_const_prop),
    ("Enable miscellaneous optimizations", |v| {
        &mut v.cpuopt_misc_ir
    }),
    ("Enable misalignment check reduction", |v| {
        &mut v.cpuopt_reduce_misalign_checks
    }),
];

/// The two host-MMU toggles are `SwitchableSetting`s upstream, because they can
/// be overridden per game.
const SWITCHABLE_OPTIONS: &[(&str, SwitchableField)] = &[
    (
        "Enable Host MMU Emulation (general memory instructions)",
        |v| &mut v.cpuopt_fastmem,
    ),
    (
        "Enable Host MMU Emulation (exclusive memory instructions)",
        |v| &mut v.cpuopt_fastmem_exclusives,
    ),
];

/// Trailing plain-`Setting<bool>` rows, after the switchable pair.
const TRAILING_BOOL_OPTIONS: &[(&str, BoolField)] = &[
    (
        "Enable recompilation of exclusive memory instructions",
        |v| &mut v.cpuopt_recompile_exclusives,
    ),
    ("Enable fallbacks for invalid memory accesses", |v| {
        &mut v.cpuopt_ignore_memory_aborts
    }),
];

/// Build the CPU sub-tab of Debug — upstream `ConfigureCpuDebug`.
pub fn page(runtime_lock: bool) -> Page {
    let (scroller, column) = w::page();

    let (group, content) = w::group("Toggle CPU Optimizations");

    let warning = gtk::Label::new(Some(
        "For debugging only.\n\
         If you're not sure what these do, keep all of these enabled.\n\
         These settings, when disabled, only take effect when CPU Debugging is enabled.",
    ));
    warning.set_xalign(0.0);
    content.append(&warning);

    let mut bool_checks = Vec::new();
    let mut switchable_checks = Vec::new();

    for (label, field) in BOOL_OPTIONS {
        let active = {
            let mut values = common::settings::values_mut();
            *field(&mut values).get_value()
        };
        let check = w::check_row(label, active);
        check.set_sensitive(runtime_lock);
        content.append(&check);
        bool_checks.push((*field, check));
    }

    for (label, field) in SWITCHABLE_OPTIONS {
        let active = {
            let mut values = common::settings::values_mut();
            *field(&mut values).get_value()
        };
        let check = w::check_row(label, active);
        check.set_sensitive(runtime_lock);
        content.append(&check);
        switchable_checks.push((*field, check));
    }

    for (label, field) in TRAILING_BOOL_OPTIONS {
        let active = {
            let mut values = common::settings::values_mut();
            *field(&mut values).get_value()
        };
        let check = w::check_row(label, active);
        check.set_sensitive(runtime_lock);
        content.append(&check);
        bool_checks.push((*field, check));
    }

    column.append(&group);

    let note = gtk::Label::new(Some(
        "CPU settings are available only when game is not running.",
    ));
    note.set_xalign(0.0);
    note.set_margin_top(8);
    column.append(&note);

    Page::new("CPU", scroller, move || {
        let mut values = common::settings::values_mut();
        for (field, check) in &bool_checks {
            field(&mut values).set_value(check.is_active());
        }
        for (field, check) in &switchable_checks {
            field(&mut values).set_value(check.is_active());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_optimization_row_is_listed_once() {
        let mut labels: Vec<&str> = BOOL_OPTIONS
            .iter()
            .chain(TRAILING_BOOL_OPTIONS.iter())
            .map(|(label, _)| *label)
            .chain(SWITCHABLE_OPTIONS.iter().map(|(label, _)| *label))
            .collect();
        let count = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), count, "duplicate optimization row");
    }

    #[test]
    fn row_count_matches_upstream_ui() {
        // `configure_cpu_debug.ui` declares twelve check boxes.
        assert_eq!(
            BOOL_OPTIONS.len() + SWITCHABLE_OPTIONS.len() + TRAILING_BOOL_OPTIONS.len(),
            12
        );
    }

    #[test]
    fn each_accessor_targets_a_distinct_setting() {
        let mut values = Values::default();
        for (index, (_, field)) in BOOL_OPTIONS.iter().enumerate() {
            field(&mut values).set_value(index % 2 == 0);
        }
        for (index, (_, field)) in BOOL_OPTIONS.iter().enumerate() {
            assert_eq!(*field(&mut values).get_value(), index % 2 == 0);
        }
    }
}
