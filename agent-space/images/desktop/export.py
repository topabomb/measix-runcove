#!/usr/bin/env python3
"""Export a Docker Desktop OCI archive without its legacy Docker manifest.

MSB 0.7.2 otherwise prefers manifest.json and synthesizes a different manifest
digest. Select the linux/amd64 OCI manifest, preserve every blob verbatim, and
record that distribution digest (never the config/image ID).
"""
import argparse
import hashlib
import io
import json
import tarfile
import re
from pathlib import Path


def verified_blob(archive, descriptor, load=True):
    digest = descriptor['digest']
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', digest):
        raise ValueError('Unsupported OCI digest')
    member = archive.getmember('blobs/sha256/' + digest.split(':')[1])
    if not member.isfile() or member.size != descriptor['size']:
        raise ValueError('Invalid OCI blob size/type')
    if load and member.size > 4 * 1024 * 1024:
        raise ValueError('OCI metadata exceeds bound')
    checksum, chunks = hashlib.sha256(), []
    with archive.extractfile(member) as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            checksum.update(chunk)
            if load:
                chunks.append(chunk)
    if 'sha256:' + checksum.hexdigest() != digest:
        raise ValueError('OCI blob digest mismatch')
    return b''.join(chunks)


def select_manifest(archive, candidates, ancestors=()):
    for candidate in candidates:
        media = candidate['mediaType']
        if media.endswith(('index.v1+json', 'manifest.list.v2+json')):
            nested = json.loads(verified_blob(archive, candidate))
            result = select_manifest(archive, nested['manifests'], (*ancestors, candidate['digest']))
            if result:
                return result
        elif candidate.get('platform', {}).get('architecture') in (None, 'amd64'):
            raw = verified_blob(archive, candidate)
            manifest = json.loads(raw)
            config = json.loads(verified_blob(archive, manifest['config']))
            if config.get('architecture') == 'amd64' and config.get('os') == 'linux':
                for layer in manifest['layers']:
                    verified_blob(archive, layer, load=False)
                return candidate, manifest, (*ancestors, candidate['digest'], manifest['config']['digest'])
    return None


def export(source, destination, lock, expected_image=None):
    if source.resolve() == destination.resolve():
        raise ValueError('Source and destination must differ')
    with tarfile.open(source) as archive:
        raw_index = archive.extractfile('index.json').read(4 * 1024 * 1024 + 1)
        if len(raw_index) > 4 * 1024 * 1024:
            raise ValueError('OCI index exceeds bound')
        index = json.loads(raw_index)
        root_digest = 'sha256:' + hashlib.sha256(raw_index).hexdigest()
        selection = select_manifest(archive, index['manifests'], (root_digest,))
        if selection is None:
            raise ValueError('Archive has no linux/amd64 manifest')
        selected, manifest, identities = selection
        # Classic Docker returns the config ID; containerd image stores can
        # return the root OCI index/manifest ID. Accept only a verified ancestor
        # of this exact linux/amd64 image, never a sibling or mutable tag.
        if expected_image is not None and expected_image not in identities:
            raise ValueError('OCI archive does not belong to the built image ID')
        reference = 'runcove/runcove-workspace@' + selected['digest']
        selected = dict(selected, annotations={'org.opencontainers.image.ref.name': reference})
        raw_index = json.dumps({'schemaVersion': 2, 'mediaType': 'application/vnd.oci.image.index.v1+json',
                                'manifests': [selected]}, separators=(',', ':')).encode()
        with tarfile.open(destination, 'w') as output:
            for member in archive:
                if member.name in ('index.json', 'manifest.json'):
                    continue
                output.addfile(member, archive.extractfile(member) if member.isfile() else None)
            info = tarfile.TarInfo('index.json')
            info.size = len(raw_index)
            info.mode = 0o644
            output.addfile(info, io.BytesIO(raw_index))
    lock.write_text(json.dumps({'kind': 'desktop', 'preset': 'runcove-desktop', 'reference': reference,
                                'workerContract': 1}, indent=2) + '\n')
    return reference


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    parser.add_argument('--lock', type=Path, required=True)
    parser.add_argument('--expected-image')
    options = parser.parse_args()
    print(export(options.source, options.destination, options.lock, options.expected_image))
