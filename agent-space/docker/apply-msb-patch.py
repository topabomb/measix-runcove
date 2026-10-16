#!/usr/bin/env python3
"""Apply audited MSB patches, or verify the matching vendored Cargo inputs."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tomllib


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tree_digest(path):
    result = hashlib.sha256()
    for file in sorted(p for p in path.rglob('*') if p.is_file()):
        name, data = file.relative_to(path).as_posix().encode(), file.read_bytes()
        result.update(len(name).to_bytes(8, 'big') + name)
        result.update(len(data).to_bytes(8, 'big') + data)
    return result.hexdigest()


def load_manifest(patches):
    manifest = json.loads((patches / 'manifest.json').read_text())
    for patch in manifest['patches']:
        if digest(patches / patch['patch']) != patch['patch_sha256']:
            raise SystemExit('MSB patch digest mismatch; review upstream first')
    return manifest


def verify_vendor(vendor, patches):
    manifest = load_manifest(patches)
    for name, expected in manifest['vendored_crates'].items():
        if tree_digest(vendor / name) != expected['tree_sha256']:
            raise SystemExit(f'Vendored {name} differs from audited input')
    return manifest


def apply(source, patches):
    manifest = load_manifest(patches)
    version = tomllib.loads((source / 'Cargo.toml').read_text())['workspace']['package']['version']
    if version != manifest['version']:
        raise SystemExit('MSB version differs from the audited patch; review upstream first')
    # Preflight every file before changing any of them. Reapplication and fuzzy
    # upgrades fail closed rather than leaving a partly patched runtime.
    for patch in manifest['patches']:
        for file in patch['files']:
            if digest(source / file['file']) != file['original_sha256']:
                raise SystemExit('MSB source digest mismatch; refusing partial/fuzzy application')
    for patch in manifest['patches']:
        subprocess.run(['patch', '--batch', '--fuzz=0', '-p1', '-i', str((patches / patch['patch']).resolve())], cwd=source, check=True)
        for file in patch['files']:
            if digest(source / file['file']) != file['patched_sha256']:
                raise SystemExit('Patched MSB source digest mismatch')
    return manifest


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path, nargs='?')
    parser.add_argument('--vendor-root', type=Path)
    parser.add_argument('--patches', type=Path, default=Path(__file__).resolve().parent / 'patches')
    args = parser.parse_args()
    if args.vendor_root:
        verify_vendor(args.vendor_root.resolve(), args.patches.resolve())
    elif args.source:
        apply(args.source.resolve(), args.patches.resolve())
    else:
        parser.error('provide source or --vendor-root')
