// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! GTK counterpart of Eden `src/qt_common/util/mod.{h,cpp}`.

use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gtk::prelude::*;

pub fn choose_mod_folders(
    root: &Path,
    fallback_name: Option<&str>,
    parent: Option<&gtk::Window>,
    callback: impl FnOnce(Vec<PathBuf>) + 'static,
) {
    let paths = frontend_common::mod_manager::get_mod_folder(root);
    if paths.len() > 1 {
        let dialog = gtk::Dialog::builder()
            .title("Select Mods")
            .modal(true)
            .build();
        if let Some(parent) = parent {
            dialog.set_transient_for(Some(parent));
        }
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        dialog.add_button("Install", gtk::ResponseType::Accept);
        let checks: Vec<gtk::CheckButton> = paths
            .iter()
            .map(|path| {
                let check = gtk::CheckButton::with_label(
                    &path.file_name().unwrap_or_default().to_string_lossy(),
                );
                check.set_active(true);
                dialog.content_area().append(&check);
                check
            })
            .collect();
        let callback = RefCell::new(Some(callback));
        dialog.connect_response(move |dialog, response| {
            if let Some(callback) = callback.borrow_mut().take() {
                callback(if response == gtk::ResponseType::Accept {
                    paths
                        .iter()
                        .zip(&checks)
                        .filter(|(_, check)| check.is_active())
                        .map(|(path, _)| path.clone())
                        .collect()
                } else {
                    Vec::new()
                });
            }
            dialog.close();
        });
        dialog.present();
        return;
    }

    let detected = paths.into_iter().next();
    let default_name = detected
        .as_ref()
        .filter(|path| !path.to_string_lossy().contains("atmosphere"))
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .or(fallback_name)
        .or_else(|| root.file_name().and_then(|name| name.to_str()))
        .unwrap_or_default();

    let dialog = gtk::Dialog::builder().title("Mod Name").modal(true).build();
    if let Some(parent) = parent {
        dialog.set_transient_for(Some(parent));
    }
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("OK", gtk::ResponseType::Accept);
    let prompt = gtk::Label::new(Some("What should this mod be called?"));
    prompt.set_xalign(0.0);
    let name = gtk::Entry::new();
    name.set_text(default_name);
    dialog.content_area().append(&prompt);
    dialog.content_area().append(&name);
    let mod_type = gtk::ComboBoxText::new();
    for label in ["RomFS", "ExeFS/Patch", "Cheat"] {
        mod_type.append_text(label);
    }
    mod_type.set_active(Some(0));
    if detected.is_none() {
        let type_prompt = gtk::Label::new(Some(
            "Could not detect the mod type automatically. Please specify it:",
        ));
        type_prompt.set_xalign(0.0);
        dialog.content_area().append(&type_prompt);
        dialog.content_area().append(&crate::configuration::shared_widget::popup_safe_combo(&mod_type));
    }

    let root = root.to_path_buf();
    let callback = RefCell::new(Some(callback));
    dialog.connect_response(move |dialog, response| {
        let result = if response != gtk::ResponseType::Accept || name.text().is_empty() {
            Vec::new()
        } else if let Some(path) = detected.as_ref() {
            let renamed = path.parent().unwrap_or(&root).join(name.text().as_str());
            if renamed != *path {
                let _ = fs::remove_dir_all(&renamed);
                if fs::rename(path, &renamed).is_err() {
                    Vec::new()
                } else {
                    vec![renamed]
                }
            } else {
                vec![path.clone()]
            }
        } else {
            let kind = match mod_type.active().unwrap_or(0) {
                0 => "romfs",
                1 => "exefs",
                2 => "cheats",
                _ => "romfs",
            };
            let mod_directory = std::env::temp_dir()
                .join("ruzu")
                .join("mod")
                .join(name.text().as_str());
            let target = mod_directory.join(kind);
            let _ = fs::remove_dir_all(&mod_directory);
            if fs::create_dir_all(&target).is_ok() && copy_contents(&root, &target).is_ok() {
                vec![mod_directory]
            } else {
                Vec::new()
            }
        };
        if let Some(callback) = callback.borrow_mut().take() {
            callback(result);
        }
        dialog.close();
    });
    dialog.present();
}

pub fn extract_mod(path: &Path) -> Result<PathBuf, String> {
    let archive_file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(archive_file).map_err(|error| error.to_string())?;
    if archive.is_empty() {
        return Err(format!("Zip file {} is empty", path.display()));
    }
    let temporary = std::env::temp_dir().join("ruzu").join("unzip_mod");
    let _ = fs::remove_dir_all(&temporary);
    fs::create_dir_all(&temporary).map_err(|error| error.to_string())?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let Some(enclosed_name) = entry.enclosed_name() else {
            return Err("The ZIP contains an unsafe path".to_string());
        };
        let destination = temporary.join(enclosed_name);
        if entry.is_dir() {
            fs::create_dir_all(&destination).map_err(|error| error.to_string())?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let mut output = fs::File::create(&destination).map_err(|error| error.to_string())?;
            io::copy(&mut entry, &mut output).map_err(|error| error.to_string())?;
        }
    }
    Ok(temporary)
}

fn copy_contents(source: &Path, destination: &Path) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source = entry.path();
        let target = destination.join(entry.file_name());
        if source.is_dir() {
            fs::create_dir_all(&target)?;
            copy_contents(&source, &target)?;
        } else {
            fs::copy(source, target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extraction_rejects_parent_traversal() {
        let temporary = tempfile::tempdir().unwrap();
        let zip_path = temporary.path().join("unsafe.zip");
        let file = fs::File::create(&zip_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("../escape", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"bad").unwrap();
        writer.finish().unwrap();
        assert!(extract_mod(&zip_path).is_err());
    }
}
