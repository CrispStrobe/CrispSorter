export type PanelSource =
    | { kind: 'LocalPath'; path: string }
    | { kind: 'CloudDrive'; driveId: string; path: string }
    | { kind: 'SearchResults'; query: string }
    | { kind: 'DuplicateGroup'; groupId: string }
    | { kind: 'CatalogArchive'; archivePath: string }
    | { kind: 'RemoteSearchResults'; provider: string; query: string };

export type ContextPanel = {
    source: PanelSource;
    title: string;
};

export function cloudDrivePanel(driveId: string, path: string, title = 'Cloud files'): ContextPanel {
    return { source: { kind: 'CloudDrive', driveId, path }, title };
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
