/** Wire receipts for independently indexed documents inside a source container. */
export type ContainerEntryKind = 'zip' | 'tar' | 'gzip' | 'sevenz' | 'mailbox-message' | 'mime-part';
export type ContainerKind = 'zip' | 'tar' | 'gzip' | 'sevenz' | 'email' | 'mailbox';
export interface ContainerEntry {
    kind: ContainerEntryKind;
    index: number;
    occurrence: number;
    name: string;
    sha256: string;
    size: number;
}
export interface ContainerSourceReceipt { kind: ContainerKind; sha256: string; size: number }
export interface ContainerCoverage {
    discovered: number;
    extracted: number;
    skipped: number;
    omissions: { path: string; reason: 'unsupported' | 'encrypted' | 'unsafe-name' | 'link' | 'no-text' }[];
    omittedDetails: number;
}
export interface ContainerMemberLocator {
    version: 1;
    rootOwnerId: string;
    rootDocId: string;
    rootLocationUri: string;
    rootSha256: string;
    entries: ContainerEntry[];
}

const entryKinds = new Set<unknown>(['zip', 'tar', 'gzip', 'sevenz', 'mailbox-message', 'mime-part']);
const containerKinds = new Set<unknown>(['zip', 'tar', 'gzip', 'sevenz', 'email', 'mailbox']);
const omissionReasons = new Set<unknown>(['unsupported', 'encrypted', 'unsafe-name', 'link', 'no-text']);
const bytes = (value: string): number => new TextEncoder().encode(value).byteLength;
const object = (value: unknown): Record<string, unknown> => {
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid container receipt.');
    return value as Record<string, unknown>;
};
const integer = (value: unknown, maximum: number): value is number => Number.isSafeInteger(value) && Number(value) >= 0 && Number(value) <= maximum;
const sha256 = (value: unknown): value is string => typeof value === 'string' && /^[a-f\d]{64}$/i.test(value);
const boundedText = (value: unknown, maximum: number): value is string => typeof value === 'string' && value.length > 0
    && bytes(value) <= maximum && !/[\u0000-\u001f\u007f-\u009f]/u.test(value);

function canonicalName(value: unknown): value is string {
    return boundedText(value, 2048) && !/[\\:\u202a-\u202e\u2066-\u2069]/u.test(value)
        && !value.startsWith('/') && value.split('/').every(part => part !== '' && part !== '.' && part !== '..');
}

export function readContainerEntries(value: unknown): ContainerEntry[] {
    if (!Array.isArray(value) || value.length === 0 || value.length > 4) throw new Error('Invalid container member chain.');
    return value.map(item => {
        const entry = object(item);
        if (!entryKinds.has(entry.kind) || !integer(entry.index, 2047) || !integer(entry.occurrence, 2047)
            || !canonicalName(entry.name) || !sha256(entry.sha256) || !integer(entry.size, 32 * 1024 * 1024)) {
            throw new Error('Invalid container member byte receipt.');
        }
        return { kind: entry.kind as ContainerEntryKind, index: entry.index, occurrence: entry.occurrence,
            name: entry.name, sha256: entry.sha256, size: entry.size };
    });
}

export function readContainerSource(value: unknown): ContainerSourceReceipt {
    const source = object(value);
    if (!containerKinds.has(source.kind) || !sha256(source.sha256)
        || !integer(source.size, (source.kind === 'mailbox' ? 256 : 128) * 1024 * 1024)) {
        throw new Error('Invalid container source byte receipt.');
    }
    return { kind: source.kind as ContainerKind, sha256: source.sha256, size: source.size };
}

export function readContainerCoverage(value: unknown): ContainerCoverage {
    const coverage = object(value);
    if (!integer(coverage.discovered, 2048) || !integer(coverage.extracted, 2048) || !integer(coverage.skipped, 2048)
        || !integer(coverage.omittedDetails, 2048) || coverage.extracted + coverage.skipped > coverage.discovered
        || !Array.isArray(coverage.omissions) || coverage.omissions.length > 64
        || coverage.omittedDetails + coverage.omissions.length !== coverage.skipped) {
        throw new Error('Invalid container extraction coverage.');
    }
    const omissions = coverage.omissions.map(item => {
        const omission = object(item);
        if (!boundedText(omission.path, 8192) || !omissionReasons.has(omission.reason)) throw new Error('Invalid container omission.');
        return { path: omission.path, reason: omission.reason as ContainerCoverage['omissions'][number]['reason'] };
    });
    return { discovered: coverage.discovered, extracted: coverage.extracted, skipped: coverage.skipped,
        omittedDetails: coverage.omittedDetails, omissions };
}

