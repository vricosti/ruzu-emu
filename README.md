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

<h4 align="center"><b>ruzu</b> is a Nintendo Switch emulator written in Rust and it began as an experiment <br>nobody expected to work.
</h4>

<div align="center">

| | |
|---|---|
| **Cost** | about the price of two Switch consoles |
| **Ported** | ~500,000 lines of C++ (yuzu/Eden) only with LLMs |
| **Duration** | 6 months |

</div>

<p align="center"><i>Fun fact: the Eden emulator forbids the use of LLMs xD</i></p>

<p align="center">
  <a href="#what-this-project-actually-is">About</a> |
  <a href="#screenshots">Screenshots</a> |
  <a href="#ports-produced-along-the-way">Ports</a> |
  <a href="#platforms">Platforms</a> |
  <a href="#building">Building</a> |
  <a href="#legal">License</a>
</p>

---

## What this project actually is

I wanted to know one thing: **could frontier LLMs carry a genuinely huge piece
of software from C++ to Rust?** Not generate a plausible-looking file, not
translate a self-contained algorithm, carry a whole codebase across, one
that is:

- large: hundreds of thousands of lines across a dozen subsystems;
- deeply stateful: an HLE kernel with schedulers, fibers, IPC and services;
- unforgiving: a GPU command processor, a shader recompiler, and a JIT where a
  single wrong bit produces a black screen rather than a stack trace;
- built on C++ idioms with no direct Rust equivalent: inheritance hierarchies,
  raw pointer graphs, `shared_ptr` cycles, destructor ordering, `std::variant`.

I had serious doubts it would go anywhere. It did: the experiment ended up
producing an emulator that boots and runs commercial titles, and that reaches
the performance of the C++ original.

"Faithful" was the whole point, and it is a much harder target than "works". The
port is held to **structural parity** with the C++ source: the Rust file tree
mirrors the upstream directory tree, methods live in the file their C++
counterpart lives in, constants stay next to the code that owns them, and
lifecycle order is preserved literally. A maintainer looking at any Rust file
should be able to answer *which C++ file is this, and what is still missing?*
The contract the work is held to is written down in
[`CLAUDE.md`](CLAUDE.md) — it is the interesting artefact of this project as
much as the code is.

**Now ruzu can fly on its own.**

## Screenshots

| Game configuration | SuperTuxKart running in ruzu |
|---|---|
| ![Chocolate Doom configuration in ruzu](docs/configure-doom.png) | ![SuperTuxKart running in ruzu](docs/supertuxkart.png) |

## Ports produced along the way

Getting the emulator to run meant porting its dependencies too. Each of these is
a standalone Rust crate in its own right:

