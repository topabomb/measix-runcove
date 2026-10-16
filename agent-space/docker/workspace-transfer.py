#!/usr/bin/env python3
"""Offline selected-workspace export/import. Never imports VM or account state.
Stop source writers and mount only the selected workspace before exporting.
Import publishes a new directory; it never merges with or overwrites a target.
"""
import argparse, ctypes, hashlib, io, json, os, shutil, stat, tarfile, uuid
from pathlib import Path, PurePosixPath

MANIFEST='.runcove-workspace.json'

def sha(path):
 with os.fdopen(os.open(path,os.O_RDONLY|os.O_NOFOLLOW),'rb') as f: return hashlib.file_digest(f,'sha256').hexdigest()

def export_workspace(source, archive):
 source=Path(source)
 if not stat.S_ISDIR(source.lstat().st_mode): raise ValueError('source must be a real directory')
 if archive.resolve().is_relative_to(source.resolve()): raise ValueError('archive must be outside source')
 entries=[]
 paths=[source]
 for root, dirs, files in os.walk(source,followlinks=False):
  paths.extend(Path(root)/n for n in sorted(dirs+files))
 try:
  with archive.open('xb') as raw:
   os.fchmod(raw.fileno(),0o600)
   with tarfile.open(fileobj=raw,mode='w') as tar:
    for path in paths:
     m=path.lstat(); relative=path.relative_to(source).as_posix(); relative='' if relative=='.' else relative
     entry={'path':relative,'uid':m.st_uid,'gid':m.st_gid,'mode':stat.S_IMODE(m.st_mode),'mtime':m.st_mtime}
     item=tarfile.TarInfo('workspace'+('/'+relative if relative else ''));item.uid=m.st_uid;item.gid=m.st_gid;item.mode=entry['mode'];item.mtime=m.st_mtime
     if stat.S_ISLNK(m.st_mode):
      entry.update(kind='symlink',link=os.readlink(path));item.type=tarfile.SYMTYPE;item.linkname=entry['link'];tar.addfile(item)
     elif stat.S_ISDIR(m.st_mode):
      entry['kind']='directory';item.type=tarfile.DIRTYPE;tar.addfile(item)
     elif stat.S_ISREG(m.st_mode):
      entry.update(kind='file',size=m.st_size,sha256=sha(path));item.size=m.st_size
      with os.fdopen(os.open(path,os.O_RDONLY|os.O_NOFOLLOW),'rb') as f: tar.addfile(item,f)
      after=path.lstat()
      # Reading may advance atime on relatime filesystems. Compare identity and
      # write-related metadata, rather than mistaking our own reads for writes.
      fields=('st_dev','st_ino','st_mode','st_uid','st_gid','st_nlink','st_size','st_mtime_ns','st_ctime_ns')
      if sha(path)!=entry['sha256'] or any(getattr(after,k)!=getattr(m,k) for k in fields): raise ValueError('source changed during export: '+relative)
     else: raise ValueError('unsupported special file: '+relative)
     entries.append(entry)
    manifest=json.dumps({'schemaVersion':1,'entries':entries},sort_keys=True).encode()
    info=tarfile.TarInfo(MANIFEST);info.size=len(manifest);info.mode=0o600;tar.addfile(info,io.BytesIO(manifest))
   raw.flush();os.fsync(raw.fileno())
 except BaseException:
  # Only remove an output created by this invocation.
  if 'raw' in locals(): archive.unlink(missing_ok=True)
  raise
 return {'archiveSha256':sha(archive),'entries':len(entries),'sourceChanged':False}

def inspect_archive(archive):
 tar=tarfile.open(archive,'r:')
 try:
  members=tar.getmembers(); by_name={m.name:m for m in members}
  if len(by_name)!=len(members) or MANIFEST not in by_name: raise ValueError('duplicate entries or missing manifest')
  manifest=by_name[MANIFEST]
  if not manifest.isfile() or manifest.size>64*1024*1024: raise ValueError('invalid manifest')
  data=json.load(tar.extractfile(manifest))
  if data.get('schemaVersion')!=1: raise ValueError('unsupported manifest')
  entries=data['entries']; paths=set(); expected={MANIFEST}
  for e in entries:
   path=e['path']; parts=PurePosixPath(path).parts
   if (path and PurePosixPath(path).as_posix()!=path) or path in paths or path.startswith('/') or any(p in ('.','..') for p in parts) or '\x00' in path: raise ValueError('unsafe or duplicate path')
   paths.add(path); name='workspace'+('/'+path if path else '');expected.add(name);m=by_name.get(name)
   if type(e['mode']) is not int or not 0<=e['mode']<=0o7777: raise ValueError('invalid permission mode')
   if not m or m.uid!=e['uid'] or m.gid!=e['gid'] or m.mode!=e['mode']: raise ValueError('metadata mismatch')
   if e['kind']=='file':
    if not m.isfile() or m.size!=e['size'] or hashlib.file_digest(tar.extractfile(m),'sha256').hexdigest()!=e['sha256']: raise ValueError('file digest mismatch')
   elif e['kind']=='directory':
    if not m.isdir(): raise ValueError('directory mismatch')
   elif e['kind']=='symlink':
    if not m.issym() or m.linkname!=e['link']: raise ValueError('symlink mismatch')
   else: raise ValueError('unsupported entry kind')
  if expected!=set(by_name) or '' not in paths: raise ValueError('unlisted archive content or root missing')
  kinds={e['path']:e['kind'] for e in entries}
  if kinds['']!='directory': raise ValueError('root is not a directory')
  for path in paths-{''}:
   parent=PurePosixPath(path).parent.as_posix();parent='' if parent=='.' else parent
   if kinds.get(parent)!='directory': raise ValueError('non-directory archive parent')
  return tar,entries
 except BaseException: tar.close();raise

