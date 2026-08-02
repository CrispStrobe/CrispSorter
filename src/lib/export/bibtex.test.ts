import { describe, expect, it } from 'vitest';
import { buildBibFile } from './bibtex';

const sources = [
	{ title: 'On Something', author: 'Ada Lovelace', year: '1843', filename: 'a.pdf', path: '/x/a.pdf' },
	{ title: 'On Another', author: 'Alan Turing', year: '1936', filename: 'b.pdf', path: '/x/b.pdf' },
];

describe('buildBibFile', () => {
	// AI Act Art 50(2): these entries are AI-inferred metadata leaving the
	// machine in a file. The badge in BatchReview tells the person who ran the
	// export and nobody who receives the .bib, so the marking has to be in the
	// document. Added after the 2026-08-02 audit found this export unmarked.
	it('marks the file as AI-generated', () => {
		const bib = buildBibFile(sources);
		expect(bib).toContain('% ai-generated: true');
		expect(bib).toContain('digital-source-type: trainedAlgorithmicMedia');
	});

	it('keeps the marking outside the entries, where every reader ignores it', () => {
		const bib = buildBibFile(sources);
		// Everything before the first `@` is comment territory. A marking that
		// landed inside an entry would corrupt the bibliography, which is how a
		// marking gets deleted by the first person who uses the file.
		const preamble = bib.slice(0, bib.indexOf('@'));
		expect(preamble).toContain('ai-generated: true');
		for (const line of preamble.split('\n')) {
			expect(line === '' || line.startsWith('%')).toBe(true);
		}
	});

	it('still produces one parseable entry per source', () => {
		const bib = buildBibFile(sources);
		expect(bib.match(/@misc\{/g)).toHaveLength(2);
		expect(bib).toContain('Lovelace');
		expect(bib).toContain('Turing');
	});

	it('marks an empty export too', () => {
		// The zero-entry case is exactly where a "only add the header if there
		// is content" shortcut would quietly drop the marking.
		expect(buildBibFile([])).toContain('% ai-generated: true');
	});
});
