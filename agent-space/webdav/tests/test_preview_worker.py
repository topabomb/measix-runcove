"""Real media and private decoder fault regressions; no running service required."""
import importlib.util
import io
import math
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('preview_worker', Path(__file__).parents[1] / 'src/preview_worker.py')
worker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(worker)


class PreviewWorkerTests(unittest.TestCase):
    def decode(self, path):
        program = "import os,sys; f=os.open(sys.argv[1],os.O_RDONLY); os.dup2(f,3); p=sys.argv[2]; code=open(p).read(); sys.argv=['worker',sys.argv[3]]; exec(compile(code,p,'exec'))"
        run = subprocess.run(['/usr/bin/python3', '-I', '-c', program, str(path), str(SPEC.origin), '128'],
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
        self.assertEqual(run.returncode, 0, (path.name, run.stderr))
        from PIL import Image
        return Image.open(io.BytesIO(run.stdout)).convert('RGBA')

    def test_ebml_accepts_equivalent_size_encodings_and_rejects_other_doctype(self):
        self.assertTrue(worker.webm(io.BytesIO(b'\x1aE\xdf\xa3\x8a\x42\x82\x40\x04webm\xec\x80')))
        self.assertFalse(worker.webm(io.BytesIO(b'\x1aE\xdf\xa3\x8b\x42\x82\x88matroska')))
        with self.assertRaises(worker.Failure) as failure:
            worker.webm(io.BytesIO(b'\x1aE\xdf\xa3\x10\x20\x00\x00'))
        self.assertEqual(failure.exception.code, worker.LIMIT)

    def test_one_codec_table_verifies_regular_and_named_alpha_decoders_and_webp_encoder(self):
        table = (b' DEV.L. h264 H.264 (decoders: h264 h264_qsv) (encoders: libx264)\n'
                 b' DEV.L. vp8 VP8 (decoders: vp8 libvpx) (encoders: libvpx)\n'
                 b' DEV.L. vp9 VP9 (decoders: vp9 libvpx-vp9) (encoders: libvpx-vp9)\n'
                 b' DEVILS webp WebP (encoders: libwebp_anim libwebp)\n')
        for codec, decoder in [('h264', None), ('vp8', 'libvpx'), ('vp9', 'libvpx-vp9')]:
            with self.subTest(codec=codec), patch.object(worker, 'command', return_value=table) as command:
                worker.video_dependencies(codec, decoder)
                command.assert_called_once_with(['/usr/bin/ffmpeg', '-v', 'error', '-codecs'], 64 * 1024)

    def test_missing_decoder_is_unsupported_and_missing_encoder_is_dependency(self):
        webp = b' DEVILS webp WebP (encoders: libwebp_anim libwebp)\n'
        for codec, decoder, table, expected in [
            ('h264', None, b' .EV.L. h264 H.264\n' + webp, worker.UNSUPPORTED),
            ('unknown', None, webp, worker.UNSUPPORTED),
            ('h264', None, b' DEV.L. h264 H.264\n V..... png PNG\n', worker.DEPENDENCY),
            ('h264', None, b' DEV.L. h264 H.264\n D.VILS webp WebP (encoders: libwebp)\n', worker.DEPENDENCY),
            ('h264', None, b' DEV.L. h264 H.264\n DEVILS webp WebP (encoders: libwebp_anim)\n', worker.DEPENDENCY),
            ('h264', None, b' DEV.L. h264 H.264\n DEVILS webp WebP\n', worker.DEPENDENCY),
            ('vp9', 'libvpx-vp9', b' DEV.L. vp9 VP9 (decoders: vp9)\n' + webp, worker.UNSUPPORTED),
            ('vp8', 'libvpx', b' DEV.L. vp8 VP8 (decoders: vp8 libvpx-vp9)\n' + webp, worker.UNSUPPORTED),
        ]:
            with self.subTest(codec=codec, decoder=decoder, table=table), patch.object(worker, 'command', return_value=table) as command:
                with self.assertRaises(worker.Failure) as failure:
                    worker.video_dependencies(codec, decoder)
                self.assertEqual(failure.exception.code, expected)
                command.assert_called_once_with(['/usr/bin/ffmpeg', '-v', 'error', '-codecs'], 64 * 1024)

    def test_missing_codec_tool_is_dependency_failure_without_a_second_probe(self):
        with patch.object(worker, 'command', side_effect=worker.Failure(worker.DEPENDENCY)) as command:
            with self.assertRaises(worker.Failure) as failure:
                worker.video_dependencies('h264')
            self.assertEqual(failure.exception.code, worker.DEPENDENCY)
            command.assert_called_once_with(['/usr/bin/ffmpeg', '-v', 'error', '-codecs'], 64 * 1024)

    def test_real_webm_alternate_vint_and_arbitrary_video_rotation(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            webm, movie = root / 'clip.webm', root / 'clip.mp4'
            for path, codec in [(webm, 'libvpx'), (movie, 'libx264')]:
                subprocess.run(['/usr/bin/ffmpeg', '-v', 'error', '-f', 'lavfi', '-i',
                                'color=red:s=320x180:d=1', '-c:v', codec, '-threads', '1', str(path)], check=True)
            data = webm.read_bytes()
            self.assertEqual(data[4] & 0x80, 0x80)
            at = data.index(b'\x42\x82\x84webm')
            data = data[:at + 2] + b'\x40\x04' + data[at + 3:]
            data = data[:4] + bytes([data[4] + 1]) + data[5:]
            webm.write_bytes(data)
            # Write a valid tkhd display matrix for 45 degrees, rather than a
            # text rotate tag which a muxer can silently normalize or omit.
            data = bytearray(movie.read_bytes())
            at = data.index(b'tkhd') + 44
            unit = round(2 ** .5 / 2 * 65536)
            data[at:at + 36] = struct.pack('>9i', unit, -unit, 0, unit, unit, 0, 0, 0, 1 << 30)
            movie.write_bytes(data)
            for path in (webm, movie):
                subprocess.run(['/usr/bin/ffprobe', '-v', 'error', str(path)], check=True)
                # Opening fd3 in the child preserves the exact production input
                # contract without inheriting the test runner's descriptors.
                image = self.decode(path)
                self.assertLessEqual(max(image.size), 128)
                if path == movie:
                    self.assertLessEqual(abs(image.width - image.height), 1)
                    pixel = image.convert('RGB').getpixel((image.width // 2, image.height // 2))
                    self.assertGreater(pixel[0], 200)

    def test_rotation_direction_sar_corners_and_reflection(self):
        from PIL import Image, ImageDraw
        colors = [(240, 10, 10), (10, 240, 10), (10, 10, 240), (240, 240, 10)]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = Image.new('RGB', (320, 160))
            draw = ImageDraw.Draw(source)
            for (x, y), color in zip([(0, 0), (160, 0), (0, 80), (160, 80)], colors):
                draw.rectangle((x, y, x + 159, y + 79), fill=color)
            source.save(root / 'marks.png')
            for angle, sar, reflected in [(90, 1, False), (45, 1, False), (45, 2, False), (45, 2, True)]:
                movie = root / f'marks-{angle}-{sar}-{reflected}.mp4'
                subprocess.run(['/usr/bin/ffmpeg', '-v', 'error', '-loop', '1', '-i', str(root / 'marks.png'),
                                '-t', '0.3', '-vf', f'setsar={sar}', '-c:v', 'libx264', '-threads', '1',
                                '-pix_fmt', 'yuv420p', str(movie)], check=True)
                radians = math.radians(angle)
                a, b = round(math.cos(radians) * 65536), -round(math.sin(radians) * 65536)
                c, d = -b, a
                if reflected:
                    b, d = -b, -d  # Flip the output y axis of the matrix.
                data = bytearray(movie.read_bytes())
                at = data.index(b'tkhd') + 44
                data[at:at + 36] = struct.pack('>9i', a, b, 0, c, d, 0, 0, 0, 1 << 30)
                movie.write_bytes(data)
                preview = self.decode(movie)
                # Independently project the centers of four distinct source
                # quadrants through the physical display matrix (SAR first).
                width, height = 128, math.floor(64 / sar)
                cosine, sine = a / 65536, -abs(b) / 65536
                bound_w = abs(width * cosine) + abs(height * sine)
                bound_h = abs(height * cosine) + abs(width * sine)
                factor = min(1, 128 / math.ceil(bound_w), 128 / math.ceil(bound_h))
                for (x, y), color in zip([(-width / 4, -height / 4), (width / 4, -height / 4),
                                           (-width / 4, height / 4), (width / 4, height / 4)], colors):
                    px = cosine * x - sine * y
                    py = sine * x + cosine * y
                    if reflected:
                        py = -py
                    pixel = preview.getpixel((round(preview.width / 2 + px * factor),
                                              round(preview.height / 2 + py * factor)))
                    self.assertTrue(all(abs(pixel[i] - color[i]) < 45 for i in range(3)),
                                    (angle, sar, reflected, color, pixel, preview.size))

    def test_webm_vp8_and_vp9_transparency_survives_decode(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            Image.new('RGBA', (320, 180), (240, 10, 10, 90)).save(root / 'alpha.png')
            for codec in ('libvpx', 'libvpx-vp9'):
                movie = root / (codec + '.webm')
                subprocess.run(['/usr/bin/ffmpeg', '-v', 'error', '-loop', '1', '-i', str(root / 'alpha.png'),
                                '-t', '0.3', '-c:v', codec, '-threads', '1', '-auto-alt-ref', '0',
                                '-pix_fmt', 'yuva420p', str(movie)], check=True)
                preview = self.decode(movie)
                self.assertEqual(preview.size, (128, 72))
                self.assertLessEqual(abs(preview.getpixel((64, 36))[3] - 90), 2, codec)


if __name__ == '__main__':
    unittest.main()
