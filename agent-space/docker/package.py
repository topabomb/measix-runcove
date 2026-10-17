#!/usr/bin/env python3
"""Package an exact qualified Runcove image and its deployment files, no user data."""
import argparse
from datetime import datetime,timezone
import gzip,hashlib,json,shutil,subprocess,zipfile,tarfile,re
from pathlib import Path
from urllib.parse import unquote,urlsplit
from release_identity import source_digest
from qualification import SHARED_CHECKS
PROJECT=Path(__file__).resolve().parents[1]

def sha(path):
 with path.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()

def validate(target,result,upgrade,info):
 labels=info['Config']['Labels'] or {}
 if labels.get('io.runcove.target')!=target or labels.get('io.runcove.source-sha256')!=source_digest():raise ValueError('image target/source differs from current build inputs')
 if info['Os']!='linux' or info['Architecture']!='amd64':raise ValueError('only qualified Linux/amd64 artifacts are supported')
 if not result.get('passed') or not result.get('cleanup_passed') or result.get('image_id')!=info['Id']:raise ValueError('exact image verification and successful cleanup required')
 if not re.fullmatch(r'runcove-[0-9a-f]{16}',labels.get('org.opencontainers.image.version','')):raise ValueError('application identity missing; state schema is not an application version')
 if target=='standalone':
  required={'lost_desktop_owner_blocks_recreation_and_control_until_container_restart','stopped_data_child_dies_with_killed_control_owner_and_helper_recovers','profile_preparation_cannot_follow_user_links_with_root_privileges','root_task_owner_keeps_final_results_in_memory_without_following_user_paths','rewritten_host_workbench_preserves_own_origin_and_nonce_checks','active_mcp_sse_closes_on_graceful_shutdown','starts_without_kvm_msb_or_docker_socket','single_space_routes_have_no_management_or_user_selection','shared_nine_tools_and_nonroot_secret_isolation','dav_workbench_and_mcp_edits_share_writable_user_ownership','graceful_restart_preserves_workspace_and_expires_page_session','shared_test_browser_vm.py','shared_test_media_vm.py','shared_test_desktop_files_vm.py','shared_test_browser_lifecycle_vm.py','shared_real_vnc_keyboard_clipboard_viewers_responsive_and_idle'}
  required.add('artifact_listing_respects_payload_permissions_through_user_symlinks')
  if result.get('scope')!='final image' or not result.get('full') or not required.issubset(result.get('checks',[])):raise ValueError('full final-image Standalone qualification required')
 else:
  if not result.get('full') or result.get('scope')!='final image' or not SHARED_CHECKS.issubset(result.get('checks',[])):raise ValueError('full final-image Server desktop qualification required')
  if result.get('idle_seconds',0)<=300 or labels.get('io.agent-space.state-schema')!='3':raise ValueError('Server lifecycle qualification and schema3 required')
  required={'original_credentials_and_identity','workspace_profile_and_files','new_server_tools_desktop_dav_publish','restart_persistence','isolated_complete_backup_rollback'}
  if not upgrade or not upgrade.get('passed') or not upgrade.get('cleanup_passed') or upgrade.get('image_id')!=info['Id'] or not required.issubset(upgrade.get('checks',[])):raise ValueError('exact pre-refactor production image upgrade/rollback rehearsal required')
 return labels

def verify_archive_identity(archive,image_id):
 digest=image_id.removeprefix('sha256:');found=False;index=None;legacy=None;objects={}
 with tarfile.open(archive,'r|gz') as tar:
  for member in tar:
   if member.name in ('index.json','manifest.json'):
    value=json.load(tar.extractfile(member))
    if member.name=='index.json':index=value
    else:legacy=value
   if member.name.startswith('blobs/sha256/') and member.isfile() and member.size<=2*1024*1024:
    raw=tar.extractfile(member).read()
    try:objects['sha256:'+member.name.rsplit('/',1)[1]]=json.loads(raw)
    except (ValueError,UnicodeDecodeError):pass
   if member.name in ('blobs/sha256/'+digest,digest+'.json'):
    if (hashlib.sha256(raw).hexdigest() if member.name.startswith('blobs/') else hashlib.file_digest(tar.extractfile(member),'sha256').hexdigest())!=digest:raise ValueError('archive image descriptor digest mismatch')
    found=True
 referenced=any(m['digest']==image_id for m in (index or {}).get('manifests',[])) or any(m.get('Config') in ('blobs/sha256/'+digest,digest+'.json') for m in (legacy or []))
 if not found or not referenced:raise ValueError('archive does not contain the exact qualified image identity')
 descriptor=objects.get(image_id,{})
 while 'manifests' in descriptor:
  choices=descriptor['manifests'];selected=next((m for m in choices if m.get('platform',{}).get('architecture')=='amd64' and m.get('platform',{}).get('os')=='linux'),choices[0]);descriptor=objects[selected['digest']]
 return descriptor.get('config',{}).get('digest',image_id)

