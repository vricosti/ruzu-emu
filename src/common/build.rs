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

fn msvc_target_directory(target_arch: &str) -> Option<&'static str> {
    match target_arch {
        "x86_64" => Some("x64"),
        "x86" => Some("x86"),
        "aarch64" => Some("arm64"),
        "arm" => Some("arm"),
        _ => None,
    }
}

fn find_msvc_compiler(repository: &Path) -> Option<OsString> {
    let vswhere = env::var_os("VSWHERE")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            env::var_os("ProgramFiles(x86)")
                .map(PathBuf::from)
                .map(|path| path.join("Microsoft Visual Studio/Installer/vswhere.exe"))
                .filter(|path| path.is_file())
        })
        .or_else(|| {
            env::var_os("ProgramFiles")
                .map(PathBuf::from)
                .map(|path| path.join("Microsoft Visual Studio/Installer/vswhere.exe"))
                .filter(|path| path.is_file())
        })?;
    let installation = command_text(
        &vswhere.into_os_string(),
        &[
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ],
        repository,
    )?;
    let tools_root = PathBuf::from(installation).join("VC/Tools/MSVC");
    let mut toolsets = std::fs::read_dir(tools_root)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect::<Vec<_>>();
    toolsets.sort_by_key(|entry| entry.file_name());

    let host = if cfg!(target_arch = "x86_64") {
        "Hostx64"
    } else {
        "Hostx86"
    };
    let target = msvc_target_directory(&env::var("CARGO_CFG_TARGET_ARCH").ok()?)?;
    toolsets.into_iter().rev().find_map(|toolset| {
        let compiler = toolset
            .path()
            .join("bin")
            .join(host)
            .join(target)
            .join("cl.exe");
        compiler.is_file().then(|| compiler.into_os_string())
    })
}

fn compiler_id(repository: &Path) -> String {
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let compiler = env::var_os("CXX").unwrap_or_else(|| {
        if target_env == "msvc" {
            find_msvc_compiler(repository).unwrap_or_else(|| OsString::from("cl.exe"))
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

fn exact_release_tag(repository: &Path, package_version: &str) -> Option<String> {
    let expected = format!("v{package_version}");
    git_value(repository, &["describe", "--tags", "--exact-match", "HEAD"])
        .filter(|tag| tag == &expected)
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
    println!(
        "cargo:rerun-if-changed={}",
        common_git.join("refs/tags").display()
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
    println!("cargo:rerun-if-env-changed=VSWHERE");
    println!("cargo:rerun-if-env-changed=GIT_REV");
    println!("cargo:rerun-if-env-changed=GIT_BRANCH");
    println!("cargo:rerun-if-env-changed=GIT_TAG");

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
    let package_version = env::var("CARGO_PKG_VERSION").unwrap();
    let release_tag = env::var("GIT_TAG")
        .ok()
        .filter(|tag| tag == &format!("v{package_version}"))
        .or_else(|| exact_release_tag(repository, &package_version));
    let build_version = release_tag.unwrap_or_else(|| format!("{short_revision}-{branch}"));

    println!("cargo:rustc-env=GIT_REV={revision}");
    println!("cargo:rustc-env=GIT_BRANCH={branch}");
    println!("cargo:rustc-env=GIT_DESC={build_version}");
    println!("cargo:rustc-env=BUILD_NAME=Ruzu");
    println!("cargo:rustc-env=BUILD_VERSION={build_version}");
    println!("cargo:rustc-env=COMPILER_ID={}", compiler_id(repository));
}
