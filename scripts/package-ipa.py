"""Validate a native iOS app and archive it for TrollStore, without Apple signing."""
import hashlib
import json
import plistlib
import re
import subprocess
import sys
import zipfile
from pathlib import Path


def package(source: Path, destination: Path):
    apps = list(source.glob('*.app'))
    if len(apps) != 1:
        raise ValueError(f'Expected exactly one .app in {source}; found {len(apps)}')
    app = apps[0]
    with (app / 'Info.plist').open('rb') as f:
        info = plistlib.load(f)
    minimum = tuple(int(v) for v in info['MinimumOSVersion'].split('.'))
    if not (15, 0) <= minimum <= (15, 8, 7):
        raise ValueError(f'Unsupported minimum OS: {minimum}')
    if 2 not in info.get('UIDeviceFamily', []):
        raise ValueError('App does not declare iPad support')
    if info.get('UIRequiresFullScreen') is not True:
        raise ValueError('App must require full screen (no iPad Split View or Slide Over)')
    executable = app / info['CFBundleExecutable']
    archs = subprocess.check_output(['xcrun', 'lipo', '-archs', str(executable)], text=True).strip()
    if 'arm64' not in archs.split():
        raise ValueError(f'Missing arm64 device executable: {archs}')
    load_commands = subprocess.check_output(['xcrun', 'otool', '-l', str(executable)], text=True)
    if re.search(r'platform\s+(?:IOSSIMULATOR|7)\b', load_commands):
        raise ValueError('Executable is an iOS simulator build')
    device_minos = re.findall(r'\bminos\s+(\d+\.\d+(?:\.\d+)?)', load_commands)
    for value in device_minos:
        if tuple(map(int, value.split('.'))) > (15, 8, 7):
            raise ValueError(f'Mach-O deployment minimum exceeds target: {value}')
    destination.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(destination, 'w', zipfile.ZIP_DEFLATED) as z:
        for file in sorted(app.rglob('*')):
            if file.is_file():
                z.write(file, 'Payload/' + app.name + '/' + file.relative_to(app).as_posix())
    with zipfile.ZipFile(destination) as z:
        if z.testzip() is not None:
            raise ValueError('IPA archive integrity failed')
    digest = hashlib.sha256(destination.read_bytes()).hexdigest()
    destination.with_suffix('.ipa.sha256').write_text(digest + '  ' + destination.name + '\n')
    evidence = {k: info.get(k) for k in ('CFBundleIdentifier', 'CFBundleShortVersionString', 'CFBundleVersion', 'MinimumOSVersion', 'UIDeviceFamily', 'UIRequiresFullScreen')}
    evidence.update(architectures=archs, macho_minimums=device_minos, sha256=digest,
                    signed_for_app_store=False, installation='TrollStore only; installation/launch not verified by packaging')
    destination.with_suffix('.build.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print(json.dumps(evidence, indent=2))


if __name__ == '__main__':
    package(Path(sys.argv[1]), Path(sys.argv[2]))