def copy_manuals(bundle):
 # Keep canonical paths, including the repository index and retained evidence.
 # A fixed experiment date would silently omit later release/review records.
 shutil.copyfile(PROJECT.parent/'README.md',bundle/'README.md')
 manuals=bundle/'docs';manuals.mkdir()
 for path in sorted((PROJECT.parent/'docs').glob('*.md')):shutil.copyfile(path,manuals/path.name)
 evidence=PROJECT.parent/'docs/evidence'
 if evidence.exists():shutil.copytree(evidence,manuals/'evidence',ignore=shutil.ignore_patterns('*.log','__pycache__'))
 # Retain local reference links (including patch manifests and the index's
 # AGENTS link), without copying arbitrary paths outside the repository.
 repository=PROJECT.parent.resolve();pending=[bundle/'README.md',*manuals.rglob('*.md')];seen=set()
 while pending:
  document=pending.pop()
  if document in seen:continue
  seen.add(document);source=repository/document.relative_to(bundle)
  prose=re.sub(r'```.*?```','',document.read_text(),flags=re.S)
  for link in re.findall(r'!?\[[^]\n]*\]\(([^)\n]+)\)',prose):
   url=urlsplit(link.split(' "',1)[0].strip('<>'))
   if url.scheme or url.netloc or not url.path:continue
   reference=(source.parent/unquote(url.path)).resolve()
   if not reference.is_relative_to(repository) or not reference.exists():raise ValueError('unavailable repository documentation reference: '+url.path)
   target=bundle/reference.relative_to(repository)
   if reference.is_file() and not target.exists():
    target.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(reference,target)
    if target.suffix=='.md':pending.append(target)

