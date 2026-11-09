#!/usr/bin/env node
// Measures the actual production model in separate Node processes. This is a
// CPU/history regression receipt, not a browser or physical-device benchmark.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';
import { build } from 'esbuild';

const root = fileURLToPath(new URL('../../', import.meta.url));
const currentPath = path.join(root, 'src/lib/ocr/model.ts');
const baselinePath = path.join(root, 'tests/ocr-workspace/history-baseline.ts');
const sha = (value) => createHash('sha256').update(value).digest('hex');
const text = 'Überprüfung: café — שלום 123; العربية; 東京. ';

function fixture(count) {
  const project = {
    version: 1, id: 'stress-project', name: 'Large multilingual review',
    createdAt: '2026-01-01T00:00:00.000Z', updatedAt: '2026-01-01T00:00:00.000Z',
    documents: [{ id: 'document', name: 'Multipage source', sourceUri: '/fixture/document.pdf' }],
    pages: [], confidenceThreshold: .85, profileId: 'balanced',
  };
  for (let index = 0; index < count; index++) project.pages.push({
    id: `page-${index}`, documentId: 'document', sourcePage: index + 1, name: `Page ${index + 1}`,
    imagePath: `/fixture/page-${index}.png`, originalImagePath: `/fixture/page-${index}.png`,
    width: 2400, height: 3200, widthPoints: 540, heightPoints: 720,
    status: 'recognized', excluded: false, approved: false, error: null, elapsedMs: 120,
    provenance: { engine: 'fixture' }, profileId: 'balanced', contentRevision: `revision-${index}`,
    regions: Array.from({ length: 4 }, (_, region) => ({
      id: `region-${index}-${region}`, text: text.repeat(2), originalText: text.repeat(2),
      x: 100, y: 100 + region * 100, w: 2000, h: 80, confidence: .92,
      char_conf: [], confidenceBasis: 'recognition', charConfidenceUnit: 'unknown',
      charConfidenceAligned: false, kind: 'text', review: 'unreviewed', excluded: false,
    })),
  });
  return project;
}

async function worker(bundle, mode, count, budget) {
  const api = await import(pathToFileURL(bundle).href);
  const project = fixture(count), selected = Math.floor(count / 2);
  const getText = (state) => state.present.pages[selected].regions[0].text;
  const projectBytes = Buffer.byteLength(JSON.stringify(project));
  global.gc?.();
  const startMemory = process.memoryUsage();
  let peakHeap = startMemory.heapUsed, peakRss = startMemory.rss;
  const sample = () => { const value = process.memoryUsage(); peakHeap = Math.max(peakHeap, value.heapUsed); peakRss = Math.max(peakRss, value.rss); };
  const durations = [], started = performance.now();
  let history = api.createHistory(project, 50, budget);
  for (let index = 0; index < 20; index++) {
    const begin = performance.now();
    const replace = (state, value) => ({ ...state, pages: state.pages.map((page, i) => i === selected ? {
      ...page, regions: page.regions.map((region, r) => r === 0 ? { ...region, text: value, review: 'corrected' } : region),
    } : page) });
    history = api.commitHistory(history, replace(history.present, `Edit ${index}: ${text}`));
    // The UI coalesces typing inside one history entry. That mutable present
    // must never inherit the previous entry's byte-size accounting.
    history = { ...history, present: replace(history.present, `Typed ${index}: ${text.repeat(3)}`) };
    durations.push(performance.now() - begin); sample();
  }
  const finalText = getText(history);
  const undoCount = Math.min(3, history.past.length);
  for (let i = 0; i < undoCount; i++) { history = api.undoHistory(history); sample(); }
  for (let i = 0; i < undoCount; i++) { history = api.redoHistory(history); sample(); }
  const elapsedMs = performance.now() - started;
  assert.equal(getText(history), finalText, 'Undo/redo must restore coalesced text');
  assert.equal(project.pages[selected].regions[0].text, text.repeat(2), 'Input is immutable');
  assert.equal(history.present.pages.length, count);
  assert.equal(new Set(history.present.pages.map((page) => page.id)).size, count);
  const sizes = [...history.past, ...history.future].map((entry) => Buffer.byteLength(JSON.stringify(entry)));
  const retainedBytes = sizes.reduce((sum, value) => sum + value, 0);
  assert.ok(retainedBytes <= budget, 'History must remain within its UTF-8 budget');
  if (history.pastBytes) assert.deepEqual([...history.pastBytes, ...history.futureBytes], sizes);
  const sorted = durations.slice().sort((a, b) => a - b);
  return {
    mode, pages: count, regions: count * 4, project_bytes: projectBytes,
    history_budget_bytes: budget, retained_history_bytes: retainedBytes,
    retained_snapshots: history.past.length + history.future.length,
    edits: durations.length, undo_redo_pairs: undoCount,
    elapsed_ms: elapsedMs, median_edit_ms: sorted[Math.floor(sorted.length / 2)], p95_edit_ms: sorted[Math.floor(sorted.length * .95)],
    baseline_heap_bytes: startMemory.heapUsed, peak_sampled_heap_bytes: peakHeap, peak_sampled_rss_bytes: peakRss,
    process_max_rss_kib: process.resourceUsage().maxRSS,
    integrity: 'passed', final_text_sha256: sha(finalText),
  };
}

if (process.argv[2] === '--worker') {
  const [, , , bundle, mode, count, budget] = process.argv;
  process.stdout.write(JSON.stringify(await worker(bundle, mode, Number(count), Number(budget))));
} else {
  const output = path.resolve(process.argv[2] ?? path.join(root, 'test-results/ocr-history-benchmark.json'));
  const temp = await mkdtemp(path.join(tmpdir(), 'ocr-history-'));
  try {
    const bundles = {};
    for (const [mode, entry] of Object.entries({ baseline: baselinePath, optimized: currentPath })) {
      bundles[mode] = path.join(temp, `${mode}.mjs`);
      await build({ entryPoints: [entry], outfile: bundles[mode], bundle: true, platform: 'node', format: 'esm', logLevel: 'error' });
    }
    const results = [];
    for (const [pages, budget] of [[1000, 24 * 1024 ** 2], [5000, 1024 ** 2]]) {
      for (const mode of ['baseline', 'optimized']) results.push(JSON.parse(execFileSync(process.execPath,
        ['--expose-gc', fileURLToPath(import.meta.url), '--worker', bundles[mode], mode, String(pages), String(budget)],
        { encoding: 'utf8', timeout: 120000, maxBuffer: 1024 * 1024 })));
      assert.equal(results.at(-1).final_text_sha256, results.at(-2).final_text_sha256);
      assert.equal(results.at(-1).project_bytes, results.at(-2).project_bytes);
    }
    const report = {
      schema_version: 1, captured_at: new Date().toISOString(), node: process.version,
      platform: process.platform, architecture: process.arch,
      source_sha256: sha(await readFile(currentPath)), baseline_sha256: sha(await readFile(baselinePath)),
      baseline_commit: '78edd8101bee3651b59cc50af183385527cd92bc',
      scope: 'Node model/history CPU and memory receipt; excludes WebView rendering, page pixels and native model inference.',
      timing_gate: 'No fragile wall-clock threshold. Integrity and history-byte bounds are required.', results,
    };
    await mkdir(path.dirname(output), { recursive: true });
    await writeFile(output, JSON.stringify(report, null, 2) + '\n');
    console.log(JSON.stringify({ report: output, results }, null, 2));
  } finally { await rm(temp, { recursive: true, force: true }); }
}
