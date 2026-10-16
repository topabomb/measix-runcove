#!/usr/bin/env python3
"""Record the exact shipped common service, helper and UI bytes."""
import argparse, hashlib, json
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--target',choices=['server','standalone'],required=True)
p.add_argument('--bins',type=Path,required=True)
p.add_argument('--ui',type=Path,required=True)
p.add_argument('--workspace-lock',type=Path,required=True)
p.add_argument('--source',required=True)
p.add_argument('--revision',required=True)
p.add_argument('--output',type=Path,required=True)
a=p.parse_args()
def digest(path):
 with path.open('rb') as f: return hashlib.file_digest(f,'sha256').hexdigest()
service='agent-space-mcp' if a.target=='server' else 'runcove-standalone'
result={'target':a.target,'source_revision':a.revision,'source_sha256':a.source,
 'worker_contract':1,'workspace':json.loads(a.workspace_lock.read_text()),
 'binaries':{name:digest(a.bins/name) for name in [service,'agent-space-static-http','agent-space-webdav','agent-space-desktop']},
 'ui':{p.relative_to(a.ui).as_posix():digest(p) for p in sorted(a.ui.rglob('*')) if p.is_file()}}
assert result['ui'], 'UI assets missing'
a.output.write_text(json.dumps(result,indent=2)+'\n')
