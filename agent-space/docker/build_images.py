#!/usr/bin/env python3
"""One build/cache/publication owner for Workspace, Server and Standalone."""
from contextlib import contextmanager
import fcntl
import fnmatch
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import uuid

ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / 'docker/.build'


def run(arguments, capture=False):
    result = subprocess.run([str(value) for value in arguments], check=True,
                            stdout=subprocess.PIPE if capture else None, text=True)
    return result.stdout.strip() if capture else None


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


@contextmanager
def locked(path):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('a+b') as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        yield


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def copy_synced(source, destination):
    shutil.copy2(source, destination, follow_symlinks=False)
    if not destination.is_symlink():
        sync_directory(destination)


def validate_publication(journal, manifest, owner):
    if manifest['root'] != str(owner.resolve()):
        raise ValueError('publication journal belongs to another owner')
    for index, entry in enumerate(manifest['entries']):
        destination, prepared = Path(entry['destination']), Path(entry['prepared'])
        if (not destination.is_absolute() or not destination.parent.resolve().is_relative_to(owner.resolve())
                or prepared.parent != destination.parent
                or prepared.name != '.runcove-build-' + journal.name + '-' + str(index)
                or entry['backup'] != str(index)):
            raise ValueError('invalid publication recovery path')


def restore_publication(journal, manifest, owner):
    validate_publication(journal, manifest, owner)
    for entry in manifest['entries']:
        destination, prepared = Path(entry['destination']), Path(entry['prepared'])
        if prepared.parent != destination.parent or not prepared.name.startswith('.runcove-build-'):
            raise ValueError('invalid publication recovery path')
        if entry['old']:
            prepared.unlink(missing_ok=True)
            copy_synced(journal / entry['backup'], prepared)
            os.replace(prepared, destination)
        else:
            destination.unlink(missing_ok=True)
        prepared.unlink(missing_ok=True)
        sync_directory(destination.parent)


def recover_publications(transactions=None, owner=ROOT):
    transactions = transactions or CACHE / 'publications'
    if not transactions.exists():
        return
    for journal in sorted(transactions.iterdir()):
        if journal.is_symlink() or not journal.is_dir() or not re.fullmatch(r'[0-9a-f]{32}', journal.name):
            raise ValueError('unknown build publication entry')
        manifest_file = journal / 'manifest.json'
        if manifest_file.exists():
            manifest = json.loads(manifest_file.read_text())
            validate_publication(journal, manifest, owner)
            if not (journal / 'committed').exists():
                restore_publication(journal, manifest, owner)
            else:
                for entry in manifest['entries']:
                    Path(entry['prepared']).unlink(missing_ok=True)
        shutil.rmtree(journal)


def publish_group(files, transactions=None, owner=ROOT):
    """Same-filesystem replacements plus a durable rollback journal.

    Every reader holds build.lock. After process termination, the next owner
    restores the previous generation before attempting any cache read/build.
    """
    transaction = uuid.uuid4().hex
    journal = (transactions or CACHE / 'publications') / transaction
    journal.mkdir(parents=True)
    entries = []
    for index, destination in enumerate(files):
        destination = Path(os.path.abspath(destination))
        destination.parent.mkdir(parents=True, exist_ok=True)
        old = destination.exists() or destination.is_symlink()
        if old:
            copy_synced(destination, journal / str(index))
        entries.append({'destination': str(destination), 'prepared': str(destination.parent / ('.runcove-build-' + transaction + '-' + str(index))), 'old': old, 'backup': str(index)})
    manifest = {'root': str(owner.resolve()), 'entries': entries}
    manifest_file = journal / 'manifest.json'
    pending_manifest = journal / 'manifest.pending'
    pending_manifest.write_text(json.dumps(manifest))
    sync_directory(pending_manifest)
    os.replace(pending_manifest, manifest_file)
    sync_directory(manifest_file)
    sync_directory(journal)
    try:
        for entry, source in zip(entries, files.values()):
            copy_synced(source, Path(entry['prepared']))
        for entry in entries:
            os.replace(entry['prepared'], entry['destination'])
            sync_directory(Path(entry['destination']).parent)
        (journal / 'committed').write_text('committed\n')
        sync_directory(journal / 'committed')
        sync_directory(journal)
    except BaseException:
        restore_publication(journal, manifest, owner)
        raise
    finally:
        # SIGKILL/TERM leaves the journal intact for the next invocation.
        if (journal / 'committed').exists():
            shutil.rmtree(journal)


