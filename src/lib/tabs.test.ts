import { describe, expect, it, vi } from 'vitest';

// lucide-svelte icons need the svelte plugin to resolve; visibleTabs never
// renders them, so stub the icon imports tabs.ts uses.
vi.mock('lucide-svelte', () => ({
	Database: 'i',
	HardDrive: 'i',
	FileText: 'i',
	Languages: 'i',
	Library: 'i',
	ListChecks: 'i',
	MessageSquare: 'i',
	ScanText: 'i',
	Sparkles: 'i',
	Eye: 'i',
	Image: 'i',
	Mic: 'i',
	Volume2: 'i',
}));

import { AITOOLKIT_TABS, CORE_TABS, MOBILE_TABS, visibleTabs } from './tabs';

describe('tab registry', () => {
	it('core tabs without `requires` are visible on every build', () => {
		const ids = visibleTabs(CORE_TABS, new Set()).map((t) => t.id);
		// The document workflow is the product; it is never gated.
		expect(ids).toEqual(expect.arrayContaining(['batch', 'drives', 'chat', 'catalog', 'pdf']));
		// Exactly the tabs carrying `requires` are the ones that drop out.
		const gated = CORE_TABS.filter((t) => t.requires?.length).map((t) => t.id);
		expect(gated.sort()).toEqual(['aitoolkit', 'ocr']);
		for (const id of gated) expect(ids).not.toContain(id);
	});

	it('AIToolkit tabs are gated by service:* capabilities', () => {
		expect(visibleTabs(AITOOLKIT_TABS, new Set()).length).toBe(0);
		const ids = visibleTabs(
			AITOOLKIT_TABS,
			new Set(['build:aitoolkit', 'service:chat', 'service:extract']),
		).map((t) => t.id);
		expect(ids).toContain('ai:chat');
		expect(ids).toContain('ai:extract');
		expect(ids).not.toContain('ai:vision');
	});

	// PLAN P36.16. The AIToolkit backend lives in a private repo, so a
	// shipped build must not offer the tabs at all — not the parent, and not
	// the eight capability children even if a stale `service:*` set survives.
	it('every AIToolkit tab needs the build to carry AIToolkit', () => {
		expect(visibleTabs(CORE_TABS, new Set()).map((t) => t.id)).not.toContain('aitoolkit');
		expect(visibleTabs(CORE_TABS, new Set(['build:aitoolkit'])).map((t) => t.id)).toContain(
			'aitoolkit',
		);

		// The children are the subtle case: gating only the parent would
		// leave these rendering, because `visibleTabs` filters each tab
		// independently rather than walking a hierarchy.
		const everyService = new Set(AITOOLKIT_TABS.map((t) => `service:${t.id.slice(3)}`));
		expect(visibleTabs(AITOOLKIT_TABS, everyService)).toHaveLength(0);
		expect(
			visibleTabs(AITOOLKIT_TABS, new Set([...everyService, 'build:aitoolkit'])),
		).toHaveLength(AITOOLKIT_TABS.length);
	});

	it('MOBILE_TABS is the core mobile subset', () => {
		const ids = MOBILE_TABS.map((t) => t.id);
		expect(ids).toEqual(['batch', 'chat', 'catalog', 'translate', 'ocr']);
		expect(ids).not.toContain('history');
	});

	// PLAN P36.5. The iOS release job passes no `--features`, so no OCR
	// tier is compiled in — and the tab used to render anyway, opening onto
	// nothing. App Review Guideline 2.1 (App Completeness) is the most
	// common rejection there is.
	it('the OCR tab is hidden when no OCR tier was compiled in', () => {
		expect(visibleTabs(CORE_TABS, new Set()).map((t) => t.id)).not.toContain('ocr');
		expect(visibleTabs(CORE_TABS, new Set(['build:ocr'])).map((t) => t.id)).toContain('ocr');
	});

	// The mobile bar has to obey the same gate. Filtering `MOBILE_TABS`
	// directly — which is what the bottom nav used to do — would put the
	// dead tab back on precisely the platform it was dead on.
	it('the mobile bar applies the same build gate as the desktop nav', () => {
		expect(visibleTabs(MOBILE_TABS, new Set()).map((t) => t.id)).not.toContain('ocr');
		expect(visibleTabs(MOBILE_TABS, new Set(['build:ocr'])).map((t) => t.id)).toContain('ocr');
	});

	// `build:*` and `service:*` share one capability set. They must not be
	// able to satisfy each other's gates.
	it('build and service capabilities live in separate namespaces', () => {
		const buildOnly = new Set(['build:ocr']);
		expect(visibleTabs(AITOOLKIT_TABS, buildOnly).length).toBe(0);
		const serviceOnly = new Set(['service:ocr']);
		expect(visibleTabs(CORE_TABS, serviceOnly).map((t) => t.id)).not.toContain('ocr');
	});
});