def import_workspace(archive,destination,apply=False):
 tar,entries=inspect_archive(archive)
 try:
  destination=Path(destination).absolute()
  if destination.exists() or destination.is_symlink(): raise ValueError('destination already exists')
  archived='.agent-space/restored-runtime-'+uuid.uuid4().hex
  def mapped(path):
   for old,new in [('.agent-space/uploads',archived+'/uploads'),('.agent-space-upload',archived+'/legacy-uploads')]:
    if path==old or path.startswith(old+'/'): return new+path[len(old):]
   return path
  # Archival adds parents absent from the original archive. Validate the
  # projected namespace during dry-run too, before creating a staging tree.
  projected={mapped(e['path']):e['kind'] for e in entries}
  if len(projected)!=len(entries): raise ValueError('runtime archival path collision')
  for path in projected:
   for parent in PurePosixPath(path).parents:
    name=parent.as_posix();name='' if name=='.' else name
    if name in projected and projected[name]!='directory':
     raise ValueError('runtime archival parent must be a directory: '+name)
  unknown=sorted({(e['uid'],e['gid']) for e in entries if (e['uid'],e['gid']) not in ((0,0),(1001,1001))})
  report={'applied':False,'archiveSha256':sha(archive),'entries':len(entries),'unsupportedOwners':unknown,'mapping':{'0:0':'1001:1001','1001:1001':'1001:1001'}}
  if not apply: return report
  if unknown: raise ValueError('unsupported ownership requires explicit source preparation; no ownership guessed')
  if os.geteuid()!=0: raise ValueError('import requires root to establish UID/GID1001')
  # Root must not traverse a staging namespace another UID can rename.
  # A sticky root-owned /tmp is safe for a root-owned private stage.
  for ancestor in [destination.parent,*destination.parent.parents]:
   m=ancestor.lstat()
   if not stat.S_ISDIR(m.st_mode) or m.st_uid!=0 or ((m.st_mode&0o022) and not (m.st_mode&stat.S_ISVTX)):
    raise ValueError('import parent ancestry must be root-owned, non-writable by other users and contain no symlinks')
  stage=destination.parent/('.runcove-import-'+uuid.uuid4().hex);stage.mkdir(mode=0o700)
  try:
   # No extractall: all names, parents and payloads were checked before writes.
   for e in sorted(entries,key=lambda e:(len(PurePosixPath(e['path']).parts),e['kind']!='directory',e['path'])):
    relative=mapped(e['path']);path=stage/relative
    path.parent.mkdir(parents=True,exist_ok=True)
    if e['kind']=='directory': path.mkdir(exist_ok=True)
    elif e['kind']=='symlink': path.symlink_to(e['link'])
    else:
     member=tar.getmember('workspace/'+e['path'])
     with path.open('xb') as f,tar.extractfile(member) as stream:
      shutil.copyfileobj(stream,f);f.flush();os.fsync(f.fileno())
     if sha(path)!=e['sha256']: raise ValueError('target digest mismatch')
    # Keep the staging root private to root until all path-based writes end.
    if e['path']:
     os.chown(path,1001,1001,follow_symlinks=False)
     if e['kind']!='symlink': os.chmod(path,e['mode']);os.utime(path,(e['mtime'],e['mtime']))
   # Synthetic archive parents are user data too. Never follow a source link.
   for path in stage.rglob('*'):
    os.chown(path,1001,1001,follow_symlinks=False)
   for e in sorted(entries,key=lambda e:len(PurePosixPath(e['path']).parts),reverse=True):
    if e['kind']!='symlink' and e['path']:
     # chown can clear setuid/setgid. Restore all permission bits only after
     # ownership mapping has ended; neither mapped owner can elevate to root.
     os.chmod(stage/mapped(e['path']),e['mode'])
     os.utime(stage/mapped(e['path']),(e['mtime'],e['mtime']))
   for directory,_,_ in os.walk(stage,topdown=False,followlinks=False):
    fd=os.open(directory,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW);os.fsync(fd);os.close(fd)
   root_entry=next(e for e in entries if e['path']=='')
   root_fd=os.open(stage,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
   libc=ctypes.CDLL(None,use_errno=True)
   try:
    if libc.renameat2(-100,os.fsencode(stage),-100,os.fsencode(destination),1): raise OSError(ctypes.get_errno(),'atomic no-overwrite import publication failed')
    # No path-based mutation after publication: the open root stays pinned.
    os.fchown(root_fd,1001,1001);os.fchmod(root_fd,root_entry['mode']);os.utime(root_fd,(root_entry['mtime'],root_entry['mtime']));os.fsync(root_fd)
   finally:os.close(root_fd)
   fd=os.open(destination.parent,os.O_RDONLY|os.O_DIRECTORY);os.fsync(fd);os.close(fd)
   report.update(applied=True,archivedRuntime=archived)
   return report
  finally:
   if stage.exists(): shutil.rmtree(stage)
 finally: tar.close()

def main():
 p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='command',required=True)
 e=sub.add_parser('export');e.add_argument('--source',type=Path,required=True);e.add_argument('--archive',type=Path,required=True)
 i=sub.add_parser('import');i.add_argument('--archive',type=Path,required=True);i.add_argument('--destination',type=Path,required=True);i.add_argument('--apply',action='store_true')
 a=p.parse_args();print(json.dumps(export_workspace(a.source,a.archive) if a.command=='export' else import_workspace(a.archive,a.destination,a.apply),indent=2))
if __name__=='__main__':main()
