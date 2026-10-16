"""Fault injection for guest cleanup; no VM or /workspace mutation required."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import uuid

spec = importlib.util.spec_from_file_location(
    "managed", Path(__file__).resolve().parents[1] / "guest" / "managed.py"
)
managed = importlib.util.module_from_spec(spec)
spec.loader.exec_module(managed)


class UploadCleanupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.stage = Path(self.temp.name) / "current"
        self.stage.mkdir()
        self.legacy = Path(self.temp.name) / "legacy"
        self.item = {
            "service": "webdav", "instanceId": uuid.uuid4().hex,
            "bootId": str(uuid.uuid4()),
        }
        (self.stage / "owner").write_text("agent-space-upload-v1:space\n")
        self.child = self.stage / self.item["instanceId"]
        self.child.mkdir()
        self.data = self.child / uuid.uuid4().hex
        self.data.write_bytes(b"preserve until ownership is verified")
        self.marker = self.child / "boot-id"
        self.marker.write_text(self.item["bootId"] + "\n")
        real_open = os.open

        def open_stage(path, *args, **kwargs):
            if path == "/workspace/.agent-space/uploads":
                path = str(self.stage)
            elif path == "/workspace/.agent-space-upload":
                path = str(self.legacy)
            return real_open(path, *args, **kwargs)

        self.redirect = patch.object(managed.os, "open", side_effect=open_stage)
        self.redirect.start()
        self.addCleanup(self.redirect.stop)

    def clean(self):
        managed.clean_uploads(self.item, {"owner": "space"})

    def test_unrelated_legacy_file_is_ordinary_and_does_not_block_cleanup(self):
        self.legacy.write_text("user file")
        self.clean()
        self.assertEqual(self.legacy.read_text(), "user file")
        self.assertFalse(self.child.exists())

    def test_owned_legacy_upload_is_cleaned_through_the_same_checks(self):
        self.stage.rename(self.legacy)
        self.clean()
        self.assertFalse((self.legacy / self.item["instanceId"]).exists())
        self.assertTrue((self.legacy / "owner").exists())

    def test_wrong_boot_marker_is_checked_before_any_unlink(self):
        self.marker.write_text(str(uuid.uuid4()) + "\n")
        with patch.object(managed.os, "listdir", return_value=[self.data.name, "boot-id"]):
            with self.assertRaises(RuntimeError):
                self.clean()
        self.assertTrue(self.data.exists(), "mismatched boot must preserve staged bytes")

    def test_missing_boot_marker_preserves_nonempty_directory(self):
        self.marker.unlink()
        with self.assertRaises(RuntimeError):
            self.clean()
        self.assertTrue(self.data.exists())

    def test_unexpected_entry_preflight_preserves_all_files(self):
        unknown = self.child / "unrecognized"
        unknown.write_text("not a managed upload")
        with patch.object(managed.os, "listdir", return_value=["boot-id", self.data.name, unknown.name]):
            with self.assertRaises((RuntimeError, ValueError)):
                self.clean()
        self.assertTrue(self.data.exists())
        self.assertTrue(unknown.exists())

    def test_verified_uploads_and_empty_prepare_residue_are_cleaned(self):
        self.clean()
        self.assertFalse(self.child.exists())
        self.child.mkdir()
        self.clean()
        self.assertFalse(self.child.exists())


if __name__ == "__main__":
    unittest.main()
