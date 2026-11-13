"""Inspect actual release downloads; no separate source/receipt admission."""
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import struct
import subprocess
import tarfile
import zipfile

BUNDLE = 'com.crispstrobe.crispsorter'


def gh(*args):
    return subprocess.check_output(['gh', *args], text=True)


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def download_packages(repo, tag, version, expected, is_package, output):
    if tag != 'v' + version or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', version):
        raise ValueError('Release tag must match the application version')
    selected = json.loads(gh('release', 'view', tag, '--repo', repo, '--json', 'databaseId'))
    release = json.loads(gh('api', f"repos/{repo}/releases/{selected['databaseId']}"))
    if release.get('draft') is not True:
        raise ValueError('Refusing to modify a published release')
    assets = release.get('assets', [])
    names = [a['name'] for a in assets]
    if len(names) != len(set(names)) or {n for n in names if is_package(n)} != expected:
        raise ValueError('Missing, unsigned, or stale release packages')
    downloaded = {}
    for asset in assets:
        name = asset['name']
        if name not in expected:
            continue
        gh('release', 'download', tag, '--repo', repo, '--pattern', name, '--dir', str(output))
        path = output / name
        if not path.is_file() or path.is_symlink() or path.stat().st_size <= 0 or path.stat().st_size != asset['size']:
            raise ValueError('Downloaded package size differs from release asset')
        digest = sha(path)
        if asset.get('digest') is not None and asset['digest'] != 'sha256:' + digest:
            raise ValueError('Downloaded package digest differs from release asset')
        downloaded[name] = path
        print(f'Verified package bytes: {name} SHA-256 {digest}')
    return downloaded


def safe_name(name):
    path = PurePosixPath(name)
    return not path.is_absolute() and '..' not in path.parts and '\\' not in name


def zip_inventory(archive):
    entries = archive.infolist()
    names = [entry.filename for entry in entries]
    if (len(entries) > 50_000 or len(names) != len(set(names))
            or not all(safe_name(n) for n in names)
            or sum(entry.file_size for entry in entries) > 4 * 1024**3):
        raise ValueError('Unsafe or oversized package archive')
    return names


def windows_zip(path):
    with zipfile.ZipFile(path) as archive:
        zip_inventory(archive)
        exe = archive.getinfo('crispsorter.exe')
        if exe.is_dir() or exe.file_size < 64:
            raise ValueError('Windows package lacks its application executable')
        with archive.open(exe) as stream:
            dos = stream.read(64)
            offset = struct.unpack_from('<I', dos, 60)[0]
            if dos[:2] != b'MZ' or not 64 <= offset <= min(exe.file_size - 6, 1024**2):
                raise ValueError('Invalid Windows executable header')
            stream.seek(offset)
            if stream.read(6) != b'PE\0\0\x64\x86':
                raise ValueError('Windows executable is not x86-64')


def mac_tar(path, version):
    with tarfile.open(path, 'r:gz') as archive:
        members, names, total = [], set(), 0
        for member in archive:
            total += member.size
            if len(members) >= 50_000 or member.name in names or not safe_name(member.name) or total > 4 * 1024**3:
                raise ValueError('Unsafe or oversized macOS app archive')
            names.add(member.name)
            members.append(member)
        plists = [m for m in members if m.name.endswith('.app/Contents/Info.plist')]
        if len(plists) != 1 or not plists[0].isfile() or plists[0].size > 1024**2:
            raise ValueError('macOS archive must contain one application plist')
        info = plistlib.loads(archive.extractfile(plists[0]).read())
        if info.get('CFBundleIdentifier') != BUNDLE or info.get('CFBundleShortVersionString') != version:
            raise ValueError('macOS bundle or version differs from the release')
        executable = info.get('CFBundleExecutable')
        if not isinstance(executable, str) or not executable or PurePosixPath(executable).name != executable:
            raise ValueError('Invalid macOS executable name')
        root = plists[0].name.removesuffix('Info.plist')
        member = archive.getmember(root + 'MacOS/' + executable)
        if not member.isfile():
            raise ValueError('macOS application executable is missing')
        header = archive.extractfile(member).read(8)
        if header != b'\xcf\xfa\xed\xfe\x0c\x00\x00\x01':
            raise ValueError('macOS executable is not ARM64 Mach-O')


def mac_dmg(path):
    if path.stat().st_size < 512:
        raise ValueError('Truncated macOS disk image')
    with path.open('rb') as stream:
        stream.seek(-512, 2)
        if stream.read(4) != b'koly':
            raise ValueError('macOS package is not a UDIF disk image')


def android_manifest(path, version):
    # The publication job explicitly installs Android aapt; local callers can
    # also use an existing configured SDK without preparing another report.
    tool = shutil.which('aapt')
    if not tool:
        for key in ['ANDROID_HOME', 'ANDROID_SDK_ROOT']:
            if os.environ.get(key):
                candidates = sorted((Path(os.environ[key]) / 'build-tools').glob('*/aapt'))
                if candidates:
                    tool = str(candidates[-1]); break
    if not tool:
        raise FileNotFoundError('Android aapt is required to inspect APK metadata')
    text = subprocess.check_output([tool, 'dump', 'badging', str(path)], text=True, timeout=30)
    package = re.search(r"(?m)^package: name='([^']+)'[^\n]* versionName='([^']+)'", text)
    if not package or package[1] != BUNDLE or package[2] != version:
        raise ValueError('APK application bundle or version differs from the release')