def main():
 parser=argparse.ArgumentParser(description=__doc__)
 parser.add_argument('--target',choices=['server','standalone'],required=True)
 parser.add_argument('--verification',type=Path,required=True)
 parser.add_argument('--files',type=Path,required=True,help='Matching 4 GiB DAV, truncate and cancellation report')
 parser.add_argument('--storage',type=Path,required=True,help='Matching final-image ENOSPC and rejected-upload deadline report')
 parser.add_argument('--upgrade',type=Path,help='Required for Server: isolated production-version rehearsal report')
 parser.add_argument('--output',type=Path,required=True)
 a=parser.parse_args();result=json.loads(a.verification.read_text());upgrade=json.loads(a.upgrade.read_text()) if a.upgrade else None
 # The original tag is descriptive; the qualified image ID owns the evidence.
 info=json.loads(subprocess.check_output(['docker','image','inspect',result['image_id']],text=True))[0]
 labels=validate(a.target,result,upgrade,info)
 files=json.loads(a.files.read_text())
 required_files={'synchronized real guest truncate yields incomplete HTTP body, not padded success','80 cancelled real downloads release guest file descriptors and connection slots'}
 if not files.get('passed') or not files.get('cleanup_passed') or files.get('image_id')!=info['Id'] or files.get('full_read',{}).get('bytes')!=4294967296 or not required_files.issubset(files.get('checks',[])):raise ValueError('matching complete 4 GiB file verification required')
 storage=json.loads(a.storage.read_text())
 if not storage.get('passed') or not storage.get('cleanup_passed') or storage.get('scope')!='final image' or storage.get('target')!=a.target or storage.get('image_id')!=info['Id'] or storage.get('attempts',0)<20 or storage.get('rejected_upload',{}).get('progress_frames',0)<=10 or 'early rejected PUT response and fixed total drain deadline' not in storage.get('checks',[]):raise ValueError('matching final-image storage failure/deadline qualification required')
 stamp=datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ');version=labels['org.opencontainers.image.version'];name=f'runcove-{a.target}-{version}-linux-amd64-{stamp}'
 destination=a.output.resolve();destination.mkdir(parents=True,exist_ok=True);bundle=destination/name;bundle.mkdir()
 archive=bundle/'image.tar.gz'
 with archive.open('xb') as raw,gzip.GzipFile(filename='',fileobj=raw,mode='wb',mtime=0) as compressed:
  process=subprocess.Popen(['docker','save',info['Id']],stdout=subprocess.PIPE)
  try:
   shutil.copyfileobj(process.stdout,compressed)
   if process.wait():raise RuntimeError('docker save failed')
  finally:
   process.stdout.close()
   if process.poll() is None:process.terminate();process.wait()
 config_id=verify_archive_identity(archive,info['Id'])
 if not re.fullmatch(r'sha256:[0-9a-f]{64}',config_id):raise ValueError('invalid archived config identity')
 package_tag='runcove-'+a.target+':'+version
 (bundle/'.env').write_text('RUNCOVE_IMAGE='+package_tag+'\n')
 loader=(PROJECT/'docker/load-package.sh.in').read_text()
 for key,value in {'IMAGE_ID':info['Id'],'CONFIG_ID':config_id,'TAG':package_tag,'SOURCE':labels['io.runcove.source-sha256'],'ARCHIVE_SHA':sha(archive)}.items():loader=loader.replace('@'+key+'@',value)
 (bundle/'load.sh').write_text(loader);(bundle/'load.sh').chmod(0o755)
 (bundle/'configs.example').mkdir()
 config='config.toml' if a.target=='server' else 'standalone.toml'
 shutil.copyfile(PROJECT/'docker/configs.example'/config,bundle/'configs.example'/config)
 shutil.copyfile(PROJECT/f'docker/compose.{a.target}.yaml',bundle/'compose.yaml')
 shutil.copyfile(PROJECT/'docker/workspace-transfer.py',bundle/'workspace-transfer.py')
 if a.target=='standalone':shutil.copyfile(PROJECT/'docker/playwright-seccomp.json',bundle/'playwright-seccomp.json')
 else:shutil.copyfile(PROJECT/'msb-runtime.sha256',bundle/'msb-runtime.sha256')
 copy_manuals(bundle)
 (bundle/'README.txt').write_text('Measix Runcove '+a.target+'\n\nVerify SHA256SUMS, then run: sh load.sh\nDeployment: docs/deployment.md\nMigration: docs/operations.md\nProvide the target credentials and follow first-time initialization before docker compose up -d.\nCurrent image evidence: verification.json, files.json, storage.json, release.json and manifest.json.\nSource development commands in the manuals require a repository checkout.\n')
 shutil.copyfile(a.verification,bundle/'verification.json')
 shutil.copyfile(a.files,bundle/'files.json')
 shutil.copyfile(a.storage,bundle/'storage.json')
 if a.upgrade:shutil.copyfile(a.upgrade,bundle/'upgrade.json')
 for name in ['LICENSE-APACHE','PATCHES.md']:shutil.copyfile(PROJECT/'vendor/dav-server'/name,bundle/('dav-server-'+name))
 cid=subprocess.check_output(['docker','create',info['Id']],text=True).strip()
 try:subprocess.run(['docker','cp',cid+':/opt/agent-space/release.json',str(bundle/'release.json')],check=True)
 finally:subprocess.run(['docker','rm','-v',cid],check=True,stdout=subprocess.DEVNULL)
 release=json.loads((bundle/'release.json').read_text())
 if release['target']!=a.target or release['source_sha256']!=labels['io.runcove.source-sha256']:raise ValueError('embedded release metadata mismatch')
 manifest={'target':a.target,'image':result['image'],'image_id':info['Id'],'repo_digests':info.get('RepoDigests',[]),'application_version':version,'platform':'linux/amd64','source_revision':labels['org.opencontainers.image.revision'],'source_sha256':release['source_sha256'],'image_config_id':config_id,'package_tag':package_tag,'archive_sha256':sha(archive),'verification_sha256':sha(bundle/'verification.json'),'files_sha256':sha(bundle/'files.json'),'created_at_utc':stamp,'contents':'image, deployment, migration and validation artifacts; no user data or live credentials'}
 manifest['storage_sha256']=sha(bundle/'storage.json')
 (bundle/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
 (bundle/'SHA256SUMS').write_text(''.join(f'{sha(p)}  {p.relative_to(bundle).as_posix()}\n' for p in sorted(bundle.rglob('*')) if p.is_file()))
 zipped=destination/(bundle.name+'.zip')
 with zipfile.ZipFile(zipped,'w',compression=zipfile.ZIP_STORED,allowZip64=True) as output:
  for path in sorted(bundle.rglob('*')):
   if path.is_file():output.write(path,path.relative_to(bundle).as_posix())
 with zipfile.ZipFile(zipped) as verify:
  if verify.testzip() is not None:raise ValueError('ZIP integrity failed')
 digest=sha(zipped);zipped.with_suffix('.zip.sha256').write_text(f'{digest}  {zipped.name}\n')
 print(json.dumps({'package':str(zipped),'bytes':zipped.stat().st_size,'sha256':digest,'image_id':info['Id']},indent=2))
if __name__=='__main__':main()
