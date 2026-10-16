#!/usr/bin/env python3
"""Build a runtime-only repair image without changing the deployed application."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import uuid

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('sqlite_link', ROOT / 'verify-sqlite-link.py')
sqlite_link = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sqlite_link)


def docker(*args):
    return subprocess.check_output(['docker', *args], text=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base-image', required=True)
    parser.add_argument('--runtime-assets', type=Path, default=ROOT / '.build/sqlite351-runtime')
    parser.add_argument('--image', required=True, help='A new tag; historical tags are never overwritten')
    args = parser.parse_args()
    base = json.loads(docker('image', 'inspect', args.base_image))[0]
    if base['Architecture'] != 'amd64' or base['Os'] != 'linux':
        raise SystemExit('Only Linux/amd64 is verified')
    if base['Config'].get('Entrypoint') != ['docker-entrypoint.sh']:
        raise SystemExit('Unsupported base entrypoint; review the original deployment')
    if subprocess.run(['docker', 'image', 'inspect', args.image], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
        raise SystemExit('Destination tag already exists; choose a fresh tag')
    assets = args.runtime_assets.resolve()
    manifest = json.loads((assets / 'runtime.json').read_text())
    if manifest['sdk_source_modified']:
        raise SystemExit('Runtime assets include an SDK patch; build and deploy the matching application and CLI together, not a runtime-only repair')
    if manifest['microsandbox'] != '0.7.2' or manifest['sqlite'] != '3.51.3' or manifest['profile'] != 'release':
        raise SystemExit('Unverified runtime assets')
    if manifest['source_revision'] != '60d4dc8a436fb9365491567ec21d073e924e3c6d' or manifest['source_archive_sha256'] != 'fecd277ccfddbb4a131e4c9dfe056a87a014926c78dd705776d738158c483828':
        raise SystemExit('Runtime source does not match the verified official 0.7.2 archive')
    if (assets / 'msb.sha256').read_text() != manifest['msb_sha256'] + '  bin/msb\n':
        raise SystemExit('Runtime cache checksum does not match its manifest')
    with (assets / 'msb').open('rb') as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != manifest['msb_sha256']:
            raise SystemExit('Runtime checksum mismatch')
    # Removing the last temporary tag can also remove an originally untagged
    # image record. Keep an immutable ID-based tag for rollback beforehand.
    rollback_tag = 'agent-space:repair-base-' + base['Id'].split(':')[1][:16]
    existing = subprocess.run(['docker', 'image', 'inspect', rollback_tag], capture_output=True, text=True)
    if existing.returncode == 0 and json.loads(existing.stdout)[0]['Id'] != base['Id']:
        raise SystemExit('Rollback tag points at another image; refusing to overwrite it')
    docker('tag', base['Id'], rollback_tag)
    with tempfile.TemporaryDirectory(prefix='as-repair-build-') as directory:
        work = Path(directory)
        name = 'as-repair-inspect-' + uuid.uuid4().hex[:10]
        docker('create', '--name', name, base['Id'])
        try:
            docker('cp', name + ':/usr/local/bin/agent-space-mcp', str(work / 'agent-space-mcp'))
            version = docker('run', '--rm', '--entrypoint', '/usr/local/bin/msb', base['Id'], '--version').strip()
            if version != 'msb 0.7.2':
                raise SystemExit('Runtime-only repair requires the same SDK/CLI 0.7.2')
            if sqlite_link.linked_version(work / 'agent-space-mcp') != '3.51.3':
                raise SystemExit('Service is not linked to SQLite 3.51.3. Rebuild that application version first; do not change only msb.')
        finally:
            docker('rm', '-v', name)
        for filename in ('msb', 'sqlite-check', 'runtime.json', 'msb.sha256'):
            shutil.copy2(assets / filename, work / filename)
        shutil.copy2(ROOT / 'runtime-repair-entrypoint.sh', work / 'runtime-repair-entrypoint.sh')
        # BuildKit interprets bare sha256 image IDs as registry names in FROM.
        # Give this exact local ID a disposable tag, never reuse a historical tag.
        base_tag = 'agent-space-repair-base:' + uuid.uuid4().hex[:12]
        docker('tag', base['Id'], base_tag)
        try:
            subprocess.run(['docker', 'build', '--build-arg', 'BASE_IMAGE=' + base_tag, '--build-arg', 'BASE_IMAGE_ID=' + base['Id'], '-f', str(ROOT / 'Dockerfile.runtime-repair'), '-t', args.image, str(work)], check=True)
        finally:
            docker('image', 'rm', base_tag)
    new_id = docker('image', 'inspect', args.image, '--format', '{{.Id}}').strip()
    print(json.dumps({'image': args.image, 'image_id': new_id, 'base_image_id': base['Id'], 'runtime': manifest}, indent=2))


if __name__ == '__main__':
    main()
