import subprocess
import tempfile
import unittest
from pathlib import Path
from package_sources import source_changes


class CorrespondingSourceTests(unittest.TestCase):
    def test_native_build_rewrites_are_recorded_and_application_edits_are_refused(self):
        with tempfile.TemporaryDirectory(prefix='crossdesk-source-test-') as directory:
            root = Path(directory)
            def git(*args, **kwargs):
                return subprocess.run(['git', *args], cwd=root, check=True, capture_output=True, **kwargs)
            git('init')
            git('config', 'core.autocrlf', 'false')
            (root / 'web/dist').mkdir(parents=True)
            (root / 'src').mkdir()
            (root / 'Cargo.toml').write_text('[package]\nname = "crossdesk"\n')
            (root / 'web/dist/app.js').write_text('original bundle\n')
            (root / 'src/main.rs').write_text('fn main() {}\n')
            git('add', '.')
            git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-m', 'fixture')
            self.assertEqual(source_changes(root), b'')
            (root / 'web/dist/app.js').write_text('native rebuilt bundle\n')
            (root / 'Cargo.toml').write_text('[package]\nname = "crossdesk"\n# native build rewrite\n')
            patch = source_changes(root)
            self.assertIn(b'native rebuilt bundle', patch)
            git('apply', '--reverse', input=patch)
            self.assertEqual(source_changes(root), b'')
            (root / 'src/main.rs').write_text('fn main() { panic!("uncommitted"); }\n')
            with self.assertRaisesRegex(RuntimeError, 'Commit application source'):
                source_changes(root)


if __name__ == '__main__':
    unittest.main()
