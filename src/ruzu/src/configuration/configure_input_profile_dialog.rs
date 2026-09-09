// SPDX-FileCopyrightText: Copyright 2020 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! GTK counterpart of `yuzu/configuration/configure_input_profile_dialog.{h,cpp,ui}`.
//!
//! Upstream builds a `ConfigureInputPlayer` with player index 9 — the scratch
//! `NpadIdType::Other` controller — and shows it inside a modal dialog whose
//! button box is wired to `accept()` only. Nothing is applied on OK: a profile
//! is written by the profile controls inside the player widget itself.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::ResponseType;

use super::configure_input_player::{self, InputProfileContext};

/// Upstream's `ConfigureInputPlayer` for the profile editor is constructed with
/// `player_index = 9`, which `Core::HID::HIDCore` maps to `NpadIdType::Other`.
const PROFILE_PLAYER_INDEX: usize = 9;

/// Height of upstream's `.ui` geometry (70x540). The nominal 70-pixel width is not
/// a constraint in Qt — the layout grows the dialog to the player widget's
/// `sizeHint` — so only the height carries over.
const DEFAULT_HEIGHT: i32 = 540;

/// `profile_context` is owned by the caller, mirroring upstream: the
/// `QtControllerSelectorDialog` holds a single `std::unique_ptr<InputProfiles>`
/// member and hands it to every `ConfigureInputProfileDialog` it opens.
pub fn present(
    source: &impl IsA<gtk::Widget>,
    input_subsystem: Rc<RefCell<input_common::InputSubsystem>>,
    hid_core: Arc<parking_lot::Mutex<hid_core::hid_core::HIDCore>>,
    profile_context: Rc<InputProfileContext>,
) {
    let dialog = gtk::Dialog::builder()
        .title(&crate::i18n::tr("Create Input Profile"))
        .modal(true)
        .build();
    if let Some(parent) = source.root().and_downcast::<gtk::Window>() {
        dialog.set_transient_for(Some(&parent));
    }

    let page = configure_input_player::page(
        PROFILE_PLAYER_INDEX,
        input_subsystem,
        hid_core,
        profile_context,
        None,
        false,
    );

    // Upstream's `ui->controllerLayout->addWidget(profile_widget)`. The Clear and
    // Defaults buttons that upstream adds to the dialog already live inside
    // ruzu's player page, so they are not duplicated here.
    dialog.content_area().append(&page.widget);

    // `configure_input_player::page` wraps the player controls in a `ScrolledWindow`
    // built with `propagate_natural_width(false)`, because `ConfigureDialog` gives it
    // a size through its own 1290x850 window. A bare dialog gives it nothing, so the
    // scroller collapses to its minimum and hides the controls. Qt has no equivalent
    // problem: `QScrollArea` reports the child's `sizeHint`, which is what grows the
    // upstream dialog past its nominal 70-pixel width. Ask for the same here.
    if let Some(scroller) = page.widget.downcast_ref::<gtk::ScrolledWindow>() {
        scroller.set_propagate_natural_width(true);
    }
    dialog.set_default_size(-1, DEFAULT_HEIGHT);

    dialog.add_button(&crate::i18n::tr("Cancel"), ResponseType::Cancel);
    dialog.add_button(&crate::i18n::tr("OK"), ResponseType::Accept);
    dialog.set_default_response(ResponseType::Accept);

    // Upstream connects `buttonBox` to `accept()` / `reject()` and nothing else:
    // neither response applies the mapping, so both simply close.
    //
    // `page` is moved into this closure on purpose. Its `apply` field holds the only
    // strong `Rc<PlayerPage>` — every other reference taken inside
    // `configure_input_player::page` is a `Weak` — so the closure's lifetime is what
    // keeps the page alive. Dropping it with the dialog runs `Drop for PlayerPage`,
    // which takes the scratch controller back out of configuration mode exactly as
    // `~ConfigureInputPlayer` does.
    dialog.connect_response(move |dialog, _| {
        let _own_page_until_the_dialog_dies = &page;
        dialog.close();
    });

    dialog.present();
}
