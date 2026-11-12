/**
 * What this build can actually do — PLAN P36.5.
 *
 * Mirrors `src-tauri/src/capabilities.rs`. The backend reports `cfg!`
 * flags; nothing here guesses.
 *
 * ## Why this replaced `platform.ts`
 *
 * The old module sniffed `navigator.userAgent` to decide what to render.
 * That answered the wrong question — "am I on iOS?" was standing in for
 * "was OCR compiled into this binary?" — and the two had already come
 * apart: the iOS release job passes no `--features`, so the OCR and
 * Translate tabs rendered with nothing behind them (App Review Guideline
 * 2.1, App Completeness). It also could not see the Mac App Store build
 * at all, which is desktop by every user-agent measure and yet has no
 * sidecar providers, no Tesseract and no printing.
 *
 * Use `caps` for anything that depends on what was built. Reach for
 * `platform` only where the decision is genuinely about the OS.
 */

import { invoke } from '@tauri-apps/api/core';
import { writable, derived, get } from 'svelte/store';
import { supportsOcrWorkbench } from './workflow';
import { beginOperation } from './operationLog';

export interface Capabilities {
	platform: string;
	mobile: boolean;
	desktop: boolean;
	sidecars: boolean;
	dev_tools: boolean;
	/** PLAN P36.16 — the AIToolkit panels. Off in shipped builds. */
	aitoolkit: boolean;
	/** Optional cloud-backup-compatible API connector. */
	cloud_backup: boolean;

	local_llm: boolean;
	launch_local_servers: boolean;

	ocr: boolean;
	ocr_tiers: string[];
	pdf_render: boolean;
	pdf_zpdf: boolean;

	asr: boolean;
	tts: boolean;
	tts_watermarked: boolean;

	translate_align: boolean;
	translate_nmt: boolean;

	drive_filen_native: boolean;
	drive_internxt_native: boolean;
	drive_subprocess: boolean;
	fuse: boolean;

	audio_glint: boolean;
	audio_ffmpeg: boolean;

	direct_print: boolean;
	share_sheet: boolean;

	flags: string[];
}

/**
 * Pre-probe defaults: everything off.
 *
 * Deliberately pessimistic. A control that appears once the probe answers
 * is a moment of UI settling; a control that appears immediately and then
 * turns out to be unbacked is the dead-surface bug this module exists to
 * remove. Defaulting to "off" means the failure mode of a probe that
 * never returns is a smaller app, not a broken one.
 */
const UNKNOWN: Capabilities = {
	platform: 'unknown',
	mobile: false,
	desktop: false,
	sidecars: false,
	dev_tools: false,
	aitoolkit: false,
	cloud_backup: false,
	local_llm: false,
	launch_local_servers: false,
	ocr: false,
	ocr_tiers: [],
	pdf_render: false,
	pdf_zpdf: false,
	asr: false,
	tts: false,
	tts_watermarked: false,
	translate_align: false,
	translate_nmt: false,
	drive_filen_native: false,
	drive_internxt_native: false,
	drive_subprocess: false,
	fuse: false,
	audio_glint: false,
	audio_ffmpeg: false,
	direct_print: false,
	share_sheet: false,
	flags: []
};

export const caps = writable<Capabilities>(UNKNOWN);

/** True once the backend has answered — lets views hold a spinner rather than flashing an empty state. */
export const capsLoaded = writable(false);
export const capsLoading = writable(false);
export const capsError = writable<string | null>(null);

/** Build keys for tab visibility, including the workbench derived from reported OCR tiers. */
export const buildFlags = derived(caps, ($caps) => {
    const flags = new Set($caps.flags);
    flags.delete('build:ocr-workbench');
    if (supportsOcrWorkbench($caps)) flags.add('build:ocr-workbench');
    return flags;
});

let probe: Promise<Capabilities> | null = null;

/**
 * Ask the backend once and cache it. Safe to call from anywhere; every
 * caller after the first awaits the same in-flight request.
 *
 * Capabilities are compile-time facts, so there is nothing to invalidate
 * — with one wrinkle: `ocr_tiers` also reflects model files on disk, so a
 * user who downloads OCR weights mid-session will not see the tab appear
 * until relaunch. Acceptable, and better than re-probing on every render.
 */
export async function loadCapabilities(): Promise<Capabilities> {
	if (probe) return probe;
	capsLoading.set(true);
	const operation = beginOperation('app.capabilities');
	probe = invoke<unknown>('build_capabilities')
		.then((c) => {
			if (!c || typeof c !== 'object' || Array.isArray(c)) throw new Error('Invalid app capabilities response.');
			const data = c as Record<string, unknown>;
			const valid = typeof data.platform === 'string' && data.platform.trim() !== '' && data.platform !== 'unknown'
				&& Object.entries(UNKNOWN).every(([key, value]) => {
					if (Array.isArray(value)) return Array.isArray(data[key]) && (data[key] as unknown[]).every(item => typeof item === 'string');
					return typeof data[key] === typeof value;
				});
			if (!valid) throw new Error('Invalid app capabilities response.');
			const capabilities = c as Capabilities;
			caps.set(capabilities);
			capsError.set(null);
			capsLoaded.set(true);
			operation.complete();
			return capabilities;
		})
		.catch((e) => {
			// Keep gated tools unavailable until a valid response arrives.
			// The shell exposes this failure and can retry the probe alone.
			console.error('[capabilities] probe failed; conditional UI stays hidden', e);
			operation.fail(e);
			caps.set(UNKNOWN);
			capsError.set(e instanceof Error ? e.message : String(e));
			probe = null;
			capsLoaded.set(true);
			return UNKNOWN;
		})
		.finally(() => capsLoading.set(false));
	return probe;
}

/** Synchronous read for non-reactive call sites. Returns the pessimistic default before the probe resolves. */
export function currentCaps(): Capabilities {
	return get(caps);
}

/**
 * The OS, for the few decisions that really are about the platform —
 * a macOS-only share sheet, a keyboard-shortcut legend.
 *
 * Reported by the backend from `std::env::consts::OS`, so unlike the
 * user-agent sniff it does not confuse iPadOS with macOS. Anything that
 * is really asking "was this compiled in?" belongs on `caps` instead.
 */
export function platform(): string {
	return get(caps).platform;
}
