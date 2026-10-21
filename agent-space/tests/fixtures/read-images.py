"""Dependency-free media fixtures for dedicated qualification workspaces."""
import base64
import hashlib
import json
import os
from pathlib import Path
import struct
import zlib

root = Path('/workspace/qualification/media')
root.mkdir(parents=True, exist_ok=True)


def chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))


def png(width, height, alpha=False, noise=False):
    channels = 4 if alpha else 3
    row = bytes(width * channels)
    raw = b''.join(b'\0' + (os.urandom(len(row)) if noise else row) for _ in range(height))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6 if alpha else 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b'')


data = png(500, 500, noise=True)
(root/'wrong-extension.txt').write_bytes(data)
(root/'large-output.png').write_bytes(png(1500, 1200, noise=True))
(root/'near-limit.png').write_bytes(png(1800, 1800, noise=True))
(root/'transparent.png').write_bytes(png(3000, 20, alpha=True))
(root/'corrupt.png').write_bytes(b'\x89PNG\r\n\x1a\ninvalid')
with (root/'large.png').open('wb') as stream:
    stream.write(b'\x89PNG\r\n\x1a\n')
    stream.truncate(10 * 1024 * 1024 + 1)
(root/'pixels.png').write_bytes(png(4001, 4000))
(root/'unsupported.bmp').write_bytes(b'BM' + bytes(64))
(root/'text.png').write_text('first\nsecond\n')
(root/'utf16.txt').write_bytes('first\nsecond\n'.encode('utf-16'))
(root/'long.txt').write_text('x' * 100000)
(root/'large.txt').write_text('line\n' * 2200000)
gif = base64.b64decode('R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7')
(root/'first.gif').write_bytes(gif[:-1] + gif[19:-1] + gif[-1:])
# Two-frame APNG: the first full canvas is red, the second is blue.
header = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 40, 20, 8, 2, 0, 0, 0)) + chunk(b'acTL', struct.pack('>II', 2, 0))
for index, color in enumerate([b'\xff\0\0', b'\0\0\xff']):
    header += chunk(b'fcTL', struct.pack('>IIIIIHHBB', index, 40, 20, 0, 0, 1, 10, 0, 0))
    pixels = zlib.compress((b'\0' + color * 40) * 20)
    header += chunk(b'IDAT', pixels) if index == 0 else chunk(b'fdAT', struct.pack('>I', 2) + pixels)
(root/'animated.png').write_bytes(header + chunk(b'IEND', b''))
formats = json.loads(Path('/workspace/qualification/formats.json').read_text())
for name, encoded in formats.items():
    (root/name).write_bytes(base64.b64decode(encoded))
jpeg = (root/'jpeg').read_bytes()
exif = b'Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0'
(root/'orientation.jpeg').write_bytes(jpeg[:2] + b'\xff\xe1' + struct.pack('>H', len(exif)+2) + exif + jpeg[2:])
# Animated WebP container around the browser-generated opaque red frame.
# https://developers.google.com/speed/webp/docs/riff_container
def riff_chunk(kind, data):
    return kind + struct.pack('<I', len(data)) + data + bytes(len(data) % 2)
webp = (root/'webp').read_bytes()
offset, frame = 12, b''
while offset + 8 <= len(webp):
    size = struct.unpack('<I', webp[offset+4:offset+8])[0]
    end = offset + 8 + size + size % 2
    if webp[offset:offset+4] in (b'VP8 ', b'VP8L', b'ALPH'):
        frame += webp[offset:end]
    offset = end
u24 = lambda n: n.to_bytes(3, 'little')
body = b'WEBP' + riff_chunk(b'VP8X', b'\x02\0\0\0' + u24(39) + u24(19)) + riff_chunk(b'ANIM', b'\0\0\0\xff\0\0')
body += riff_chunk(b'ANMF', bytes(6) + u24(39) + u24(19) + u24(100) + b'\0' + frame) * 2
(root/'animated.webp').write_bytes(b'RIFF' + struct.pack('<I', len(body)) + body)
print(json.dumps({'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data)}))
