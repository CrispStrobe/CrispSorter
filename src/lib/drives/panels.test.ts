import { describe, expect, it } from 'vitest';
import { catalogArchivePanel, cloudDrivePanel, duplicateGroupPanel, localPathPanel, panelSourceKey, remoteSearchPanel } from './panels';

describe('context panel sources', () => {
    it('keeps a cloud path provenance-safe', () => {
        const panel = cloudDrivePanel('drive-1', '/reports/annual.pdf');
        expect(panel.title).toBe('Cloud files');
        expect(panelSourceKey(panel.source)).toBe('drive:drive-1:/reports/annual.pdf');
    });

    it('distinguishes remote/search/duplicate contexts', () => {
        expect(panelSourceKey({ kind: 'SearchResults', query: 'invoice' })).toBe('search:invoice');
        expect(panelSourceKey({ kind: 'DuplicateGroup', groupId: 'g7', items: [], decision: 'review' })).toBe('duplicates:g7');
        expect(panelSourceKey({ kind: 'RemoteSearchResults', provider: 'internxt', query: 'paper' }))
            .toBe('remote:internxt:paper');
        expect(panelSourceKey(remoteSearchPanel('cloud-backup', 'paper').source))
            .toBe('remote:cloud-backup:paper');
        expect(panelSourceKey(catalogArchivePanel('/catalog/archive.caf').source))
            .toBe('archive:/catalog/archive.caf');
        expect(panelSourceKey(localPathPanel('/Users/alice/paper.pdf').source))
            .toBe('local:/Users/alice/paper.pdf');
        expect(panelSourceKey(duplicateGroupPanel('g8', [{ path: '/a', size: 1, mtime: 0, hash: null, role: 'source' }]).source)).toBe('duplicates:g8');
        expect(duplicateGroupPanel('g9', [], 'keep_source').source.decision).toBe('keep_source');
    });
});
