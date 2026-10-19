"""Package the locally built Windows binary and the complete Mac build sources."""
from pathlib import Path
import shutil
import subprocess
import zipfile

root = Path(__file__).resolve().parent.parent
destination = root.parent.parent / "output" / "CrossDesk-0.1.0"
destination.mkdir(parents=True, exist_ok=True)
(destination / "scripts").mkdir(exist_ok=True)
binary = root / "target/release/crossdesk.exe"
if not binary.is_file():
    raise SystemExit("Build the Windows release binary before packaging.")

for name in ("README.md", "LICENSE", "THIRD_PARTY_NOTICES.md", "UPSTREAM_README.md", "VALIDATION.md", "Start Windows.cmd", "scripts/start-windows.ps1"):
    shutil.copy2(root / name, destination / name)
shutil.copy2(binary, destination / "crossdesk.exe")

def add_file(archive, path, name):
    info = zipfile.ZipInfo(name)
    info.create_system = 3
    info.compress_type = zipfile.ZIP_DEFLATED
    executable = path.suffix in (".command", ".sh")
    info.external_attr = ((0o100755 if executable else 0o100644) << 16)
    archive.writestr(info, path.read_bytes())

files = subprocess.check_output(
    ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=root
).decode("utf-8").split("\0")
with zipfile.ZipFile(destination / "CrossDesk-Mac-source.zip", "w") as archive:
    for name in sorted(set(files)):
        path = root / name
        if path.is_file() and path.suffix != ".key" and name != "mac-server.txt":
            add_file(archive, path, "CrossDesk-source/" + name)

windows_files = ("crossdesk.exe", "README.md", "LICENSE", "THIRD_PARTY_NOTICES.md", "UPSTREAM_README.md", "VALIDATION.md", "Start Windows.cmd", "scripts/start-windows.ps1")
with zipfile.ZipFile(destination / "CrossDesk-Windows.zip", "w") as archive:
    for name in windows_files:
        add_file(archive, destination / name, "CrossDesk/" + name)

for archive_path in destination.glob("*.zip"):
    with zipfile.ZipFile(archive_path) as archive:
        assert archive.testzip() is None
        assert not any(name.endswith(".key") for name in archive.namelist())
        if "Mac-source" in archive_path.name:
            entry = archive.getinfo("CrossDesk-source/Build and Start Mac.command")
            assert (entry.external_attr >> 16) & 0o111
    print(f"{archive_path}: {archive_path.stat().st_size:,} bytes")
