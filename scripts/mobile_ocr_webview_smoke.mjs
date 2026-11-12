#!/usr/bin/env node
/**
 * Real Android WebView -> Tauri -> Rust -> CrispEmbed smoke.
 *
 * Run on a dedicated, fresh emulator or an explicitly selected physical Android
 * test device with the exact debug APK already installed and running. No fixture adapter, native mocks,
 * automatic consent, system save picker, data wipe, or physical-camera claim.
 * The emulator-only DocumentsUI case selects its own pinned PDF fixture.
 *
 * Wry wry-v0.55.1 src/android/main_pipe.rs enables WebView debugging in debug
 * builds; Tauri tauri-v2.11.3 runtime defaults devtools to true in those builds.
 * Playwright v1.62.1 packages/playwright-core/src/server/android/android.ts uses
 * the WebView socket's /devtools/browser WebSocket endpoint.
 * Sources:
 * https://github.com/tauri-apps/wry/blob/wry-v0.55.1/src/android/main_pipe.rs
 * https://github.com/tauri-apps/tauri/blob/tauri-v2.11.3/crates/tauri-runtime-wry/src/lib.rs
 * https://github.com/microsoft/playwright/blob/v1.62.1/packages/playwright-core/src/server/android/android.ts
 *
 * node scripts/mobile_ocr_webview_smoke.mjs --serial emulator-5554 \
 *   --apk path/to/debug.apk --inputs path/to/prepared-inputs \
 *   --output path/to/new-receipt-directory --source-commit <40hex> \
 *   --embed-commit <40hex> --build-receipt path/to/build-receipt.json \
 *   [--runtime-receipt path/to/native-build.json] [--device-kind physical] \
 *   [--repeat-pages 8] [--max-rss-mib 1024]
 *
 * PLAYWRIGHT_MODULE_PATH must identify an installed playwright package directory
 * (version 1.62.1). The script reads no weights from the host fixture directory:
 * the application downloads and verifies the baseline itself over HTTPS.
 */
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { execFile } from 'node:child_process';
import fs from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { isWithinAppRoot, probeOptions, processMemory, summarizeMemory, textErrorRate, validateBuildSource, validatePackagedAndroidLibraries } from './mobile_ocr_probe_utils.mjs';
import { assessQualityFixture } from './mobile_ocr_quality.mjs';

const PACKAGE = 'com.crispstrobe.crispsorter';
const PLAYWRIGHT_VERSION = '1.62.1';
const execFileAsync = promisify(execFile);
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const hex = (value, length) => typeof value === 'string' && new RegExp('^[0-9a-f]{' + length + '}$', 'i').test(value);
const stamp = () => new Date().toISOString();

async function fileHash(filename) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(filename)) hash.update(chunk);
  return hash.digest('hex');
}

async function bounded(promise, ms, label) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(label + ' timed out after ' + ms + ' ms')), ms); }),
    ]);
  } finally { clearTimeout(timer); }
}

function remotePath(value) {
  assert(typeof value === 'string' && value.startsWith('/') && /^[A-Za-z0-9_./+~=-]+$/.test(value), 'Unexpected device path.');
  assert.equal(path.posix.normalize(value), value, 'Device path is not normalized.');
  return value;
}

function archivedPath(document) {
  let value = document.archive_uri ?? document.source_uri;
  if (typeof value === 'string' && value.startsWith('file://')) {
    const uri = new URL(value);
    assert(['', 'localhost'].includes(uri.hostname) && !uri.search && !uri.hash, 'Unexpected private archive URI.');
    value = decodeURIComponent(uri.pathname);
  }
  return remotePath(value);
}

function validateAsset(asset, appData) {
  assert(asset && Number.isInteger(asset.width) && Number.isInteger(asset.height), 'Missing raster dimensions.');
  assert(asset.width > 0 && asset.height > 0 && asset.width * asset.height <= 100_000_000, 'Invalid raster size.');
  assert(remotePath(asset.image_path).startsWith(appData + '/ocr-workspace/assets/'), 'Page is not a managed app asset.');
  for (const key of ['page_width_points', 'page_height_points']) {
    if (asset[key] != null) assert(Number.isFinite(asset[key]) && asset[key] > 0 && asset[key] <= 14_400, 'Invalid physical page dimensions.');
  }
  return asset;
}

function validateRecognition(result, appData, expected = 'CRISP MOBILE OCR\nDocument 2048\nTotal 125.00', language = 'eng') {
  validateAsset(result, appData);
  assert(Array.isArray(result.regions) && result.regions.length > 0, 'Real OCR returned no regions.');
  for (const region of result.regions) {
    assert.equal(typeof region.text, 'string');
    assert([region.x, region.y, region.w, region.h].every(Number.isFinite), 'Nonfinite OCR geometry.');
    assert(region.x >= 0 && region.y >= 0 && region.w > 0 && region.h > 0, 'Empty or negative OCR geometry.');
    assert(region.x + region.w <= result.width + 0.01 && region.y + region.h <= result.height + 0.01, 'OCR box is outside the returned raster.');
  }
  const text = result.regions.map(region => region.text).join('\n');
  assert(text.trim().length > 0, 'Actual OCR returned empty text.');
  const cer = textErrorRate(expected, text);
  assert(cer <= 0.25, 'Actual OCR exceeded the fixture Unicode character error limit (0.25): ' + cer + '\n' + text);
  const config = result.diagnostics?.requested_config;
  assert(config && config.router === false && config.stages?.length === 1, 'The saved explicit engine settings did not reach native recognition.');
  assert.equal(config.stages[0].engine, 'tesseract');
  assert.equal(config.stages[0].rec_model, 'tesseract-' + language);
  assert.equal(config.stages[0].cleanup.enabled, false);
  return text;
}

function projectRegion(region) {
  return {
    id: 'region-' + randomUUID(), text: region.text, originalText: region.text,
    x: region.x, y: region.y, w: region.w, h: region.h,
    confidence: Number.isFinite(region.confidence) ? region.confidence : null,
    char_conf: Array.isArray(region.char_conf) ? region.char_conf : [],
    confidenceBasis: ['recognition', 'detection', 'manual'].includes(region.confidence_basis) ? region.confidence_basis : 'unknown',
    charConfidenceUnit: ['unicode_codepoint', 'token'].includes(region.char_conf_unit) ? region.char_conf_unit : 'unknown',
    charConfidenceAligned: region.char_conf_aligned === true,
    kind: 'text', review: 'unreviewed', excluded: false,
  };
}

