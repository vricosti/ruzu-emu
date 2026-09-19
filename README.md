<!--
SPDX-FileCopyrightText: 2025 ruzu contributors
SPDX-License-Identifier: GPL-3.0-or-later
-->

<h1 align="center">
  <br>
  <a href="https://github.com/vricosti/ruzu-emu"><img src="./src/ruzu/assets/ruzu-rusty-lemon.png" alt="ruzu" width="200"></a>
  <br>
  <b>ruzu</b>
  <br>
</h1>

<p align="center">
  An experimental, open-source Nintendo Switch emulator written in Rust.<br>
  For Linux, Windows and macOS.
</p>

<p align="center">
  <a href="https://github.com/vricosti/ruzu-emu/releases">Downloads</a> |
  <a href="#features">Features</a> |
  <a href="#screenshots">Screenshots</a> |
  <a href="#building">Building</a> |
  <a href="https://discord.gg/ZebAZ6yFF">Discord</a> |
  <a href="https://github.com/vricosti/ruzu-emu/issues">Report a bug</a>
</p>

## About

Ruzu brings Nintendo Switch emulation to desktop computers, with a graphical
game library, controller-friendly navigation and configurable graphics and input.

The project builds on the work of Yuzu and Eden, with development focused on
compatibility, rendering correctness, performance and usability. It is developed
with the assistance of AI coding agents, alongside testing and user feedback.

Ruzu is under active development. Compatibility and performance vary by game,
hardware and graphics backend. Bugs, crashes and incomplete features are still
expected; keep backups of your saves before testing new versions.

## Features

- **Game library:** browse your game directories, manage per-game settings and
  view detected updates and add-ons.
- **Controller-friendly interface:** navigate the library, menus and supported
  dialogs with a controller, as well as a keyboard and mouse.
- **Graphics backends:** Vulkan and OpenGL, plus an experimental native Metal
  backend on macOS. Availability depends on the platform.
- **CPU emulation:** ARM32 and ARM64 guest execution through the rdynarmic
  dynamic recompiler, with x86-64 and ARM64 host backends.
- **Customizable interface:** themes, translations, configurable hotkeys and
  game-list display options.
- **Homebrew support:** run homebrew applications, with both a graphical
  frontend and a command-line frontend for testing.
- **System applets:** experimental Home Menu and other applet support.
  Availability depends on the installed system content.

## Downloads and platforms

