import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { backendEntries, loadBackendMetadata, snapshot, verifyInventory } from '../../scripts/native_license_inventory.mjs';

function fixture() {
  const root = mkdtempSync(path.join(process.env.NATIVE_LICENSE_TEST_TMP || os.tmpdir(), 'native-license-test-'));
  const project = path.join(root, 'project');
  mkdirSync(path.join(project, 'src-tauri'), { recursive: true });
  writeFileSync(path.join(project, 'Cargo.toml'), '[workspace]\nmembers = ["src-tauri"]\n');
  writeFileSync(path.join(project, 'Cargo.lock'), '# Self-authored identity fixture; no crates are built.\nversion = 4\n');
  writeFileSync(path.join(project, 'src-tauri/Cargo.toml'), '[package]\nname = "crispsorter"\nversion = "0.0.0"\n');
  for (const argv of [['init', '-q'], ['add', '.'], ['-c', 'user.name=License control', '-c', 'user.email=control@example.invalid', 'commit', '-qm', 'fixture']]) {
    execFileSync('git', ['-C', project, ...argv], { stdio: 'pipe' });
  }
  const names = ['crispsorter', 'crispembed', 'crispembed-sys', 'ort', 'ort-sys', 'runtime-leaf', 'build-leaf', 'dev-only', 'inactive-platform'];
  const metadata = { version: 1, workspace_root: project,
    packages: names.map((name, index) => ({ id: `package-${index}`, name, version: '1.0.0',
      manifest_path: index ? path.join(root, name, 'Cargo.toml') : path.join(project, 'src-tauri/Cargo.toml'),
      license: name === 'runtime-leaf' ? 'MIT OR Apache-2.0' : 'MIT', authors: ['Fixture author'], repository: null })),
    resolve: { nodes: names.map((name, index) => ({ id: `package-${index}`, deps: [] })) } };
  const dep = (index, kind = null) => ({ pkg: `package-${index}`, dep_kinds: [{ kind, target: null }] });
  metadata.resolve.nodes[0].deps = [dep(1), dep(3), dep(5), dep(6, 'build'), dep(7, 'dev')];
  metadata.resolve.nodes[1].deps = [dep(2)];
  metadata.resolve.nodes[3].deps = [dep(4)];
  const filename = path.join(root, 'metadata.json');
  const value = snapshot(metadata, project, 'aarch64-linux-android', 'ocr-mobile', 'cargo test-fixture');
  writeFileSync(filename, JSON.stringify(value));
  return { root, project, metadata, filename, value, clean: () => rmSync(root, { recursive: true, force: true }) };
}

test('native license inventory follows the resolved app graph and retains SPDX alternatives', () => {
  const f = fixture();
  try {
    const entries = backendEntries(f.metadata, f.project);
    assert.equal(entries.length, 7);
    assert(entries.some(e => e.name === 'build-leaf'));
    assert(!entries.some(e => ['dev-only', 'inactive-platform'].includes(e.name)));
    assert.equal(entries.find(e => e.name === 'runtime-leaf').license, 'MIT OR Apache-2.0');
    assert(entries.every(e => e.source === 'Backend'));
    assert.deepEqual(loadBackendMetadata(f.filename, f.project, 'aarch64-linux-android', 'ocr-mobile').entries, entries);
    const alias = path.join(f.root, 'project-alias');
    symlinkSync(f.project, alias, 'dir');
    assert.deepEqual(loadBackendMetadata(f.filename, alias, 'aarch64-linux-android', 'ocr-mobile').entries, entries);
  } finally { f.clean(); }
});

test('missing native bindings, duplicate IDs and unresolved edges cannot produce a partial inventory', () => {
  const f = fixture();
  try {
    for (const mutate of [m => m.resolve.nodes[0].deps.shift(), m => m.packages.push(m.packages[0]),
      m => m.resolve.nodes.pop(), m => { m.resolve.nodes[0].deps[0].pkg = 'missing'; },
      m => { m.resolve.nodes[0].deps[0].dep_kinds = []; }]) {
      const value = structuredClone(f.metadata);
      mutate(value);
      if (value.resolve.nodes.length === f.metadata.resolve.nodes.length - 1) {
        value.resolve.nodes[0].deps.push({ pkg: 'package-8', dep_kinds: [{ kind: null }] });
      }
      assert.throws(() => backendEntries(value, f.project));
    }
  } finally { f.clean(); }
});

test('native metadata rejects another target, feature selection, source, or dependency lock', () => {
  const f = fixture();
  try {
    assert.throws(() => loadBackendMetadata(f.filename, f.project, 'x86_64-linux-android', 'ocr-mobile'));
    assert.throws(() => loadBackendMetadata(f.filename, f.project, 'aarch64-linux-android', 'desktop'));
    for (const update of [{ source_commit: 'f'.repeat(40) }, { backend_count: 0 }, { metadata_sha256: '0'.repeat(64) }]) {
      writeFileSync(f.filename, JSON.stringify({ ...f.value, ...update }));
      assert.throws(() => loadBackendMetadata(f.filename, f.project, 'aarch64-linux-android', 'ocr-mobile'));
    }
    writeFileSync(f.filename, JSON.stringify(f.value));
    writeFileSync(path.join(f.project, 'Cargo.lock'), 'changed dependency resolution');
    assert.throws(() => loadBackendMetadata(f.filename, f.project, 'aarch64-linux-android', 'ocr-mobile'), /lock changed/);
  } finally { f.clean(); }
});

test('explicit metadata tampering and changed application manifests fail before packaging', () => {
  const f = fixture();
  try {
    const changed = structuredClone(f.value);
    changed.metadata.packages[1].license = 'invented';
    writeFileSync(f.filename, JSON.stringify(changed));
    assert.throws(() => loadBackendMetadata(f.filename, f.project, 'aarch64-linux-android', 'ocr-mobile'), /metadata bytes changed/);
    writeFileSync(f.filename, JSON.stringify(f.value));
    writeFileSync(path.join(f.project, 'src-tauri/Cargo.toml'), '[package]\nname = "changed"\n');
    assert.throws(() => loadBackendMetadata(f.filename, f.project, 'aarch64-linux-android', 'ocr-mobile'));
  } finally { f.clean(); }
});

test('generated native builds reject empty, incomplete, or changed backend license entries', () => {
  const f = fixture();
  try {
    const entries = backendEntries(f.metadata, f.project);
    const inventory = { counts: { backend: entries.length }, licenses: [...entries, { name: 'native-example', source: 'Native' }] };
    verifyInventory(inventory, entries);
    for (const invalid of [{ counts: { backend: 0 }, licenses: [] },
      { counts: { backend: entries.length }, licenses: entries.slice(1) }]) {
      assert.throws(() => verifyInventory(invalid, entries));
    }
    const altered = structuredClone(inventory);
    altered.licenses[0].license = 'changed';
    assert.throws(() => verifyInventory(altered, entries));
  } finally { f.clean(); }
});

