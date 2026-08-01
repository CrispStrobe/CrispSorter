export type PanelSource =
    | { kind: 'LocalPath'; path: string }
    | { kind: 'CloudDrive'; driveId: string; path: string }
    | { kind: 'SearchResults'; query: string }
    | { kind: 'DuplicateGroup'; groupId: string; items: DuplicateContextItem[]; decision: DuplicateDecision }
    | { kind: 'CatalogArchive'; archivePath: string }
    | { kind: 'RemoteSearchResults'; provider: string; query: string };

export type ContextPanel = {
    source: PanelSource;
    title: string;
};

export type PanelOf<K extends PanelSource['kind']> = ContextPanel & {
    source: Extract<PanelSource, { kind: K }>;
};

export type DuplicateContextItem = {
    path: string;
    size: number;
    mtime: number;
    hash: string | null;
    role: 'source' | 'destination';
};

export type DuplicateDecision = 'review' | 'keep_source' | 'keep_destination' | 'keep_both';

export function cloudDrivePanel(driveId: string, path: string, title = 'Cloud files'): ContextPanel {
    return { source: { kind: 'CloudDrive', driveId, path }, title };
}

export function localPathPanel(path: string, title = 'Local file'): ContextPanel {
    return { source: { kind: 'LocalPath', path }, title };
}

export function searchResultsPanel(query: string, title = 'Search results'): ContextPanel {
    return { source: { kind: 'SearchResults', query }, title };
}

export function duplicateGroupPanel(
    groupId: string,
    items: DuplicateContextItem[] = [],
    decision: DuplicateDecision = 'review',
    title = 'Duplicate group',
): PanelOf<'DuplicateGroup'> {
    return { source: { kind: 'DuplicateGroup', groupId, items, decision }, title };
}

export function remoteSearchPanel(
    provider: string,
    query: string,
    title = 'Remote search',
): ContextPanel {
    return { source: { kind: 'RemoteSearchResults', provider, query }, title };
}

export function catalogArchivePanel(
    archivePath: string,
    title = 'Catalog archive',
): ContextPanel {
    return { source: { kind: 'CatalogArchive', archivePath }, title };
}

export function panelSourceKey(source: PanelSource): string {
    switch (source.kind) {
        case 'LocalPath': return `local:${source.path}`;
        case 'CloudDrive': return `drive:${source.driveId}:${source.path}`;
        case 'SearchResults': return `search:${source.query}`;
        case 'DuplicateGroup': return `duplicates:${source.groupId}`;
        case 'CatalogArchive': return `archive:${source.archivePath}`;
        case 'RemoteSearchResults': return `remote:${source.provider}:${source.query}`;
    }
}
