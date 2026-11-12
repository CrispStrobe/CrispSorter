import assert from 'node:assert/strict';
import test from 'node:test';
import { verifyAndroidSandbox, canonicalAppPath, isWithinAppRoot, probeOptions, processMemory, summarizeMemory, textErrorRate, validateBuildSource, validatePackagedAndroidLibraries } from '../../scripts/mobile_ocr_probe_utils.mjs';

const sandboxPackage = 'com.crispstrobe.crispsorter';
const sandboxRoot = '/data/user/0/' + sandboxPackage;
const sandboxAlias = '/data/data/' + sandboxPackage;
const sandboxIdentity = '123:456:10123:10123:41c0';
function sandboxDevice(overrides = {}) {
  const replies = {
    pwd: sandboxRoot, 'id -u': '10123',
    ['realpath ' + sandboxRoot]: sandboxRoot,
    ['realpath ' + sandboxAlias]: sandboxAlias,
    ['stat -L -c %d:%i:%u:%g:%f ' + sandboxRoot]: sandboxIdentity,
    ['stat -L -c %d:%i:%u:%g:%f ' + sandboxAlias]: sandboxIdentity,
    ...overrides,
  };
  const calls = [];
  return { calls, read: async args => {
    calls.push(args);
    const key = args.join(' ');
    assert(Object.hasOwn(replies, key), 'Unexpected device command: ' + key);
    return replies[key];
  } };
}

test('distinct realpath spellings require device evidence of the same app-owned directory', async () => {
  // The retained emulator returned these two distinct spellings. Matching
  // identity below is a simulated prerequisite, not retained device evidence.
  const device = sandboxDevice();
  const evidence = await verifyAndroidSandbox(sandboxPackage, device.read);
  assert.deepEqual(evidence.verified_roots, [sandboxRoot, sandboxAlias]);
  assert.equal(evidence.canonical_identity, sandboxIdentity);
  assert.deepEqual(evidence.observations.map(item => item.realpath), [sandboxRoot, sandboxAlias]);
  assert(evidence.observations.every(item => item.identity === sandboxIdentity && item.resolved_identity === sandboxIdentity));
  const file = '/ocr-workspace/assets/scan.png';
  assert.equal(canonicalAppPath(sandboxAlias + file, evidence.canonical_root, evidence.verified_roots), sandboxRoot + file);
  // A device that uses one realpath spelling also needs matching stat evidence.
  const canonicalDevice = sandboxDevice({ ['realpath ' + sandboxAlias]: sandboxRoot });
  await verifyAndroidSandbox(sandboxPackage, canonicalDevice.read);
  const aliasDevice = sandboxDevice({ pwd: sandboxAlias });
  assert.equal((await verifyAndroidSandbox(sandboxPackage, aliasDevice.read)).canonical_root, sandboxAlias);
});

test('matching root spellings cannot bypass device, inode, owner or directory checks', async () => {
  for (const identity of ['999:456:10123:10123:41c0', '123:999:10123:10123:41c0',
    '123:456:10999:10123:41c0', '123:456:10123:10999:41c0', '123:456:10123:10123:81c0', 'invalid']) {
    const evidence = {};
    const device = sandboxDevice({ ['realpath ' + sandboxAlias]: sandboxRoot,
      ['stat -L -c %d:%i:%u:%g:%f ' + sandboxAlias]: identity });
    await assert.rejects(verifyAndroidSandbox(sandboxPackage, device.read, evidence));
    assert(!evidence.verified_roots.includes(sandboxAlias));
    assert.equal(evidence.observations.at(-1).identity, identity);
  }
});

test('sandbox verification refuses other packages, users, siblings and unexpected realpaths', async () => {
  for (const outside of [sandboxRoot + '-other', '/data/user/10/' + sandboxPackage, '/data/data/other.app', '/sdcard']) {
    const device = sandboxDevice({ ['realpath ' + sandboxAlias]: outside });
    await assert.rejects(verifyAndroidSandbox(sandboxPackage, device.read));
    assert(!device.calls.some(args => args[0] === 'stat' && args.at(-1) === outside));
    await assert.rejects(verifyAndroidSandbox(sandboxPackage, sandboxDevice({ pwd: outside }).read));
  }
  await assert.rejects(verifyAndroidSandbox(sandboxPackage, sandboxDevice({ 'id -u': '0' }).read));
  await assert.rejects(verifyAndroidSandbox(sandboxPackage, sandboxDevice({ 'id -u': 'not-a-uid' }).read));
});

test('canonical app data may be the sandbox root or a descendant, never a sibling prefix', () => {
  assert.equal(isWithinAppRoot('/sandbox/app', '/sandbox/app'), true);
  assert.equal(isWithinAppRoot('/sandbox/app', '/sandbox/app/files'), true);
  assert.equal(isWithinAppRoot('/sandbox/app', '/sandbox/application'), false);
  assert.equal(isWithinAppRoot('/sandbox/app', '/elsewhere'), false);
  assert.equal(isWithinAppRoot('', '/anything'), false);
});

test('only device-verified Android package roots share a canonical spelling', () => {
  const root = '/data/user/0/com.crispstrobe.crispsorter';
  const alias = '/data/data/com.crispstrobe.crispsorter';
  const suffix = '/ocr-workspace/assets/' + 'a'.repeat(64) + '.png';
  // Reproduce the original lexical containment failure without relaxing scope.
  assert.equal((alias + suffix).startsWith(root + '/ocr-workspace/assets/'), false);
  assert.equal(canonicalAppPath(alias + suffix, root, [root, alias]), root + suffix);
  assert.equal(canonicalAppPath(root + suffix, root, [root, alias]), root + suffix);
  assert.equal(canonicalAppPath(root, root, [root, alias]), root);
  assert.throws(() => canonicalAppPath(alias + suffix, root, [root]));
  assert.throws(() => canonicalAppPath(root + suffix, root, [alias]));
  for (const value of [alias + '-other' + suffix, '/data/data/other.app' + suffix,
    '/data/user/10/com.crispstrobe.crispsorter' + suffix, '/sdcard' + suffix,
    alias + '/ocr-workspace/../outside.png', alias + '//ocr-workspace/a.png', 'relative.png']) {
    assert.throws(() => canonicalAppPath(value, root, [root, alias]));
  }
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

test('retained candidate boundary scope requires emulator, native receipt and distinct harness identity', () => {
  const base = argumentsFor('emulator-5554');
  const replay = [...base, '--runtime-receipt', 'runtime.json', '--qualification', 'candidate-boundary',
    '--harness-source-commit', 'c'.repeat(40)];
  assert.equal(probeOptions(replay).qualification, 'candidate-boundary');
  assert.equal(probeOptions(base).qualification, 'full');
  assert.throws(() => probeOptions([...base, '--qualification', 'candidate-boundary']), /harness source/);
  assert.throws(() => probeOptions([...replay, '--device-kind', 'physical']), /emulator/);
  assert.throws(() => probeOptions([...base, '--qualification', 'other']), /Unknown qualification/);
});

test('recognition scores preserve non-Latin text, accents, punctuation and missing words', () => {
  assert.equal(textErrorRate('Résumé 2048', 'Re\u0301sume\u0301\n2048'), 0);
  assert(textErrorRate('Invoice 2048', 'Invoice') > 0.3);
  assert.equal(textErrorRate('文書', '文件'), 0.5);
  assert.equal(textErrorRate('العربية', ''), 1);
  assert(textErrorRate('125.00', '12500') > 0);
  assert.throws(() => textErrorRate('  ', 'anything'));
});
