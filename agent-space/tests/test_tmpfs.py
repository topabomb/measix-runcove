import importlib.util
import io
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("tmpfs", Path(__file__).parents[1] / "guest/tmpfs.py")
tmpfs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tmpfs)


class TmpfsTests(unittest.TestCase):
    def test_missing_mount_is_rejected(self):
        with patch("builtins.open", return_value=io.StringIO("")):
            with self.assertRaisesRegex(RuntimeError, "unambiguous"):
                tmpfs.mount_identity("/tmp")

    def test_disk_mount_is_rejected(self):
        with patch("builtins.open", return_value=io.StringIO("41 33 254:32 / /tmp rw - ext4 /dev/vdc rw\n")):
            with self.assertRaisesRegex(RuntimeError, "not tmpfs"):
                tmpfs.mount_identity("/tmp")

    def test_stacked_mounts_are_rejected(self):
        line = "41 33 0:33 / /tmp rw - tmpfs tmpfs rw,size=128m\n"
        with patch("builtins.open", return_value=io.StringIO(line + line)):
            with self.assertRaisesRegex(RuntimeError, "unambiguous"):
                tmpfs.mount_identity("/tmp")

    def test_mount_path_escapes_are_decoded(self):
        line = "41 33 0:33 / /tmp\\040space rw - tmpfs tmpfs rw,size=128m\n"
        with patch("builtins.open", return_value=io.StringIO(line)):
            self.assertEqual(tmpfs.mount_identity("/tmp space")[0], "41")

    def test_symlink_or_zero_size_is_rejected_before_mount(self):
        with patch.object(tmpfs.subprocess, "run") as mount:
            with patch.object(tmpfs.os.path, "realpath", return_value="/workspace"):
                with self.assertRaisesRegex(RuntimeError, "invalid"):
                    tmpfs.grow("/tmp", 512)
            with self.assertRaisesRegex(RuntimeError, "invalid"):
                tmpfs.grow("/tmp", 0)
            mount.assert_not_called()


if __name__ == "__main__":
    unittest.main()