def gzip_file(source, destination):
    with source.open('rb') as incoming, destination.open('wb') as output:
        with gzip.GzipFile(filename='', mode='wb', fileobj=output, mtime=0, compresslevel=6) as compressed:
            shutil.copyfileobj(incoming, compressed, 1024 * 1024)


def image_info(name):
    result = subprocess.run(['docker', 'image', 'inspect', name], capture_output=True, text=True)
    if result.returncode:
        if re.search(r'\bNo such image\b', result.stderr):
            return None
        raise ValueError('Docker inspect failed; image absence is unproven: ' + result.stderr.strip())
    values = json.loads(result.stdout)
    if len(values) != 1:
        raise ValueError('unexpected Docker image response')
    return values[0]


def require_oci_store():
    try:
        status = json.loads(run(['docker', 'info', '--format', '{{json .DriverStatus}}'], capture=True))
    except ValueError:
        status = None
    if not isinstance(status, list) or ['driver-type', 'io.containerd.snapshotter.v1'] not in status:
        raise ValueError('OCI image export requires Docker containerd image store; enable the containerd image store before building')


def require_image(name):
    info = image_info(name)
    if info is None or info.get('Os') != 'linux' or info.get('Architecture') != 'amd64':
        raise ValueError('expected a local linux/amd64 image: ' + name)
    return info


def read_base_images(project):
    images = json.loads((project / 'docker/base-images.lock.json').read_text())
    keys = {'pythonNode', 'rustBuilder', 'serverUbuntu'}
    if not isinstance(images, dict) or images.keys() != keys | {'workspaceSourceDateEpoch'}:
        raise ValueError('base-images.lock.json must define the three build images and the Workspace epoch')
    for key in sorted(keys):
        reference = images[key]
        if not isinstance(reference, str) or not re.fullmatch(r'[a-z0-9][a-z0-9./:_-]*@sha256:[0-9a-f]{64}', reference):
            raise ValueError('expected an immutable image reference in base-images.lock.json: ' + key)
    epoch = images['workspaceSourceDateEpoch']
    if type(epoch) is not int or not 0 <= epoch <= min(int(time.time()), 253402300799):
        raise ValueError('invalid Workspace epoch in base-images.lock.json')
    return images


def input_digest(project, *, base_image, epoch):
    # COPY trees are recursive; repository-relative paths make equal checkouts
    # share an identity. Ignore policy and exporter are themselves inputs.
    paths = [project / '.dockerignore', project / 'docker/build_images.py']
    for directory in ('images/desktop', 'guest/desktop'):
        paths.extend(path for path in (project / directory).rglob('*') if path.is_file())
    digest = hashlib.sha256()
    # Only the selected base image and Workspace epoch affect this target.
    # Changing application revisions or other base images can reuse its cache.
    digest.update(b'runcove-workspace-base-v2\0' + base_image.encode('ascii') + b'\0' + str(epoch).encode('ascii') + b'\0')
    for path in sorted(set(paths)):
        relative = path.relative_to(project)
        if (any(part in ('node_modules', '__pycache__', '.git', '.secrets') for part in relative.parts)
                or path.suffix == '.md'
                or any(fnmatch.fnmatch(path.name, pattern) for pattern in ('.env', '.env.*', '*.key', '*.p12', '*.pfx', 'id_rsa', 'id_ed25519'))):
            continue
        name, data = relative.as_posix().encode(), path.read_bytes()
        digest.update(len(name).to_bytes(8, 'big') + name)
        digest.update(len(data).to_bytes(8, 'big') + data)
    return digest.hexdigest()


