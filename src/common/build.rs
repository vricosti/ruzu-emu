// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

fn command_text(program: &OsString, args: &[&str], directory: &Path) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(directory)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(text.trim().to_owned())
}

fn git_value(repository: &Path, args: &[&str]) -> Option<String> {
    command_text(&OsString::from("git"), args, repository).filter(|value| !value.is_empty())
}

fn version_after(text: &str, marker: &str) -> Option<String> {
    let suffix = text.split_once(marker)?.1.trim_start();
    suffix
        .split_whitespace()
        .next()
        .map(|version| version.trim_end_matches(',').to_owned())
}

fn msvc_version_in(text: &str) -> Option<String> {
    let versions = text
        .split_whitespace()
        .filter(|word| {
            let word = word.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '.'
            });
            word.contains('.')
                && word
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.')
        })
        .map(|word| {
            word.trim_matches(|character: char| !character.is_ascii_digit() && character != '.')
                .to_owned()
        })
        .collect::<Vec<_>>();
    let primary = versions.first()?;
    versions
        .iter()
        .find(|version| {
            version.len() > primary.len()
                && version.starts_with(primary)
                && version.as_bytes().get(primary.len()) == Some(&b'.')
        })
        .cloned()
        .or_else(|| Some(primary.clone()))
}

fn compiler_id(repository: &Path) -> String {
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let compiler = env::var_os("CXX").unwrap_or_else(|| {
        if target_env == "msvc" {
            OsString::from("cl.exe")
        } else {
            OsString::from("c++")
        }
    });
    let args: &[&str] = if target_env == "msvc" {
        &["/Bv"]
    } else {
        &["--version"]
    };
    // `cl.exe /Bv` prints the complete compiler version, then exits with
    // D8003 because no source file was supplied. The output is nevertheless
    // authoritative; requiring a successful status here discarded it and
    // made every MSVC build report "Unknown compiler".
    let Some(output) = Command::new(&compiler)
        .args(args)
        .current_dir(repository)
        .output()
        .ok()
        .filter(|output| output.status.success() || target_env == "msvc")
        .map(|output| {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            text.trim().to_owned()
        })
        .filter(|output| !output.is_empty())
    else {
        return "Unknown compiler".to_owned();
    };

    if let Some(version) = version_after(&output, "Apple clang version") {
        return format!("AppleClang {version}");
    }
    if let Some(version) = version_after(&output, "clang version") {
        return format!("Clang {version}");
    }
    if target_env == "msvc" || output.contains("Microsoft (R) C/C++") {
        return msvc_version_in(&output)
            .map(|version| format!("MSVC {version}"))
            .unwrap_or_else(|| "MSVC".to_owned());
    }

    let version = output
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().last())
        .unwrap_or("unknown");
    format!("GNU {version}")
}

fn git_directory(repository: &Path) -> Option<PathBuf> {
    let dot_git = repository.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }

    let git_file = std::fs::read_to_string(dot_git).ok()?;
    let path = git_file.trim().strip_prefix("gitdir: ")?;
    let path = PathBuf::from(path);
    Some(if path.is_absolute() {
        path
    } else {
        repository.join(path)
    })
}

fn git_common_directory(repository: &Path) -> Option<PathBuf> {
    let path = PathBuf::from(git_value(repository, &["rev-parse", "--git-common-dir"])?);
    Some(if path.is_absolute() {
        path
    } else {
        repository.join(path)
    })
}

fn track_git_head(repository: &Path) {
    let Some(dot_git) = git_directory(repository) else {
        return;
    };
    let common_git = git_common_directory(repository).unwrap_or_else(|| dot_git.clone());
    println!("cargo:rerun-if-changed={}", dot_git.join("HEAD").display());
    println!(
        "cargo:rerun-if-changed={}",
        common_git.join("packed-refs").display()
    );
    if let Ok(head) = std::fs::read_to_string(dot_git.join("HEAD")) {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            println!(
                "cargo:rerun-if-changed={}",
                common_git.join(reference).display()
            );
        }
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repository = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&manifest_dir);
    track_git_head(repository);
    println!("cargo:rerun-if-env-changed=CXX");
    println!("cargo:rerun-if-env-changed=GIT_REV");
    println!("cargo:rerun-if-env-changed=GIT_BRANCH");

    let revision = env::var("GIT_REV")
        .ok()
        .filter(|revision| !revision.is_empty())
        .or_else(|| git_value(repository, &["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    let short_revision: String = revision.chars().take(10).collect();
    let branch = env::var("GIT_BRANCH")
        .ok()
        .filter(|branch| !branch.is_empty())
        .or_else(|| git_value(repository, &["branch", "--show-current"]))
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "detached".to_owned());
    let build_version = format!("{short_revision}-{branch}");

    println!("cargo:rustc-env=GIT_REV={revision}");
    println!("cargo:rustc-env=GIT_BRANCH={branch}");
    println!("cargo:rustc-env=GIT_DESC={build_version}");
    println!("cargo:rustc-env=BUILD_NAME=Ruzu");
    println!("cargo:rustc-env=BUILD_VERSION={build_version}");
    println!("cargo:rustc-env=COMPILER_ID={}", compiler_id(repository));
}
