// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK counterpart of yuzu's QTranslator ownership in GMainWindow. The Qt
// frontend is excluded from structural porting, so ruzu keeps the equivalent
// locale selection and widget translation in its GTK frontend.

use std::cell::Cell;
#[cfg(test)]
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
#[cfg(not(test))]
use std::sync::RwLock;

use gtk::prelude::*;

struct Catalogs {
    translations: HashMap<String, HashMap<String, String>>,
    sources: HashMap<String, String>,
}

fn catalogs() -> &'static Catalogs {
    static CATALOGS: OnceLock<Catalogs> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        let mut translations: HashMap<String, HashMap<String, String>> =
            serde_json::from_str(include_str!("../i18n/catalogs.json"))
                .expect("embedded interface translation catalogs are valid JSON");
        let migration_translations: HashMap<String, HashMap<String, String>> =
            serde_json::from_str(include_str!("../i18n/migration_catalogs.json"))
                .expect("embedded migration translation catalogs are valid JSON");
        let menu_translations: HashMap<String, HashMap<String, String>> =
            serde_json::from_str(include_str!("../i18n/menu_catalogs.json"))
                .expect("embedded menu translation catalogs are valid JSON");
        let overlay_translations: HashMap<String, HashMap<String, String>> =
            serde_json::from_str(include_str!("../i18n/overlay_catalogs.json"))
                .expect("embedded overlay translation catalogs are valid JSON");
        for extra_catalog in [
            migration_translations,
            menu_translations,
            overlay_translations,
        ] {
            for (locale, messages) in extra_catalog {
                translations.entry(locale).or_default().extend(messages);
            }
        }
        let mut sources = HashMap::new();
        for messages in translations.values() {
            for (source, translated) in messages {
                sources
                    .entry(translated.clone())
                    .and_modify(|existing: &mut String| {
                        // Prefer the plain source when menu and dialog text
                        // intentionally share a translation. This prevents a
                        // translated dialog title from becoming "&Title"
                        // when switching back to English.
                        if existing.starts_with('&') && !source.starts_with('&') {
                            existing.clone_from(source);
                        }
                    })
                    .or_insert_with(|| source.clone());
            }
        }
        Catalogs {
            translations,
            sources,
        }
    })
}

fn catalog_translation(locale: &str, source: &str) -> Option<&'static str> {
    catalogs()
        .translations
        .get(locale)?
        .get(source)
        .map(String::as_str)
}

fn catalog_source(translated: &str) -> Option<&'static str> {
    catalogs().sources.get(translated).map(String::as_str)
}

/// Locale code and native display name, in upstream's `<System>`, English,
/// translated-catalog order.
pub const AVAILABLE_LANGUAGES: &[(&str, &str)] = &[
    ("", "<System>"),
    ("en", "English"),
    ("ar", "العربية"),
    ("ca", "Català"),
    ("cs", "Čeština"),
    ("da", "Dansk"),
    ("de", "Deutsch"),
    ("el", "Ελληνικά"),
    ("es", "Español"),
    ("fi", "Suomi"),
    ("fr", "Français (France)"),
    ("hu", "Magyar"),
    ("id", "Bahasa Indonesia"),
    ("it", "Italiano"),
    ("ja_JP", "日本語"),
    ("ko_KR", "한국어"),
    ("nb", "Norsk bokmål"),
    ("nl", "Nederlands"),
    ("pl", "Polski"),
    ("pt_BR", "Português (Brasil)"),
    ("pt_PT", "Português (Portugal)"),
    ("ru_RU", "Русский"),
    ("sv", "Svenska"),
    ("tr_TR", "Türkçe"),
    ("uk", "Українська"),
    ("vi", "Tiếng Việt"),
    ("vi_VN", "Tiếng Việt (Việt Nam)"),
    ("zh_CN", "简体中文"),
    ("zh_TW", "繁體中文"),
];

#[cfg(not(test))]
static CONFIGURED_LANGUAGE: OnceLock<RwLock<String>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static CONFIGURED_LANGUAGE: RefCell<String> = RefCell::new("en".to_string());
}

thread_local! {
    static RETRANSLATING: Cell<bool> = const { Cell::new(false) };
}

struct RetranslationGuard;