Get prebuilt packages from the [releases page](https://github.com/vricosti/ruzu-emu/releases).
Check each release's notes for available packages and known issues.

| Platform | Package |
|---|---|
| Windows — x86-64 | Installer or standalone ZIP |
| macOS — Apple Silicon | Application bundle in a ZIP |
| macOS — Intel | Application bundle in a ZIP |
| Linux — x86-64 | AppImage |
| Ubuntu | DEB package matching your Ubuntu release |
| Steam Deck | Dedicated Zen 2 AppImage |

The native **Metal backend remains alpha**. Steam Deck packages target its CPU;
they are not a generic replacement for the Linux AppImage on older PCs.
BSD build scripts are also available, but building successfully does not
guarantee runtime compatibility.

### Try a free homebrew game

[FreeBrick](https://github.com/vricosti/freebrick) is a free, open-source brick
breaker available as Switch homebrew. Download
[freebrick.nro](https://github.com/vricosti/freebrick/raw/refs/heads/main/switch/freebrick.nro)
and open it in Ruzu using **File → Load File**.

## Screenshots

| Game configuration | SuperTuxKart running in Ruzu |
|---|---|
| ![Chocolate Doom configuration in Ruzu](docs/configure-doom.png) | ![SuperTuxKart running in Ruzu](docs/supertuxkart.png) |

## Building

### Clone the repository

Clone with submodules to include the external dependencies:

```sh
git clone --recurse-submodules https://github.com/vricosti/ruzu-emu.git
cd ruzu-emu
```

For an existing checkout:

```sh
git submodule update --init --recursive
```

### Linux, macOS and BSD

From the repository root:

```sh
./build.sh
```

This is sufficient to check dependencies and build Ruzu in release mode;
a separate Cargo command is not needed. The script asks for confirmation
before installing missing system packages or Rust.

The workspace declares Rust 1.85 as its minimum version. Native dependencies
include GTK 4.6 or newer, graphics and audio libraries, and a C/C++ build
toolchain. Let the platform script handle the dependency list; SDL3 is built
from source by Cargo.

Useful options:

```sh
./build.sh --debug           # Build in debug mode
./build.sh --deps-only       # Check and install dependencies only
./build.sh --skip-deps       # Build without dependency checks
./build.sh -- --bin ruzu-cmd  # Build the command-line frontend
```

Run the Linux/BSD executable with:

```sh
./target/release/ruzu
```

On macOS, the build creates an application bundle:

```sh
open ./target/release/ruzu.app
```

See [macOS MoltenVK notes](docs/macos-moltenvk.md) for the Vulkan dependency
used by macOS builds.

### Windows

From an ordinary Command Prompt:

```bat
build.bat
```

The script checks the Visual Studio Build Tools, Rust and vcpkg dependencies
and stages the executable and runtime libraries under
`build\x86_64-pc-windows-msvc\release`.

Use `build.bat -Debug` for a debug build, or
`build.bat -VcpkgRoot D:\path\to\vcpkg` to select an existing vcpkg installation.

### Packaging

| Target | Command |
|---|---|
| Linux AppImage | `./build.sh appimage` |
| Steam Deck AppImage (Docker) | `./build.sh steamdeck` |
| Linux DEB / macOS ZIP | `./build.sh package` |
| Windows ZIP and installer | `build.bat package` |

Windows installer packaging also requires NSIS 3.

See the [Linux AppImage guide](docs/appimage.md) and
[Steam Deck packaging guide](docs/steamdeck-appimage.md) for requirements,
output locations and testing instructions.

These commands package the current checkout without committing, tagging or
pushing. Package names use the exact version tag at HEAD when available,
otherwise the branch and commit; local modifications add a `-dirty` suffix.

Maintainers can explicitly start the interactive release workflow with
`./build.sh package --official` or `build.bat package -Official`.
That workflow updates the version, builds, tags and pushes the current branch
and tag after successful packaging. It does not upload release assets.

## Development and testing

The code is organized as a Cargo workspace under `src/`. Core components
include the emulated kernel and services, graphics and shader processing,
audio, input, and the desktop frontend.

The project also includes:

- [**rdynarmic**](src/rdynarmic): the ARM dynamic recompiler.
- [**rxbyak**](https://github.com/vricosti/rxbyak): a Rust x86-64 JIT assembler
  based on Xbyak.

Read [AGENTS.md](AGENTS.md) before changing emulation code. Changes should remain
traceable to their upstream counterparts where applicable, and behavior changes
should have focused regression tests.

Example test commands:

```sh
cargo test -p common
cargo test -p core
cargo test -p rdynarmic
```

For command-line testing with a homebrew application:

```sh
cargo run --release --bin ruzu-cmd -- -g "/path/to/freebrick.nro"
```

Tests are complemented by runtime validation: passing unit tests alone does
not establish game compatibility or performance.

## Community and bug reports

Join the [Ruzu Discord community](https://discord.gg/ZebAZ6yFF) to discuss
the emulator, share feedback and help test new builds.

Report reproducible problems through [GitHub Issues](https://github.com/vricosti/ruzu-emu/issues).
Include:

- the Ruzu version or commit;
- your operating system, CPU, GPU and graphics driver;
- the graphics backend and relevant settings;
- steps to reproduce, logs, and screenshots or a short recording when useful.

Remove personal information from logs before sharing them. Do not upload
copyrighted game files, keys or system files.

## License

Ruzu is licensed under **GPL-3.0-or-later**. Third-party dependencies retain
their respective licenses.

Ruzu does not include Nintendo games, keys or system files and is not
affiliated with Nintendo. This software should not be used to play games you
have not legally obtained.

## Acknowledgements

Ruzu would not exist without the work of the wider emulation and open-source
communities. Special thanks to:

- **the yuzu/eden team and its contributors**, for the foundations of the
  emulator and years of hardware research;
- **Merry (MerryMage)** and the **dynarmic contributors**, including
  **Lioncash**, for their work on ARM dynamic recompilation;
- **Mitsunari Shigeo (herumi)**, author of **Xbyak**;
- the maintainers of **Rust**, **GTK**, **SDL**, **Vulkan**, and the libraries
  and tools used throughout the project;
- everyone contributing code, testing builds, reporting bugs and helping
  other users.
