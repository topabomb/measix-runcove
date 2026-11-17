"""Private bounded decoder, embedded in the DAV helper; input is only fd 3."""
import ctypes
import io
import json
import math
import os
import resource
import selectors
import signal
import subprocess
import sys
import time
import warnings

LIMIT, UNSUPPORTED, DECODE, DEPENDENCY, TIMEOUT = 42, 41, 43, 44, 45
PIXELS, OUTPUT = 32_000_000, 1024 * 1024
START = time.monotonic()


class Failure(Exception):
    def __init__(self, code):
        self.code = code


def child_cpu(process):
    try:
        with open('/proc/' + str(process.pid) + '/stat') as stream:
            fields = stream.read().rsplit(')', 1)[1].split()
        live = (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK')
    except (OSError, ValueError, IndexError):
        live = 0
    done = resource.getrusage(resource.RUSAGE_CHILDREN)
    return time.process_time() + done.ru_utime + done.ru_stime + live


def command(arguments, maximum):
    parent = os.getpid()

    def prepare():
        if ctypes.CDLL(None).prctl(1, signal.SIGKILL, 0, 0, 0) != 0 or os.getppid() != parent:
            os._exit(DECODE)

    try:
        process = subprocess.Popen(arguments, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, pass_fds=(3,), env={'LANG': 'C.UTF-8'},
                                   cwd='/', preexec_fn=prepare)
    except FileNotFoundError:
        raise Failure(DEPENDENCY)
    output, errors = bytearray(), bytearray()
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ, True)
            selector.register(process.stderr, selectors.EVENT_READ, False)
            while selector.get_map():
                if time.monotonic() - START >= 9.5 or child_cpu(process) >= 8:
                    raise Failure(TIMEOUT)
                for key, _ in selector.select(.02):
                    data = os.read(key.fileobj.fileno(), 8192)
                    if not data:
                        selector.unregister(key.fileobj)
                    elif key.data:
                        output.extend(data)
                        if len(output) > maximum:
                            raise Failure(LIMIT)
                    else:
                        errors.extend(data)
                        if len(errors) > 64 * 1024:
                            raise Failure(LIMIT)
            # EOF can precede process exit; keep the same deadline/CPU budget.
            while process.poll() is None:
                if time.monotonic() - START >= 9.5 or child_cpu(process) >= 8:
                    raise Failure(TIMEOUT)
                time.sleep(.01)
        if process.returncode:
            if any(message in errors for message in (b'Cannot allocate memory', b'Out of memory', b'max pixel count')):
                raise Failure(LIMIT)
            raise Failure(DECODE)
        return bytes(output)
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
        process.stdout.close()
        process.stderr.close()


def dimensions(width, height):
    if width < 1 or height < 1 or width * height > PIXELS:
        raise Failure(LIMIT)


def vint(source, identifier=False):
    first = source.read(1)
    if not first or first == b'\0':
        raise Failure(DECODE)
    width = 1
    while not first[0] & (0x80 >> (width - 1)):
        width += 1
    if width > (4 if identifier else 8):
        raise Failure(DECODE)
    remaining = source.read(width - 1)
    if len(remaining) != width - 1:
        raise Failure(DECODE)
    value = int.from_bytes(first + remaining, 'big')
    if not identifier:
        value &= (1 << (7 * width)) - 1
        if value == (1 << (7 * width)) - 1:
            raise Failure(DECODE)  # An EBML header cannot have unknown length.
    return value


def webm(source):
    """Parse only the bounded EBML header, accepting every legal size VINT."""
    source.seek(0)
    if vint(source, True) != 0x1a45dfa3:
        raise Failure(DECODE)
    length = vint(source)
    if length > 1024 * 1024:
        raise Failure(LIMIT)
    end, doctype = source.tell() + length, None
    while source.tell() < end:
        identifier, length = vint(source, True), vint(source)
        if source.tell() + length > end:
            raise Failure(DECODE)
        if identifier == 0x4282:
            if doctype is not None or length > 64:
                raise Failure(DECODE)
            doctype = source.read(length)
            if len(doctype) != length:
                raise Failure(DECODE)
        else:
            if len(source.read(length)) != length:
                raise Failure(DECODE)
    return doctype == b'webm'