impl Drop for RetranslationGuard {
    fn drop(&mut self) {
        RETRANSLATING.set(false);
    }
}

#[cfg(not(test))]
fn configured_language() -> &'static RwLock<String> {
    // Tests and non-GTK helpers are deterministic until the frontend applies
    // its stored locale. `main` explicitly sets `""` when System is selected.
    CONFIGURED_LANGUAGE.get_or_init(|| RwLock::new("en".to_string()))
}

pub fn set_language(locale: &str) {
    #[cfg(not(test))]
    {
        *configured_language().write().unwrap() = locale.to_string();
    }
    #[cfg(test)]
    CONFIGURED_LANGUAGE.with(|language| *language.borrow_mut() = locale.to_string());
}

fn toolkit_language_override(locale: &str) -> Option<String> {
    (!locale.is_empty()).then(|| resolve_catalog_locale(locale))
}

/// Make GTK's own gettext strings follow the explicitly selected interface
/// language. This must run before GTK is initialized; otherwise native widget
/// text such as `About`, `Credits`, and `Created by` remains cached in the
/// system language. An empty locale deliberately preserves `<System>`.
pub fn configure_toolkit_language(locale: &str) {
    if let Some(locale) = toolkit_language_override(locale) {
        std::env::set_var("LANGUAGE", locale);
    }
}

pub fn language() -> String {
    #[cfg(not(test))]
    {
        configured_language().read().unwrap().clone()
    }
    #[cfg(test)]
    {
        CONFIGURED_LANGUAGE.with(|language| language.borrow().clone())
    }
}

pub fn is_retranslating() -> bool {
    RETRANSLATING.get()
}

fn effective_language() -> String {
    let configured = language();
    if !configured.is_empty() {
        return resolve_catalog_locale(&configured);
    }

    let environment_locale = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .map(|value| resolve_catalog_locale(&value));
    environment_locale
        .or_else(|| operating_system_locale().map(|locale| resolve_catalog_locale(&locale)))
        .unwrap_or_else(|| "en".to_string())
}

/// Return the desktop locale when the user selected `<System>`. Qt performs
/// this lookup internally; Ruzu's catalog is independent of Qt/gettext, so it
/// must explicitly query Windows when the POSIX locale variables are absent.
#[cfg(target_os = "windows")]
fn operating_system_locale() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

    // Win32's `LOCALE_NAME_MAX_LENGTH` is 85 UTF-16 code units including the
    // terminator, but windows-sys 0.59 does not export the SDK macro.
    const LOCALE_NAME_CAPACITY: usize = 85;
    let mut locale = [0u16; LOCALE_NAME_CAPACITY];
    let length = unsafe { GetUserDefaultLocaleName(locale.as_mut_ptr(), locale.len() as i32) };
    (length > 1)
        .then(|| String::from_utf16(&locale[..length as usize - 1]).ok())
        .flatten()
}

#[cfg(not(target_os = "windows"))]
fn operating_system_locale() -> Option<String> {
    gtk::glib::language_names()
        .into_iter()
        .map(|locale| locale.to_string())
        .find(|locale| !locale.is_empty() && locale != "C")
}

fn resolve_catalog_locale(locale: &str) -> String {
    let normalized = locale
        .split([':', '.', '@'])
        .next()
        .unwrap_or(locale)
        .replace('-', "_");
    if let Some((code, _)) = AVAILABLE_LANGUAGES
        .iter()
        .find(|(code, _)| !code.is_empty() && code.eq_ignore_ascii_case(&normalized))
    {
        return (*code).to_string();
    }
    let language = normalized.split('_').next().unwrap_or("en");
    AVAILABLE_LANGUAGES
        .iter()
        .find(|(code, _)| *code == language)
        .or_else(|| {
            AVAILABLE_LANGUAGES
                .iter()
                .find(|(code, _)| code.starts_with(&format!("{language}_")))
        })
        .map(|(code, _)| (*code).to_string())
        .unwrap_or_else(|| "en".to_string())
}

