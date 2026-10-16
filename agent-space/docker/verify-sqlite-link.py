#!/usr/bin/env python3
"""Read the linked sqlite3_version symbol before stripping a Linux ELF binary."""
import argparse
from pathlib import Path
import struct
import subprocess


def linked_version(path):
    symbols = subprocess.check_output(['nm', '--defined-only', str(path)], text=True)
    addresses = [int(line.split()[0], 16) for line in symbols.splitlines()
                 if line.split()[-1:] == ['sqlite3_version']]
    if len(addresses) != 1:
        raise ValueError('Expected exactly one sqlite3_version symbol; verify before stripping')
    with path.open('rb') as stream:
        header = stream.read(64)
        if header[:6] != b'\x7fELF\x02\x01':
            raise ValueError('Expected little-endian ELF64')
        phoff = struct.unpack_from('<Q', header, 32)[0]
        phentsize, phnum = struct.unpack_from('<HH', header, 54)
        address = addresses[0]
        for index in range(phnum):
            stream.seek(phoff + index * phentsize)
            kind, _, offset, vaddr, _, filesz, _, _ = struct.unpack('<IIQQQQQQ', stream.read(56))
            if kind == 1 and vaddr <= address < vaddr + filesz:
                stream.seek(offset + address - vaddr)
                return stream.read(32).split(b'\0', 1)[0].decode('ascii')
    raise ValueError('sqlite3_version is outside file-backed LOAD segments')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--expected', default='3.51.3')
    args = parser.parse_args()
    actual = linked_version(args.binary)
    if actual != args.expected:
        raise SystemExit(f'{args.binary}: linked SQLite {actual}, expected {args.expected}')
    print(f'{args.binary.name}: linked SQLite {actual}')