/** UI parsing is defensive; the native opener independently verifies stored identity. */
export function readContainerMember(value: unknown): ContainerMemberLocator | null {
    try {
        const locator = object(value);
        if (locator.version !== 1 || !boundedText(locator.rootOwnerId, 512) || !locator.rootOwnerId.trim()
            || !boundedText(locator.rootDocId, 512) || !locator.rootDocId.trim()
            || !boundedText(locator.rootLocationUri, 8192) || locator.rootLocationUri.startsWith('crisp+member:')
            || !sha256(locator.rootSha256)) return null;
        return { version: 1, rootOwnerId: locator.rootOwnerId, rootDocId: locator.rootDocId,
            rootLocationUri: locator.rootLocationUri, rootSha256: locator.rootSha256,
            entries: readContainerEntries(locator.entries) };
    } catch { return null; }
}

export function isContainerMemberUri(uri: string): boolean { return uri.startsWith('crisp+member://'); }

export interface ContainerSearchSource {
    doc_id: string;
    owner_id?: string;
    location_uri: string;
    metadata_json?: string;
    container_member?: unknown;
}

export function resultContainerMember(result: ContainerSearchSource): ContainerMemberLocator | null {
    if (!isContainerMemberUri(result.location_uri)) return null;
    let value = result.container_member;
    if (value == null && result.metadata_json) {
        try { value = JSON.parse(result.metadata_json)?.container_member; } catch { return null; }
    }
    const locator = readContainerMember(value);
    const identity = /^crisp\+member:\/\/v1\/[a-f\d]{64}\/([a-f\d]{64})$/.exec(result.location_uri);
    return locator && locator.rootOwnerId === result.owner_id && identity?.[1] === result.doc_id ? locator : null;
}

export function resultContainerCoverage(result: { metadata_json?: string; container_coverage?: unknown }): ContainerCoverage | null {
    try {
        const value = result.container_coverage ?? (result.metadata_json && JSON.parse(result.metadata_json)?.container_coverage);
        return value ? readContainerCoverage(value) : null;
    } catch { return null; }
}

export interface OpenedContainerMember {
    path: string;
    filename: string;
    rootLocationUri: string;
    rootSha256: string;
    ownerId: string;
    docId: string;
    sourceHash: string;
}

/** A late or mismatched native reply never becomes another document's preview. */
export function readOpenedContainerMember(value: unknown, result: ContainerSearchSource & { source_hash?: string }): OpenedContainerMember {
    const opened = object(value);
    if (opened.ownerId !== result.owner_id || opened.docId !== result.doc_id
        || !boundedText(opened.path, 16384) || !(/^\//.test(opened.path) || /^[a-z]:[\\/]/i.test(opened.path) || /^\\\\[^\\]/.test(opened.path))
        || !boundedText(opened.filename, 2048) || /[\\/]/.test(opened.filename)
        || !boundedText(opened.rootLocationUri, 8192) || !sha256(opened.rootSha256) || !sha256(opened.sourceHash)
        || (result.source_hash && result.source_hash.toLowerCase() !== opened.sourceHash.toLowerCase())) {
        throw new Error('The verified copy does not match the selected indexed member. Search again.');
    }
    const locator = resultContainerMember(result);
    if (locator && (opened.rootLocationUri !== locator.rootLocationUri
        || opened.rootSha256.toLowerCase() !== locator.rootSha256.toLowerCase()
        || opened.filename !== locator.entries.at(-1)?.name.split('/').at(-1)
        || opened.sourceHash.toLowerCase() !== locator.entries.at(-1)?.sha256.toLowerCase())) {
        throw new Error('The member source changed since these results were returned. Search again.');
    }
    return opened as unknown as OpenedContainerMember;
}

/** Complete logical provenance; names are display text, never filesystem joins. */
export function containerBreadcrumb(locator: ContainerMemberLocator): string {
    return [locator.rootLocationUri, ...locator.entries.map(entry => entry.name
        + (entry.occurrence > 0 ? ` (occurrence ${entry.occurrence + 1})` : ''))].join(' › ');
}
