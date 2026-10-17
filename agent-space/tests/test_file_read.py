"""Content classification regressions, without a workspace or media library."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("file_tool", Path(__file__).parents[1] / "guest/file.py")
file_tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(file_tool)


class FileReadTests(unittest.TestCase):
    def test_short_reads_are_completed_without_exceeding_the_binary_budget(self):
        class ShortReads(io.BytesIO):
            def read(self, size=-1):
                return super().read(min(size, 7))
        stream = ShortReads(bytes(range(100)))
        self.assertEqual(file_tool.read_bounded(stream, 23), bytes(range(23)))
        self.assertEqual(stream.tell(), 23)
        self.assertEqual(file_tool.read_bounded(stream, 100), bytes(range(23, 100)))

    def read(self, data, multimodal):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input"
            path.write_bytes(data)
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                status = file_tool.read_file(str(path), dict(offset=1, limit=500, multimodal=multimodal))
            self.assertEqual(status, 0)
            return out.getvalue(), err.getvalue().strip()

    def test_short_ascii_media_prefixes_do_not_turn_text_into_images(self):
        for data in [b"BM25 ranks documents\n", b"II* is an expression\n"]:
            for multimodal in [False, True]:
                out, kind = self.read(data, multimodal)
                self.assertEqual(kind, "RUNCOVE_READ_TEXT")
                self.assertEqual(out, "     1\t" + data.decode())

    def test_binary_unsupported_images_never_fall_back_to_text(self):
        for data in [b"BM" + bytes(64), b"II*\0" + bytes(64), b"MM\0*" + bytes(64)]:
            out, kind = self.read(data, True)
            self.assertEqual(kind, "RUNCOVE_READ_MEDIA")
            self.assertEqual(json.loads(out)["error"]["code"], "UNSUPPORTED_MEDIA")


if __name__ == "__main__":
    unittest.main()
