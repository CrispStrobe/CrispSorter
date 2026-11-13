#!/usr/bin/env python3
"""Check actual desktop downloads after successful platform build/signing jobs."""
import argparse
from pathlib import Path
import subprocess
import tempfile

from release_package_assets import gh, sha, download_packages, windows_zip, mac_tar, mac_dmg


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['tag', 'repo', 'version']:
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    expected = {f'CrispSorter_{args.version}_amd64.deb', f'CrispSorter_{args.version}_aarch64.dmg',
                f'CrispSorter_{args.version}_x64-windows-portable.zip', 'CrispSorter_aarch64.app.tar.gz'}
    def package(name):
        return (name.endswith(('.deb', '.dmg', '.exe', '.msi'))
                or (name.endswith('.app.tar.gz') and 'ios' not in name.lower())
                or ('windows-portable' in name and name.endswith('.zip')))
    with tempfile.TemporaryDirectory(prefix='crispsorter-desktop-assets-') as directory:
        files = download_packages(args.repo, args.tag, args.version, expected, package, Path(directory))
        for name, path in files.items():
            if name.endswith('.deb'):
                values = subprocess.check_output(['dpkg-deb', '-f', str(path), 'Version', 'Architecture'], text=True)
                fields = dict(line.split(': ', 1) for line in values.splitlines())
                if fields != {'Version': args.version, 'Architecture': 'amd64'}:
                    raise ValueError('Linux package version or architecture differs from the release')
            elif name.endswith('.zip'): windows_zip(path)
            elif name.endswith('.app.tar.gz'): mac_tar(path, args.version)
            else: mac_dmg(path)
    print('Desktop package downloads verified; platform jobs enforce signing and native tests.')


if __name__ == '__main__': main()
