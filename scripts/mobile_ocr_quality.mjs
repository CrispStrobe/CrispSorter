import assert from 'node:assert/strict';
import { textErrorRate } from './mobile_ocr_probe_utils.mjs';

// Assess actual native outputs without repairing, reordering, or substituting
// text. Geometry checks are separate from the reference-text edit distance.
export function assessQualityFixture(result, fixture) {
  assert.equal(result.width, fixture.width);
  assert.equal(result.height, fixture.height);
  assert(Array.isArray(result.regions) && result.regions.length > 0, 'Native quality case returned no regions.');
  const text = result.regions.map(region => region.text).join('\n');
  const reference = fixture.lines.map(line => line.text).join('\n');
  const cer = textErrorRate(reference, text);
  assert(cer <= fixture.max_unicode_cer, 'Native quality case exceeded its Unicode CER limit: ' + cer);
  const normalized = text.normalize('NFC');
  for (const token of fixture.required_tokens) {
    assert(normalized.includes(token.normalize('NFC')), 'Native output lost a required word or diacritic: ' + token);
  }
  const observation = { text, unicode_cer: cer, required_tokens_preserved: [...fixture.required_tokens] };
  if (fixture.columns) {
    const { body_top, gutter_left, gutter_right, minimum_regions_each } = fixture.columns;
    const sequence = [];
    const counts = { left: 0, right: 0 };
    const previousY = { left: -Infinity, right: -Infinity };
    for (let index = 0; index < result.regions.length; index++) {
      const region = result.regions[index];
      if (region.y + region.h / 2 < body_top) continue;
      const centre = region.x + region.w / 2;
      const side = centre < gutter_left ? 'left' : centre > gutter_right ? 'right' : null;
      assert(side, 'Native body region is centred in the known empty column gutter.');
      assert(region.y + region.h / 2 >= previousY[side] - region.h / 2,
        'Native reading order moved upwards inside a column.');
      previousY[side] = region.y + region.h / 2;
      counts[side]++;
      sequence.push({ index, side, x: region.x, y: region.y, w: region.w, h: region.h });
    }
    assert(counts.left >= minimum_regions_each && counts.right >= minimum_regions_each,
      'Native reading order cannot pass with a missing or fragmented-away column.');
    const firstRight = sequence.findIndex(item => item.side === 'right');
    assert(firstRight > 0 && sequence.slice(firstRight).every(item => item.side === 'right'),
      'Native reading order interleaved the two columns.');
    observation.columns = { counts, observed_sequence: sequence, left_before_right: true };
  }
  return observation;
}
