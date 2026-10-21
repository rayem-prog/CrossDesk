"""Record reproducible native build rewrites without accepting edited application code."""
from pathlib import Path
import subprocess


def source_changes(root: Path) -> bytes:
    changed = subprocess.check_output(['git', 'diff', '--name-only', '-z', 'HEAD', '--'], cwd=root).decode().split('\0')
    changed = [name for name in changed if name]
    unexpected = [name for name in changed if name != 'Cargo.toml' and not name.startswith('web/dist/')]
    if unexpected:
        raise RuntimeError(f'Commit application source before packaging: {unexpected}')
    if not changed:
        return b''
    print('Recording native build rewrites:', ', '.join(changed))
    return subprocess.check_output(['git', 'diff', '--binary', 'HEAD', '--'], cwd=root)
