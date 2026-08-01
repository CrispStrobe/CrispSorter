// Capability-driven tab registry (grafted from the AIToolkit crossplatform shell,
// EXT-0). The desktop nav renders from CORE_TABS instead of hardcoded buttons, so
// additional tabs — e.g. AIToolkit sidecar capabilities — can be contributed
// without editing the nav markup. Labels resolve via i18n.t.nav[id]; icons are
// lucide-svelte components.

import type { ComponentType } from 'svelte';
import {
	Database,
	Eye,
    FileText,
    HardDrive,
	Image,
	Languages,
	Library,
	ListChecks,
	MessageSquare,
	Mic,
	ScanText,
	Sparkles,
	Volume2,
} from 'lucide-svelte';

export interface TabDef {
	id: string;
	icon: ComponentType;
	/** Fallback label when i18n.t.nav[id] is absent (e.g. grafted tabs). */
	label?: string;
	/** Render a divider before this tab. */
	separatorBefore?: boolean;
	/** Show in the compact mobile bottom bar. */
	mobile?: boolean;
	/** Capabilities required to show this tab; undefined/empty = always visible. */
	requires?: string[];
}

/** CrispSorter's built-in tabs, in nav order (settings lives in nav-bottom). */
export const CORE_TABS: TabDef[] = [
    { id: 'batch', icon: ListChecks, mobile: true },
    { id: 'drives', icon: HardDrive, separatorBefore: true },
	{ id: 'chat', icon: MessageSquare, mobile: true },
	{ id: 'history', icon: Database },
	{ id: 'catalog', icon: Library, separatorBefore: true, mobile: true },
	{ id: 'translate', icon: Languages, mobile: true },
	{ id: 'ocr', icon: ScanText, mobile: true },
	{ id: 'pdf', icon: FileText },
	{ id: 'aitoolkit', icon: Sparkles, label: 'AIToolkit', separatorBefore: true },
];

/** Core tabs shown in the compact mobile bottom bar (settings is added separately). */
export const MOBILE_TABS: TabDef[] = CORE_TABS.filter((t) => t.mobile);

/**
 * AIToolkit sidecar capabilities as first-class tabs. Ids are namespaced `ai:<cap>`
 * and gated by the `service:<cap>` capability the backend advertises, so they only
 * appear when connected + the feature is enabled.
 */
export const AITOOLKIT_TABS: TabDef[] = [
	{ id: 'ai:chat', icon: MessageSquare, label: 'AI Chat', requires: ['service:chat'], separatorBefore: true },
	{ id: 'ai:translate', icon: Languages, label: 'AI Translate', requires: ['service:translate'] },
	{ id: 'ai:vision', icon: Eye, label: 'AI Vision', requires: ['service:vision'] },
	{ id: 'ai:ocr', icon: ScanText, label: 'AI OCR', requires: ['service:ocr'] },
	{ id: 'ai:transcription', icon: Mic, label: 'AI Transcribe', requires: ['service:transcription'] },
	{ id: 'ai:tts', icon: Volume2, label: 'AI Speak', requires: ['service:tts'] },
	{ id: 'ai:images', icon: Image, label: 'AI Images', requires: ['service:images'] },
	{ id: 'ai:extract', icon: FileText, label: 'AI Extract', requires: ['service:extract'] },
];

/** Tabs whose required capabilities are all present. */
export function visibleTabs(tabs: TabDef[], caps: Set<string>): TabDef[] {
	return tabs.filter((t) => !t.requires || t.requires.every((r) => caps.has(r)));
}
