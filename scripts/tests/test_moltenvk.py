import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "fetch_moltenvk", Path(__file__).resolve().parents[1] / "fetch-moltenvk.py"
)
moltenvk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(moltenvk)


class MoltenVK(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.cache = root / "cache"
        self.destination = root / "Frameworks/libMoltenVK.dylib"
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w") as archive:
            entry = tarfile.TarInfo(moltenvk.MEMBER)
            entry.size = 7
            archive.addfile(entry, io.BytesIO(b"library"))
        self.archive = data.getvalue()

    def test_verified_download_and_offline_cache(self):
        with patch.object(moltenvk, "SHA512", hashlib.sha512(self.archive).hexdigest()):
            with patch.object(moltenvk.urllib.request, "urlopen",
                              return_value=io.BytesIO(self.archive)) as download:
                moltenvk.install(self.destination, self.cache)
                download.assert_called_once_with(moltenvk.URL, timeout=120)
            self.assertEqual(self.destination.read_bytes(), b"library")
            self.assertEqual(self.destination.stat().st_mode & 0o777, 0o755)
            with patch.object(moltenvk.urllib.request, "urlopen") as download:
                moltenvk.install(self.destination, self.cache)
                download.assert_not_called()

    def test_invalid_download_never_installed_or_cached(self):
        with patch.object(moltenvk.urllib.request, "urlopen",
                          return_value=io.BytesIO(b"invalid")):
            with self.assertRaisesRegex(ValueError, "SHA-512 mismatch"):
                moltenvk.install(self.destination, self.cache)
        self.assertFalse(self.destination.exists())
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_corrupt_cache_preserves_existing_library(self):
        self.cache.mkdir()
        (self.cache / f"MoltenVK-macOS-{moltenvk.VERSION}.tar").write_bytes(b"invalid")
        self.destination.parent.mkdir()
        self.destination.write_bytes(b"previous")
        with patch.object(moltenvk.urllib.request, "urlopen") as download:
            with self.assertRaisesRegex(ValueError, "SHA-512 mismatch"):
                moltenvk.install(self.destination, self.cache)
            download.assert_not_called()
        self.assertEqual(self.destination.read_bytes(), b"previous")


if __name__ == "__main__":
    unittest.main()
