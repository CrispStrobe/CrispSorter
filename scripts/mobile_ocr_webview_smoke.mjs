#!/usr/bin/env node
/**
 * Real Android WebView -> Tauri -> Rust -> CrispEmbed smoke.
 *
 * Run only on a dedicated, fresh x86_64 emulator after mobile_simulator_smoke.py
 * has installed and launched the debug APK. No fixture adapter, native mocks,
 * automatic consent, system save picker, data wipe, or physical-camera claim.
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
 *   --embed-commit <40hex> [--runtime-receipt path/to/native-build.json]
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

const PACKAGE = 'com.crispstrobe.crispsorter';
const PLAYWRIGHT_VERSION = '1.62.1';
const execFileAsync = promisify(execFile);
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const hex = (value, length) => typeof value === 'string' && new RegExp('^[0-9a-f]{' + length + '}$', 'i').test(value);
const stamp = () => new Date().toISOString();

function options(argv) {
  const allowed = new Set(['serial', 'apk', 'inputs', 'output', 'source-commit', 'embed-commit', 'runtime-receipt']);
  const result = {};
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i]?.replace(/^--/, '');
    assert(argv[i]?.startsWith('--') && allowed.has(key), 'Unknown argument: ' + argv[i]);
    assert(argv[i + 1] && !argv[i + 1].startsWith('--') && !(key in result), 'Missing or repeated argument: ' + key);
    result[key] = argv[i + 1];
  }
  for (const key of allowed) if (key !== 'runtime-receipt') assert(result[key], 'Required: --' + key);
  assert(/^emulator-\d+$/.test(result.serial), 'Select a dedicated Android emulator explicitly.');
  assert(hex(result['source-commit'], 40) && hex(result['embed-commit'], 40), 'Full source and paired native commit IDs are required.');
  return result;
}

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

function validateAsset(asset, appData) {
  assert(asset && Number.isInteger(asset.width) && Number.isInteger(asset.height), 'Missing raster dimensions.');
  assert(asset.width > 0 && asset.height > 0 && asset.width * asset.height <= 100_000_000, 'Invalid raster size.');
  assert(remotePath(asset.image_path).startsWith(appData + '/ocr-workspace/assets/'), 'Page is not a managed app asset.');
  for (const key of ['page_width_points', 'page_height_points']) {
    if (asset[key] != null) assert(Number.isFinite(asset[key]) && asset[key] > 0 && asset[key] <= 14_400, 'Invalid physical page dimensions.');
  }
  return asset;
}

function validateRecognition(result, appData) {
  validateAsset(result, appData);
  assert(Array.isArray(result.regions) && result.regions.length > 0, 'Real OCR returned no regions.');
  for (const region of result.regions) {
    assert.equal(typeof region.text, 'string');
    assert([region.x, region.y, region.w, region.h].every(Number.isFinite), 'Nonfinite OCR geometry.');
    assert(region.x >= 0 && region.y >= 0 && region.w > 0 && region.h > 0, 'Empty or negative OCR geometry.');
    assert(region.x + region.w <= result.width + 0.01 && region.y + region.h <= result.height + 0.01, 'OCR box is outside the returned raster.');
  }
  const text = result.regions.map(region => region.text).join('\n');
  assert(text.trim().length > 0 && /\b2048\b/.test(text), 'Actual OCR did not recognize the synthetic document number 2048: ' + text);
  const config = result.diagnostics?.requested_config;
  assert(config && config.router === false && config.stages?.length === 1, 'The saved explicit engine settings did not reach native recognition.');
  assert.equal(config.stages[0].engine, 'tesseract');
  assert.equal(config.stages[0].rec_model, 'tesseract-eng');
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
  const opt = options(process.argv.slice(2));
  const output = path.resolve(opt.output);
  await fs.mkdir(path.dirname(output), { recursive: true });
  await fs.mkdir(output); // Never overwrite evidence from an earlier run.
  const receipt = {
    schema_version: 1, kind: 'android-webview-native-ocr', started_at: stamp(), status: 'running',
    synthetic_only: true, physical_camera_tested: false, corpus_quality_qualified: false,
    system_save_picker_tested: false, ui_interactions_tested: false,
    source: { sorter_commit: opt['source-commit'], crispembed_commit: opt['embed-commit'],
      identity_basis: 'CI checkout arguments, installed APK SHA-256, and optional native build receipt',
      github_run_id: process.env.GITHUB_RUN_ID ?? null, github_sha: process.env.GITHUB_SHA ?? null },
    serial: opt.serial, package: PACKAGE, steps: [], cleanup_errors: [],
  };
  const consoleLog = [];
  let browser, page, forwardPort, pid, privateRoot, appData, exportPath;
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
      for (const model of inputs.models) {
        assert(hex(model.sha256, 64) && /^[a-z0-9-]+$/.test(model.name));
        assert(/^[A-Za-z0-9_-]+\.gguf$/.test(model.filename));
        assert(model.url.startsWith('https://'));
      }
      assert.equal(await adbText(['shell', 'getprop', 'ro.kernel.qemu']), '1', 'Target is not an emulator.');
      const abis = (await adbText(['shell', 'getprop', 'ro.product.cpu.abilist'])).split(',');
      assert(abis.includes('x86_64'), 'This smoke requires the x86_64 runtime build.');
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
      assert.equal(installedHash?.toLowerCase(), apkHash, 'Running installation is not the supplied APK.');
      receipt.apk = { sha256: apkHash, installed_path: installedPath, abis };
      receipt.device = {
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
        await fs.writeFile(path.join(output, 'native-build-receipt.json'), runtime);
        receipt.source.native_build_receipt_sha256 = digest(runtime);
      }
      return { bytes, inputs };
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
      assert.equal(value.pdf_import, false);
      assert.equal(value.default_engine, 'tesseract');
      assert.equal(value.default_language, 'eng');
      assert(value.formats.includes('pdf'), 'The paired reviewed-PDF renderer is unavailable.');
      assert(value.engines.includes('tesseract'));
      const rawAppData = remotePath(await invoke('get_app_data_dir'));
      appData = remotePath((await runAs(['realpath', rawAppData])).stdout.trim());
      assert(appData.startsWith(privateRoot + '/'), 'Native app data is outside the app sandbox.');
      const projects = await invoke('ocr_workspace_list');
      assert.deepEqual(projects, [], 'Use a fresh emulator; existing projects must not be overwritten or erased.');
      const registry = await invoke('embedder_registry_list', {}, 60_000);
      for (const model of prepared.inputs.models) {
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
        const directory = appData + '/ocr-models/' + digest(model.name);
        const manifestPath = directory + '/current.json';
        const manifest = JSON.parse((await privateBytes(manifestPath, 128 * 1024)).toString('utf8'));
        assert.equal(manifest.schema_version, 1);
        assert.equal(manifest.model_name, model.name);
        assert(/^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i.test(manifest.version));
        assert.deepEqual(manifest.files, [{ filename: model.filename, url: model.url, sha256: model.sha256, primary: true }],
          'Baseline registry artifacts differ from the immutable test lock.');
        const artifact = directory + '/' + manifest.version + '/' + model.filename;
        assert.equal(await privateDigest(artifact), model.sha256);
        installed.push({ name: model.name, manifest_path: manifestPath, version: manifest.version, filename: model.filename, sha256: model.sha256 });
      }
      const registry = await invoke('embedder_registry_list', {}, 60_000);
      for (const model of prepared.inputs.models) assert.equal(registry.find(item => item.name === model.name)?.cached, true);
      receipt.verified_models = installed;
    }, 150_000);

    const imported = await step('import the synthetic PNG through the real raw binary IPC', async () => {
      const importBudget = budget(60_000, 'raw image import');
      const result = await bounded(page.evaluate(async ({ base64, name }) => {
        const binary = atob(base64);
        const bytes = new Uint8Array(binary.length);
        for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
        const metadata = btoa(JSON.stringify({ name })); // Fixed ASCII test name only.
        return window.__TAURI_INTERNALS__.invoke('ocr_workspace_import_image', bytes, { headers: { 'x-ocr-metadata': metadata } });
      }, { base64: prepared.bytes.toString('base64'), name: 'Synthetic mobile OCR.png' }), importBudget, 'raw image import');
      assert.equal(result.pages.length, 1);
      const asset = validateAsset(result.pages[0], appData);
      assert.equal(asset.width, prepared.inputs.fixture.width);
      assert.equal(asset.height, prepared.inputs.fixture.height);
      assert(remotePath(result.source_uri).startsWith(appData + '/ocr-workspace/sources/'));
      assert.equal(await privateDigest(result.source_uri), prepared.inputs.fixture.png_sha256, 'The archived original bytes changed during import.');
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
      documents: [{ id: 'document-' + randomUUID(), sourceUri: imported.source_uri, name: imported.name }],
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
        const result = await invoke('ocr_workspace_recognize', {
          pagePath: target.imagePath, config: settings.config, engine: settings.engine, language: settings.language, zone: null,
          pageWidthPoints: target.widthPoints, pageHeightPoints: target.heightPoints,
        }, 180_000);
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

    await step('verify originals remain unchanged and the app process survives', async () => {
      assert.equal(await privateDigest(imported.source_uri), prepared.inputs.fixture.png_sha256);
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
