"""Package one native binary with launch scripts and its corresponding project source."""
import argparse
from pathlib import Path
import subprocess
import tomllib
import zipfile

root = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--platform", choices=("Mac-ARM64", "Windows-x64"), required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
if not args.binary.is_file():
    parser.error("native binary does not exist")
version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
name = f"CrossDesk-{version}-{args.platform}"
args.output.mkdir(parents=True, exist_ok=True)
archive_path = args.output / f"{name}.zip"
source_paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root).decode().strip()


def add(archive, path, relative, executable=False):
    entry = zipfile.ZipInfo(f"{name}/{relative}")
    entry.create_system = 3
    entry.compress_type = zipfile.ZIP_DEFLATED
    mode = 0o100755 if executable or path.suffix in (".command", ".sh") else 0o100644
    entry.external_attr = mode << 16
    archive.writestr(entry, path.read_bytes())


with zipfile.ZipFile(archive_path, "w") as archive:
    for relative in ("README.md", "VALIDATION.md", "LICENSE", "THIRD_PARTY_NOTICES.md", "UPSTREAM_README.md"):
        add(archive, root / relative, relative)
    if args.platform == "Mac-ARM64":
        add(archive, args.binary, "crossdesk", executable=True)
        for relative in ("Start Mac.command", "scripts/start-mac-binary.sh"):
            add(archive, root / relative, relative)
    else:
        add(archive, args.binary, "crossdesk.exe", executable=True)
        for relative in ("Start Windows.cmd", "scripts/start-windows.ps1"):
            add(archive, root / relative, relative)
    for relative in sorted(set(source_paths)):
        if not relative:
            continue
        path = root / relative
        if path.suffix == ".key" or path.name == "mac-server.txt":
            raise RuntimeError("private pairing material must not be tracked or packaged")
        if path.is_file():
            add(archive, path, f"source/{relative}")
    archive.writestr(f"{name}/COMMIT.txt", f"CrossDesk {version}\nSource commit: {commit}\n")
with zipfile.ZipFile(archive_path) as archive:
    assert archive.testzip() is None
    assert not any(n.endswith(".key") for n in archive.namelist())
    assert f"{name}/source/Cargo.lock" in archive.namelist()
    if args.platform == "Mac-ARM64":
        assert archive.getinfo(f"{name}/crossdesk").external_attr >> 16 & 0o111
print(f"{archive_path}: {archive_path.stat().st_size:,} bytes; source {commit}")