def build_id(dockerfile, directory, arguments=(), project=ROOT, *, epoch):
    iidfile = directory / 'image.iid'
    metadata_file = directory / 'image.metadata.json'
    # BuildKit's default provenance adds a fresh attestation index even for an
    # entirely cached build. Release identity is recorded by this build owner;
    # avoid changing content tags solely because that wrapper has a new time.
    run(['docker', 'build', '--platform', 'linux/amd64', '--iidfile', iidfile,
         '--metadata-file', metadata_file, '--provenance=false',
         '--build-arg', 'SOURCE_DATE_EPOCH='+str(epoch),
         '--output', 'type=image,unpack=false,rewrite-timestamp=true',
         *arguments, '-f', dockerfile, project])
    iid = iidfile.read_text().strip()
    metadata = json.loads(metadata_file.read_text())
    config, distribution = metadata['containerimage.config.digest'], metadata['containerimage.digest']
    identities = {config, distribution}
    if any(not re.fullmatch(r'sha256:[0-9a-f]{64}', value) for value in identities) or iid not in identities:
        raise ValueError('build did not return a valid image identity')
    # With provenance disabled Docker29's iidfile contains the config digest,
    # but its containerd store may only accept the platform manifest as an ID.
    # Classic stores still use the config. Both must match this build metadata.
    for candidate in dict.fromkeys((distribution, iid)):
        info = image_info(candidate)
        if info is not None:
            if info.get('Os') != 'linux' or info.get('Architecture') != 'amd64' or info['Id'] not in identities:
                raise ValueError('build image identity or platform differs from metadata')
            return info['Id']
    raise ValueError('built image is not available in the local Docker store')


def workspace(output, project, *, base_image, epoch):
    output.mkdir(parents=True, exist_ok=True)
    browser = json.loads(run([sys.executable, project/'images/desktop/install_browser.py', 'build-args',
                              '--lock', project/'images/desktop/chromium-download.lock.json'], capture=True))
    arguments = ['--build-arg', 'PYTHON_NODE_IMAGE='+base_image,
                 '--build-arg', 'CHROMIUM_URL='+browser['CHROMIUM_URL'],
                 '--build-arg', 'CHROMIUM_SHA256='+browser['CHROMIUM_SHA256']]
    source = input_digest(project, base_image=base_image, epoch=epoch)
    try:
        iid = (output / 'workspace.image-id').read_text().strip()
        info = image_info(iid)
        ready = json.loads((output / 'desktop.commit.json').read_text())
        if (info and info['Id'] == iid and info.get('Os') == 'linux' and info.get('Architecture') == 'amd64'
                and (output / 'desktop.source').read_text().strip() == source
                and ready == {'source': source, 'imageId': iid, 'reference': (output / 'desktop.reference').read_text().strip(), 'archive': sha256(output / 'desktop.tar.gz')}
                and (output / 'desktop.archive.sha256').read_text().strip() == ready['archive']):
            workspace_lock(output, project)
            print('[desktop] using verified offline image ' + iid)
            return iid
    except (OSError, ValueError, KeyError):
        pass
    with tempfile.TemporaryDirectory(prefix='desktop-export-', dir=output) as temporary:
        stage = Path(temporary)
        iid = build_id(project / 'images/desktop/Dockerfile', stage, arguments, project=project, epoch=epoch)
        run(['docker', 'save', iid, '-o', stage / 'docker.tar'])
        reference = run([sys.executable, project / 'images/desktop/export.py', stage / 'docker.tar',
                         stage / 'desktop.tar', '--lock', stage / 'workspace.lock.json', '--expected-image', iid], capture=True)
        record = json.loads((stage / 'workspace.lock.json').read_text())
        if reference != record['reference'] or not re.fullmatch(r'runcove/runcove-workspace@sha256:[0-9a-f]{64}', reference):
            raise ValueError('exported workspace identity is invalid')
        gzip_file(stage / 'desktop.tar', stage / 'desktop.tar.gz')
        archive = sha256(stage / 'desktop.tar.gz')
        for name, text in {'desktop.reference': reference, 'desktop.source': source,
                           'desktop.archive.sha256': archive, 'workspace.image-id': iid}.items():
            (stage / name).write_text(text + '\n')
        (stage / 'desktop.commit.json').write_text(json.dumps({'source': source, 'imageId': iid, 'reference': reference, 'archive': archive}) + '\n')
        files = {output / name: stage / name for name in ('desktop.tar.gz', 'desktop.reference', 'desktop.source', 'desktop.archive.sha256', 'workspace.image-id')}
        files[output / 'workspace.lock.json'] = stage / 'workspace.lock.json'
        files[output / 'desktop.commit.json'] = stage / 'desktop.commit.json'
        publish_group(files, output / '.publications', output)
        workspace_lock(output, project)
        return iid


