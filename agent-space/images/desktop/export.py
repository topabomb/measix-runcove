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
from pathlib import Path


def export(source, destination, lock):
    if source.resolve() == destination.resolve():
        raise ValueError('Source and destination must differ')
    with tarfile.open(source) as archive:
        index = json.load(archive.extractfile('index.json'))
        candidates = index['manifests']
        while candidates[0]['mediaType'].endswith('index.v1+json'):
            candidates = json.load(archive.extractfile('blobs/sha256/' + candidates[0]['digest'].split(':')[1]))['manifests']
        selected = next(item for item in candidates if item.get('platform', {}).get('architecture') == 'amd64'
                        and item.get('platform', {}).get('os') == 'linux')
        raw = archive.extractfile('blobs/sha256/' + selected['digest'].split(':')[1]).read()
        assert selected['digest'] == 'sha256:' + hashlib.sha256(raw).hexdigest()
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
    options = parser.parse_args()
    print(export(options.source, options.destination, options.lock))
