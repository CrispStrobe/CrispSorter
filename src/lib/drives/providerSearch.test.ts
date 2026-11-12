import { describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { providerSearchNotice, searchProvider, type ProviderSearchReport } from './providerSearch';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const complete: ProviderSearchReport = {
    results: [], scanned_entries: 100, scan_limit: 100, scan_limit_reached: false,
    results_limit_reached: false,
};

describe('provider search completeness', () => {
    it('requests the additive report command and retains matches and scan metadata', async () => {
        const report = {
            ...complete,
            results: [{ path: '/report.txt', is_dir: false, size: 12, mtime_unix: null }],
            scan_limit_reached: true,
        };
        vi.mocked(invoke).mockResolvedValueOnce(report);
        const options = { driveId: 'd1', root: '/', query: 'report', maxDepth: 8, maxResults: 1, content: false };
        expect(await searchProvider(options)).toEqual(report);
        expect(invoke).toHaveBeenCalledWith('drive_search_report', options);
    });

    it('warns even when a truncated directory-only scan has no matches', () => {
        const notice = providerSearchNotice({ ...complete, scan_limit_reached: true });
        expect(notice).toContain('100 files and folders');
        expect(notice).toContain('Results may be incomplete');
        expect(notice).toContain('search a smaller folder');
    });

    it('warns about the result cap even when traversal completed', () => {
        const notice = providerSearchNotice({
            ...complete,
            results: [{ path: '/report.txt', is_dir: false, size: 12, mtime_unix: null }],
            results_limit_reached: true,
        });
        expect(notice).toContain('Result limit reached (1)');
        expect(notice).toContain('additional matches may exist');
        expect(notice).not.toContain('Search stopped');
    });

    it('reports both limits when both the scan and result caps were reached', () => {
        const notice = providerSearchNotice({
            ...complete, scan_limit_reached: true, results_limit_reached: true,
        });
        expect(notice).toContain('Search stopped');
        expect(notice).toContain('Result limit reached');
    });

    it('does not warn when exact-budget traversal completed below the result cap', () => {
        expect(providerSearchNotice(complete)).toBe('');
    });
});