def workspace_lock(output, project):
    record = json.loads((output / 'workspace.lock.json').read_text())
    reference = (output / 'desktop.reference').read_text().strip()
    if record != {'kind': 'desktop', 'preset': 'runcove-desktop', 'reference': reference, 'workerContract': 1}:
        raise ValueError('workspace lock differs from verified cache')
    publish_group({ROOT / 'images/desktop-image.lock.json': output / 'workspace.lock.json'})
    if project != ROOT:
        shutil.copy2(output / 'workspace.lock.json', project / 'images/desktop-image.lock.json')


def snapshot_source(destination):
    # Docker reads this private copy, never a live worktree whose bytes may
    # change after the recorded SOURCE_DIGEST. Runtime caches are pinned later.
    destination.mkdir()
    for name in ('Cargo.toml', 'Cargo.lock', 'msb-runtime.sha256', '.dockerignore'):
        if (ROOT / name).exists():
            shutil.copy2(ROOT / name, destination / name)
    def ignored(directory, names):
        relative = Path(directory).relative_to(ROOT)
        return [name for name in names if name in ('target', '.git', 'node_modules', '__pycache__', 'dist', '.secrets')
                or (relative == Path('docker') and name in ('.build', 'configs'))
                or any(fnmatch.fnmatch(name, pattern) for pattern in ('.env', '.env.*', '*.key', '*.p12', '*.pfx', 'id_rsa', 'id_ed25519'))]
    for name in ('src', 'agent-space-mcp', 'static-http', 'webdav', 'build-support', 'guest', 'desktop-helper', 'desktop-ui', 'images', 'vendor', 'docker'):
        if (ROOT / name).exists():
            shutil.copytree(ROOT / name, destination / name, symlinks=True, ignore=ignored)


def runtime_inputs():
    version = next(item['version'] for item in tomllib.loads((ROOT / 'Cargo.lock').read_text())['package'] if item['name'] == 'microsandbox')
    if json.loads((ROOT / 'docker/patches/manifest.json').read_text())['version'] != version:
        raise ValueError('SDK differs from audited runtime manifest')
    home = Path(os.environ.get('MSB_HOME', str(Path.home() / '.microsandbox')))
    binary = home / 'bin/msb'
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError('microsandbox runtime is missing at ' + str(home))
    if run([binary, '--version'], capture=True).split()[-1] != version:
        raise ValueError('microsandbox runtime version differs from SDK')
    libraries, seen = [], set()
    for line in (ROOT / 'msb-runtime.sha256').read_text().splitlines():
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        record = re.fullmatch(r'([0-9a-f]{64})[ \t]+\*?(bin/msb|lib/libkrunfw\.so\.\d+(?:\.\d+)+)', line.strip())
        if not record:
            raise ValueError('invalid audited runtime checksum record')
        digest, name = record.groups()
        if name in seen or sha256(home / name) != digest:
            raise ValueError('runtime does not match verified artifacts: ' + name)
        seen.add(name)
        if re.fullmatch(r'lib/libkrunfw\.so\.\d+(?:\.\d+)+', name):
            libraries.append(home / name)
    if len(libraries) != 1 or 'bin/msb' not in seen:
        raise ValueError('expected audited msb and exactly one libkrunfw file')
    return version, binary, libraries[0]


