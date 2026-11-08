import { describe, expect, it, vi } from 'vitest';
import { partialScanMessage, scanForCatalog, SHOWN_ISSUES, type ScanReport } from './catalogScan';

const text = { confirm_partial: '{count} unreadable in {root}:\n{examples}' };

function report(issues: number, suppressed = 0): ScanReport<{ files: number }> {
    return {
        index: { files: 3 },
        issues: Array.from({ length: issues }, (_, i) => ({
            path: `/vol/locked-${i}`,
            kind: 'traversal' as const,
            message: 'Permission denied',
        })),
        suppressed_issues: suppressed,
        skipped_symlinks: 0,
        skipped_special_entries: 0,
        excluded_by_size: 0,
    };
}

describe('scanForCatalog', () => {
    it('returns a complete scan without asking', async () => {
        const invoke = vi.fn().mockResolvedValue(report(0));
        const confirm = vi.fn();
        const result = await scanForCatalog(invoke, confirm, '/vol', text);
        expect(result).toEqual({ index: { files: 3 }, complete: true, issues: 0 });
        expect(confirm).not.toHaveBeenCalled();
        expect(invoke).toHaveBeenCalledWith('catalog_scan_dir_report', {
            root: '/vol',
            hashAlgo: null,
            maxSizeBytes: null,
        });
    });

    it('saves a partial scan only after the user agrees', async () => {
        const invoke = vi.fn().mockResolvedValue(report(2));
        const yes = vi.fn().mockResolvedValue(true);
        expect(await scanForCatalog(invoke, yes, '/vol', text)).toEqual({
            index: { files: 3 },
            complete: false,
            issues: 2,
        });
        expect(yes.mock.calls[0][0]).toContain('/vol/locked-1: Permission denied');

        const no = vi.fn().mockResolvedValue(false);
        expect(await scanForCatalog(invoke, no, '/vol', text)).toBeNull();
    });

    it('treats suppressed-only diagnostics as incomplete', async () => {
        const invoke = vi.fn().mockResolvedValue(report(0, 4));
        const no = vi.fn().mockResolvedValue(false);
        expect(await scanForCatalog(invoke, no, '/vol', text)).toBeNull();
        expect(no).toHaveBeenCalledOnce();
    });

    it('propagates scan errors such as a missing root', async () => {
        const invoke = vi.fn().mockRejectedValue('/gone: No such file or directory');
        await expect(scanForCatalog(invoke, vi.fn(), '/gone', text)).rejects.toBe(
            '/gone: No such file or directory',
        );
    });
});

describe('partialScanMessage', () => {
    it('counts every issue but quotes only the first few paths', () => {
        const msg = partialScanMessage(report(SHOWN_ISSUES + 2, 10), '/vol', text);
        expect(msg.startsWith(`${SHOWN_ISSUES + 12} unreadable in /vol:`)).toBe(true);
        expect(msg.split('\n').filter((l) => l.startsWith('• /vol/'))).toHaveLength(SHOWN_ISSUES);
        expect(msg).toContain('• … +12');
    });
});
