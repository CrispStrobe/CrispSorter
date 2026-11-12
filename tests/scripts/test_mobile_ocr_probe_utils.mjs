import assert from 'node:assert/strict';
import test from 'node:test';
import { isWithinAppRoot, probeOptions, processMemory, summarizeMemory, textErrorRate, validateBuildSource, validatePackagedAndroidLibraries } from '../../scripts/mobile_ocr_probe_utils.mjs';

test('canonical app data may be the sandbox root or a descendant, never a sibling prefix', () => {
  assert.equal(isWithinAppRoot('/sandbox/app', '/sandbox/app'), true);
  assert.equal(isWithinAppRoot('/sandbox/app', '/sandbox/app/files'), true);
  assert.equal(isWithinAppRoot('/sandbox/app', '/sandbox/application'), false);
  assert.equal(isWithinAppRoot('/sandbox/app', '/elsewhere'), false);
  assert.equal(isWithinAppRoot('', '/anything'), false);
});

const argumentsFor = serial => ['--serial', serial, '--apk', 'test.apk', '--inputs', 'inputs', '--output', 'evidence',
  '--source-commit', 'a'.repeat(40), '--embed-commit', 'b'.repeat(40), '--build-receipt', 'build.json'];

const sourceProof = generated => ({ schema_version: 2, application_source_clean: true,
  tracked_source_clean: !generated, cargo_lock_sha256: 'a'.repeat(64),
  generated_tracked_files: generated ? [{ path: 'static/licenses.json', bytes: 100,
    sha256: 'b'.repeat(64), committed_sha256: 'c'.repeat(64), generated_at: '2026-11-09T01:00:00Z',
    counts: { frontend: 1, backend: 0, total: 1 } }] : [] });

test('build source proof separates recorded generated data from immutable application code', () => {
  assert.deepEqual(validateBuildSource(sourceProof(false)), []);
  assert.equal(validateBuildSource(sourceProof(true))[0].path, 'static/licenses.json');
  for (const change of [{ schema_version: 1 }, { application_source_clean: false },
    { tracked_source_clean: true }, { cargo_lock_sha256: '' }, { generated_tracked_files: [] }]) {
    assert.throws(() => validateBuildSource({ ...sourceProof(true), ...change }));
  }
});

test('build source proof never extends the generated exception to code or dependency locks', () => {
  for (const change of [{ path: 'Cargo.lock' }, { path: 'src-tauri/src/lib.rs' }, { sha256: '' },
    { bytes: 0 }, { counts: { frontend: 1, backend: 0, total: 2 } }]) {
    const proof = sourceProof(true);
    Object.assign(proof.generated_tracked_files[0], change);
    assert.throws(() => validateBuildSource(proof));
  }
});

test('generated native license counts remain explicit and consistent with the receipt total', () => {
  const proof = sourceProof(true);
  proof.generated_tracked_files[0].counts = { frontend: 1, backend: 9, native: 1, total: 11 };
  validateBuildSource(proof);
  for (const counts of [{ frontend: 1, backend: 9, native: 1, total: 10 },
    { frontend: 1, backend: 9, native: -1, total: 9 },
    { frontend: 1, backend: 9, native: 1, extra: 1, total: 12 }]) {
    proof.generated_tracked_files[0].counts = counts;
    assert.throws(() => validateBuildSource(proof));
  }
});

test('every final APK native library needs its own load-alignment evidence', () => {
  const name = 'lib/x86_64/libapplication.so';
  const artifact = { abis: ['x86_64'], native_libraries: { [name]: 'd'.repeat(64) },
    native_elf: { [name]: { abi: 'x86_64', minimum_load_alignment: 16384, load_segments: [{}] } } };
  validatePackagedAndroidLibraries(artifact);
  assert.throws(() => validatePackagedAndroidLibraries({ ...artifact, native_elf: {} }));
  for (const change of [{ minimum_load_alignment: 4096 }, { abi: 'arm64-v8a' }, { load_segments: [] }]) {
    const invalid = structuredClone(artifact);
    Object.assign(invalid.native_elf[name], change);
    assert.throws(() => validatePackagedAndroidLibraries(invalid));
  }
});

test('physical devices require explicit selection and cannot masquerade as emulators', () => {
  assert.equal(probeOptions(argumentsFor('emulator-5554'))['device-kind'], 'emulator');
  assert.throws(() => probeOptions(argumentsFor('PHONE123')));
  assert.equal(probeOptions([...argumentsFor('PHONE123'), '--device-kind', 'physical'])['device-kind'], 'physical');
  assert.throws(() => probeOptions([...argumentsFor('emulator-5554'), '--device-kind', 'physical']));
  assert.throws(() => probeOptions([...argumentsFor('device; wipe'), '--device-kind', 'physical']));
});

test('a repeat run has a bounded page count and explicit memory limit', () => {
  const value = probeOptions(argumentsFor('emulator-5554'));
  assert.equal(value['repeat-pages'], 8);
  assert.equal(value['max-rss-mib'], 1024);
  for (const input of ['0', '-1', '65', 'NaN', '1.5']) {
    assert.throws(() => probeOptions([...argumentsFor('emulator-5554'), '--repeat-pages', input]));
  }
  assert.throws(() => probeOptions([...argumentsFor('emulator-5554'), '--serial', 'emulator-5556']));
  assert.throws(() => probeOptions(argumentsFor('emulator-5554').slice(0, -2)));
});

test('memory sampling keeps current, kernel peak and proportional values distinct', () => {
  assert.deepEqual(processMemory('VmRSS:\t102400 kB\nVmHWM:\t150000 kB\nVmSize:\t1234567 kB\n', ' TOTAL PSS: 98200 TOTAL RSS: 102400\n'),
    { rss_kib: 102400, high_water_rss_kib: 150000, virtual_kib: 1234567, pss_kib: 98200, pss_source: 'dumpsys-total-pss' });
  assert.equal(processMemory('', '       Pss  Private\n TOTAL   45678 99 100\n').pss_kib, 45678);
  const missing = processMemory('permission denied', 'not available');
  assert.equal(missing.rss_kib, null);
  assert.equal(missing.pss_kib, null);
  assert.equal(processMemory('VmRSS: 1048576 bytes\n', '').rss_kib, null);
});

test('sample summaries cannot turn an unobserved metric into zero', () => {
  const empty = summarizeMemory([]);
  assert.equal(empty.sampled_peak_pss_kib, null);
  const observed = summarizeMemory([{ rss_kib: 20, high_water_rss_kib: 30, pss_kib: null }, { rss_kib: 18, high_water_rss_kib: 35, pss_kib: 15 }]);
  assert.equal(observed.sampled_peak_rss_kib, 20);
  assert.equal(observed.process_high_water_rss_kib, 35);
  assert.equal(observed.sampled_peak_pss_kib, 15);
});

test('recognition scores preserve non-Latin text, accents, punctuation and missing words', () => {
  assert.equal(textErrorRate('Résumé 2048', 'Re\u0301sume\u0301\n2048'), 0);
  assert(textErrorRate('Invoice 2048', 'Invoice') > 0.3);
  assert.equal(textErrorRate('文書', '文件'), 0.5);
  assert.equal(textErrorRate('العربية', ''), 1);
  assert(textErrorRate('125.00', '12500') > 0);
  assert.throws(() => textErrorRate('  ', 'anything'));
});
