// Capability-driven tab registry (grafted from the AIToolkit crossplatform shell,
// EXT-0). The desktop nav renders from CORE_TABS instead of hardcoded buttons, so
// additional tabs — e.g. AIToolkit sidecar capabilities — can be contributed
// without editing the nav markup. Labels resolve via i18n.t.nav[id]; icons are
// lucide-svelte components.

import type { Component } from 'svelte';
import {
	Database,
	FileText,
	Languages,
	Library,
	ListChecks,
	MessageSquare,
	ScanText,
} from 'lucide-svelte';

export interface TabDef {
	id: string;
	icon: Component<any>;
	/** Render a divider before this tab. */
	separatorBefore?: boolean;
	/** Capabilities required to show this tab; undefined/empty = always visible. */
	requires?: string[];
}

/** CrispSorter's built-in tabs, in nav order (settings lives in nav-bottom). */
export const CORE_TABS: TabDef[] = [
	{ id: 'batch', icon: ListChecks },
	{ id: 'chat', icon: MessageSquare },
	{ id: 'history', icon: Database },
	{ id: 'catalog', icon: Library, separatorBefore: true },
	{ id: 'translate', icon: Languages },
	{ id: 'ocr', icon: ScanText },
	{ id: 'pdf', icon: FileText },
];

/** Tabs whose required capabilities are all present. */
export function visibleTabs(tabs: TabDef[], caps: Set<string>): TabDef[] {
	return tabs.filter((t) => !t.requires || t.requires.every((r) => caps.has(r)));
}
