"""Tiny real archives for receipt-free publication checks; no signing/build work."""
import hashlib
import io
import json
from pathlib import Path
import plistlib
import shutil
import struct
import tarfile
import tempfile
import unittest
import zipfile
from unittest.mock import patch
import release_package_assets as assets

VERSION='0.15.3'


def windows(path):
    data=bytearray(134);data[:2]=b'MZ';struct.pack_into('<I',data,60,128);data[128:]=b'PE\0\0\x64\x86'
    with zipfile.ZipFile(path,'w') as archive:archive.writestr('crispsorter.exe',data)


def mac(path,version=VERSION):
    info=plistlib.dumps({'CFBundleIdentifier':assets.BUNDLE,'CFBundleShortVersionString':version,'CFBundleExecutable':'crispsorter'})
    with tarfile.open(path,'w:gz') as archive:
        for name,data in [('Info.plist',info),('MacOS/crispsorter',b'\xcf\xfa\xed\xfe\x0c\x00\x00\x01')]:
            member=tarfile.TarInfo('CrispSorter.app/Contents/'+name);member.size=len(data);archive.addfile(member,io.BytesIO(data))


def ipa(path,version=VERSION):
    with zipfile.ZipFile(path,'w') as archive:
        info={'CFBundleIdentifier':assets.BUNDLE,'CFBundleShortVersionString':version,'CFBundleVersion':'42','CFBundleExecutable':'crispsorter'}
        archive.writestr('Payload/App.app/Info.plist',plistlib.dumps(info))
        archive.writestr('Payload/App.app/crispsorter',b'executable')
        archive.writestr('Payload/App.app/embedded.mobileprovision',b'profile')


def apk(path):
    with zipfile.ZipFile(path,'w') as archive:
        archive.writestr('AndroidManifest.xml',b'binary manifest fixture inspected by SDK tool')
        archive.writestr('lib/arm64-v8a/libapp.so',b'native fixture')


class Draft:
    def __init__(self,root):self.root=root;self.draft=True;self.refresh()
    def refresh(self):
        self.assets=[{'name':p.name,'size':p.stat().st_size,'digest':'sha256:'+hashlib.sha256(p.read_bytes()).hexdigest()} for p in self.root.iterdir() if p.is_file()]
    def gh(self,*command):
        if command[:2]==('release','view'):return json.dumps({'databaseId':123})
        if command[0]=='api':return json.dumps({'draft':self.draft,'assets':self.assets})
        if command[:2]==('release','download'):
            shutil.copyfile(self.root/command[command.index('--pattern')+1],Path(command[command.index('--dir')+1])/command[command.index('--pattern')+1]);return ''
        raise AssertionError(command)


class DirectAssets(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)

    def test_real_mac_windows_archive_metadata(self):
        windows(self.root/'app.zip');assets.windows_zip(self.root/'app.zip')
        mac(self.root/'app.tar.gz');assets.mac_tar(self.root/'app.tar.gz',VERSION)
        with self.assertRaises(ValueError):assets.mac_tar(self.root/'app.tar.gz','0.14.0')

    def test_wrong_native_architecture_and_malformed_zip_fail(self):
        with zipfile.ZipFile(self.root/'app.zip','w') as archive:archive.writestr('crispsorter.exe',b'MZ'+b'\0'*100)
        with self.assertRaises(ValueError):assets.windows_zip(self.root/'app.zip')
        (self.root/'app.zip').write_bytes(b'invalid zip')
        with self.assertRaises(zipfile.BadZipFile):assets.windows_zip(self.root/'app.zip')

    def test_archive_bounds_apply_before_reading_oversized_payload(self):
        large=tarfile.TarInfo('huge-file');large.size=4*1024**3+1
        # A tar header is enough: the bound must reject before trying to seek
        # over the deliberately absent enormous payload.
        import gzip
        with gzip.open(self.root/'oversized.tar.gz','wb') as stream:stream.write(large.tobuf())
        with self.assertRaisesRegex(ValueError,'oversized'):assets.mac_tar(self.root/'oversized.tar.gz',VERSION)
        entry=zipfile.ZipInfo('huge-file');entry.file_size=4*1024**3+1
        with patch.object(zipfile.ZipFile,'infolist',return_value=[entry]):
            with self.assertRaisesRegex(ValueError,'oversized'):
                assets.zip_inventory(zipfile.ZipFile.__new__(zipfile.ZipFile))

    def test_disk_image_structure(self):
        (self.root/'app.dmg').write_bytes(b'koly'+b'\0'*508);assets.mac_dmg(self.root/'app.dmg')
        (self.root/'app.dmg').write_bytes(b'garbage')
        with self.assertRaises(ValueError):assets.mac_dmg(self.root/'app.dmg')

    def test_apk_manifest_uses_real_metadata_tool_and_rejects_wrong_version(self):
        with patch.object(assets.shutil,'which',return_value='/usr/bin/aapt'),patch.object(assets.subprocess,'check_output',return_value="package: name='com.crispstrobe.crispsorter' versionName='0.15.3'\n") as run:
            assets.android_manifest(self.root/'app.apk',VERSION)
            self.assertEqual(run.call_args.args[0][:3],['/usr/bin/aapt','dump','badging'])
            with self.assertRaises(ValueError):assets.android_manifest(self.root/'app.apk','0.14.0')

    def test_draft_transport_hash_size_inventory_and_no_receipt_requirement(self):
        source=self.root/'source';source.mkdir();(source/'package.deb').write_bytes(b'package');draft=Draft(source)
        with patch.object(assets,'gh',side_effect=draft.gh):
            files=assets.download_packages('example/app','v'+VERSION,VERSION,{'package.deb'},lambda n:n.endswith('.deb'),self.root)
            self.assertEqual(set(files),{'package.deb'})
        for mode in ['public','missing','duplicate','digest','size','stale']:
            draft=Draft(source)
            if mode=='public':draft.draft=False
            if mode=='missing':draft.assets=[]
            if mode=='duplicate':draft.assets*=2
            if mode=='digest':draft.assets[0]['digest']='sha256:'+'0'*64
            if mode=='size':draft.assets[0]['size']=999
            if mode=='stale':draft.assets.append({'name':'older.deb','size':1,'digest':None})
            with self.subTest(mode=mode),patch.object(assets,'gh',side_effect=draft.gh),self.assertRaises(ValueError):
                assets.download_packages('example/app','v'+VERSION,VERSION,{'package.deb'},lambda n:n.endswith('.deb'),self.root)


if __name__=='__main__':unittest.main()
