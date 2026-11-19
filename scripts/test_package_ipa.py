import importlib.util
import json
import plistlib
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('package_ipa', Path(__file__).with_name('package-ipa.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.app = self.root / 'build' / 'ihomeview.app'
        self.app.mkdir(parents=True)
        self.info = {'CFBundleIdentifier': 'io.github.thedtvn.ihomeview',
                     'CFBundleExecutable': 'ihomeview', 'MinimumOSVersion': '15.0',
                     'UIDeviceFamily': [2], 'CFBundleVersion': '1', 'UIRequiresFullScreen': True}
        self.write_info()
        (self.app / 'ihomeview').write_bytes(b'fixture, not an executable')
        self.out = self.root / 'artifacts' / 'ihomeview.ipa'
        self.addCleanup(self.temp.cleanup)

    def write_info(self):
        (self.app / 'Info.plist').write_bytes(plistlib.dumps(self.info))

    @patch.object(module.subprocess, 'check_output', side_effect=['arm64', 'platform IOS\nminos 15.0'])
    def test_payload_and_evidence(self, _):
        module.package(self.app.parent, self.out)
        with zipfile.ZipFile(self.out) as archive:
            self.assertIn('Payload/ihomeview.app/Info.plist', archive.namelist())
            info = plistlib.loads(archive.read('Payload/ihomeview.app/Info.plist'))
            self.assertIs(info['UIRequiresFullScreen'], True)
        self.assertIs(json.loads(self.out.with_suffix('.build.json').read_text())['UIRequiresFullScreen'], True)
        self.assertEqual(json.loads(self.out.with_suffix('.build.json').read_text())['architectures'], 'arm64')

    def test_rejects_multitasking(self):
        for value in (False, None, 'true'):
            with self.subTest(value=value):
                self.info.pop('UIRequiresFullScreen', None)
                if value is not None:
                    self.info['UIRequiresFullScreen'] = value
                self.write_info()
                with self.assertRaisesRegex(ValueError, 'must require full screen'):
                    module.package(self.app.parent, self.out)

    def test_rejects_newer_ipados(self):
        self.info['MinimumOSVersion'] = '16.0'
        self.write_info()
        with self.assertRaisesRegex(ValueError, 'Unsupported minimum'):
            module.package(self.app.parent, self.out)

    @patch.object(module.subprocess, 'check_output', side_effect=['arm64', 'platform 7\nminos 15.0'])
    def test_rejects_simulator(self, _):
        with self.assertRaisesRegex(ValueError, 'simulator'):
            module.package(self.app.parent, self.out)

    @patch.object(module.subprocess, 'check_output', side_effect=['arm64', 'platform IOS\nminos 17.0'])
    def test_rejects_macho_minimum(self, _):
        with self.assertRaisesRegex(ValueError, 'Mach-O deployment'):
            module.package(self.app.parent, self.out)


if __name__ == '__main__':
    unittest.main()
