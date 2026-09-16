# Bundled MoltenVK

Release app bundles use `V380-Ori/Ryujinx.MoltenVK`, tag `v1.4.1-ryujinx`,
matching Eden's `cpmfile.json` MoltenVK entry. This is not the Homebrew release.
`scripts/fetch-moltenvk.py` pins the archive URL and SHA-512, verifies downloads
and cached archives, and installs its universal arm64/x86_64 dylib.

`build.sh` and `build.sh package` use this dependency when creating `ruzu.app`.
The first bundle build needs network access; subsequent builds reuse the archive
under `target/deps/moltenvk/`. A checksum mismatch aborts packaging; remove the
reported cached archive to download it again. There is no fallback to an Eden
build directory or a Homebrew library.

`MOLTENVK_LIBRARY=/absolute/path/libMoltenVK.dylib` remains an explicit packaging
override for developer testing. The script prints when it is used. Existing
Homebrew and Eden installations are not modified or uninstalled.

This controls the app bundle, not the system Vulkan loader. Bare `ruzu-cmd`
launches still follow their existing runtime lookup; use `LIBVULKAN_PATH` to
select this dylib explicitly when testing outside the app bundle. An existing
ZIP must be regenerated to include the changed dependency.