| Crate | Ports | Upstream |
|---|---|---|
| [**rxbyak**](https://github.com/vricosti/rxbyak) | Xbyak, a C++ JIT assembler — x86-64 machine-code encoding, bit-identical to upstream | [herumi/xbyak](https://github.com/herumi/xbyak) |
| **rdynarmic**| dynarmic, an ARM dynamic recompiler — AArch32 and AArch64 frontends, x86-64 and ARM64 backends, ~650 IR opcodes | [lioncash/dynarmic](https://github.com/lioncash/dynarmic) |

rdynarmic's ARM32 and ARM64 translation is validated by **differential fuzzing
against the C++ dynarmic oracle**: the same instruction encodings are fed to
both implementations and the resulting register and flag state is compared.
That, rather than "the game boots", is what makes a JIT port credible.

Testing an emulator needs something to run on it, so one more project came out
of this work:

| Project | What it is | Upstream |
|---|---|---|
| [**FreeBrick**](https://github.com/vricosti/freebrick) | A brick breaker in C++/SFML, with builds for Linux, macOS, Windows and Switch homebrew. MIT licensed, with original or freely licensed art and audio, so it can be shipped with the emulator | [dawid-wolinski/arkanoid-game](https://github.com/dawid-wolinski/arkanoid-game) |

Being homebrew, it boots without keys or firmware — which makes it a
reproducible test case rather than one more game nobody can legally hand you.

## Platforms

The emulator runs on both **x86-64** and **aarch64** hosts. rdynarmic carries a
backend for each, so guest ARM code is JIT-compiled natively on either
architecture rather than interpreted.

A **RISC-V** host was planned — compiling the workspace on riscv64 first, then
adding a RISC-V backend to rdynarmic. That work is not implemented yet.

### Compilation compiled(not tested) on

Results from [the compatibility
report](https://github.com/vricosti/ruzu-emu) of 18 August 2026. Each platform
started from a clean image with neither build dependencies nor a Rust toolchain
installed; `build.sh` installed them, and the validation command was
`cargo build --locked --bin ruzu`.

| Platform | Package manager | GTK | Rust | Result |
|---|---|---:|---:|---|
| Ubuntu 22.04.5 LTS | apt | 4.6.9 | 1.97.1 (rustup) | OK |
| Ubuntu 24.04.4 LTS | apt | 4.14.5 | 1.97.1 (rustup) | OK |
| Ubuntu 26.04 LTS | apt | 4.22.4 | 1.97.1 (rustup) | OK |
| Debian 13 (trixie) | apt | 4.18.6 | 1.97.1 (rustup) | OK |
| Fedora 44 | dnf | 4.22.4 | 1.97.1 (rustup) | OK |
| Arch Linux | pacman | 4.22.4 | 1.97.1 (rustup) | OK |
| openSUSE Tumbleweed | zypper | 4.22.4 | 1.97.1 (rustup) | OK |
| Alpine 3.24 | apk | 4.22.4 | 1.97.1 (rustup, musl) | OK |
| FreeBSD 15.1-RELEASE | pkg | 4.20.4 | 1.97.1 (rustup) | OK |
| NetBSD 10.1 | pkgin | 4.22.4 | 1.97.1 (rustup) | OK |
| OpenBSD 7.9 | pkg_add | 4.22.3 | 1.94.1 (native package) | OK |
| macOS Tahoe 26 | Homebrew | — | rustup | Not yet run — Homebrew support is implemented in `build.sh`, pending validation on real Apple hardware |

NetBSD needs two steps that `build.sh` cannot take on its own, because they
provision the package manager and X11 themselves: install `pkgin` with
`pkg_add`, and unpack the `xbase`, `xcomp` and `xfont` base sets. pkgsrc
publishes no X11 packages for NetBSD.

## Building

### Requirements

- Rust **1.85** or newer (the workspace `rust-version`; the platforms above were
  validated with 1.97.1).
- **GTK 4.6** or newer, plus Vulkan headers, OpenSSL, FFmpeg, glslang, CMake and
  a C/C++ toolchain. SDL3 is compiled statically from source by Cargo;
  `build.sh` installs the remaining platform packages.

### Clone

`rdynarmic` is part of the workspace. The remaining external crates (`rxbyak`
and `rhazel`) are git submodules, so clone recursively:

```sh
git clone --recurse-submodules <repository-url> ruzu
cd ruzu
```

Already cloned without them?

```sh
git submodule update --init --recursive
```

### Build

From the root of the clone:

```sh
./build.sh
```

`build.sh` is sufficient to install the required dependencies and build Ruzu;
no separate Cargo command is needed. It dispatches to
`scripts/build-linux.sh`, `scripts/build-bsd.sh` or `scripts/build-macos.sh`
based on `uname -s`. Each one checks the platform
dependencies, then compiles the workspace in release. The dependency step is
idempotent, and it asks separately before installing system packages and before
installing Rust — it will not install either without confirmation. On macOS the
build finishes by packaging `target/release/ruzu.app`, which is what carries the
Info.plist, the icon and the bundled MoltenVK.

```sh
./build.sh --debug            # debug profile instead of release
./build.sh --deps-only        # only check and install dependencies
./build.sh --skip-deps        # build without re-checking the dependencies
./build.sh -- --bin ruzu-cmd  # everything after `--` goes to cargo
```

### Run

```sh
./target/release/ruzu
```

On macOS, build the native application bundle with:

```sh
./scripts/build-macos-app.sh
open ./target/release/ruzu.app
```

The bundle contains the ruzu executable, application metadata and icon, and
MoltenVK under `Contents/Frameworks`, matching the upstream macOS bundle
layout. Set `MOLTENVK_LIBRARY=/path/to/libMoltenVK.dylib` to package a specific
MoltenVK build instead of the Homebrew installation.

On Windows, run `build.bat` from an ordinary Command Prompt. It detects or
installs Visual Studio Build Tools, Rust and vcpkg, then configures the current
prompt and creates a standalone Release build in
`build\x86_64-pc-windows-msvc\release`. Pass `-Debug` to use the `debug`
subdirectory instead. Cargo keeps its intermediate artifacts under `target`;
only `ruzu.exe` and the matching vcpkg runtime DLLs are staged in `build`, so
the executable can be launched outside the build prompt. An existing standalone
vcpkg is selected from `VCPKG_ROOT`, `PATH`, or common locations. To select one
explicitly, use
`build.bat -VcpkgRoot D:\path\to\vcpkg`; add `-Yes` for unattended dependency
installation. Packaging additionally requires
[NSIS 3](https://nsis.sourceforge.io/Download); the portable runtime directory
and installer are then generated with:

```bat
build.bat package
```

To deliberately create a test package from another branch, use:

```bat
build.bat package -ForcePackage
```

`-ForcePackage` bypasses only the Git `main`-branch checks and prints a warning;
all build, dependency, runtime-file, and NSIS validations remain enabled.

The script builds both `ruzu.exe` and `ruzu-cmd.exe`, stages the dynamic
`x64-windows-ruzu` vcpkg DLLs and GTK/GLib runtime data, then writes the package
and installer under `target\package`. Packaging is accepted only when Ruzu and
all initialized project submodules are checked out on their `main` branches and
the submodules match the commits recorded by Ruzu. The advanced staging-only
and existing-binary modes remain available by invoking
`dist\package-windows.ps1` directly with `-StageOnly` or `-SkipBuild`.

There is also a headless command-line frontend:

```sh
cargo run --bin ruzu-cmd -- -g "/path/to/game.nsp"
```

With logging, and with cache/config kept out of your real profile:

```sh
env XDG_CACHE_HOME=/tmp/ruzu-cache \
    XDG_CONFIG_HOME=/tmp/ruzu-config \
    RUST_LOG=info cargo run --bin ruzu-cmd -- -g "/path/to/game.nsp"
```

> Do **not** override `XDG_DATA_HOME`. ruzu falls back to an existing yuzu NAND
> directory under `$XDG_DATA_HOME` when its own is empty; pointing it at a fresh
> temporary directory makes ruzu synthesize placeholder system archives instead.

### Tests

```sh
cargo test -p common
cargo test -p core
cargo test -p rdynarmic
```

Tests here are focused parity regressions — a test exists for a specific
upstream contract, edge case or previously-fixed bug. Green tests are treated as
necessary but *not* sufficient: they prove exercised behaviour works, not that
the structure, ownership or lifecycle match upstream.

### OpenBSD note

The default login class caps a process's data memory at 1.5 GiB, which is not
enough to compile the `core` crate. Raise it and build single-threaded:

```sh
ulimit -d 6291456
cargo build --locked --bin ruzu -j 1
```

rustup publishes no OpenBSD host toolchain, so install the native `rust`
package there; `build.sh` refuses to substitute something else silently.

## Legal

ruzu is a clean-room-in-spirit port of GPL-licensed software and inherits its
licensing: **GPL-3.0-or-later**. It ships no Nintendo code, keys or system
files, and you need to provide your own dumps of anything it loads.

## Acknowledgements

*Nanos gigantum humeris insidentes* — dwarfs standing on the shoulders of
giants.

This project writes no new ideas. Every algorithm, every hard-won workaround for
undocumented hardware, every subtle ordering constraint in this repository was
discovered by someone else, and this port only translates their work into
another language. It exists solely because of:

- **the yuzu/eden team and its contributors**, for years of reverse-engineering the
  Switch and for a codebase clear enough that a faithful port is even
  conceivable;
- **Merry (MerryMage)**, author of **dynarmic**, and **Lioncash**, whose fork is
  the reference for this port — a recompiler whose correctness is the reason
  guest code runs at all;
- **Mitsunari Shigeo (herumi)**, author of **Xbyak**, an x86-64 assembler so
  well-shaped that its structure survived translation to Rust almost intact;
- the maintainers of **Rust**, **GTK**, **SDL**, **Vulkan** and the crate
  ecosystem this port stands on.

Any bug you find here is a translation error introduced by this port, not a flaw
in the work it was translated from. The credit runs entirely in the other
direction.
