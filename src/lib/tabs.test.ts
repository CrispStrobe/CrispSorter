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
	it('core tabs are always visible (no requires)', () => {
		const ids = visibleTabs(CORE_TABS, new Set()).map((t) => t.id);
		expect(ids).toContain('batch');
		expect(ids).toContain('aitoolkit');
	});

	it('AIToolkit tabs are gated by service:* capabilities', () => {
		expect(visibleTabs(AITOOLKIT_TABS, new Set()).length).toBe(0);
		const ids = visibleTabs(AITOOLKIT_TABS, new Set(['service:chat', 'service:extract'])).map(
			(t) => t.id,
		);
		expect(ids).toContain('ai:chat');
		expect(ids).toContain('ai:extract');
		expect(ids).not.toContain('ai:vision');
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
