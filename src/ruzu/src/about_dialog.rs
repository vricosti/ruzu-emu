// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK counterpart of yuzu/about_dialog.h, yuzu/about_dialog.cpp, and
// yuzu/aboutdialog.ui.

use gtk::prelude::*;

const PROJECT_URL: &str = "https://github.com/vricosti/ruzu-emu";
const CONTRIBUTORS_URL: &str = "https://github.com/vricosti/ruzu-emu/graphs/contributors";
const LICENSE_URL: &str = "https://github.com/vricosti/ruzu-emu/blob/main/LICENSE";
const RUSTY_LEMON_ICON: &[u8] = include_bytes!("../assets/ruzu-rusty-lemon.png");
const ABOUT_DESCRIPTION: &str =
    "Ruzu is an experimental open-source emulator for the Nintendo Switch, created by porting yuzu to Rust through AI agents.";
const LEGAL_NOTICE: &str =
    "This software should not be used to play games you have not legally obtained.";
const TRADEMARK_NOTICE: &str =
    "Nintendo Switch is a trademark of Nintendo. ruzu is not affiliated with Nintendo in any way.";

fn build_info() -> String {
    format!(
        "{} | {} | {}",
        common::scm_rev::BUILD_NAME,
        common::scm_rev::BUILD_VERSION,
        common::scm_rev::COMPILER_ID
    )
}

fn links_markup() -> String {
    [
        ("Website", PROJECT_URL),
        ("Source Code", PROJECT_URL),
        ("Contributors", CONTRIBUTORS_URL),
        ("License", LICENSE_URL),
    ]
    .into_iter()
    .map(|(label, url)| {
        let label = gtk::glib::markup_escape_text(&crate::i18n::tr(label));
        format!("<a href=\"{url}\">{label}</a>")
    })
    .collect::<Vec<_>>()
    .join(" | ")
}

pub struct AboutDialog {
    dialog: gtk::Dialog,
}

impl AboutDialog {
    /// Mirrors upstream `AboutDialog::AboutDialog` with the same dedicated
    /// logo/content/links layout, implemented with GTK widgets.
    pub fn new(parent: &impl IsA<gtk::Window>) -> Self {
        let dialog = gtk::Dialog::builder()
            .transient_for(parent)
            .modal(true)
            .title(crate::i18n::tr("About ruzu"))
            .default_width(700)
            .default_height(385)
            .resizable(false)
            .build();

        let content = dialog.content_area();
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_spacing(12);

        let body = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        body.set_vexpand(true);

        let logo = gtk::Picture::new();
        logo.set_size_request(200, 200);
        logo.set_halign(gtk::Align::Start);
        logo.set_valign(gtk::Align::Start);
        logo.set_can_shrink(true);
        logo.set_keep_aspect_ratio(true);
        let icon_bytes = gtk::glib::Bytes::from_static(RUSTY_LEMON_ICON);
        if let Ok(texture) = gtk::gdk::Texture::from_bytes(&icon_bytes) {
            logo.set_paintable(Some(&texture));
        }
        body.append(&logo);

        let details = gtk::Box::new(gtk::Orientation::Vertical, 10);
        details.set_hexpand(true);

        let name = gtk::Label::new(None);
        name.set_markup("<span size=\"xx-large\">Ruzu</span>");
        name.set_halign(gtk::Align::Start);
        details.append(&name);

        let build = gtk::Label::new(Some(&build_info()));
        build.set_halign(gtk::Align::Start);
        build.set_selectable(true);
        build.set_wrap(true);
        details.append(&build);

        let description = gtk::Label::new(Some(&format!(
            "{}\n\n{}",
            crate::i18n::tr(ABOUT_DESCRIPTION),
            crate::i18n::tr(LEGAL_NOTICE)
        )));
        description.set_halign(gtk::Align::Start);
        description.set_valign(gtk::Align::Start);
        description.set_wrap(true);
        description.set_xalign(0.0);
        description.set_vexpand(true);
        details.append(&description);

        let links = gtk::Label::new(None);
        links.set_markup(&links_markup());
        links.set_halign(gtk::Align::Start);
        links.set_wrap(true);
        let link_parent = dialog.clone();
        links.connect_activate_link(move |_, uri| {
            if let Err(error) = crate::gtk_compat::open_external_uri(uri) {
                log::error!("Failed to open About link {uri}: {error}");
                let detail =
                    crate::i18n::tr_args("Unable to open the URL \"%1\".", &[uri.to_owned()]);
                crate::gtk_compat::show_warning(Some(&link_parent), "Error opening URL", &detail);
            }
            gtk::glib::Propagation::Stop
        });
        details.append(&links);

        let liability = gtk::Label::new(None);
        let notice = gtk::glib::markup_escape_text(&crate::i18n::tr(TRADEMARK_NOTICE));
        liability.set_markup(&format!("<span size=\"small\">{notice}</span>"));
        liability.set_halign(gtk::Align::Start);
        liability.set_wrap(true);
        liability.set_xalign(0.0);
        details.append(&liability);

        body.append(&details);
        content.append(&body);

        dialog.add_button(&crate::i18n::tr("OK"), gtk::ResponseType::Ok);
        dialog.connect_response(|dialog, _| dialog.close());

        Self { dialog }
    }

    pub fn present(&self) {
        self.dialog.present();
        crate::gtk_compat::focus_dialog_response(&self.dialog, gtk::ResponseType::Ok);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_links_are_github_owned_and_exclude_social_networks() {
        for url in [PROJECT_URL, CONTRIBUTORS_URL, LICENSE_URL] {
            assert!(url.starts_with("https://github.com/vricosti/ruzu-emu"));
        }

        let markup = links_markup();
        assert_eq!(markup.matches("<a href=").count(), 4);
        for excluded in ["discord", "stoat", "stt.gg", "twitter", "nitter"] {
            assert!(!markup.to_ascii_lowercase().contains(excluded));
        }
    }

    #[test]
    fn project_metadata_is_ruzu_owned() {
        assert!(ABOUT_DESCRIPTION.starts_with("Ruzu is an experimental"));
        assert!(ABOUT_DESCRIPTION.contains("porting yuzu to Rust through AI agents"));
        assert!(build_info().contains(common::scm_rev::COMPILER_ID));
        assert!(RUSTY_LEMON_ICON.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
