#!/usr/bin/env node
// Native CI inventories the exact locked dependency graph. It does not install
// another scanner or guess licenses from package names or unresolved features.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const MAX_METADATA_BYTES = 64 * 1024 * 1024;
const sha256 = data => createHash('sha256').update(data).digest('hex');
const featureList = value => [...new Set(String(value ?? '').split(',').map(x => x.trim()).filter(Boolean))].sort();
const gitHead = project => execFileSync('git', ['-C', project, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();

export function backendEntries(metadata, project) {
  project = realpathSync(project);
  assert.equal(metadata.version, 1, 'Expected Cargo metadata format 1.');
  assert.equal(realpathSync(metadata.workspace_root), project, 'Cargo metadata belongs to another workspace.');
  assert(Array.isArray(metadata.packages) && metadata.packages.length > 0);
  assert(Array.isArray(metadata.resolve?.nodes) && metadata.resolve.nodes.length > 0, 'Cargo dependency resolution is missing.');
  const packages = new Map(metadata.packages.map(p => [p.id, p]));
  const nodes = new Map(metadata.resolve.nodes.map(p => [p.id, p]));
  assert.equal(packages.size, metadata.packages.length, 'Duplicate Cargo package identifiers.');
  assert.equal(nodes.size, metadata.resolve.nodes.length, 'Duplicate Cargo resolution identifiers.');
  const application = metadata.packages.filter(p => p.name === 'crispsorter'
    && realpathSync(p.manifest_path) === realpathSync(path.join(project, 'src-tauri/Cargo.toml')));
  assert.equal(application.length, 1, 'Metadata must resolve the actual application manifest.');
  const visited = new Set(), pending = [application[0].id];
  while (pending.length) {
    const id = pending.pop();
    if (visited.has(id)) continue;
    assert(packages.has(id) && nodes.has(id), 'The resolved application graph is incomplete: ' + id);
    visited.add(id);
    assert(Array.isArray(nodes.get(id).deps), 'Cargo dependency kinds are missing.');
    for (const dep of nodes.get(id).deps) {
      assert(Array.isArray(dep.dep_kinds) && dep.dep_kinds.length > 0, 'Cargo dependency kinds are missing.');
      // Include build dependencies; do not present test-only workspace crates
      // or inactive target packages as dependencies of this application build.
      if (dep.dep_kinds.some(kind => kind.kind == null || kind.kind === 'build')) pending.push(dep.pkg);
    }
  }
  const selected = [...visited].map(id => packages.get(id));
  for (const name of ['crispembed', 'crispembed-sys', 'ort', 'ort-sys']) {
    assert(selected.some(p => p.name === name), 'Native application dependency is missing: ' + name);
  }
  return selected.map(dep => {
    assert(typeof dep.name === 'string' && dep.name && typeof dep.version === 'string' && dep.version);
    return { name: dep.name, version: dep.version, license: dep.license || 'Unknown',
      author: Array.isArray(dep.authors) && dep.authors.length ? dep.authors.join(', ') : 'Various',
      link: dep.repository || `https://crates.io/crates/${dep.name}`, source: 'Backend' };
  }).sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version) || a.link.localeCompare(b.link));
}

export function snapshot(metadata, project, target, features, cargoVersion) {
  project = realpathSync(project);
  assert(/^[A-Za-z0-9_-]+$/.test(target), 'A specific Cargo target is required.');
  const selectedFeatures = featureList(features);
  assert(selectedFeatures.length && selectedFeatures.every(x => /^[A-Za-z0-9_-]+$/.test(x)), 'Select explicit native application features.');
  const entries = backendEntries(metadata, project);
  return { schema_version: 1, kind: 'locked-native-license-metadata',
    source_commit: gitHead(project), cargo_lock_sha256: sha256(readFileSync(path.join(project, 'Cargo.lock'))),
    application_manifest_sha256: sha256(readFileSync(path.join(project, 'src-tauri/Cargo.toml'))),
    workspace_manifest_sha256: sha256(readFileSync(path.join(project, 'Cargo.toml'))),
    target, features: selectedFeatures, default_features: false, cargo_version: cargoVersion,
    backend_count: entries.length, metadata_sha256: sha256(JSON.stringify(metadata)), metadata };
}

