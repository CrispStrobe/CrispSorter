import assert from 'node:assert/strict';

const hex = (value, length) => typeof value === 'string' && new RegExp('^[0-9a-f]{' + length + '}$', 'i').test(value);

export function validateBuildSource(build) {
  assert.equal(build.schema_version, 2, 'A source-aware app build receipt is required.');
  assert.equal(build.application_source_clean, true, 'Application source or dependency lock changed.');
  assert(hex(build.cargo_lock_sha256, 64), 'Exact resolved dependency lock identity is required.');
  const generated = build.generated_tracked_files;
  assert(Array.isArray(generated) && generated.length <= 1, 'Unexpected generated tracked files.');
  assert.equal(build.tracked_source_clean, generated.length === 0, 'Tracked tree status contradicts its generated inventory.');
  if (generated.length) {
    const file = generated[0];
    assert.equal(file.path, 'static/licenses.json', 'Only the generated license inventory may differ.');
    assert(Number.isSafeInteger(file.bytes) && file.bytes > 0 && file.bytes <= 8 * 1024 * 1024);
    assert(hex(file.sha256, 64) && hex(file.committed_sha256, 64));
    assert(Number.isFinite(Date.parse(file.generated_at)), 'Generated inventory timestamp is missing.');
    const keys = file.counts && Object.keys(file.counts).sort();
    assert(keys && (JSON.stringify(keys) === '["backend","frontend","total"]'
      || JSON.stringify(keys) === '["backend","frontend","native","total"]'));
    assert(keys.every(key => Number.isSafeInteger(file.counts[key]) && file.counts[key] >= 0));
    assert.equal(file.counts.total, file.counts.frontend + file.counts.backend + (file.counts.native ?? 0));
  }
  return generated;
}

export function validatePackagedAndroidLibraries(artifact) {
  assert.equal(artifact.abis?.length, 1);
  const names = Object.keys(artifact.native_libraries ?? {}).sort();
  assert(names.length > 0);
  assert.deepEqual(Object.keys(artifact.native_elf ?? {}).sort(), names, 'Every final APK library needs an ELF inspection.');
  for (const name of names) {
    const elf = artifact.native_elf[name];
    assert(hex(artifact.native_libraries[name], 64));
    assert.equal(elf.abi, artifact.abis[0]);
    assert(Number.isSafeInteger(elf.minimum_load_alignment) && elf.minimum_load_alignment >= 16384,
      'Final APK native load alignment does not support 16 KiB pages: ' + name);
    assert(Array.isArray(elf.load_segments) && elf.load_segments.length > 0);
  }
}

export function probeOptions(argv) {
  const required = ['serial', 'apk', 'inputs', 'output', 'source-commit', 'embed-commit', 'build-receipt'];
  const allowed = new Set([...required, 'runtime-receipt', 'device-kind', 'repeat-pages', 'max-rss-mib']);
  const result = {};
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i]?.replace(/^--/, '');
    assert(argv[i]?.startsWith('--') && allowed.has(key), 'Unknown argument: ' + argv[i]);
    assert(argv[i + 1] && !argv[i + 1].startsWith('--') && !(key in result), 'Missing or repeated argument: ' + key);
    result[key] = argv[i + 1];
  }
  for (const key of required) assert(result[key], 'Required: --' + key);
  result['device-kind'] ??= 'emulator';
  assert(['emulator', 'physical'].includes(result['device-kind']), 'Device kind must be emulator or physical.');
  if (result['device-kind'] === 'emulator') {
    assert(/^emulator-\d+$/.test(result.serial), 'Select a dedicated Android emulator explicitly.');
  } else {
    assert(/^[A-Za-z0-9][A-Za-z0-9._:-]{1,127}$/.test(result.serial) && !result.serial.startsWith('emulator-'),
      'Select a dedicated physical device explicitly; emulator serials cannot establish physical execution.');
  }
  assert(hex(result['source-commit'], 40) && hex(result['embed-commit'], 40), 'Full source and paired native commit IDs are required.');
  for (const [key, fallback, low, high] of [['repeat-pages', 8, 2, 64], ['max-rss-mib', 1024, 256, 8192]]) {
    const input = result[key] ?? String(fallback);
    assert(/^\d+$/.test(input), '--' + key + ' must be an integer.');
    result[key] = Number(input);
    assert(result[key] >= low && result[key] <= high, '--' + key + ' must be between ' + low + ' and ' + high + '.');
  }
  return result;
}

export function processMemory(status, meminfo) {
  const number = value => value != null && Number.isSafeInteger(Number(value)) ? Number(value) : null;
  const proc = field => number(status.match(new RegExp('^' + field + ':\\s+(\\d+)\\s+kB\\s*$', 'm'))?.[1]);
  const pss = meminfo.match(/^\s*TOTAL PSS:\s*(\d+)\b/m)?.[1];
  // Older dumpsys versions label the aggregate row TOTAL, below a Pss header.
  const legacyPss = /^\s*Pss\s/m.test(meminfo) ? meminfo.match(/^\s*TOTAL\s+(\d+)\b/m)?.[1] : undefined;
  return { rss_kib: proc('VmRSS'), high_water_rss_kib: proc('VmHWM'), virtual_kib: proc('VmSize'),
    pss_kib: number(pss ?? legacyPss),
    pss_source: pss != null ? 'dumpsys-total-pss' : legacyPss != null ? 'dumpsys-total-row' : null };
}

export function summarizeMemory(samples) {
  const peak = field => {
    const values = samples.map(item => item[field]).filter(value => Number.isFinite(value));
    return values.length ? Math.max(...values) : null;
  };
  return { sample_count: samples.length, sampled_peak_rss_kib: peak('rss_kib'),
    process_high_water_rss_kib: peak('high_water_rss_kib'), sampled_peak_pss_kib: peak('pss_kib'),
    interpretation: 'PSS/RSS are discrete samples. VmHWM is the kernel process high-water mark. This is an observed bounded fixture run, not a device capacity or production workload claim.' };
}

export function textErrorRate(expected, actual) {
  // Preserve letters from every script and combining marks. ASCII-only scoring
  // silently discards multilingual failures. Whitespace is collapsed; case,
  // punctuation and digits remain significant and Unicode is normalized to NFC.
  const normalize = text => [...text.normalize('NFC').replace(/\s+/gu, ' ').trim()];
  const left = normalize(expected), right = normalize(actual);
  assert(left.length > 0, 'Recognition reference text must not be empty.');
  let row = Array.from({ length: right.length + 1 }, (_, index) => index);
  for (let i = 1; i <= left.length; i++) {
    const next = [i];
    for (let j = 1; j <= right.length; j++) {
      next[j] = Math.min(next[j - 1] + 1, row[j] + 1, row[j - 1] + (left[i - 1] !== right[j - 1]));
    }
    row = next;
  }
  return row[right.length] / left.length;
}
