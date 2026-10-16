#!/usr/bin/env python3
"""Hash named build inputs, including uncommitted source, without timestamps."""
import hashlib
from pathlib import Path

PROJECT = Path(__file__).resolve().parents[1]


def source_digest():
    files = [PROJECT / name for name in (
        'Cargo.toml', 'Cargo.lock', 'msb-runtime.sha256',
        'docker/Dockerfile', 'docker/docker-entrypoint.sh',
        'docker/build.sh', 'docker/release_identity.py',
        'docker/release-metadata.py', 'docker/build-standalone.sh', 'docker/Dockerfile.standalone',
        'docker/standalone-entrypoint.sh', 'docker/playwright-seccomp.json',
        'docker/compose.server.yaml', 'docker/compose.standalone.yaml',
        'docker/verify-sqlite-link.py', 'docker/sqlite-check.c',
        'docker/apply-msb-patch.py',
    )]
    for directory in ('src', 'agent-space-mcp/src', 'static-http', 'webdav', 'build-support', 'guest', 'desktop-helper', 'desktop-ui', 'images', 'vendor', 'docker/patches', 'docker/configs.example'):
        files.extend(path for path in (PROJECT / directory).rglob('*')
                     if path.is_file() and not any(part in ('__pycache__', 'node_modules', 'dist') for part in path.parts))
    files.append(PROJECT / 'agent-space-mcp/Cargo.toml')
    digest = hashlib.sha256()
    for path in sorted(set(files)):
        name, data = path.relative_to(PROJECT).as_posix().encode(), path.read_bytes()
        digest.update(len(name).to_bytes(8, 'big') + name)
        digest.update(len(data).to_bytes(8, 'big') + data)
    return digest.hexdigest()


if __name__ == '__main__':
    print(source_digest())