def video_dependencies(codec, decoder=None):
    # Use the codec capability table: an identified codec without a compiled
    # decoder is unsupported input, distinct from a corrupt packet (422).
    codecs = command(['/usr/bin/ffmpeg', '-v', 'error', '-codecs'], 64 * 1024).decode('utf-8')
    if not any(len(row) >= 2 and len(row[0]) == 6 and row[0][0] == 'D' and row[1] == codec
               for row in map(str.split, codecs.splitlines())):
        raise Failure(UNSUPPORTED)
    if decoder:
        decoders = command(['/usr/bin/ffmpeg', '-v', 'error', '-decoders'], 64 * 1024).decode('utf-8')
        if not any(len(row) >= 2 and len(row[0]) == 6 and row[1] == decoder
                   for row in map(str.split, decoders.splitlines())):
            raise Failure(UNSUPPORTED)
    encoders = command(['/usr/bin/ffmpeg', '-v', 'error', '-encoders'], 64 * 1024).decode('utf-8')
    if not any(len(row) >= 2 and len(row[0]) == 6 and row[1] == 'libwebp'
               for row in map(str.split, encoders.splitlines())):
        raise Failure(DEPENDENCY)


def scale(size):
    aspect = 'if(gt(sar,0),sar,1)'
    width = f'iw*{aspect}'
    factor = f'min(1,min({size}/({width}),{size}/ih))'
    return f"scale=w='max(1,floor({width}*{factor}))':h='max(1,floor(ih*{factor}))',setsar=1"


def orientation(video, size):
    rotation = float(video.get('tags', {}).get('rotate', 0))
    matrix = None
    for side in video.get('side_data_list', []):
        rotation = float(side.get('rotation', rotation))
        if 'displaymatrix' in side:
            rows = side['displaymatrix'].strip().splitlines()
            matrix = [int(value) for row in rows for value in row.split(':', 1)[1].split()]
    if not math.isfinite(rotation):
        raise Failure(DECODE)
    if abs(rotation / 90 - round(rotation / 90)) <= .001:
        return False, scale(size)  # Native autorotate also preserves reflections.
    # Normalize SAR before rotating. Keep all four corners for arbitrary angles
    # instead of FFmpeg 5.1's default rotate filter's cropped output canvas.
    flip = False
    if matrix is not None:
        if len(matrix) != 9:
            raise Failure(DECODE)
        a, b, c, d = matrix[0], matrix[1], matrix[3], matrix[4]
        flip = a * d - b * c < 0
        # The screen transform is [[a,c],[b,d]]. With negative determinant,
        # factor it as rotation(theta) * vflip; theta still uses atan2(b,a).
        rotation = -math.degrees(math.atan2(b, a))
    theta = -math.fmod(rotation, 360)
    filters = scale(size) + ',format=rgba'
    if flip:
        filters += ',vflip'
    angle = f'{theta:.12f}*PI/180'
    filters += f",rotate='{angle}':ow='ceil(rotw({angle}))':oh='ceil(roth({angle}))':c=black@0," + scale(size)
    return True, filters


