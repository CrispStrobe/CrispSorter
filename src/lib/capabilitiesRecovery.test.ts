import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

beforeEach(() => {
    vi.resetModules();
    vi.mocked(invoke).mockReset();
    vi.spyOn(console, 'error').mockImplementation(() => {});
});
afterEach(() => vi.restoreAllMocks());

describe('capability recovery', () => {
    it('keeps tools gated after failures and malformed reports, then accepts a complete retry', async () => {
        const { loadCapabilities, currentCaps, buildFlags, capsError, capsLoading } = await import('./capabilities');
        const valid = { ...currentCaps(), platform: 'linux', desktop: true, ocr: true, ocr_tiers: ['crispembed'], flags: ['build:ocr'] };
        vi.mocked(invoke).mockRejectedValueOnce(new Error('Backend unavailable'));
        await loadCapabilities();
        expect(currentCaps().platform).toBe('unknown');
        expect(get(buildFlags).size).toBe(0);
        expect(get(capsError)).toBe('Backend unavailable');
        expect(get(capsLoading)).toBe(false);

        vi.mocked(invoke).mockResolvedValueOnce({ platform: 'linux', flags: ['build:ocr-workbench'] });
        await loadCapabilities();
        expect(currentCaps().platform).toBe('unknown');
        expect(get(buildFlags).size).toBe(0);
        expect(get(capsError)).toBe('Invalid app capabilities response.');

        vi.mocked(invoke).mockResolvedValueOnce(valid);
        await loadCapabilities();
        expect(currentCaps()).toEqual(valid);
        expect(get(buildFlags).has('build:ocr-workbench')).toBe(true);
        expect(get(capsError)).toBeNull();
        expect(invoke).toHaveBeenCalledTimes(3);
    });

    it('shares an in-flight retry and caches a successful capability report', async () => {
        const { loadCapabilities, currentCaps, capsLoading } = await import('./capabilities');
        const valid = { ...currentCaps(), platform: 'ios', mobile: true };
        let release!: (value: unknown) => void;
        vi.mocked(invoke).mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
        const first = loadCapabilities();
        const second = loadCapabilities();
        expect(get(capsLoading)).toBe(true);
        expect(invoke).toHaveBeenCalledTimes(1);
        expect(currentCaps().platform).toBe('unknown');
        release(valid);
        expect(await Promise.all([first, second])).toEqual([valid, valid]);
        expect(get(capsLoading)).toBe(false);
        await loadCapabilities();
        expect(invoke).toHaveBeenCalledTimes(1);
    });
});
