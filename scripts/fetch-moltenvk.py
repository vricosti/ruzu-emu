#!/usr/bin/env python3
"""Install Eden's pinned MoltenVK artifact into a macOS app bundle."""

import argparse
import hashlib
import os
from pathlib import Path
import shutil
import tarfile
import tempfile
import urllib.request

# Eden cpmfile.json: moltenvk; externals/CMakeLists.txt: MOLTENVK_LIBRARY.
VERSION = "v1.4.1-ryujinx"
URL = f"https://github.com/V380-Ori/Ryujinx.MoltenVK/releases/download/{VERSION}/MoltenVK-macOS.tar"
SHA512 = "5695b36ca5775819a71791557fcb40a4a5ee4495be6b8442e0b666d0c436bec02aae68cc6210183f7a5c986bdbec0e117aecfad5396e496e9c2fd5c89133a347"
# CPM strips the outer MoltenVK directory; its inner dylib is a symlink to
# dynamic/dylib. Read the regular member directly without extracting symlinks.
MEMBER = "MoltenVK/MoltenVK/dynamic/dylib/macOS/libMoltenVK.dylib"


def verify(archive: Path) -> None:
    with archive.open("rb") as source:
        digest = hashlib.sha512()
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != SHA512:
        raise ValueError(f"MoltenVK SHA-512 mismatch: {archive}; remove this archive and retry")


def install(destination: Path, cache: Path) -> None:
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / f"MoltenVK-macOS-{VERSION}.tar"
    if not archive.exists():
        with tempfile.TemporaryDirectory(prefix=".moltenvk-", dir=cache) as temporary:
            download = Path(temporary) / "archive.tar"
            with urllib.request.urlopen(URL, timeout=120) as response, download.open("wb") as output:
                shutil.copyfileobj(response, output)
            verify(download)
            os.replace(download, archive)
    # Recheck cached bytes too; never silently substitute another distribution.
    verify(archive)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".moltenvk-", dir=destination.parent) as temporary:
        library = Path(temporary) / destination.name
        with tarfile.open(archive) as source:
            member = source.getmember(MEMBER)
            if not member.isfile():
                raise ValueError(f"MoltenVK archive member is not a regular file: {MEMBER}")
            with source.extractfile(member) as data, library.open("wb") as output:
                shutil.copyfileobj(data, output)
        library.chmod(0o755)
        os.replace(library, destination)
    print(f"Installed MoltenVK {VERSION} (SHA-512 verified): {destination}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--cache", type=Path,
                        default=Path(__file__).resolve().parent.parent / "target/deps/moltenvk")
    args = parser.parse_args()
    install(args.destination, args.cache)