fn replace_brand_outside_urls(
    text: &str,
    lower_from: &str,
    lower_to: &str,
    title_from: &str,
    title_to: &str,
) -> String {
    let mut output = String::with_capacity(text.len());
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.starts_with("https://") || remaining.starts_with("http://") {
            let url_end = remaining
                .char_indices()
                .find_map(|(index, character)| {
                    (index > 0
                        && (character.is_whitespace()
                            || matches!(character, '\'' | '"' | '<' | '>')))
                    .then_some(index)
                })
                .unwrap_or(remaining.len());
            output.push_str(&remaining[..url_end]);
            remaining = &remaining[url_end..];
        } else if let Some(rest) = remaining.strip_prefix(title_from) {
            output.push_str(title_to);
            remaining = rest;
        } else if let Some(rest) = remaining.strip_prefix(lower_from) {
            output.push_str(lower_to);
            remaining = rest;
        } else {
            let character = remaining.chars().next().unwrap();
            output.push(character);
            remaining = &remaining[character.len_utf8()..];
        }
    }
    output
}

fn normalize_for_catalog(text: &str) -> (String, bool) {
    if catalog_source(text).is_some()
        || AVAILABLE_LANGUAGES
            .iter()
            .any(|(locale, _)| catalog_translation(locale, text).is_some())
    {
        return (text.to_string(), false);
    }

    let branded = replace_brand_outside_urls(text, "ruzu", "yuzu", "Ruzu", "Yuzu");
    if catalog_source(&branded).is_some()
        || AVAILABLE_LANGUAGES
            .iter()
            .any(|(locale, _)| catalog_translation(locale, &branded).is_some())
    {
        return (branded, false);
    }

    let mnemonic = branded.replace('_', "&");
    let converted = mnemonic != branded
        && (catalog_source(&mnemonic).is_some()
            || AVAILABLE_LANGUAGES
                .iter()
                .any(|(locale, _)| catalog_translation(locale, &mnemonic).is_some()));
    if converted {
        (mnemonic, true)
    } else {
        (branded, false)
    }
}

/// Translate an English frontend string using the selected UI locale. Input
/// may already be translated, which lets an open window switch languages in
/// either direction without rebuilding every widget.
pub fn tr(text: &str) -> String {
    let (normalized, mnemonic) = normalize_for_catalog(text);
    let source = catalog_source(&normalized).unwrap_or(&normalized);
    let locale = effective_language();
    let translated = catalog_translation(&locale, source).unwrap_or(source);
    let translated = if mnemonic {
        translated.replace('&', "_")
    } else {
        translated.to_string()
    };
    replace_brand_outside_urls(&translated, "yuzu", "ruzu", "Yuzu", "Ruzu")
}

/// Translate a Qt-style `%1`, `%2`, ... template and substitute its values.
pub fn tr_args(source: &str, arguments: &[String]) -> String {
    let mut translated = tr(source);
    for (index, value) in arguments.iter().enumerate() {
        translated = translated.replace(&format!("%{}", index + 1), value);
    }
    translated
}

/// Translate every textual GTK property below `root`. GTK stores button and
/// check-button captions as label children, so the recursive label pass also
/// covers those controls.
pub fn translate_widget_tree(root: &impl IsA<gtk::Widget>) {
    if RETRANSLATING.replace(true) {
        return;
    }
    let _guard = RetranslationGuard;
    translate_widget(root.as_ref());
}

fn translate_widget(widget: &gtk::Widget) {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.set_label(&tr(label.label().as_str()));
    }
    if let Some(window) = widget.downcast_ref::<gtk::Window>() {
        if let Some(title) = window.title() {
            window.set_title(Some(&tr(title.as_str())));
        }
    }
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        if let Some(placeholder) = entry.placeholder_text() {
            entry.set_placeholder_text(Some(&tr(placeholder.as_str())));
        }
    }
    if let Some(dropdown) = widget.downcast_ref::<gtk::DropDown>() {
        if let Some(strings) = dropdown.model().and_downcast::<gtk::StringList>() {
            let selected = dropdown.selected();
            let translated: Vec<String> = (0..strings.n_items())
                .filter_map(|index| strings.string(index))
                .map(|value| tr(value.as_str()))
                .collect();
            let translated_refs: Vec<&str> = translated.iter().map(String::as_str).collect();
            strings.splice(0, strings.n_items(), &translated_refs);
            dropdown.set_selected(selected);
        }
    }
    if let Some(tooltip) = widget.tooltip_text() {
        widget.set_tooltip_text(Some(&tr(tooltip.as_str())));
    }

    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        translate_widget(&current);
    }
}