def prepare_runtime(version, binary, library, *, seed):
    images, libs = CACHE / 'images', CACHE / 'libs'
    images.mkdir(parents=True, exist_ok=True)
    libs.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='runtime-', dir=CACHE) as temporary:
        stage = Path(temporary)
        shutil.copy2(library, stage / library.name)
        major = re.match(r'libkrunfw\.so\.\d+', library.name)[0]
        (stage / major).symlink_to(library.name)
        (stage / 'libkrunfw.so').symlink_to(major)
        publish_group({libs / name: stage / name for name in (library.name, major, 'libkrunfw.so')})
        for stale in libs.glob('libkrunfw.so*'):
            if stale.name not in (library.name, major, 'libkrunfw.so'):
                stale.unlink()
        try:
            recorded = json.loads((images / 'default.identity.json').read_text())
            if recorded == {'source': seed, 'runtimeVersion': version, 'archive': sha256(images / 'default.tar.gz')}:
                return
        except (OSError, ValueError, KeyError):
            pass
        # A clean MSB_HOME need not have this seed materialized. The CLI's
        # default pull policy reuses an existing immutable digest without -f.
        run([binary, 'image', 'pull', seed, '-q'])
        run([binary, 'image', 'save', seed, '-o', stage / 'default.tar', '-q'])
        gzip_file(stage / 'default.tar', stage / 'default.tar.gz')
        (stage / 'default.identity.json').write_text(json.dumps({'source': seed, 'runtimeVersion': version, 'archive': sha256(stage / 'default.tar.gz')}) + '\n')
        publish_group({images / name: stage / name for name in ('default.tar.gz', 'default.identity.json')})


def publish_tags(iid, names, immutable):
    # Check-and-tag is serialized across this user's checkouts sharing a daemon.
    with locked(Path.home() / '.cache/runcove/image-publication.lock'):
        for name in immutable:
            existing = image_info(name)
            if existing and existing['Id'] != iid:
                raise ValueError('refusing to replace immutable content tag: ' + name)
        for name in dict.fromkeys(names):
            run(['docker', 'tag', iid, name])
            if require_image(name)['Id'] != iid:
                raise ValueError('image identity changed during publication: ' + name)


