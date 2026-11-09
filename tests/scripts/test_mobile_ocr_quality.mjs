import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import { assessQualityFixture } from '../../scripts/mobile_ocr_quality.mjs';
import { textErrorRate } from '../../scripts/mobile_ocr_probe_utils.mjs';

const spec = JSON.parse(fs.readFileSync(new URL('../../tools/mobile-ocr-probe/quality.json', import.meta.url), 'utf8'));
// These are negative controls for evidence validation, never native outputs.
const resultFor = fixture => ({ width: fixture.width, height: fixture.height,
  regions: fixture.lines.map(line => ({ text: line.text, x: line.x, y: line.y, w: 500, h: 60 })) });

test('German evidence preserves composed/decomposed Unicode and rejects lost diacritics below the CER ceiling', () => {
  const fixture = spec.cases.find(item => item.language === 'deu');
  const actual = resultFor(fixture);
  for (const region of actual.regions) region.text = region.text.normalize('NFD');
  assert.equal(assessQualityFixture(actual, fixture).unicode_cer, 0);
  for (const region of actual.regions) region.text = region.text.normalize('NFD').replace(/\p{Mark}/gu, '');
  assert(textErrorRate(fixture.lines.map(line => line.text).join('\n'), actual.regions.map(region => region.text).join('\n')) < fixture.max_unicode_cer);
  assert.throws(() => assessQualityFixture(actual, fixture), /diacritic/);
});

test('two-column evidence requires both columns and their observed geometric order', () => {
  const fixture = spec.cases.find(item => item.columns);
  const actual = resultFor(fixture);
  const observed = assessQualityFixture(actual, fixture);
  assert.equal(observed.columns.left_before_right, true);
  assert.deepEqual(observed.columns.counts, { left: 4, right: 4 });
  const rowMajor = { ...actual, regions: [...actual.regions].sort((a, b) => a.y - b.y || a.x - b.x) };
  const rowReference = { ...fixture, lines: rowMajor.regions.map(region => ({ text: region.text })) };
  // Even an accidentally row-major text reference cannot hide bad geometry.
  assert.throws(() => assessQualityFixture(rowMajor, rowReference), /interleaved/);
  const missing = { ...actual, regions: actual.regions.filter(region => region.x < fixture.columns.gutter_left) };
  const missingReference = { ...fixture, required_tokens: [], lines: missing.regions.map(region => ({ text: region.text })) };
  assert.throws(() => assessQualityFixture(missing, missingReference), /missing/);
});

test('quality gates cannot ignore empty output, changed raster size or nonfinite recognition geometry', () => {
  const fixture = spec.cases.find(item => item.columns);
  assert.throws(() => assessQualityFixture({ ...resultFor(fixture), regions: [] }, fixture), /no regions/);
  assert.throws(() => assessQualityFixture({ ...resultFor(fixture), width: 1 }, fixture));
  const invalid = resultFor(fixture);
  invalid.regions[1].x = Number.NaN;
  assert.throws(() => assessQualityFixture(invalid, fixture), /gutter/);
});