export function loadBackendMetadata(filename, project, target, features) {
  project = realpathSync(project);
  assert(statSync(filename).isFile() && statSync(filename).size <= MAX_METADATA_BYTES, 'Native license metadata exceeds its size bound.');
  const value = JSON.parse(readFileSync(filename, 'utf8'));
  assert.equal(value.schema_version, 1);
  assert.equal(value.kind, 'locked-native-license-metadata');
  assert.equal(value.source_commit, gitHead(project), 'Native license metadata source changed.');
  assert.equal(value.cargo_lock_sha256, sha256(readFileSync(path.join(project, 'Cargo.lock'))), 'Native license dependency lock changed.');
  assert.equal(value.application_manifest_sha256, sha256(readFileSync(path.join(project, 'src-tauri/Cargo.toml'))));
  assert.equal(value.workspace_manifest_sha256, sha256(readFileSync(path.join(project, 'Cargo.toml'))));
  assert.equal(value.target, target, 'Native license target differs from the app build.');
  assert.deepEqual(value.features, featureList(features), 'Native license features differ from the app build.');
  assert.equal(value.default_features, false);
  assert.equal(value.metadata_sha256, sha256(JSON.stringify(value.metadata)), 'Native license metadata bytes changed.');
  const entries = backendEntries(value.metadata, project);
  assert.equal(value.backend_count, entries.length);
  assert(entries.length > 4, 'A nonempty resolved backend inventory is required.');
  return { entries, receipt: value };
}

export function verifyInventory(inventory, expected) {
  assert(Array.isArray(inventory.licenses));
  const backend = inventory.licenses.filter(item => item.source === 'Backend')
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version) || a.link.localeCompare(b.link));
  assert.deepEqual(backend, expected, 'Packaged backend licenses do not match the locked native graph.');
  assert.equal(inventory.counts?.backend, expected.length);
  assert(expected.length > 4, 'A nonempty backend inventory is required.');
}

function main() {
  const [mode, ...argv] = process.argv.slice(2);
  assert(['prepare', 'verify'].includes(mode), 'Use prepare or verify.');
  const options = {};
  for (let i = 0; i < argv.length; i += 2) {
    assert(['--project', '--target', '--features', '--output', '--metadata', '--inventory'].includes(argv[i]));
    assert(argv[i + 1] && !argv[i + 1].startsWith('--') && !(argv[i] in options));
    options[argv[i]] = argv[i + 1];
  }
  const project = realpathSync(options['--project'] || '.');
  const target = options['--target'] || process.env.LICENSES_CARGO_TARGET;
  const features = options['--features'] || process.env.LICENSES_CARGO_FEATURES;
  assert(target && features, 'Exact native target and features are required.');
  if (mode === 'prepare') {
    assert(options['--output'], 'A fresh metadata output path is required.');
    const lockBefore = sha256(readFileSync(path.join(project, 'Cargo.lock')));
    const args = ['metadata', '--locked', '--format-version', '1', '--manifest-path',
      path.join(project, 'src-tauri/Cargo.toml'), '--filter-platform', target,
      '--no-default-features', '--features', features];
    const metadata = JSON.parse(execFileSync('cargo', args, { cwd: project, encoding: 'utf8',
      maxBuffer: MAX_METADATA_BYTES, timeout: 600_000, stdio: ['ignore', 'pipe', 'inherit'] }));
    assert.equal(sha256(readFileSync(path.join(project, 'Cargo.lock'))), lockBefore, 'Cargo metadata changed the dependency lock.');
    const value = snapshot(metadata, project, target, features,
      execFileSync('cargo', ['--version'], { encoding: 'utf8' }).trim());
    const output = path.resolve(options['--output']);
    const relative = path.relative(project, output);
    assert(relative === '..' || relative.startsWith('..' + path.sep) || path.isAbsolute(relative),
      'Keep generated native metadata outside the source checkout.');
    mkdirSync(path.dirname(output), { recursive: true });
    writeFileSync(output, JSON.stringify(value), { flag: 'wx' });
    console.log(`Recorded ${value.backend_count} backend licenses for ${target} from the locked application graph.`);
  } else {
    const filename = options['--metadata'] || process.env.LICENSES_CARGO_METADATA;
    assert(filename, 'Native license metadata is required.');
    const { entries } = loadBackendMetadata(filename, project, target, features);
    verifyInventory(JSON.parse(readFileSync(options['--inventory'] || path.join(project, 'static/licenses.json'), 'utf8')), entries);
    console.log(`Verified ${entries.length} packaged backend license entries against the exact native metadata.`);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
