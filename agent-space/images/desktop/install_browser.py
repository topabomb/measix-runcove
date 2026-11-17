#!/usr/bin/env python3
"""Install the locked official Chrome archive through Playwright's own CLI."""
import argparse
import functools
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import re
import select
import shutil
import signal
import subprocess
import sys
import time
from urllib.parse import urlsplit


def read_lock(path):
    value = json.loads(Path(path).read_text())
    fields = {'schemaVersion', 'browser', 'playwrightVersion', 'revision', 'browserVersion', 'url', 'sha256'}
    if (not isinstance(value, dict) or set(value) != fields
            or type(value['schemaVersion']) is not int or value['schemaVersion'] != 1
            or value['browser'] != 'chromium'
            or not isinstance(value['playwrightVersion'], str) or not re.fullmatch(r'\d+\.\d+\.\d+', value['playwrightVersion'])
            or not isinstance(value['revision'], str) or not re.fullmatch(r'\d+', value['revision'])
            or not isinstance(value['browserVersion'], str) or not re.fullmatch(r'\d+\.\d+\.\d+\.\d+', value['browserVersion'])
            or not isinstance(value['sha256'], str) or not re.fullmatch(r'[0-9a-f]{64}', value['sha256'])
            or value['url'] != 'https://cdn.playwright.dev/builds/cft/'+value['browserVersion']+'/linux64/chrome-linux64.zip'):
        raise ValueError('invalid Chromium download lock')
    return value


def validate_package(lock, root):
    core = root/'node_modules/playwright-core'
    package = json.loads((core/'package.json').read_text())
    browsers = json.loads((core/'browsers.json').read_text())['browsers']
    chromium = [browser for browser in browsers if browser['name'] == 'chromium']
    if (package['version'] != lock['playwrightVersion'] or len(chromium) != 1
            or chromium[0]['revision'] != lock['revision'] or chromium[0]['browserVersion'] != lock['browserVersion']
            or chromium[0].get('revisionOverrides')):
        raise ValueError('installed Playwright Chromium identity differs from download lock')


class SeedHandler(BaseHTTPRequestHandler):
    def __init__(self, *args, archive, path, **kwargs):
        self.archive, self.download_path = archive, path
        super().__init__(*args, **kwargs)

    def log_message(self, *_):
        pass  # No request paths, proxy settings or environment in build logs.

    def do_HEAD(self):
        self.send_response(405)
        self.send_header('Allow', 'GET')
        self.end_headers()

    def do_GET(self):
        if self.path != self.download_path:
            self.send_error(404)
            return
        with self.archive.open('rb') as stream:
            self.send_response(200)
            self.send_header('Content-Type', 'application/zip')
            self.send_header('Content-Length', str(os.fstat(stream.fileno()).st_size))
            self.end_headers()
            try:
                shutil.copyfileobj(stream, self.wfile, 1024 * 1024)
            except (BrokenPipeError, ConnectionResetError):
                pass


def serve(archive, path):
    handler = functools.partial(SeedHandler, archive=Path(archive), path=path)
    with HTTPServer(('127.0.0.1', 0), handler) as server:
        print(server.server_port, flush=True)
        server.serve_forever()


def ready(process, seconds):
    deadline, data = time.monotonic()+seconds, bytearray()
    while time.monotonic() < deadline:
        readable, _, _ = select.select([process.stdout], [], [], max(0, deadline-time.monotonic()))
        if readable:
            part = os.read(process.stdout.fileno(), 256)
            if not part:
                break
            data.extend(part)
            if len(data) > 256:
                break
            if b'\n' in data:
                if re.fullmatch(rb'[0-9]{1,5}\n', data) and 0 < int(data) < 65536:
                    return int(data)
                break
    raise RuntimeError('private browser seed failed to become ready')


def wait_without_reaping(process):
    while True:
        status = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        if status is not None:
            return
        time.sleep(.02)


def reap(process):
    if process.returncode is not None:
        return process.returncode
    # Keep the unreaped leader PID reserved while terminating its private group.
    # This covers the downloader child on interruption without signaling a reused
    # process-group identity after Popen.wait() has collected the leader.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    return process.wait(timeout=5)


def install(lock_path, archive, root, *, node='node', cli=None, startup_timeout=5):
    lock, archive, root = read_lock(lock_path), Path(archive), Path(root)
    validate_package(lock, root)
    with archive.open('rb') as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != lock['sha256']:
            raise ValueError('Chromium archive checksum differs from download lock')
    server = installer = None
    try:
        server = subprocess.Popen([sys.executable, '-I', str(Path(__file__).resolve()), 'serve',
                                   '--archive', str(archive), '--path', urlsplit(lock['url']).path],
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  start_new_session=True, env={})
        port = ready(server, startup_timeout)
        environment = dict(os.environ)
        environment.pop('PLAYWRIGHT_DOWNLOAD_HOST', None)
        environment.pop('PLAYWRIGHT_FFMPEG_DOWNLOAD_HOST', None)
        environment['PLAYWRIGHT_CHROMIUM_DOWNLOAD_HOST'] = 'http://127.0.0.1:'+str(port)
        bypass = ','.join(filter(None, (environment.get('NO_PROXY', ''), environment.get('no_proxy', ''),
                                        '127.0.0.1', 'localhost')))
        environment['NO_PROXY'] = environment['no_proxy'] = bypass
        command = [node, str(cli or root/'node_modules/playwright/cli.js'), 'install', '--with-deps', '--no-shell', 'chromium']
        installer = subprocess.Popen(command, env=environment, start_new_session=True)
        wait_without_reaping(installer)
        status = reap(installer)
        if status:
            raise subprocess.CalledProcessError(status, command)
    finally:
        try:
            if installer is not None:
                reap(installer)
        finally:
            if server is not None:
                try:
                    reap(server)
                finally:
                    server.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['build-args', 'install', 'serve'])
    parser.add_argument('--lock', type=Path)
    parser.add_argument('--archive', type=Path)
    parser.add_argument('--root', type=Path, default=Path('/opt/agent-tools'))
    parser.add_argument('--node', default='node')
    parser.add_argument('--cli', type=Path)
    parser.add_argument('--path')
    args = parser.parse_args()
    if args.mode == 'build-args':
        lock = read_lock(args.lock)
        print(json.dumps({'CHROMIUM_URL': lock['url'], 'CHROMIUM_SHA256': lock['sha256']}))
    elif args.mode == 'serve':
        serve(args.archive, args.path)
    else:
        def interrupted(number, _):
            raise SystemExit(128+number)
        signal.signal(signal.SIGTERM, interrupted)
        signal.signal(signal.SIGINT, interrupted)
        install(args.lock, args.archive, args.root, node=args.node, cli=args.cli)


if __name__ == '__main__':
    main()