/// Translate `<attribute translatable="yes">` text before GtkBuilder parses
/// the menu model. This is the GTK equivalent of QTranslator translating the
/// actions declared by upstream's `main.ui`.
pub fn translate_builder_xml(xml: &str) -> String {
    const PREFIX: &str = "translatable=\"yes\">";
    const SUFFIX: &str = "</attribute>";

    let mut output = String::with_capacity(xml.len());
    let mut remaining = xml;
    while let Some(prefix_pos) = remaining.find(PREFIX) {
        let value_start = prefix_pos + PREFIX.len();
        let Some(relative_end) = remaining[value_start..].find(SUFFIX) else {
            break;
        };
        let value_end = value_start + relative_end;
        output.push_str(&remaining[..value_start]);
        output.push_str(&escape_xml_text(&tr(&remaining[value_start..value_end])));
        remaining = &remaining[value_end..];
    }
    output.push_str(remaining);
    output
}

fn escape_xml_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap()
    }

    #[test]
    fn translations_handle_plain_mnemonic_brand_and_locale_switches() {
        let _guard = test_lock();
        set_language("fr");
        assert_eq!(tr("Cancel"), "Annuler");
        assert_eq!(tr("Add Game Directory"), "Ajouter un répertoire de jeux");
        assert_eq!(tr("Favorites"), "Favoris");
        let quickstart = tr(
            "Encryption keys are missing. <br>Please follow <a href='https://yuzu-mirror.github.io/help/quickstart/'>the ruzu quickstart guide</a> to install your keys and firmware, then add your games.",
        );
        assert!(quickstart.contains("https://yuzu-mirror.github.io/help/quickstart/"));
        assert!(quickstart.contains("guide de démarrage rapide ruzu"));
        assert_eq!(tr("_File"), "_Fichier");
        assert_eq!(tr("About ruzu"), "À propos de ruzu");
        set_language("de");
        assert_eq!(tr("Annuler"), "Abbrechen");
        set_language("en");
        assert_eq!(tr("Annuler"), "Cancel");
    }

    #[test]
    fn brand_normalization_does_not_rewrite_urls() {
        assert_eq!(
            replace_brand_outside_urls(
                "Yuzu: https://yuzu-mirror.github.io/help/quickstart/",
                "yuzu",
                "ruzu",
                "Yuzu",
                "Ruzu"
            ),
            "Ruzu: https://yuzu-mirror.github.io/help/quickstart/"
        );
    }

    #[test]
    fn migration_tool_messages_exist_in_every_supported_catalog() {
        let _guard = test_lock();
        for &(locale, _) in AVAILABLE_LANGUAGES {
            if locale.is_empty() || locale == "en" {
                continue;
            }
            set_language(locale);
            assert_ne!(tr("_Migration Tool"), "_Migration Tool", "{locale}");
            assert_ne!(
                tr("Copy from (recommended)"),
                "Copy from (recommended)",
                "{locale}"
            );
            assert_ne!(
                tr("Share with (symbolic link / junction point)"),
                "Share with (symbolic link / junction point)",
                "{locale}"
            );
            assert_ne!(
                tr("The existing Ruzu copy of the selected data will be deleted and replaced with a symbolic link (or a junction point on Windows)."),
                "The existing Ruzu copy of the selected data will be deleted and replaced with a symbolic link (or a junction point on Windows).",
                "{locale}"
            );
            assert_ne!(
                tr("The existing symbolic link (or junction point on Windows) will be deleted and the selected source data will be copied into Ruzu."),
                "The existing symbolic link (or junction point on Windows) will be deleted and the selected source data will be copied into Ruzu.",
                "{locale}"
            );
            assert_ne!(
                tr("No compatible source emulator data was found."),
                "No compatible source emulator data was found.",
                "{locale}"
            );
        }
        set_language("fr");
        let french_title = tr("Migration Tool");
        set_language("en");
        assert_eq!(tr(&french_title), "Migration Tool");
    }

    #[test]
    fn french_startup_dialogs_are_fully_translated() {
        let _guard = test_lock();
        set_language("fr");
        for source in [
            "User Data Migration",
            "Ruzu found data from another emulator. Choose the source, the data, and how Ruzu should use it.",
            "Source data is never moved or deleted. Copy is the recommended default; shared links make both emulators use the same directories. Shader caches are never migrated.",
            "Source emulator",
            "Migration method",
            "Copy from (recommended)",
            "Share with (symbolic link / junction point)",
            "No migration",
            "Firmware",
            "Keys",
            "Game directories",
            "Import only the configured game folder paths. Game files are not copied or shared.",
            "Firmware, keys, and configured game directories are offered. Game files, save data, settings, updates, DLC, SD card data, mods, and shader caches remain unchanged.",
            "System",
            "Next",
            "Force X11 as Graphics Backend",
            "External Content",
            "Add directories to scan for DLCs and Updates without installing to NAND",
            "Add Directory",
            "Remove Selected",
            "Select External Content Directory...",
            "Directory Already Added",
            "This directory is already in the list.",
            "From Folder",
            "From ZIP",
            "Select Dumped Firmware ZIP",
            "Zipped Archives (*.zip)",
            "Firmware extraction failed",
            "Firmware cleanup failed",
            "Decryption keys are missing. Install them now?",
            "No",
            "Yes",
            "Wayland Detected!",
            "Wayland is known to have significant performance issues and mysterious bugs.\nIt's recommended to use X11 instead.\n\nWould you like to force it for future launches?",
            "Use X11",
            "Continue with Wayland",
            "Don't show again",
            "Restart Required",
            "Restart Ruzu to apply the X11 backend.",
        ] {
            assert_ne!(tr(source), source, "missing French translation: {source}");
        }
    }

    #[test]
    fn multiplayer_menu_uses_edens_french_translations() {
        let _guard = test_lock();
        set_language("fr");
        assert_eq!(tr("_Multiplayer"), "_Multijoueur");
        assert_eq!(
            tr("_Browse Public Game Lobby"),
            "_Parcourir le menu des jeux publics"
        );
        assert_eq!(tr("_Direct Connect to Room"), "_Connexion directe au salon");
        assert_eq!(tr("_Show Current Room"), "_Afficher le salon actuel");
        assert_eq!(tr("_Leave Room"), "_Quitter le salon");
    }

    #[test]
    fn system_locale_uses_language_environment_prefix() {
        let _guard = test_lock();
        let old_language = std::env::var_os("LANGUAGE");
        set_language("");
        std::env::set_var("LANGUAGE", "fr_FR:en");
        assert_eq!(effective_language(), "fr");
        match old_language {
            Some(value) => std::env::set_var("LANGUAGE", value),
            None => std::env::remove_var("LANGUAGE"),
        }
    }

    #[test]
    fn os_locale_resolves_to_a_supported_catalog_or_english() {
        if let Some(locale) = operating_system_locale() {
            let resolved = resolve_catalog_locale(&locale);
            assert!(AVAILABLE_LANGUAGES
                .iter()
                .any(|(candidate, _)| *candidate == resolved));
        }
        assert_eq!(resolve_catalog_locale("xx-Unsupported"), "en");
    }

    #[test]
    fn explicit_language_overrides_the_gtk_gettext_locale() {
        assert_eq!(toolkit_language_override("en").as_deref(), Some("en"));
        assert_eq!(toolkit_language_override("fr_FR").as_deref(), Some("fr"));
        assert_eq!(toolkit_language_override(""), None);
    }

    #[test]
    fn closing_software_uses_the_selected_interface_language() {
        let _guard = test_lock();
        set_language("fr");
        assert_eq!(tr("Closing software..."), "Fermeture du logiciel...");

        set_language("en");
        assert_eq!(tr("Closing software..."), "Closing software...");
    }

    #[test]
    fn builder_xml_translates_only_translatable_attributes() {
        let _guard = test_lock();
        set_language("fr");
        let translated = translate_builder_xml(
            r#"<attribute name="label" translatable="yes">_File</attribute><attribute name="action">app.file</attribute>"#,
        );
        assert!(translated.contains(">_Fichier</attribute>"));
        assert!(translated.contains(">app.file</attribute>"));
        set_language("en");
    }
}
