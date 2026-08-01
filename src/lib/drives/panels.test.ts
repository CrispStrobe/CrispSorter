import { describe, expect, it } from 'vitest';
import { cloudDrivePanel, duplicateGroupPanel, panelSourceKey } from './panels';

describe('context panel sources', () => {
    it('keeps a cloud path provenance-safe', () => {
        const panel = cloudDrivePanel('drive-1', '/reports/annual.pdf');
        expect(panel.title).toBe('Cloud files');
        expect(panelSourceKey(panel.source)).toBe('drive:drive-1:/reports/annual.pdf');
    });

    it('distinguishes remote/search/duplicate contexts', () => {
        expect(panelSourceKey({ kind: 'SearchResults', query: 'invoice' })).toBe('search:invoice');
        expect(panelSourceKey({ kind: 'DuplicateGroup', groupId: 'g7', items: [] })).toBe('duplicates:g7');
        expect(panelSourceKey({ kind: 'RemoteSearchResults', provider: 'internxt', query: 'paper' }))
            .toBe('remote:internxt:paper');
        expect(panelSourceKey(duplicateGroupPanel('g8', [{ path: '/a', size: 1, mtime: 0, hash: null, role: 'source' }]).source)).toBe('duplicates:g8');
    });
});