def main():
    target = sys.argv[1]
    if target not in ('workspace', 'server', 'standalone'):
        raise ValueError('unknown build target')
    if len(sys.argv) > (3 if target == 'workspace' else 2):
        raise ValueError('unexpected build arguments')
    alias = os.environ.get('IMAGE_NAME', '')
    if alias and (not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9._/:\-]*', alias) or '@' in alias):
        raise ValueError('invalid image alias')
    if re.fullmatch(r'agent-space:\d+\.\d+\.\d+', alias):
        raise ValueError('refusing to replace a historical SDK-version tag')
    require_oci_store()
    revision = run(['git', '-C', ROOT, 'rev-parse', 'HEAD'], capture=True)
    if not re.fullmatch(r'[0-9a-fA-F]{40,64}', revision):
        raise ValueError('checkout has no valid HEAD')
    revision = revision.lower()
    raw_epoch = run(['git', '-C', ROOT, 'show', '-s', '--format=%ct', revision], capture=True)
    if (not re.fullmatch(r'0|[1-9][0-9]{0,11}', raw_epoch)
            or int(raw_epoch) > 253402300799):
        raise ValueError('git commit epoch must be canonical nonnegative Unix seconds through year 9999')
    epoch = int(raw_epoch)
    if epoch > int(time.time()):
        raise ValueError('git commit epoch is in the future; deterministic timestamp export requires a past or current epoch')
    for required in ('images/desktop/Dockerfile', 'images/desktop/export.py', 'images/desktop/install_browser.py',
                     'images/desktop/chromium-download.lock.json', '.dockerignore'):
        if not (ROOT / required).is_file():
            raise ValueError('missing build input: ' + required)
    if target != 'workspace':
        required = 'docker/Dockerfile.standalone' if target == 'standalone' else 'docker/Dockerfile'
        if not (ROOT / required).is_file() or not (ROOT / 'docker/release_identity.py').is_file():
            raise ValueError('missing application build input')
    runtime = runtime_inputs() if target == 'server' else None
    output = Path(sys.argv[2]).resolve() if target == 'workspace' and len(sys.argv) == 3 else CACHE / 'images'
    with locked(CACHE / 'build.lock'), locked(output / '.build.lock'), tempfile.TemporaryDirectory(prefix='context-', dir=CACHE) as temporary_context:
        recover_publications()
        recover_publications(output / '.publications', output)
        project = Path(temporary_context) / 'source'
        snapshot_source(project)
        images = read_base_images(project)
        iid = workspace(output, project, base_image=images['pythonNode'], epoch=images['workspaceSourceDateEpoch'])
        if target == 'workspace':
            print('[desktop] built ' + iid)
            return
        if runtime:
            prepare_runtime(*runtime, seed=images['pythonNode'])
            config = ROOT / 'docker/configs/config.toml'
            if not config.exists():
                config.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / 'docker/configs.example/config.toml', config)
        if runtime:
            for directory in ('images', 'libs'):
                cache_destination = project / 'docker/.build' / directory
                cache_destination.mkdir(parents=True, exist_ok=True)
                names = ('default.tar.gz', 'desktop.tar.gz', 'desktop.reference') if directory == 'images' else tuple(path.name for path in (CACHE / 'libs').glob('libkrunfw.so*'))
                for name in names:
                    shutil.copy2(CACHE / directory / name, cache_destination / name, follow_symlinks=False)
        source = run([sys.executable, project / 'docker/release_identity.py'], capture=True)
        if not re.fullmatch(r'[0-9a-f]{64}', source):
            raise ValueError('invalid source identity')
        build_inputs = {'sourceSha256': source, 'sourceRevision': revision, 'sourceDateEpoch': epoch}
        build_identity = hashlib.sha256(json.dumps(build_inputs, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        version = 'runcove-' + build_identity[:16]
        tag = 'runcove-' + target + ':' + version
        arguments = ['--build-arg', 'APP_VERSION=' + version, '--build-arg', 'SOURCE_REVISION=' + revision,
                     '--build-arg', 'SOURCE_DIGEST=' + source, '--build-arg', 'PYTHON_NODE_IMAGE=' + images['pythonNode'],
                     '--build-arg', 'RUST_BUILDER_IMAGE=' + images['rustBuilder']]
        if target == 'standalone':
            base = 'runcove-workspace:sha256-' + iid.removeprefix('sha256:')
            publish_tags(iid, [base], [base])
            arguments += ['--build-arg', 'WORKSPACE_IMAGE=' + base]
        else:
            arguments += ['--build-arg', 'SERVER_UBUNTU_IMAGE=' + images['serverUbuntu'],
                          '--build-arg', 'MSB_LIB_VERSION=' + runtime[0]]
        dockerfile = project / 'docker' / ('Dockerfile.standalone' if target == 'standalone' else 'Dockerfile')
        with tempfile.TemporaryDirectory(prefix=target + '-', dir=CACHE) as temporary:
            candidate = build_id(dockerfile, Path(temporary), arguments, project, epoch=epoch)
            if target == 'standalone' and require_image(base)['Id'] != iid:
                raise ValueError('workspace identity changed during build')
            names = [tag] + ([alias] if alias else [])
            immutable = [name for name in names if name == tag or re.search(r':(?:runcove-|sha256-)', name)]
            publish_tags(candidate, names, immutable)
            print('Built ' + tag + ' (' + candidate + ')')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print('[build] ERROR: ' + str(error), file=sys.stderr)
        raise SystemExit(1)
