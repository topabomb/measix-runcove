#!/usr/bin/env python3
"""Hash named build inputs, including uncommitted source, without timestamps."""
import hashlib
import fnmatch
from pathlib import Path

PROJECT = Path(__file__).resolve().parents[1]


def source_digest():
    files = [PROJECT / name for name in (
        'Cargo.toml', 'Cargo.lock', 'msb-runtime.sha256', '.dockerignore',
        'docker/Dockerfile', 'docker/docker-entrypoint.sh',
        'docker/build.sh', 'docker/build_images.py', 'docker/base-images.lock.json', 'docker/release_identity.py',
        'docker/release-metadata.py', 'docker/build-standalone.sh', 'docker/Dockerfile.standalone',
        'docker/standalone-entrypoint.sh', 'docker/playwright-seccomp.json',
        'docker/compose.server.yaml', 'docker/compose.standalone.yaml',
        'docker/verify-sqlite-link.py', 'docker/sqlite-check.c',
        'docker/apply-msb-patch.py',
    )]
    for directory in ('src', 'agent-space-mcp/src', 'static-http', 'webdav', 'build-support', 'guest', 'desktop-helper', 'desktop-ui', 'images', 'vendor', 'docker/patches', 'docker/configs.example'):
        files.extend(path for path in (PROJECT / directory).rglob('*')
                     if path.is_file() and not any(part in ('__pycache__', 'node_modules', 'dist', 'target', '.git', '.secrets') for part in path.relative_to(PROJECT).parts)
                     and not any(fnmatch.fnmatch(path.name, pattern) for pattern in ('.env', '.env.*', '*.key', '*.p12', '*.pfx', 'id_rsa', 'id_ed25519')))
    files.append(PROJECT / 'agent-space-mcp/Cargo.toml')
    digest = hashlib.sha256()
    for path in sorted(set(files)):
        name, data = path.relative_to(PROJECT).as_posix().encode(), path.read_bytes()
        digest.update(len(name).to_bytes(8, 'big') + name)
        digest.update(len(data).to_bytes(8, 'big') + data)
    return digest.hexdigest()


if __name__ == '__main__':
    print(source_digest())
