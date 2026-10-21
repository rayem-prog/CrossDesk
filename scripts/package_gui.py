"""Package a native CrossDesk GUI, corresponding source and dependency notices."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--artifact', type=Path, required=True)
parser.add_argument('--platform', choices=('Mac-ARM64', 'Windows-x64'), required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
if not args.artifact.exists():
    parser.error('native app/installer does not exist')
cargo = tomllib.loads((ROOT / 'Cargo.toml').read_text())
config = json.loads((ROOT / 'tauri.conf.json').read_text())
version = cargo['package']['version']
assert config['productName'] == 'CrossDesk' and config['version'] == version
assert {'pairing', 'discovery'}.issubset(cargo['features']['gui'])
subprocess.run(['git', 'diff', '--exit-code', 'HEAD', '--'], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
name = f'CrossDesk-{version}-{args.platform}-GUI'
args.output.mkdir(parents=True, exist_ok=True)
archive_path = args.output / f'{name}.zip'
commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT).decode().strip()
tracked = subprocess.check_output(['git', 'ls-files', '-z'], cwd=ROOT).decode().split('\0')
target = 'aarch64-apple-darwin' if args.platform == 'Mac-ARM64' else 'x86_64-pc-windows-msvc'
metadata = json.loads(subprocess.check_output([
    'cargo', 'metadata', '--format-version', '1', '--locked', '--features', 'gui',
    '--filter-platform', target,
], cwd=ROOT))

def add(archive, path, relative):
    entry = zipfile.ZipInfo(f'{name}/{relative}')
    entry.create_system = 3
    entry.compress_type = zipfile.ZIP_DEFLATED
    if path.is_symlink():
        entry.external_attr = (stat.S_IFLNK | 0o777) << 16
        archive.writestr(entry, os.readlink(path).encode())
    else:
        mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
        entry.external_attr = (stat.S_IFREG | mode) << 16
        archive.writestr(entry, path.read_bytes())

with zipfile.ZipFile(archive_path, 'w', compresslevel=6) as archive:
    artifact = args.artifact.resolve()
    if artifact.is_dir():
        for path in sorted(artifact.rglob('*')):
            if path.is_file() or path.is_symlink():
                add(archive, path, f'{artifact.name}/{path.relative_to(artifact).as_posix()}')
    else:
        add(archive, artifact, artifact.name)
    for relative in ('README.md', 'VALIDATION.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'UPSTREAM_README.md'):
        add(archive, ROOT / relative, relative)
    for relative in sorted(set(tracked)):
        if not relative:
            continue
        path = ROOT / relative
        if path.suffix == '.key' or path.name in ('mac-server.txt', 'preferences.json', 'device-id'):
            raise RuntimeError(f'private application data must not be packaged: {relative}')
        if path.is_file():
            add(archive, path, f'source/{relative}')
    dependencies = [f'Native dependencies for {target}, GUI/pairing/discovery enabled:']
    for package in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
        if package['source'] is None:
            continue
        dependency = f'{package["name"]}-{package["version"]}'
        directory = Path(package['manifest_path']).parent
        dependencies.append(f'{dependency}: {package["license"] or "see license file"}; {package["source"]}')
        for path in sorted(directory.rglob('*')):
            if path.is_file() and path.name not in ('.cargo-ok', '.cargo-vcs-info.json'):
                add(archive, path, f'third-party-source/{dependency}/{path.relative_to(directory).as_posix()}')
    # Include installed JavaScript package license/notice files, including bundled subdependencies.
    web = ROOT / 'web/node_modules'
    web_index = []
    for manifest in sorted(web.glob('*/package.json')) + sorted(web.glob('@*/*/package.json')):
        package = json.loads(manifest.read_text())
        web_index.append(f'{package.get("name")}@{package.get("version")}: {package.get("license", "see notices")}')
    for path in sorted(web.rglob('*')):
        if path.is_file() and path.name.lower().startswith(('license', 'licence', 'notice', 'copying')):
            add(archive, path, f'web-licenses/{path.relative_to(web).as_posix()}')
    archive.writestr(f'{name}/WEB_DEPENDENCIES.txt', '\n'.join(web_index) + '\n')
    archive.writestr(f'{name}/THIRD_PARTY_SOURCES.txt', '\n'.join(dependencies) + '\n')
    archive.writestr(f'{name}/COMMIT.txt', f'CrossDesk {version}\nSource commit: {commit}\nBuild: --locked --features gui\n')
with zipfile.ZipFile(archive_path) as archive:
    assert archive.testzip() is None
    assert not any(n.endswith('.key') and '/third-party-source/' not in n for n in archive.namelist())
    assert f'{name}/source/Cargo.lock' in archive.namelist()
    assert f'{name}/source/src/easy_connect/mod.rs' in archive.namelist()
    if args.platform == 'Mac-ARM64':
        assert archive.getinfo(f'{name}/CrossDesk.app/Contents/MacOS/crossdesk').external_attr >> 16 & 0o111
checksum = hashlib.file_digest(archive_path.open('rb'), 'sha256').hexdigest()
archive_path.with_suffix('.zip.sha256').write_text(f'{checksum}  {archive_path.name}\n')
print(f'{archive_path}: {archive_path.stat().st_size:,} bytes; source {commit}; SHA256 {checksum}')
