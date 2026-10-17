"""Fault injection for guest cleanup; no VM or /workspace mutation required."""
import importlib.util
import json
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


class DesktopJobCleanupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.item = dict(service="desktop", instanceId=str(uuid.uuid4()), pid=123,
                         startTicks=1, bootId=str(uuid.uuid4()), executableDevice=1,
                         executableInode=1, binaryDigest="binary", launchDigest="launch")
        self.directory = Path(self.temp.name) / self.item["instanceId"]
        self.directory.mkdir()
        (self.directory / "instance.json").write_text(json.dumps({"instance": self.item, "settings": {}}))
        self.job = self.directory / ("job-" + str(uuid.uuid4()))
        self.job.mkdir()
        for name in ["result.json", "result.json.tmp", "observation.json"]:
            (self.job / name).write_text("{}")
        for patcher in [patch.object(managed, "RUN", self.temp.name),
                        patch.object(managed, "clean_uploads"),
                        patch.dict(managed.EXECUTABLES, desktop=str(Path(self.temp.name) / "helper"))]:
            patcher.start()
            self.addCleanup(patcher.stop)

    def test_interrupted_checkpoint_and_legacy_observation_are_reclaimed(self):
        managed.clean_record(self.item)
        self.assertFalse(self.directory.exists())

    def test_checkpoint_symlink_blocks_cleanup_before_deleting_outputs(self):
        pending = self.job / "result.json.tmp"
        pending.unlink()
        pending.symlink_to(self.directory / "instance.json")
        with self.assertRaisesRegex(RuntimeError, "unexpected desktop job entry"):
            managed.clean_record(self.item)
        self.assertTrue((self.job / "result.json").exists())
        self.assertTrue((self.directory / "instance.json").exists())


if __name__ == "__main__":
    unittest.main()