async function main() {
  const opt = probeOptions(process.argv.slice(2));
  const output = path.resolve(opt.output);
  await fs.mkdir(path.dirname(output), { recursive: true });
  await fs.mkdir(output); // Never overwrite evidence from an earlier run.
  const receipt = {
    schema_version: 1, kind: 'android-webview-native-ocr', started_at: stamp(), status: 'running',
    synthetic_only: true, physical_camera_tested: false, corpus_quality_qualified: false,
    system_save_picker_tested: false, system_document_picker_tested: false,
    ui_interactions_tested: false, device_kind: opt['device-kind'],
    physical_device_qualified: false,
    source: { sorter_commit: opt['source-commit'], crispembed_commit: opt['embed-commit'],
      identity_basis: 'Clean checkout build receipt, matched packaged native code/data, and exact installed APK SHA-256',
      github_run_id: process.env.GITHUB_RUN_ID ?? null, github_sha: process.env.GITHUB_SHA ?? null },
    serial: opt.serial, package: PACKAGE, steps: [], cleanup_errors: [], memory_samples: [],
    limits: { repeated_inference_pages: opt['repeat-pages'], maximum_process_rss_mib: opt['max-rss-mib'] },
  };
  const consoleLog = [];
  let browser, page, forwardPort, pid, privateRoot, appData, exportPath;
  let pickerFixturePath, pickerDumpPath;
  let deadline = Infinity;
  function budget(limit, label) {
    const remaining = Math.floor(Math.min(limit, deadline - Date.now()));
    assert(remaining > 0, label + ' exceeded the current step deadline.');
    return remaining;
  }
  const saveReceipt = async () => {
    const temporary = path.join(output, 'receipt.json.tmp');
    await fs.writeFile(temporary, JSON.stringify(receipt, null, 2) + '\n');
    await fs.rename(temporary, path.join(output, 'receipt.json'));
  };
  const adb = (args, { timeout = 30_000, maxBuffer = 4 * 1024 * 1024, encoding = 'utf8' } = {}) =>
    execFileAsync('adb', ['-s', opt.serial, ...args], { timeout: budget(timeout, 'adb'), maxBuffer, encoding });
  const adbText = async (args, settings) => (await adb(args, settings)).stdout.trim();
  const runAs = (args, settings) => adb(['exec-out', 'run-as', PACKAGE, ...args], settings);
  async function sampleMemory(label) {
    assert(/^\d+$/.test(pid), 'Memory samples require the verified application PID.');
    const values = await Promise.allSettled([
      runAs(['cat', '/proc/' + pid + '/status'], { timeout: 10_000, maxBuffer: 128 * 1024 }),
      adb(['shell', 'dumpsys', 'meminfo', pid], { timeout: 10_000, maxBuffer: 512 * 1024 }),
    ]);
    const status = values[0].status === 'fulfilled' ? values[0].value.stdout : '';
    const meminfo = values[1].status === 'fulfilled' ? values[1].value.stdout : '';
    const sample = { at: stamp(), label, pid, ...processMemory(status, meminfo),
      errors: values.flatMap(item => item.status === 'rejected' ? [String(item.reason).slice(0, 2000)] : []) };
    receipt.memory_samples.push(sample);
    await fs.mkdir(path.join(output, 'memory'), { recursive: true });
    await fs.writeFile(path.join(output, 'memory', String(receipt.memory_samples.length).padStart(4, '0') + '.txt'),
      'Process status\n' + status + '\nDumpsys meminfo\n' + meminfo);
    return sample;
  }
  async function measuredRecognition(label, work) {
    const before = await sampleMemory(label + ':before');
    assert(Number.isFinite(before.rss_kib) && Number.isFinite(before.pss_kib), 'Application RSS/PSS could not be measured before inference.');
    const started = Date.now();
    let pendingSample = null, samplingError = null, value, failure;
    const timer = setInterval(() => {
      if (pendingSample) return;
      pendingSample = sampleMemory(label + ':during').catch(error => { samplingError = error; }).finally(() => { pendingSample = null; });
    }, 5000);
    try { value = await work(); }
    catch (error) { failure = error; }
    finally {
      clearInterval(timer);
      if (pendingSample) await pendingSample;
    }
    const elapsed = Date.now() - started;
    if (value != null) {
      assert(/^[a-z0-9-]+$/.test(label));
      await fs.mkdir(path.join(output, 'native-observations'), { recursive: true });
      await fs.writeFile(path.join(output, 'native-observations', label + '.json'), JSON.stringify(value, null, 2) + '\n');
    }
    let after = null, afterError = null;
    try { after = await sampleMemory(label + ':after'); }
    catch (error) { afterError = error; }
    receipt.memory = summarizeMemory(receipt.memory_samples);
    receipt.inference_observations ??= [];
    receipt.inference_observations.push({ label, wall_ms: elapsed,
      native_elapsed_ms: value?.elapsed_ms ?? null, before, after });
    if (failure) throw failure;
    if (samplingError) throw samplingError;
    if (afterError) throw afterError;
    assert(Number.isFinite(after.rss_kib) && Number.isFinite(after.pss_kib), 'Application RSS/PSS could not be measured after inference.');
    assert(receipt.memory.process_high_water_rss_kib != null, 'Kernel peak RSS is unavailable; the memory gate cannot pass without it.');
    assert(receipt.memory.process_high_water_rss_kib <= opt['max-rss-mib'] * 1024,
      'Application peak RSS exceeded the explicit fixture-run limit of ' + opt['max-rss-mib'] + ' MiB.');
    return value;
  }
  async function privatePath(value) {
    remotePath(value);
    assert(value.startsWith(privateRoot + '/'), 'Refusing to inspect a file outside the dedicated test app.');
    const resolved = (await runAs(['realpath', value])).stdout.trim();
    assert.equal(resolved, value, 'Private file unexpectedly resolves through a symlink or alias.');
    return value;
  }
  async function privateBytes(value, limit = 16 * 1024 * 1024) {
    await privatePath(value);
    return (await runAs(['cat', value], { encoding: null, maxBuffer: limit })).stdout;
  }
  async function privateDigest(value) {
    await privatePath(value);
    const line = (await runAs(['sha256sum', value], { timeout: 60_000 })).stdout;
    const hash = line.match(/^([0-9a-f]{64})\s/i)?.[1];
    assert(hash, 'Device did not return a SHA-256 digest.');
    return hash.toLowerCase();
  }
  async function verifyInstalledModel(model) {
    const directory = appData + '/ocr-models/' + digest(model.name);
    const manifestPath = directory + '/current.json';
    const manifest = JSON.parse((await privateBytes(manifestPath, 128 * 1024)).toString('utf8'));
    assert.equal(manifest.schema_version, 1);
    assert.equal(manifest.model_name, model.name);
    assert(/^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i.test(manifest.version));
    assert.deepEqual(manifest.files, [{ filename: model.filename, url: model.url, sha256: model.sha256, primary: true }],
      'Installed registry artifacts differ from the immutable native test lock.');
    const artifact = directory + '/' + manifest.version + '/' + model.filename;
    assert.equal(await privateDigest(artifact), model.sha256);
    const verified = { name: model.name, manifest_path: manifestPath, version: manifest.version,
      filename: model.filename, sha256: model.sha256 };
    if (model.bytes != null) {
      const size = (await runAs(['stat', '-c', '%s', artifact])).stdout.trim();
      assert(/^\d+$/.test(size));
      assert.equal(Number(size), model.bytes, 'Installed language model length differs from its pin.');
      verified.bytes = Number(size);
    }
    return verified;
  }
  async function step(name, work, timeout = 60_000) {
    const entry = { name, started_at: stamp(), status: 'running' };
    receipt.steps.push(entry);
    await saveReceipt();
    console.log('[mobile-ocr] ' + name);
    const start = Date.now();
    const previousDeadline = deadline;
    deadline = start + timeout;
    const heartbeat = setInterval(() => console.log('[mobile-ocr] waiting: ' + name + ' (' + Math.round((Date.now() - start) / 1000) + ' s)'), 15_000);
    try {
      const result = await work();
      assert(Date.now() <= deadline, name + ' exceeded its step deadline.');
      entry.status = 'passed';
      return result;
    } catch (error) {
      entry.status = 'failed';
      entry.error = String(error?.stack ?? error).slice(0, 16_384);
      throw error;
    } finally {
      clearInterval(heartbeat);
      deadline = previousDeadline;
      entry.duration_ms = Date.now() - start;
      await saveReceipt();
    }
  }
  const invoke = (command, args = {}, timeout = 30_000) => {
    const milliseconds = budget(timeout, command);
    return bounded(page.evaluate(
      ({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args },
    ), milliseconds, command);
  };
  const importBytes = (command, bytes, metadata, timeout = 90_000) => {
    assert(['ocr_workspace_import_bytes', 'ocr_workspace_import_image'].includes(command));
    return bounded(page.evaluate(async ({ command, base64, metadata }) => {
      const header = btoa(Array.from(new TextEncoder().encode(JSON.stringify(metadata)), byte => String.fromCharCode(byte)).join(''));
      return window.__TAURI_INTERNALS__.invoke(command, { base64 }, { headers: { 'x-ocr-metadata': header } });
    }, { command, base64: bytes.toString('base64'), metadata }), budget(timeout, command), command);
  };
  async function pickerSnapshot(label, filename, selected = false) {
    assert.equal(opt['device-kind'], 'emulator', 'System picker automation is limited to the dedicated emulator.');
    assert(/^[a-z0-9-]+$/.test(label));
    assert(pickerDumpPath?.startsWith('/data/local/tmp/crisp-ocr-picker-'));
    const directory = path.join(output, 'provider-ui');
    await fs.mkdir(directory, { recursive: true });
    // Remove only this run's named scratch file so a failed dump cannot reuse
    // an earlier hierarchy. Every tap is chosen from the newly recorded tree.
    await adb(['shell', 'rm', '-f', pickerDumpPath]);
    await adb(['shell', 'uiautomator', 'dump', '--compressed', pickerDumpPath], { timeout: 20_000 });
    const xml = (await adb(['exec-out', 'cat', pickerDumpPath], { encoding: null, maxBuffer: 2 * 1024 * 1024 })).stdout;
    const xmlPath = path.join(directory, label + '.xml');
    await fs.writeFile(xmlPath, xml, { flag: 'wx' });
    const screenshot = (await adb(['exec-out', 'screencap', '-p'], { encoding: null, maxBuffer: 16 * 1024 * 1024 })).stdout;
    assert.equal(screenshot.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
    await fs.writeFile(path.join(directory, label + '.png'), screenshot, { flag: 'wx' });
    const args = [fileURLToPath(new URL('./mobile_document_picker.py', import.meta.url)), '--xml', xmlPath, '--filename', filename];
    if (selected) args.push('--selected');
    const result = JSON.parse((await execFileAsync('python3', args, { timeout: budget(10_000, 'parse observed document picker'), maxBuffer: 128 * 1024 })).stdout);
    if (result.action) {
      assert(result.action.x >= 0 && result.action.x < screenshot.readUInt32BE(16)
        && result.action.y >= 0 && result.action.y < screenshot.readUInt32BE(20), 'Observed picker target is outside the captured screen.');
    }
    receipt.document_picker.snapshots.push({ label, ...result });
    return result;
  }

  try {
    const prepared = await step('validate synthetic inputs and exact application build', async () => {
      const bytes = await fs.readFile(path.join(opt.inputs, 'page.png'));
      assert(bytes.length > 32 && bytes.length <= 2 * 1024 * 1024, 'Synthetic PNG is missing or exceeds 2 MiB.');
      assert.equal(bytes.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
      const inputBytes = await fs.readFile(path.join(opt.inputs, 'inputs.json'));
      assert(inputBytes.length <= 128 * 1024);
      const inputs = JSON.parse(inputBytes);
      const lock = JSON.parse(await fs.readFile(new URL('../tools/mobile-ocr-probe/models.json', import.meta.url)));
      assert.equal(inputs.schema_version, 1);
      assert.equal(lock.schema_version, 1);
      assert.equal(inputs.synthetic_only, true);
      assert.deepEqual(inputs.models, lock.models, 'Prepared model metadata differs from the checked-in fixture lock.');
      assert.deepEqual(inputs.models.map(model => model.name).sort(), ['dbnet-det', 'tesseract-eng']);
      assert(hex(inputs.fixture.png_sha256, 64) && hex(inputs.fixture.font_sha256, 64), 'Fixture hashes are required.');
      assert.equal(digest(bytes), inputs.fixture.png_sha256);
      assert.equal(bytes.readUInt32BE(16), inputs.fixture.width);
      assert.equal(bytes.readUInt32BE(20), inputs.fixture.height);
      assert.equal(inputs.fixture.width, 1400);
      assert.equal(inputs.fixture.height, 480);
      assert.equal(inputs.fixture.text, 'CRISP MOBILE OCR\nDocument 2048\nTotal 125.00');
      const qualitySpecBytes = await fs.readFile(new URL('../tools/mobile-ocr-probe/quality.json', import.meta.url));
      const qualitySpec = JSON.parse(qualitySpecBytes);
      assert.equal(inputs.quality_fixtures?.spec_sha256, digest(qualitySpecBytes), 'Prepared quality inputs differ from this exact source.');
      assert.equal(inputs.quality_fixtures.font_sha256, inputs.fixture.font_sha256);
      assert.deepEqual(inputs.quality_fixtures.models, qualitySpec.models);
      assert.deepEqual(qualitySpec.models.map(model => model.name), ['tesseract-deu']);
      assert.equal(inputs.quality_fixtures.cases.length, qualitySpec.cases.length);
      const quality = [];
      for (let index = 0; index < qualitySpec.cases.length; index++) {
        const { text, png_sha256, bytes: pngSize, ...actualSpec } = inputs.quality_fixtures.cases[index];
        const fixture = qualitySpec.cases[index];
        assert.deepEqual(actualSpec, fixture);
        assert.equal(text, fixture.lines.map(line => line.text).join('\n'));
        assert(/^quality-[a-z-]+\.png$/.test(fixture.filename) && hex(png_sha256, 64));
        const pixels = await fs.readFile(path.join(opt.inputs, fixture.filename));
        assert(pixels.length > 32 && pixels.length <= 2 * 1024 * 1024);
        assert.equal(pixels.length, pngSize);
        assert.equal(digest(pixels), png_sha256);
        assert.equal(pixels.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
        assert.equal(pixels.readUInt32BE(16), fixture.width);
        assert.equal(pixels.readUInt32BE(20), fixture.height);
        quality.push({ fixture, bytes: pixels, sha256: png_sha256 });
      }
      for (const model of [...inputs.models, ...qualitySpec.models]) {
        assert(hex(model.sha256, 64) && /^[a-z0-9-]+$/.test(model.name));
        assert(/^[A-Za-z0-9_-]+\.gguf$/.test(model.filename));
        assert(model.url.startsWith('https://'));
      }
      assert.equal(await adbText(['get-state']), 'device', 'Selected device is not connected and authorized.');
      const qemu = await adbText(['shell', 'getprop', 'ro.kernel.qemu']);
      if (opt['device-kind'] === 'emulator') assert.equal(qemu, '1', 'Target is not an emulator.');
      else assert.notEqual(qemu, '1', 'An emulator cannot establish physical-device execution.');
      const abis = (await adbText(['shell', 'getprop', 'ro.product.cpu.abilist'])).split(',');
      const buildBytes = await fs.readFile(opt['build-receipt']);
      assert(buildBytes.length <= 1024 * 1024, 'Application build receipt exceeds 1 MiB.');
      const build = JSON.parse(buildBytes);
      validateBuildSource(build);
      assert.equal(build.kind, 'ocr-mobile-app-build');
      assert.equal(build.platform, 'android');
      assert.equal(build.package, PACKAGE);
      assert.equal(build.sorter_commit, opt['source-commit']);
      assert.equal(build.crispembed_commit, opt['embed-commit']);
      assert.equal(build.artifact.kind, 'apk');
      validatePackagedAndroidLibraries(build.artifact);
      assert.equal(build.artifact.paired_runtime_mapped_sections_match, true);
      assert.equal(build.onnx_runtime?.runtime?.api_version, 23);
      assert.equal(build.onnx_runtime?.linkage?.mapped_sections_match, true);
      assert(build.artifact.abis.length === 1 && abis.includes(build.artifact.abis[0]), 'Installed APK ABI is unsupported by the selected device.');
      if (opt['device-kind'] === 'emulator') assert.equal(build.runtime.target, 'android-x86_64');
      await fs.writeFile(path.join(output, 'application-build-receipt.json'), buildBytes);
      receipt.source.app_build_receipt_sha256 = digest(buildBytes);
      const reportedRoot = remotePath(await adbText(['exec-out', 'run-as', PACKAGE, 'pwd']));
      assert(['/data/user/0/' + PACKAGE, '/data/data/' + PACKAGE].includes(reportedRoot), 'Unexpected app sandbox.');
      privateRoot = remotePath((await runAs(['realpath', reportedRoot])).stdout.trim());
      assert(['/data/user/0/' + PACKAGE, '/data/data/' + PACKAGE].includes(privateRoot), 'Unexpected canonical app sandbox.');
      const apkPaths = (await adbText(['shell', 'pm', 'path', PACKAGE])).split(/\r?\n/).filter(Boolean);
      assert.equal(apkPaths.length, 1, 'Install the single debug APK produced by this CI build.');
      assert(apkPaths[0].startsWith('package:'));
      const installedPath = remotePath(apkPaths[0].slice(8));
      assert(installedPath.startsWith('/data/app/') && installedPath.endsWith('/base.apk'));
      const installedHash = (await adbText(['shell', 'sha256sum', installedPath], { timeout: 60_000 })).match(/^([0-9a-f]{64})\s/i)?.[1];
      const apkHash = await fileHash(opt.apk);
      assert.equal(apkHash, build.artifact.sha256, 'Supplied APK differs from its source-bound build receipt.');
      assert.equal(installedHash?.toLowerCase(), apkHash, 'Running installation is not the supplied APK.');
      receipt.apk = { sha256: apkHash, installed_path: installedPath, abis };
      receipt.device = {
        kind: opt['device-kind'], qemu_property: qemu,
        model: await adbText(['shell', 'getprop', 'ro.product.model']),
        manufacturer: await adbText(['shell', 'getprop', 'ro.product.manufacturer']),
        sdk: await adbText(['shell', 'getprop', 'ro.build.version.sdk']),
        fingerprint: await adbText(['shell', 'getprop', 'ro.build.fingerprint']),
      };
      const packageDetails = await adbText(['shell', 'dumpsys', 'package', PACKAGE]);
      await fs.writeFile(path.join(output, 'package.txt'), packageDetails + '\n');
      receipt.inputs = { receipt_sha256: digest(inputBytes), ...inputs };
      await fs.writeFile(path.join(output, 'synthetic-input.png'), bytes);
      if (opt['runtime-receipt']) {
        const runtime = await fs.readFile(opt['runtime-receipt']);
        assert(runtime.length <= 1024 * 1024, 'Native build receipt exceeds 1 MiB.');
        JSON.parse(runtime);
        assert.equal(digest(runtime), build.runtime_receipt_sha256, 'Native build receipt differs from the application build identity.');
        await fs.writeFile(path.join(output, 'native-build-receipt.json'), runtime);
        receipt.source.native_build_receipt_sha256 = digest(runtime);
      }
      return { bytes, inputs, quality };
    }, 120_000);

    await step('attach to the running application WebView through its own debug socket', async () => {
      pid = await adbText(['shell', 'pidof', PACKAGE]);
      assert(/^\d+$/.test(pid), 'Expected one live application process from the launch smoke.');
      let socket;
      const socketDeadline = Math.min(deadline, Date.now() + 60_000);
      while (Date.now() < socketDeadline) {
        const sockets = await adbText(['shell', 'cat', '/proc/net/unix']);
        const matches = sockets.split(/\r?\n/).map(line => line.match(/@(webview_devtools_remote_[A-Za-z0-9_]+)$/)?.[1])
          .filter(name => name && (name === 'webview_devtools_remote_' + pid || name.startsWith('webview_devtools_remote_' + pid + '_')));
        if (matches.length === 1) { socket = matches[0]; break; }
        assert(matches.length <= 1, 'Multiple WebView sockets belong to this app; select one explicitly before extending this smoke.');
        await delay(1000);
      }
      assert(socket, 'No debug WebView socket for the application PID. Build a debug APK with Wry devtools enabled; this test cannot fall back to a fixture.');
      forwardPort = await adbText(['forward', 'tcp:0', 'localabstract:' + socket]);
      assert(/^\d+$/.test(forwardPort) && +forwardPort > 0 && +forwardPort < 65536, 'ADB did not allocate a valid local port.');
      const require = createRequire(import.meta.url);
      const modulePath = process.env.PLAYWRIGHT_MODULE_PATH || 'playwright';
      const playwright = require(modulePath);
      const metadata = require(modulePath.replace(/\/$/, '') + '/package.json');
      assert.equal(metadata.version, PLAYWRIGHT_VERSION, 'Use the pinned Playwright version.');
      browser = await playwright.chromium.connectOverCDP('ws://127.0.0.1:' + forwardPort + '/devtools/browser', {
        noDefaults: true, timeout: budget(30_000, 'connect CDP'),
      });
      const pageDeadline = Math.min(deadline, Date.now() + 30_000);
      while (Date.now() < pageDeadline && !page) {
        for (const candidate of browser.contexts().flatMap(context => context.pages())) {
          const candidateUrl = new URL(candidate.url());
          if (!['tauri.localhost', 'localhost'].includes(candidateUrl.hostname)) continue;
          const evaluationBudget = budget(5000, 'locate the native bridge');
          const ready = await bounded(candidate.evaluate(() =>
            typeof window.__TAURI_INTERNALS__?.invoke === 'function' && document.readyState !== 'loading',
          ).catch(() => false), evaluationBudget, 'locate the native bridge');
          if (ready) {
            assert(!page, 'Multiple native application pages are open.');
            page = candidate;
          }
        }
        if (!page) await delay(500);
      }
      assert(page, 'The debug WebView did not expose the real Tauri bridge.');
      const appUrl = new URL(page.url());
      assert(['tauri.localhost', 'localhost'].includes(appUrl.hostname) && ['http:', 'https:', 'tauri:'].includes(appUrl.protocol),
        'Unexpected application origin: ' + appUrl.origin);
      assert.equal(await bounded(page.evaluate(() => typeof window.ocrSmoke), budget(5000, 'inspect application page'), 'inspect application page'),
        'undefined', 'A synthetic fixture adapter must never satisfy native smoke.');
      page.on('console', message => { if (consoleLog.length < 1000) consoleLog.push({ at: stamp(), type: message.type(), text: message.text().slice(0, 4000) }); });
      page.on('pageerror', error => { if (consoleLog.length < 1000) consoleLog.push({ at: stamp(), type: 'pageerror', text: String(error).slice(0, 4000) }); });
      receipt.connection = { pid, socket, transport: 'adb forward + WebView CDP', playwright: metadata.version,
        webview_version: browser.version(), page_url: page.url(), native_bridge: '__TAURI_INTERNALS__.invoke' };
    }, 150_000);

    const capabilities = await step('require the native mobile OCR profile and a fresh model store', async () => {
      const value = await invoke('ocr_workspace_capabilities');
      for (const flag of ['ocr', 'crispembed', 'photo_tools', 'mobile_model_store', 'review_diagnostics']) assert.equal(value[flag], true, flag);
      // Native PDF availability is verified below when this profile exposes it.
      assert.equal(value.default_engine, 'tesseract');
      assert.equal(value.default_language, 'eng');
      assert(value.formats.includes('pdf'), 'The paired reviewed-PDF renderer is unavailable.');
      assert.equal(value.docx_export, true, 'The mobile reviewed Word writer is unavailable.');
      assert(value.formats.includes('docx'), 'Word export is not advertised by this mobile build.');
      assert(value.engines.includes('tesseract'));
      const rawAppData = remotePath(await invoke('get_app_data_dir'));
      appData = remotePath((await runAs(['realpath', rawAppData])).stdout.trim());
      receipt.app_data_scope = { is_root: appData === privateRoot, is_descendant: appData.startsWith(privateRoot + '/') };
      assert(isWithinAppRoot(privateRoot, appData), 'Native app data is outside the app sandbox.');
      const projects = await invoke('ocr_workspace_list');
      assert.deepEqual(projects, [], 'Use a fresh dedicated test installation; existing projects must not be overwritten or erased.');
      const registry = await invoke('embedder_registry_list', {}, 60_000);
      for (const model of [...prepared.inputs.models, ...prepared.inputs.quality_fixtures.models]) {
        const entry = registry.find(item => item.name === model.name);
        assert(entry && entry.filename === model.filename && entry.cached === false, 'Expected an uninstalled baseline model: ' + model.name);
      }
      receipt.capabilities = value;
      receipt.app_data = appData;
      receipt.initial_baseline_cached = false;
      return value;
    }, 90_000);

    await step('explicitly install the baseline through the application HTTPS/SHA installer', async () => {
      const requestId = randomUUID();
      const installed = await invoke('ocr_workspace_install_mobile_models', { language: 'eng', requestId }, 8 * 60_000);
      assert.deepEqual(installed, ['dbnet-det', 'tesseract-eng']);
      receipt.installer = { command: 'ocr_workspace_install_mobile_models', request_id: requestId, language: 'eng', installed };
    }, 8 * 60_000 + 5000);

    await step('independently hash every installed baseline artifact in private app storage', async () => {
      const installed = [];
      for (const model of prepared.inputs.models) {
        installed.push(await verifyInstalledModel(model));
      }
      const registry = await invoke('embedder_registry_list', {}, 60_000);
      for (const model of prepared.inputs.models) assert.equal(registry.find(item => item.name === model.name)?.cached, true);
      receipt.verified_models = installed;
    }, 150_000);

    const imported = await step('import the synthetic PNG through the real Tauri binary IPC', async () => {
      const result = await importBytes('ocr_workspace_import_image', prepared.bytes, { name: 'Synthetic mobile OCR.png', requestId: randomUUID() }, 60_000);
      assert.equal(result.pages.length, 1);
      const asset = validateAsset(result.pages[0], appData);
      assert.equal(asset.width, prepared.inputs.fixture.width);
      assert.equal(asset.height, prepared.inputs.fixture.height);
      assert(archivedPath(result).startsWith(appData + '/ocr-workspace/sources/'));
      assert.equal(await privateDigest(archivedPath(result)), prepared.inputs.fixture.png_sha256, 'The archived original bytes changed during import.');
      receipt.imported = result;
      receipt.original_asset_sha256 = await privateDigest(asset.image_path);
      return result;
    }, 90_000);

    const original = imported.pages[0];
    let rotated, corrected, perspective;
    await step('exercise native photo assessment, signed deskew, and four-corner projection', async () => {
      const assessment = await invoke('ocr_workspace_assess_photo', { pagePath: original.image_path }, 45_000);
      receipt.photo = { assessment, rotation_degrees: [3, -3] };
      assert.equal(assessment.width, original.width);
      assert.equal(assessment.height, original.height);
      for (const key of ['mean_luminance', 'contrast', 'sharpness', 'ink_fraction', 'skew_confidence']) assert(Number.isFinite(assessment[key]), key);
      assert(assessment.correction_degrees == null || (Number.isFinite(assessment.correction_degrees) && Math.abs(assessment.correction_degrees) <= 20));
      const transform = async (asset, extra) => {
        const value = await invoke('ocr_workspace_transform', {
          pagePath: asset.image_path, degrees: null, crop: null, splitX: null, quad: null, angle: null,
          pageWidthPoints: asset.page_width_points ?? null, pageHeightPoints: asset.page_height_points ?? null, ...extra,
        }, 45_000);
        assert.equal(value.pages.length, 1);
        const result = validateAsset(value.pages[0], appData);
        assert.notEqual(result.image_path, asset.image_path, 'A nonzero transform returned unchanged pixels.');
        return result;
      };
      rotated = await transform(original, { operation: 'deskew', angle: 3 });
      receipt.photo.rotated = rotated;
      corrected = await transform(rotated, { operation: 'deskew', angle: -3 });
      receipt.photo.corrected = corrected;
      const quad = [
        { x: 12, y: 10 }, { x: corrected.width - 16, y: 14 },
        { x: corrected.width - 10, y: corrected.height - 12 }, { x: 16, y: corrected.height - 16 },
      ];
      receipt.photo.quad = quad;
      perspective = await transform(corrected, { operation: 'perspective', quad });
      await fs.writeFile(path.join(output, 'synthetic-prepared.png'), await privateBytes(perspective.image_path));
      receipt.photo = { assessment, rotation_degrees: [3, -3], rotated, corrected, quad, perspective,
        qualification: 'Known manual tilt round-trip and mild perspective transform on one synthetic raster; no capture or angle-accuracy claim.' };
    }, 200_000);

    const project = {
      version: capabilities.schema_version, id: 'project-' + randomUUID(), name: 'Synthetic mobile native OCR',
      createdAt: stamp(), updatedAt: stamp(), confidenceThreshold: 0.85, profileId: 'screenshot',
      documents: [{ id: 'document-' + randomUUID(), sourceUri: imported.source_uri,
        ...(imported.archive_uri ? { archiveUri: imported.archive_uri } : {}), name: imported.name }],
      pages: [], recognitionSettings: {
        engine: 'tesseract', language: 'eng',
        config: { enabled: true, router: false, cleanup_enabled: false, denoise: false, dewarp: false,
          restore: false, sr: false, layout: false, det_model: 'dbnet-det', min_chars: 1, min_confidence: 0 },
      },
    };
    project.pages = [original, perspective].map((asset, index) => ({
      id: 'page-' + randomUUID(), documentId: project.documents[0].id, sourcePage: 1,
      name: index === 0 ? 'Original synthetic page' : 'Prepared synthetic page',
      imagePath: asset.image_path, originalImagePath: original.image_path, width: asset.width, height: asset.height,
      widthPoints: asset.page_width_points ?? asset.width * 72 / 300, heightPoints: asset.page_height_points ?? asset.height * 72 / 300,
      status: 'pending', regions: [], excluded: false, approved: false, error: null, elapsedMs: null,
      provenance: { syntheticNativeSmoke: true }, profileId: 'screenshot', contentRevision: randomUUID(),
    }));
    let saved = await step('persist and reload explicit mobile recognition settings before native OCR', async () => {
      const committed = await invoke('ocr_workspace_save', { project, expectedRevision: 0 });
      assert.equal(committed.revision, 1);
      const loaded = await invoke('ocr_workspace_load', { projectId: project.id });
      assert.equal(loaded.revision, committed.revision);
      assert.deepEqual(loaded.project.recognitionSettings, project.recognitionSettings);
      assert.deepEqual(loaded.project.pages, project.pages);
      assert.deepEqual(loaded.warnings, []);
      receipt.project_id = project.id;
      receipt.saved_settings = loaded.project.recognitionSettings;
      return loaded;
    });

    const recognized = [];
    for (let index = 0; index < 2; index++) {
      await step('recognize ' + (index === 0 ? 'original' : 'prepared') + ' pixels with saved Tesseract English settings', async () => {
        const target = saved.project.pages[index];
        const settings = saved.project.recognitionSettings;
        const result = await measuredRecognition(index === 0 ? 'original' : 'prepared', () => invoke('ocr_workspace_recognize', {
          pagePath: target.imagePath, config: settings.config, engine: settings.engine, language: settings.language, zone: null,
          pageWidthPoints: target.widthPoints, pageHeightPoints: target.heightPoints,
        }, 180_000));
        // Keep the native diagnostics even when the text or geometry gate fails.
        await fs.writeFile(path.join(output, 'recognition-' + (index === 0 ? 'original' : 'prepared') + '.json'), JSON.stringify(result, null, 2) + '\n');
        const text = validateRecognition(result, appData);
        recognized.push(result);
        target.imagePath = result.image_path;
        target.width = result.width; target.height = result.height;
        target.widthPoints = result.page_width_points ?? target.widthPoints;
        target.heightPoints = result.page_height_points ?? target.heightPoints;
        target.status = 'recognized'; target.regions = result.regions.map(projectRegion);
        target.elapsedMs = result.elapsed_ms; target.contentRevision = randomUUID();
        target.provenance = { syntheticNativeSmoke: true, diagnostics: result.diagnostics, warnings: result.warnings };
        saved.project.updatedAt = stamp();
        const committed = await invoke('ocr_workspace_save', { project: saved.project, expectedRevision: saved.revision });
        assert.equal(committed.revision, saved.revision + 1);
        saved = await invoke('ocr_workspace_load', { projectId: project.id });
        assert.equal(saved.revision, committed.revision);
        assert.deepEqual(saved.project.recognitionSettings, project.recognitionSettings);
        assert.equal(saved.project.pages[index].regions.map(region => region.text).join('\n'), text);
        receipt.recognition ??= [];
        receipt.recognition.push({ page: index === 0 ? 'original' : 'prepared', text, regions: result.regions.length,
          elapsed_ms: result.elapsed_ms, saved_revision: saved.revision, warnings: result.warnings });
      }, 220_000);
    }

    receipt.native_documents = { status: 'not_supported', pdf_import: capabilities.pdf_import,
      qualification: 'Actual app Tauri binary IPC and native document rasterizer; source-URI preservation here uses synthetic metadata and does not exercise a system provider grant.' };
    if (capabilities.pdf_import && capabilities.native_document_import) {
      const manifest = prepared.inputs.document_fixtures;
      assert(manifest?.available === true, 'This native document profile requires the checked-in document fixture set.');
      const nativeCapabilities = await invoke('plugin:mobile-provider|document_capabilities');
      assert.equal(nativeCapabilities.pdf, true);
      assert.equal(nativeCapabilities.max_source_bytes, 32 * 1024 * 1024);
      assert.equal(nativeCapabilities.max_page_pixels, 16_000_000);
      assert.equal(nativeCapabilities.max_pages, 10_000);
      receipt.native_documents.capabilities = nativeCapabilities;
      const pdf = await step('import the pinned multipage PDF through actual mobile rasterization', async () => {
        assert.equal(manifest.filename, 'two-pages.pdf');
        const bytes = await fs.readFile(path.join(opt.inputs, manifest.filename));
        assert.equal(bytes.length, manifest.bytes);
        assert.equal(digest(bytes), manifest.sha256);
        const sourceUri = 'content://com.crispstrobe.native-ocr-fixture/Case%3APDF%2Ftwo-pages.pdf';
        const result = await importBytes('ocr_workspace_import_bytes', bytes,
          { name: 'Überblick-日本語.pdf', sourceUri, requestId: randomUUID() });
        assert.equal(result.source_uri, sourceUri, 'Native import changed the supplied provider identity.');
        assert.equal(result.name, 'Überblick-日本語.pdf', 'UTF-8 import metadata did not survive Tauri IPC.');
        assert.equal(result.pages.length, manifest.pages.length);
        assert.equal(await privateDigest(archivedPath(result)), manifest.sha256, 'Native PDF import rewrote original bytes.');
        for (const [index, asset] of result.pages.entries()) {
          validateAsset(asset, appData);
          const expected = manifest.pages[index];
          assert.equal(asset.page_width_points, expected.width_points);
          assert.equal(asset.page_height_points, expected.height_points);
          assert(Math.abs(asset.width - expected.width_points * 200 / 72) <= 1);
          assert(Math.abs(asset.height - expected.height_points * 200 / 72) <= 1);
          assert(asset.width * asset.height <= nativeCapabilities.max_page_pixels);
          await fs.writeFile(path.join(output, 'native-pdf-page-' + (index + 1) + '.png'), await privateBytes(asset.image_path));
        }
        receipt.native_documents.imported_pdf = result;
        receipt.native_documents.original_sha256 = manifest.sha256;
        return result;
      }, 150_000);
      const documentId = 'document-' + randomUUID();
      saved.project.documents.push({ id: documentId, sourceUri: pdf.source_uri, archiveUri: pdf.archive_uri, name: pdf.name });
      receipt.native_documents.recognized_pages = [];
      for (let index = 0; index < pdf.pages.length; index++) {
        await step('recognize and persist actual native PDF page ' + (index + 1), async () => {
          const asset = pdf.pages[index], settings = saved.project.recognitionSettings;
          const result = await measuredRecognition('pdf-page-' + (index + 1), () => invoke('ocr_workspace_recognize', {
            pagePath: asset.image_path, config: settings.config, engine: settings.engine, language: settings.language, zone: null,
            pageWidthPoints: asset.page_width_points, pageHeightPoints: asset.page_height_points,
          }, 180_000));
          const expected = manifest.pages[index].text.join('\n');
          const text = validateRecognition(result, appData, expected);
          const target = { ...saved.project.pages[0], id: 'page-' + randomUUID(), documentId, sourcePage: index + 1,
            name: 'Native PDF page ' + (index + 1), imagePath: result.image_path, originalImagePath: asset.image_path,
            width: result.width, height: result.height, widthPoints: asset.page_width_points, heightPoints: asset.page_height_points,
            regions: result.regions.map(projectRegion), status: 'recognized', approved: false, elapsedMs: result.elapsed_ms,
            contentRevision: randomUUID(), provenance: { syntheticNativeSmoke: true, nativePdfImport: true, diagnostics: result.diagnostics } };
          saved.project.pages.push(target);
          const committed = await invoke('ocr_workspace_save', { project: saved.project, expectedRevision: saved.revision });
          assert.equal(committed.revision, saved.revision + 1);
          saved = await invoke('ocr_workspace_load', { projectId: project.id });
          assert.equal(saved.revision, committed.revision);
          assert.equal(saved.project.pages.at(-1).id, target.id);
          assert.equal(saved.project.pages.at(-1).regions.map(region => region.text).join('\n'), text);
          receipt.native_documents.recognized_pages.push({ page: index + 1, text, expected,
            unicode_cer: textErrorRate(expected, text), regions: result.regions.length, elapsed_ms: result.elapsed_ms });
        }, 220_000);
      }
      receipt.native_documents.images = [];
      for (const fixture of manifest.images ?? []) {
        const extension = fixture.filename.split('.').at(-1);
        if (!['jpg', 'jpeg'].includes(extension) && !nativeCapabilities.image_formats.includes(extension)) {
          receipt.native_documents.images.push({ filename: fixture.filename, status: 'not_supported', advertised_formats: nativeCapabilities.image_formats });
          continue;
        }
        await step('decode actual native image fixture ' + fixture.filename, async () => {
          assert(/^[a-z0-9.-]+$/.test(fixture.filename));
          const bytes = await fs.readFile(path.join(opt.inputs, fixture.filename));
          assert.equal(bytes.length, fixture.bytes);
          assert.equal(digest(bytes), fixture.sha256);
          const sourceUri = 'content://com.crispstrobe.native-ocr-fixture/Photo%3A' + fixture.filename;
          const result = await importBytes('ocr_workspace_import_bytes', bytes,
            { name: fixture.filename, sourceUri, requestId: randomUUID() });
          assert.equal(result.source_uri, sourceUri);
          assert.equal(result.pages.length, 1);
          const asset = validateAsset(result.pages[0], appData);
          assert.equal(asset.width, fixture.display_width);
          assert.equal(asset.height, fixture.display_height);
          assert.equal(await privateDigest(archivedPath(result)), fixture.sha256);
          await fs.writeFile(path.join(output, 'native-' + fixture.filename + '.png'), await privateBytes(asset.image_path));
          receipt.native_documents.images.push({ filename: fixture.filename, status: 'passed', original_sha256: fixture.sha256,
            orientation: fixture.orientation, asset });
        }, 120_000);
      }
      await step('reject a malformed PDF through the native importer', async () => {
        let rejection;
        try {
          await importBytes('ocr_workspace_import_bytes', Buffer.from('%PDF-1.7\ninvalid cross-reference and page tree\n'),
            { name: 'malformed.pdf', requestId: randomUUID() }, 45_000);
        } catch (error) { rejection = String(error); }
        assert(rejection && !/timed out|Target closed|Session closed|Connection closed/i.test(rejection),
          'A timeout or disconnected driver cannot establish malformed-document rejection.');
        receipt.native_documents.malformed_pdf_rejection = rejection;
      }, 60_000);
      receipt.native_documents.status = 'passed';
    }

    receipt.native_quality = { status: 'running', cases: [],
      spec_sha256: prepared.inputs.quality_fixtures.spec_sha256,
      qualification: 'Two self-authored German/English rasters through actual app installation, recognition, geometry ordering and durable saves. Uses classical connected-components layout, with no extra layout weights. This is not diverse corpus accuracy or physical-camera qualification.' };
    await step('install and independently verify the small German recognizer through the actual app', async () => {
      const requestId = randomUUID();
      const installed = await invoke('ocr_workspace_install_mobile_models', { language: 'deu', requestId }, 8 * 60_000);
      assert.deepEqual(installed, ['dbnet-det', 'tesseract-deu']);
      for (const model of prepared.inputs.models) {
        const checked = await verifyInstalledModel(model);
        assert.deepEqual(checked, receipt.verified_models.find(item => item.name === model.name),
          'Installing another language changed an existing verified model generation.');
      }
      const languageModel = prepared.inputs.quality_fixtures.models[0];
      const verified = await verifyInstalledModel(languageModel);
      receipt.verified_models.push(verified);
      receipt.native_quality.installer = { command: 'ocr_workspace_install_mobile_models', request_id: requestId,
        language: 'deu', installed, independently_verified_model: verified };
    }, 8 * 60_000 + 5000);
    for (const input of prepared.quality) {
      const fixture = input.fixture;
      await step('recognize and persist actual native quality case ' + fixture.id, async () => {
        await fs.writeFile(path.join(output, fixture.filename), input.bytes);
        const importedCase = await importBytes('ocr_workspace_import_image', input.bytes,
          { name: fixture.filename, requestId: randomUUID() }, 60_000);
        assert.equal(importedCase.pages.length, 1);
        const asset = validateAsset(importedCase.pages[0], appData);
        assert.equal(asset.width, fixture.width);
        assert.equal(asset.height, fixture.height);
        assert.equal(await privateDigest(archivedPath(importedCase)), input.sha256);
        const settings = { engine: 'tesseract', language: fixture.language,
          config: { ...project.recognitionSettings.config, layout: fixture.layout_engine != null,
            layout_engine: fixture.layout_engine ?? 'cc' } };
        const result = await measuredRecognition(fixture.id, () => invoke('ocr_workspace_recognize', {
          pagePath: asset.image_path, config: settings.config, engine: settings.engine, language: settings.language, zone: null,
          pageWidthPoints: asset.page_width_points, pageHeightPoints: asset.page_height_points,
        }, 180_000));
        const text = validateRecognition(result, appData, fixture.lines.map(line => line.text).join('\n'), fixture.language);
        const requested = result.diagnostics.requested_config;
        assert.equal(requested.layout, fixture.layout_engine != null);
        assert.equal(requested.layout_engine, settings.config.layout_engine);
        if (fixture.columns) {
          assert(Array.isArray(result.diagnostics.layout) && result.diagnostics.layout.length > 0,
            'Actual connected-components layout returned no evidence.');
          const order = result.diagnostics.layout_reading_order;
          assert.equal(order?.strategy, 'layout_blocks_and_column_gutters');
          assert.deepEqual([...order.indices].sort((a, b) => a - b), result.regions.map((_, index) => index),
            'Native reading-order diagnostics are not a complete region permutation.');
        }
        const observation = assessQualityFixture(result, fixture);
        const documentId = 'document-' + randomUUID(), pageId = 'page-' + randomUUID();
        saved.project.documents.push({ id: documentId, sourceUri: importedCase.source_uri,
          ...(importedCase.archive_uri ? { archiveUri: importedCase.archive_uri } : {}), name: fixture.filename });
        saved.project.pages.push({ id: pageId, documentId, sourcePage: 1, name: fixture.id,
          imagePath: result.image_path, originalImagePath: asset.image_path, width: result.width, height: result.height,
          widthPoints: result.page_width_points ?? asset.width * 72 / 300,
          heightPoints: result.page_height_points ?? asset.height * 72 / 300,
          status: 'recognized', regions: result.regions.map(projectRegion), excluded: false, approved: false,
          error: null, elapsedMs: result.elapsed_ms, profileId: 'screenshot', contentRevision: randomUUID(),
          provenance: { syntheticNativeSmoke: true, nativeQualityCase: fixture.id, recognitionSettings: settings,
            diagnostics: result.diagnostics, warnings: result.warnings } });
        saved.project.updatedAt = stamp();
        const committed = await invoke('ocr_workspace_save', { project: saved.project, expectedRevision: saved.revision });
        assert.equal(committed.revision, saved.revision + 1);
        saved = await invoke('ocr_workspace_load', { projectId: project.id });
        assert.equal(saved.revision, committed.revision);
        assert.equal(saved.project.pages.at(-1).id, pageId);
        assert.equal(saved.project.pages.at(-1).regions.map(region => region.text).join('\n'), text);
        assert.deepEqual(saved.project.pages.at(-1).provenance.recognitionSettings, settings);
        assert.equal(await adbText(['shell', 'pidof', PACKAGE]), pid, 'Application restarted during native quality recognition.');
        receipt.native_quality.cases.push({ id: fixture.id, status: 'passed', language: fixture.language,
          image_sha256: input.sha256, layout_engine: fixture.layout_engine, page_id: pageId,
          saved_revision: saved.revision, elapsed_ms: result.elapsed_ms, warnings: result.warnings, ...observation });
      }, 280_000);
    }
    assert.equal(receipt.native_quality.cases.length, prepared.quality.length);
    receipt.native_quality.status = 'passed';

    const repeatStartPages = saved.project.pages.length;
    receipt.repeated_inference = { requested_pages: opt['repeat-pages'], completed_pages: 0,
      unique_source_rasters: 2, saved_page_ids: [],
      qualification: 'Sequential actual native recognition and durable page saves, alternating two synthetic rasters. This does not establish diverse corpus accuracy or large-document capacity.' };
    for (let index = 0; index < opt['repeat-pages']; index++) {
      await step('repeat native recognition and durable page save ' + (index + 1) + '/' + opt['repeat-pages'], async () => {
        const input = saved.project.pages[index % 2];
        const settings = saved.project.recognitionSettings;
        const result = await measuredRecognition('repeat-' + (index + 1), () => invoke('ocr_workspace_recognize', {
          pagePath: input.imagePath, config: settings.config, engine: settings.engine, language: settings.language, zone: null,
          pageWidthPoints: input.widthPoints, pageHeightPoints: input.heightPoints,
        }, 180_000));
        const text = validateRecognition(result, appData);
        const pageId = 'page-' + randomUUID();
        saved.project.pages.push({ ...input, id: pageId, name: 'Repeated native observation ' + (index + 1),
          imagePath: result.image_path, width: result.width, height: result.height,
          regions: result.regions.map(projectRegion), status: 'recognized', approved: false,
          elapsedMs: result.elapsed_ms, contentRevision: randomUUID(),
          provenance: { syntheticNativeSmoke: true, repeatedObservation: index + 1, diagnostics: result.diagnostics } });
        saved.project.updatedAt = stamp();
        const committed = await invoke('ocr_workspace_save', { project: saved.project, expectedRevision: saved.revision });
        assert.equal(committed.revision, saved.revision + 1);
        saved = await invoke('ocr_workspace_load', { projectId: project.id });
        assert.equal(saved.revision, committed.revision);
        assert.equal(saved.project.pages.length, repeatStartPages + index + 1);
        assert.equal(saved.project.pages.at(-1).id, pageId);
        assert.equal(saved.project.pages.at(-1).regions.map(region => region.text).join('\n'), text);
        assert.equal(await adbText(['shell', 'pidof', PACKAGE]), pid, 'Application restarted during repeated native recognition.');
        receipt.repeated_inference.completed_pages++;
        receipt.repeated_inference.saved_page_ids.push(pageId);
      }, 220_000);
    }
    receipt.repeated_inference.saved_project_pages = saved.project.pages.length;

    await step('render the supplied review text to a private PDF and discard its staging file', async () => {
      const result = recognized[1];
      // Exercise the reviewed renderer with a deterministic caller-supplied edit.
      // This is an automated test edit, never a claim of human review.
      const regions = result.regions.map(region => ({
        ...region, confidence: region.confidence ?? null, original_text: region.text,
        text: region.text.includes('2048') ? region.text.replace('2048', '2049') : region.text,
        char_conf: [], char_conf_aligned: false, review: region.text.includes('2048') ? 'corrected' : 'unreviewed',
      }));
      assert(regions.some(region => region.text.includes('2049')));
      exportPath = remotePath(await invoke('ocr_workspace_export_target', { name: 'synthetic-reviewed.pdf' }));
      assert(exportPath.startsWith(privateRoot + '/') && /\/ocr-exports\/[0-9a-f-]{36}\/synthetic-reviewed\.pdf$/.test(exportPath));
      const exported = await invoke('ocr_workspace_export', {
        outputPath: exportPath, format: 'pdf', pdfa: false, overwrite: false, sourceUris: [imported.source_uri],
        pages: [{
          image_path: result.image_path, width: result.width, height: result.height,
          page_width_points: result.page_width_points, page_height_points: result.page_height_points,
          regions, approved: false, source_uri: imported.source_uri, source_page: 1,
          provenance: { syntheticNativeSmoke: true, automatedEdit: '2048 -> 2049; not human reviewed' },
        }],
      }, 90_000);
      assert.equal(exported.saved_path, exportPath);
      assert.equal(exported.pages, 1);
      assert.equal(exported.format, 'pdf');
      const bytes = await privateBytes(exportPath);
      assert.equal(bytes.subarray(0, 5).toString('ascii'), '%PDF-');
      assert(bytes.subarray(-1024).includes(Buffer.from('%%EOF')), 'PDF has no final EOF marker.');
      assert.equal(bytes.length, exported.bytes);
      await fs.writeFile(path.join(output, 'synthetic-reviewed.pdf'), bytes);
      receipt.pdf = { sha256: digest(bytes), bytes: bytes.length, pages: exported.pages,
        automated_text_edit: '2048 -> 2049', human_reviewed: false,
        signature_verified: true, text_layer_extracted: false, system_picker_opened: false };
      const stagingDirectory = path.posix.dirname(exportPath);
      await invoke('ocr_workspace_discard_export', { path: exportPath });
      const remaining = (await runAs(['ls', '-A', path.posix.dirname(stagingDirectory)])).stdout.split(/\r?\n/);
      assert(!remaining.includes(path.posix.basename(stagingDirectory)), 'Export staging directory remains after cleanup.');
      receipt.pdf.staging_removed = true;
      exportPath = undefined;
    }, 130_000);

    await step('persist a separate automated review and verify its native Word export independently', async () => {
      const rawRecognition = JSON.stringify(recognized);
      const existingPages = structuredClone(saved.project.pages);
      const originalPage = saved.project.pages[1];
      const mixed = '  שָׁלוֹם ABC 123 مَرْحَبًا\tEND  ';
      const reviewed = recognized[1].regions.map(region => {
        const value = projectRegion(region);
        if (value.text.includes('2048')) {
          value.text = value.text.replaceAll('2048', '2049');
          value.review = 'corrected';
          value.confidence = null;
          value.confidenceBasis = 'manual';
          value.char_conf = [];
          value.charConfidenceAligned = false;
        }
        return value;
      });
      assert(reviewed.some(region => region.text.includes('2049')),
        'The recognized source has no exact 2048 value for the declared review correction.');
      assert(reviewed.every(region => !region.text.includes('2048')));
      // This separate paragraph is an automated manual-review fixture. It is
      // neither native recognition output nor evidence of multilingual OCR.
      reviewed.push({
        id: 'region-' + randomUUID(), text: mixed, originalText: '',
        x: 0, y: 0, w: Math.min(originalPage.width, 560), h: Math.min(originalPage.height, 30),
        confidence: null, char_conf: [], confidenceBasis: 'manual',
        charConfidenceUnit: 'unknown', charConfidenceAligned: false,
        kind: 'text', review: 'corrected', excluded: false,
      });
      const reviewPage = {
        ...structuredClone(originalPage), id: 'page-' + randomUUID(),
        name: 'Automated Word review fixture', regions: reviewed, approved: false,
        contentRevision: randomUUID(),
        provenance: {
          syntheticNativeSmoke: true, automatedManualReview: true, humanReviewed: false,
          ocrQualityObservation: false, sourcePageId: originalPage.id,
          correction: '2048 -> 2049', manuallyAddedMixedDirectionParagraph: mixed,
        },
      };
      const reviewProject = structuredClone(saved.project);
      reviewProject.pages.push(reviewPage);
      reviewProject.updatedAt = stamp();
      const committed = await invoke('ocr_workspace_save', { project: reviewProject, expectedRevision: saved.revision });
      assert.equal(committed.revision, saved.revision + 1);
      saved = await invoke('ocr_workspace_load', { projectId: project.id });
      assert.equal(saved.revision, committed.revision);
      assert.deepEqual(saved.warnings, []);
      assert.deepEqual(saved.project.pages.slice(0, -1), existingPages,
        'Saving the Word review fixture changed an original recognized page.');
      const persisted = saved.project.pages.at(-1);
      assert.deepEqual(persisted, reviewPage, 'Word export must use the exact durably reloaded review.');
      assert.equal(JSON.stringify(recognized), rawRecognition, 'Automated review mutated raw recognition evidence.');
      const document = saved.project.documents.find(item => item.id === persisted.documentId);
      assert(document, 'The reviewed page lost its source document.');
      const expected = {
        schema_version: 1,
        // Word represents each CR, LF or CRLF as a paragraph boundary. It
        // preserves the logical Unicode sequence and every other character.
        paragraphs: persisted.regions.flatMap(region => region.text.split(/\r\n|\r|\n/)),
        mixed_rtl_paragraph: mixed,
        project_id: saved.project.id, saved_revision: saved.revision, page: persisted,
        raw_recognition_sha256: digest(rawRecognition),
        preexisting_pages_sha256: digest(JSON.stringify(existingPages)),
        raw_recognition_unchanged: true, preexisting_pages_unchanged: true,
        human_reviewed: false, ocr_quality_observation: false,
        qualification: 'Automated 2048 to 2049 correction plus one explicitly added mixed-direction review paragraph; raw recognition and prior pages are preserved.',
      };
      const expectedPath = path.join(output, 'synthetic-reviewed-word-state.json');
      const artifactPath = path.join(output, 'synthetic-reviewed.docx');
      const readbackPath = path.join(output, 'synthetic-reviewed-word-readback.json');
      await fs.writeFile(expectedPath, JSON.stringify(expected, null, 2) + '\n', { flag: 'wx' });
      receipt.word = {
        status: 'running', project_id: saved.project.id, saved_revision: saved.revision, page_id: persisted.id,
        automated_text_edit: '2048 -> 2049', manually_added_mixed_direction_paragraph: mixed,
        human_reviewed: false, ocr_quality_observation: false,
        raw_recognition_unchanged: true, preexisting_pages_unchanged: true,
        raw_recognition_sha256: expected.raw_recognition_sha256,
        persisted_review_artifact: path.basename(expectedPath),
        word_application_tested: false, system_picker_opened: false, provider_copy_tested: false,
        qualification: 'Actual app save/reload, native Word generation, independent OOXML logical-text readback, and private staging cleanup only.',
      };
      let artifactCaptured = false;
      try {
        exportPath = remotePath(await invoke('ocr_workspace_export_target', { name: 'synthetic-reviewed.docx' }));
        assert(exportPath.startsWith(privateRoot + '/') && /\/ocr-exports\/[0-9a-f-]{36}\/synthetic-reviewed\.docx$/.test(exportPath));
        const exported = await invoke('ocr_workspace_export', {
          outputPath: exportPath, format: 'docx', pdfa: false, overwrite: false, requestId: randomUUID(),
          sourceUris: [document.sourceUri, document.archiveUri].filter(Boolean),
          pages: [{
            id: persisted.id, name: persisted.name, image_path: persisted.imagePath,
            width: persisted.width, height: persisted.height,
            page_width_points: persisted.widthPoints, page_height_points: persisted.heightPoints,
            regions: persisted.regions.map(region => ({
              id: region.id, text: region.text, original_text: region.originalText,
              x: region.x, y: region.y, w: region.w, h: region.h,
              confidence: region.confidence, char_conf: region.char_conf,
              confidence_basis: region.confidenceBasis, char_conf_unit: region.charConfidenceUnit,
              char_conf_aligned: region.charConfidenceAligned, review: region.review, kind: region.kind,
            })),
            approved: false, source_uri: document.sourceUri, source_page: persisted.sourcePage,
            provenance: persisted.provenance,
          }],
        }, 60_000);
        // Retain the generated bytes before any returned-metadata or package
        // assertion, so a broken native writer leaves its original artifact.
        const bytes = await privateBytes(exportPath);
        await fs.writeFile(artifactPath, bytes, { flag: 'wx' });
        artifactCaptured = true;
        receipt.word.artifact = { file: path.basename(artifactPath), sha256: digest(bytes), bytes: bytes.length };
        assert.equal(exported.saved_path, exportPath);
        assert.equal(exported.pages, 1);
        assert.equal(exported.format, 'docx');
        assert.equal(exported.bytes, bytes.length);
        const reader = fileURLToPath(new URL('./verify_mobile_docx_export.py', import.meta.url));
        try {
          await execFileAsync('python3', [reader, '--artifact', artifactPath, '--expected', expectedPath, '--output', readbackPath],
            { timeout: budget(10_000, 'independent Word reader'), maxBuffer: 128 * 1024 });
        } catch (error) {
          await fs.writeFile(path.join(output, 'synthetic-reviewed-word-reader-process.json'), JSON.stringify({
            error: String(error), code: error.code ?? null,
            stdout: String(error.stdout ?? '').slice(0, 16_384), stderr: String(error.stderr ?? '').slice(0, 16_384),
          }, null, 2) + '\n', { flag: 'wx' });
          throw error;
        }
        const readback = JSON.parse(await fs.readFile(readbackPath, 'utf8'));
        assert.equal(readback.status, 'passed');
        assert.equal(readback.archive_sha256, receipt.word.artifact.sha256);
        assert.equal(readback.archive_bytes, bytes.length);
        receipt.word.readback = readback;
        const stagingDirectory = path.posix.dirname(exportPath);
        await invoke('ocr_workspace_discard_export', { path: exportPath });
        const remaining = (await runAs(['ls', '-A', path.posix.dirname(stagingDirectory)])).stdout.split(/\r?\n/);
        assert(!remaining.includes(path.posix.basename(stagingDirectory)), 'Word export staging directory remains after cleanup.');
        receipt.word.staging_removed = true;
        exportPath = undefined;
        assert.equal(await adbText(['shell', 'pidof', PACKAGE]), pid, 'Application restarted during Word export.');
        receipt.word.status = 'passed';
      } catch (error) {
        receipt.word.status = 'failed';
        if (exportPath && !artifactCaptured) {
          // A failed native command may still leave bytes worth diagnosing.
          // Give only evidence retention its own 10-second failure budget;
          // the original error remains fatal and staging cleanup still runs.
          const failedDeadline = deadline;
          deadline = Date.now() + 10_000;
          try {
            const bytes = await privateBytes(exportPath);
            await fs.writeFile(artifactPath, bytes, { flag: 'wx' });
            receipt.word.artifact = { file: path.basename(artifactPath), sha256: digest(bytes), bytes: bytes.length,
              captured_after_failure: true };
          } catch (retentionError) {
            receipt.word.artifact_retention_error = String(retentionError).slice(0, 4000);
          } finally { deadline = failedDeadline; }
        }
        throw error;
      }
    }, 140_000);

    await step('hand a capture fixture through the actual workspace file input and recognize from its UI', async () => {
      let opened = false;
      for (const candidate of await page.getByRole('button', { name: 'OCR', exact: true }).all()) {
        if (await candidate.isVisible()) { await candidate.click({ timeout: budget(15_000, 'open OCR workspace') }); opened = true; break; }
      }
      if (!opened) {
        await page.getByRole('button', { name: 'Tools', exact: true }).click({ timeout: budget(15_000, 'open mobile tools') });
        await page.locator('#mobile-tools-menu').getByRole('button', { name: 'OCR', exact: true })
          .click({ timeout: budget(15_000, 'open mobile OCR workspace') });
      }
      const workspace = page.getByRole('region', { name: 'OCR document workspace', exact: true });
      await workspace.waitFor({ state: 'visible', timeout: budget(30_000, 'workspace visible') });
      await page.waitForFunction(name => {
        const section = document.querySelector('section[aria-label="OCR document workspace"]');
        return section?.querySelector('input[aria-label="Project name"]')?.value === name
          && [...section.querySelectorAll('button')].some(button => (button.getAttribute('aria-label') ?? button.textContent).includes('Capture photo') && !button.disabled);
      }, saved.project.name, { timeout: budget(30_000, 'restore current project in the UI') });
      // Use a second real UI-created project. Reimporting the original into
      // the native-command project correctly deduplicates by source/archive;
      // that behavior must not be mistaken for a failed camera handoff.
      const priorProject = saved;
      await workspace.getByRole('button', { name: 'New', exact: true }).click({ timeout: budget(15_000, 'create UI project') });
      const uiProjectName = 'Native UI workflow ' + randomUUID();
      await workspace.getByLabel('Project name', { exact: true }).fill(uiProjectName);
      await workspace.getByRole('button', { name: 'Save', exact: true }).click({ timeout: budget(15_000, 'save UI project') });
      let uiProjectId;
      while (Date.now() < deadline) {
        const projects = await invoke('ocr_workspace_list');
        const entry = projects.find(item => item.name === uiProjectName);
        if (entry) { uiProjectId = entry.id; break; }
        await delay(500);
      }
      assert(uiProjectId && uiProjectId !== project.id, 'New UI project did not persist with a distinct identity.');
      saved = await invoke('ocr_workspace_load', { projectId: uiProjectId });
      assert.equal(saved.project.pages.length, 0);
      assert.deepEqual(await invoke('ocr_workspace_load', { projectId: project.id }), priorProject,
        'Creating the UI project changed the native-command project.');
      receipt.ui_project_id = uiProjectId;
      const before = saved.project.pages.length;
      const fileInput = workspace.getByLabel('Camera photo file', { exact: true });
      assert.equal(await fileInput.getAttribute('capture'), 'environment');
      assert.equal(await fileInput.getAttribute('accept'), 'image/*');
      // This reaches the real capture handler, native image importer, project
      // autosave and preparation UI. It deliberately does not claim that a
      // system camera opened or produced the supplied synthetic PNG.
      await fileInput.setInputFiles({ name: 'Capture handoff fixture.png', mimeType: 'image/png', buffer: prepared.bytes },
        { timeout: budget(30_000, 'capture file handoff') });
      let captured;
      while (Date.now() < deadline) {
        const loaded = await invoke('ocr_workspace_load', { projectId: uiProjectId });
        if (loaded.project.pages.length === before + 1) { saved = loaded; captured = loaded.project.pages.at(-1); break; }
        assert.equal(loaded.project.pages.length, before, 'Capture UI unexpectedly replaced or duplicated the project.');
        await delay(500);
      }
      assert(captured, 'Capture file handoff did not durably add a page.');
      assert.equal(captured.status, 'pending');
      assert.equal(captured.width, prepared.inputs.fixture.width);
      assert.equal(captured.height, prepared.inputs.fixture.height);
      assert.equal(captured.originalImagePath, captured.imagePath);
      const document = saved.project.documents.find(item => item.id === captured.documentId);
      assert(document && document.name === 'Capture handoff fixture.png');
      assert.equal(await privateDigest(archivedPath({ source_uri: document.sourceUri, archive_uri: document.archiveUri })), prepared.inputs.fixture.png_sha256);
      await page.screenshot({ path: path.join(output, 'actual-workspace-capture.png') });
      const capturedId = captured.id;
      const observed = await measuredRecognition('ui-capture', async () => {
        await workspace.locator('.command-bar').getByRole('button', { name: 'Recognize page', exact: true })
          .click({ timeout: budget(15_000, 'recognize captured page from UI') });
        while (Date.now() < deadline) {
          const loaded = await invoke('ocr_workspace_load', { projectId: uiProjectId });
          const target = loaded.project.pages.find(item => item.id === capturedId);
          assert(target, 'Captured page disappeared during UI recognition.');
          assert.notEqual(target.status, 'failed', 'UI recognition failed: ' + target.error);
          if (target.status === 'recognized') {
            saved = loaded;
            return { elapsed_ms: target.elapsedMs, page: target };
          }
          await delay(750);
        }
        throw new Error('UI recognition did not durably complete before its deadline.');
      });
      const text = observed.page.regions.map(region => region.text).join('\n');
      const cer = textErrorRate(prepared.inputs.fixture.text, text);
      assert(cer <= 0.25, 'Actual capture UI recognition exceeded the fixture Unicode CER limit: ' + cer);
      assert.equal(observed.page.approved, false);
      await page.screenshot({ path: path.join(output, 'actual-workspace-capture-recognized.png') });
      receipt.ui_interactions_tested = true;
      receipt.capture_handoff = { status: 'passed', supplied_fixture_sha256: prepared.inputs.fixture.png_sha256,
        page_id: capturedId, saved_revision: saved.revision, text, unicode_cer: cer,
        physical_camera_tested: false, system_camera_opened: false,
        scope: 'Actual workspace file input, native import, autosave, preparation view and UI-triggered recognition with a supplied synthetic image.' };
    }, 300_000);

    receipt.document_picker = { status: 'not_run', snapshots: [],
      reason: opt['device-kind'] === 'physical' ? 'Physical provider interactions require an observed device run.' : 'Native PDF import is unavailable.',
      scope: 'Dedicated Android emulator system DocumentsUI; local pinned PDF, cancellation, actual URI grant, native page import and durable project save. No cloud provider or physical-device claim.' };
    if (opt['device-kind'] === 'emulator' && capabilities.pdf_import && capabilities.native_document_import) {
      await step('cancel and select a pinned PDF through the real Android document picker', async () => {
        const manifest = prepared.inputs.document_fixtures;
        const filename = 'crisp-ocr-provider-' + randomUUID() + '.pdf';
        const source = path.join(opt.inputs, manifest.filename);
        assert.equal(await fileHash(source), manifest.sha256);
        pickerFixturePath = remotePath('/sdcard/Download/' + filename);
        pickerDumpPath = remotePath('/data/local/tmp/crisp-ocr-picker-' + randomUUID() + '.xml');
        await adb(['shell', 'test', '!', '-e', pickerFixturePath]);
        await adb(['shell', 'test', '!', '-e', pickerDumpPath]);
        await adb(['push', source, pickerFixturePath]);
        const stagedHash = (await adbText(['shell', 'sha256sum', pickerFixturePath])).match(/^([0-9a-f]{64})\s/i)?.[1];
        assert.equal(stagedHash, manifest.sha256);
        receipt.document_picker.filename = filename;
        receipt.document_picker.fixture_sha256 = manifest.sha256;
        receipt.document_picker.status = 'running';
        delete receipt.document_picker.reason;
        const workspace = page.getByRole('region', { name: 'OCR document workspace', exact: true });
        const waitReady = () => page.waitForFunction(() => {
          const section = document.querySelector('section[aria-label="OCR document workspace"]');
          return section && [...section.querySelectorAll('button')].some(button => button.textContent.trim() === 'Add documents' && !button.disabled);
        }, null, { timeout: budget(30_000, 'return from system document picker') });
        await waitReady();
        const beforeCancel = await invoke('ocr_workspace_load', { projectId: saved.project.id });
        await workspace.locator('.command-bar').getByRole('button', { name: 'Add documents', exact: true }).click();
        let cancellationOpened = false;
        for (let attempt = 0; attempt < 4; attempt++) {
          const snapshot = await pickerSnapshot('cancel-' + attempt, filename);
          if (snapshot.present) { cancellationOpened = true; break; }
          await delay(500);
        }
        assert(cancellationOpened, 'Add documents did not open the actual system picker.');
        await adb(['shell', 'input', 'keyevent', 'KEYCODE_BACK']);
        await waitReady();
        assert.deepEqual(await invoke('ocr_workspace_load', { projectId: saved.project.id }), beforeCancel,
          'Cancelling the real system picker mutated the saved project.');
        receipt.document_picker.cancellation_preserved_project = true;

        await workspace.locator('.command-bar').getByRole('button', { name: 'Add documents', exact: true }).click();
        let selected = false, returned = false;
        const navigation = new Set();
        for (let attempt = 0; attempt < 12; attempt++) {
          const snapshot = await pickerSnapshot('select-' + attempt, filename, selected);
          if (!snapshot.present) {
            if (selected) { returned = true; break; }
            await delay(500);
            continue;
          }
          const action = snapshot.action;
          if (!action) { await delay(500); continue; }
          if (action.kind === 'select-fixture') selected = true;
          else {
            assert(!navigation.has(action.kind), 'System picker repeated a navigation action without reaching the exact fixture.');
            navigation.add(action.kind);
          }
          await adb(['shell', 'input', 'tap', String(action.x), String(action.y)]);
          await delay(750);
        }
        assert(selected && returned, 'The observed picker did not return the selected fixture.');
        await waitReady();
        let importedDocument;
        while (Date.now() < deadline) {
          const loaded = await invoke('ocr_workspace_load', { projectId: saved.project.id });
          const found = loaded.project.documents.find(item => item.name === filename);
          if (found) { saved = loaded; importedDocument = found; break; }
          assert.equal(loaded.project.pages.length, beforeCancel.project.pages.length, 'Provider import partially committed pages.');
          await delay(500);
        }
        assert(importedDocument, 'Selected provider PDF was not durably imported.');
        const uri = new URL(importedDocument.sourceUri);
        assert.equal(uri.protocol, 'content:');
        assert(uri.host.endsWith('.downloads.documents') || uri.host.endsWith('.externalstorage.documents'),
          'System picker returned an unexpected local provider authority.');
        assert.equal(await privateDigest(archivedPath({ archive_uri: importedDocument.archiveUri })), manifest.sha256);
        const providerCopy = await invoke('plugin:mobile-provider|read_file', { uri: importedDocument.sourceUri });
        assert.equal(digest(Buffer.from(providerCopy.data, 'base64')), manifest.sha256, 'Actual provider grant did not read the selected original.');
        const pages = saved.project.pages.filter(item => item.documentId === importedDocument.id);
        assert.equal(pages.length, manifest.pages.length);
        assert.equal(saved.project.pages.length, beforeCancel.project.pages.length + manifest.pages.length);
        for (const [index, item] of pages.entries()) {
          const expected = manifest.pages[index];
          assert.equal(item.widthPoints, expected.width_points);
          assert.equal(item.heightPoints, expected.height_points);
          assert(Math.abs(item.width - expected.width_points * 200 / 72) <= 1);
          assert(Math.abs(item.height - expected.height_points * 200 / 72) <= 1);
          assert.equal(item.status, 'pending');
          validateAsset({ image_path: item.imagePath, width: item.width, height: item.height }, appData);
          await fs.writeFile(path.join(output, 'provider-ui', 'imported-page-' + (index + 1) + '.png'), await privateBytes(item.imagePath));
        }
        await page.screenshot({ path: path.join(output, 'provider-ui', 'imported-workspace.png') });
        receipt.document_picker = { ...receipt.document_picker, status: 'passed',
          source_uri: importedDocument.sourceUri, archive_uri: importedDocument.archiveUri,
          original_sha256: manifest.sha256, page_ids: pages.map(item => item.id), saved_revision: saved.revision,
          provider_read_after_picker: true, provider_read_after_restart: false, cloud_provider_tested: false };
        receipt.system_document_picker_tested = true;
      }, 240_000);
    }

    await step('verify originals remain unchanged and the app process survives', async () => {
      assert.equal(await privateDigest(archivedPath(imported)), prepared.inputs.fixture.png_sha256);
      assert.equal(await privateDigest(original.image_path), receipt.original_asset_sha256);
      assert.equal(await adbText(['shell', 'pidof', PACKAGE]), pid, 'The app restarted or crashed during native OCR.');
      receipt.originals_preserved = true;
      const screenshot = (await adb(['exec-out', 'screencap', '-p'], { encoding: null, maxBuffer: 16 * 1024 * 1024 })).stdout;
      assert.equal(screenshot.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
      await fs.writeFile(path.join(output, 'application-screen.png'), screenshot);
    });
    receipt.status = 'passed';
  } catch (error) {
    receipt.status = 'failed';
    receipt.error = String(error?.stack ?? error).slice(0, 32_768);
    console.error(receipt.error);
    process.exitCode = 1;
  } finally {
    for (const value of [pickerDumpPath, pickerFixturePath].filter(Boolean)) {
      try {
        assert.equal(opt['device-kind'], 'emulator');
        assert(/^\/data\/local\/tmp\/crisp-ocr-picker-[0-9a-f-]{36}\.xml$|^\/sdcard\/Download\/crisp-ocr-provider-[0-9a-f-]{36}\.pdf$/.test(value));
        await adb(['shell', 'rm', '-f', value]);
      } catch (error) { receipt.cleanup_errors.push('Remove named emulator picker fixture: ' + String(error)); }
    }
    if (exportPath && page) {
      try { await invoke('ocr_workspace_discard_export', { path: exportPath }, 10_000); }
      catch (error) { receipt.cleanup_errors.push('Export staging: ' + String(error)); }
    }
    if (pid) {
      try {
        const logs = await adb(['logcat', '-d', '--pid=' + pid, '-v', 'threadtime', '-t', '3000'], { maxBuffer: 8 * 1024 * 1024 });
        await fs.writeFile(path.join(output, 'application-logcat.txt'), logs.stdout);
      } catch (error) { receipt.cleanup_errors.push('Capture logcat: ' + String(error)); }
    }
    if (browser) {
      try { await bounded(browser.close(), 10_000, 'disconnect CDP'); }
      catch (error) { receipt.cleanup_errors.push('Disconnect CDP: ' + String(error)); }
    }
    if (forwardPort) {
      try { await adb(['forward', '--remove', 'tcp:' + forwardPort]); }
      catch (error) { receipt.cleanup_errors.push('Remove ADB forward: ' + String(error)); }
    }
    await fs.writeFile(path.join(output, 'webview-console.json'), JSON.stringify(consoleLog, null, 2) + '\n');
    if (receipt.cleanup_errors.length && receipt.status === 'passed') {
      receipt.status = 'failed';
      process.exitCode = 1;
    }
    receipt.finished_at = stamp();
    await saveReceipt();
    console.log('[mobile-ocr] ' + receipt.status + ': ' + path.join(output, 'receipt.json'));
  }
}

main().catch(error => { console.error(error?.stack ?? error); process.exitCode = 1; });
