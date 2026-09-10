"""Regression tests for scoped catalog imports."""
import json
from pathlib import Path
import tempfile
import unittest

from generate_ruzu_i18n import write_tables


class ScopedImportTests(unittest.TestCase):
    def test_preserves_existing_and_filters_context_and_unfinished_messages(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            catalogs = root / "languages"
            sources = root / "src"
            catalogs.mkdir()
            sources.mkdir()
            (sources / "dialog.rs").write_text(
                '"Existing" "New" "Other" "Unfinished" "Ruzu warning"', encoding="utf-8")
            (catalogs / "fr.ts").write_text('''<TS>
              <context><name>Dialog</name>
                <message><source>Existing</source><translation>Replacement</translation></message>
                <message><source>New</source><translation>Nouveau</translation></message>
                <message><source>Unfinished</source><translation type="unfinished">Pending</translation></message>
                <message><source>Eden warning</source><translation>Avertissement Eden</translation></message>
              </context>
              <context><name>OtherDialog</name>
                <message><source>Other</source><translation>Autre</translation></message>
              </context>
            </TS>''', encoding="utf-8")
            output = root / "catalog.json"
            output.write_text(json.dumps({"fr": {"Existing": "Preserved", "Manual": "Custom"}}))
            write_tables(output, catalogs, sources, "Dialog")
            expected = {"fr": {"Existing": "Preserved", "Manual": "Custom",
                               "New": "Nouveau", "Ruzu warning": "Avertissement Ruzu"}}
            self.assertEqual(json.loads(output.read_text()), expected)
            first = output.read_bytes()
            write_tables(output, catalogs, sources, "Dialog")
            self.assertEqual(output.read_bytes(), first)


if __name__ == "__main__":
    unittest.main()