def main():
    try:
        from PIL import Image, ImageOps, features
    except ImportError:
        raise Failure(DEPENDENCY)
    Image.MAX_IMAGE_PIXELS = PIXELS
    if not features.check('webp'):
        raise Failure(DEPENDENCY)
    warnings.simplefilter('error', Image.DecompressionBombWarning)
    size = int(sys.argv[1])
    if size not in (128, 256, 512):
        raise Failure(DECODE)
    with os.fdopen(os.dup(3), 'rb') as source:
        prefix = source.read(4096)
        source.seek(0)
        image = (prefix.startswith((b'\xff\xd8\xff', b'\x89PNG\r\n\x1a\n', b'GIF87a', b'GIF89a', b'BM'))
                 or prefix[:4] == b'RIFF' and prefix[8:12] == b'WEBP')
        if image:
            if os.fstat(3).st_size > 64 * 1024 * 1024:
                raise Failure(LIMIT)
            try:
                with Image.open(source, formats=('JPEG', 'PNG', 'WEBP', 'GIF', 'BMP')) as original:
                    dimensions(*original.size)
                    original.seek(0)
                    oriented = ImageOps.exif_transpose(original).convert('RGBA')
                    oriented.thumbnail((size, size), Image.Resampling.LANCZOS)
                    output = io.BytesIO()
                    oriented.save(output, format='WEBP', quality=85, method=4, exact=True)
                    result = output.getvalue()
            except (Image.DecompressionBombError, Image.DecompressionBombWarning, MemoryError):
                raise Failure(LIMIT)
        else:
            if prefix.startswith(b'\x1aE\xdf\xa3'):
                if not webm(source):
                    raise Failure(UNSUPPORTED)
                media_format = 'matroska'
            elif prefix.startswith(b'OggS'):
                media_format = 'ogg'
            elif prefix[4:8] in (b'ftyp', b'moov', b'mdat', b'wide', b'free', b'skip'):
                if prefix[4:8] == b'ftyp' and prefix[8:12] in (b'avif', b'avis', b'heic', b'heix', b'hevc', b'mif1'):
                    raise Failure(UNSUPPORTED)
                media_format = 'mov'
            else:
                raise Failure(UNSUPPORTED)
            inputs = ['-protocol_whitelist', 'file', '-format_whitelist', 'mov,matroska,ogg',
                      '-max_pixels', str(PIXELS), '-probesize', '1048576',
                      '-analyzeduration', '1000000', '-f', media_format]
            if media_format == 'mov':
                inputs += ['-enable_drefs', '0', '-use_absolute_path', '0']
            inputs += ['-i', '/proc/self/fd/3']
            raw = command(['/usr/bin/ffprobe', '-v', 'error', '-max_alloc', '67108864',
                           *inputs, '-select_streams', 'V:0', '-show_entries',
                           'stream=index,codec_name,width,height,sample_aspect_ratio:stream_tags=rotate,alpha_mode:stream_side_data=rotation,displaymatrix:format=duration',
                           '-of', 'json'], 64 * 1024)
            probe = json.loads(raw)
            streams = probe.get('streams', [])
            if not streams:
                raise Failure(UNSUPPORTED)
            video = streams[0]
            if video.get('codec_name') in (None, 'unknown'):
                raise Failure(UNSUPPORTED)
            decoder = None
            if str(video.get('tags', {}).get('alpha_mode', '0')) == '1':
                # Native VP8/VP9 decoders discard WebM BlockAdditional alpha.
                # Select the available alpha-capable decoder explicitly instead
                # of returning an apparently successful opaque representation.
                decoder = {'vp8': 'libvpx', 'vp9': 'libvpx-vp9'}.get(video['codec_name'])
                if decoder is None:
                    raise Failure(UNSUPPORTED)
            video_dependencies(video['codec_name'], decoder)
            width, height = int(video['width']), int(video['height'])
            dimensions(width, height)
            duration = float(probe.get('format', {}).get('duration', 'nan'))
            if not math.isfinite(duration) or duration <= 0:
                raise Failure(DECODE)
            aspect = video.get('sample_aspect_ratio', '1:1')
            if aspect not in ('N/A', '0:1'):
                numerator, denominator = map(int, aspect.split(':'))
                if numerator <= 0 or denominator <= 0:
                    raise Failure(DECODE)
            manual, filters = orientation(video, size)
            if manual:
                inputs = ['-noautorotate', *inputs]
            if decoder:
                inputs = ['-c:v', decoder, *inputs]
            result = command(['/usr/bin/ffmpeg', '-v', 'error', '-nostdin', '-max_alloc', '67108864',
                              '-threads', '1', '-filter_threads', '1', '-filter_complex_threads', '1',
                              '-ss', str(min(1, duration * .1)), *inputs, '-map', '0:V:0',
                              '-frames:v', '1', '-vf', filters,
                              '-an', '-sn', '-dn', '-c:v', 'libwebp', '-quality', '85',
                              '-compression_level', '4', '-f', 'webp', 'pipe:1'], OUTPUT)
        if len(result) > OUTPUT:
            raise Failure(LIMIT)
        with Image.open(io.BytesIO(result), formats=('WEBP',)) as preview:
            if min(preview.size) < 1 or max(preview.size) > size or getattr(preview, 'n_frames', 1) != 1:
                raise Failure(DECODE)
        sys.stdout.buffer.write(result)


if __name__ == '__main__':
    try:
        main()
    except Failure as error:
        raise SystemExit(error.code)
    except MemoryError:
        raise SystemExit(LIMIT)
    except Exception:
        # Raw decoder messages, paths and input metadata never cross the worker.
        raise SystemExit(DECODE)
