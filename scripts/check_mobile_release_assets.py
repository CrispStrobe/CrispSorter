#!/usr/bin/env python3
"""Check actual mobile downloads after successful signed platform build jobs."""
import argparse
from pathlib import Path
import tempfile
import zipfile

from release_package_assets import download_packages, android_manifest, zip_inventory
from check_testflight_ipa import check_ipa


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['tag', 'repo', 'version']:
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    expected = {f'CrispSorter_{args.version}_android-aarch64.apk', f'CrispSorter_{args.version}_ios-arm64.ipa'}
    def package(name):
        return name.endswith(('.apk', '.ipa')) or ('ios' in name.lower() and name.endswith('.app.tar.gz'))
    with tempfile.TemporaryDirectory(prefix='crispsorter-mobile-assets-') as directory:
        files = download_packages(args.repo, args.tag, args.version, expected, package, Path(directory))
        for name, path in files.items():
            if name.endswith('.ipa'): check_ipa(path, args.version)
            else:
                with zipfile.ZipFile(path) as archive:
                    names = zip_inventory(archive)
                    if not 0 < archive.getinfo('AndroidManifest.xml').file_size <= 2 * 1024**2 or not any(n.startswith('lib/arm64-v8a/') and n.endswith('.so') for n in names):
                        raise ValueError('APK lacks its manifest or ARM64 native application')
                android_manifest(path, args.version)
    print('Mobile package downloads verified; platform jobs enforce actual code signatures.')


if __name__ == '__main__': main()
