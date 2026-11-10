import { isProviderUri } from '../mobileProvider';

export interface DuplicateSource {
    id: string;
    originalPath: string;
    size: number;
    status: string;
}

export interface VerifiedDuplicateGroup<T extends DuplicateSource = DuplicateSource> {
    size: number;
    sha256: string;
    items: T[];
}

export interface DuplicateIssue<T> {
    item: T;
    reason: string;
}

export interface DuplicateScan<T extends DuplicateSource> {
    groups: VerifiedDuplicateGroup<T>[];
    failed: DuplicateIssue<T>[];
    skipped: DuplicateIssue<T>[];
    checked: number;
}

export const DUPLICATE_MAX_BYTES = 500 * 1024 * 1024;
const BUSY_OR_COMPLETED = new Set(['extracting', 'analyzing', 'moving', 'done']);

function ineligibleReason(item: DuplicateSource): string | null {
    if (isProviderUri(item.originalPath)) return 'Read-only provider document';
    if (BUSY_OR_COMPLETED.has(item.status)) return 'Processing or already sorted';
    if (!Number.isFinite(item.size) || item.size < 0) return 'File size unavailable';
    if (item.size > DUPLICATE_MAX_BYTES) return 'Larger than the 500 MiB verification limit';
    return null;
}

async function checkedHash(path: string, hashFile: (path: string) => Promise<string>): Promise<string> {
    const hash = await hashFile(path);
    if (typeof hash !== 'string' || !/^[0-9a-f]{64}$/i.test(hash)) {
        throw new Error('Could not verify SHA-256');
    }
    return hash.toLowerCase();
}

/** Size is only a cheap prefilter. A complete, successful SHA-256 is required for every match. */
export async function findVerifiedDuplicates<T extends DuplicateSource>(
    items: readonly T[],
    hashFile: (path: string) => Promise<string>,
    onProgress?: (checked: number, total: number) => void,
): Promise<DuplicateScan<T>> {
    const result: DuplicateScan<T> = { groups: [], failed: [], skipped: [], checked: 0 };
    const bySize = new Map<number, T[]>();
    const paths = new Set<string>();
    for (const original of items) {
        // Preserve the scanned identity even if a reactive row changes while hashing.
        const item = { ...original };
        const reason = ineligibleReason(item);
        if (reason || paths.has(item.originalPath)) {
            result.skipped.push({ item, reason: reason ?? 'Another entry points to the same file' });
            continue;
        }
        paths.add(item.originalPath);
        const bucket = bySize.get(item.size) ?? [];
        bucket.push(item);
        bySize.set(item.size, bucket);
    }
    const candidates = [...bySize.values()].filter(bucket => bucket.length > 1).flat();
    const hashes = new Map<string, T[]>();
    let cursor = 0;
    onProgress?.(0, candidates.length);
    // Bound disk reads rather than launching one hash task for every file at once.
    await Promise.all(Array.from({ length: Math.min(4, candidates.length) }, async () => {
        while (cursor < candidates.length) {
            const item = candidates[cursor++];
            try {
                const hash = await checkedHash(item.originalPath, hashFile);
                const key = `${item.size}:${hash}`;
                const bucket = hashes.get(key) ?? [];
                bucket.push(item);
                hashes.set(key, bucket);
            } catch (error) {
                result.failed.push({ item, reason: String(error) });
            } finally {
                result.checked++;
                onProgress?.(result.checked, candidates.length);
            }
        }
    }));
    // Keep the user's batch order, independent of hashing completion order.
    const order = new Map(items.map((item, index) => [item.id, index]));
    for (const [key, matches] of hashes) {
        if (matches.length < 2) continue;
        matches.sort((a, b) => order.get(a.id)! - order.get(b.id)!);
        result.groups.push({ size: matches[0].size, sha256: key.slice(key.indexOf(':') + 1), items: matches });
    }
    result.groups.sort((a, b) => order.get(a.items[0].id)! - order.get(b.items[0].id)!);
    return result;
}

/**
 * Delete only other copies, rechecking both the retained file and each target
 * immediately before its delete call. Failed checks/deletes stay in the batch.
 */
export async function deleteVerifiedCopies<T extends DuplicateSource>(
    group: VerifiedDuplicateGroup<T>,
    keepId: string,
    currentItems: () => readonly T[],
    hashFile: (path: string) => Promise<string>,
    deleteFile: (path: string) => Promise<void>,
): Promise<{ deleted: T[]; failed: DuplicateIssue<T>[] }> {
    const keeper = group.items.find(item => item.id === keepId);
    const targets = group.items.filter(item => item.id !== keepId);
    if (!keeper || !targets.length || new Set(group.items.map(item => item.originalPath)).size !== group.items.length) {
        throw new Error('Choose one distinct file to keep before deleting copies.');
    }
    const outcome = { deleted: [] as T[], failed: [] as DuplicateIssue<T>[] };
    function assertCurrent(item: T) {
        const current = currentItems().find(candidate => candidate.id === item.id);
        if (!current || current.originalPath !== item.originalPath || current.size !== item.size || ineligibleReason(current)) {
            throw new Error('The duplicate group changed. Refresh and review it again.');
        }
    }
    for (const item of targets) {
        try {
            assertCurrent(keeper);
            assertCurrent(item);
            const [retainedHash, targetHash] = await Promise.all([
                checkedHash(keeper.originalPath, hashFile),
                checkedHash(item.originalPath, hashFile),
            ]);
            assertCurrent(keeper);
            assertCurrent(item);
            if (retainedHash !== group.sha256 || targetHash !== group.sha256) {
                throw new Error('File contents changed since verification. Refresh and review the copies again.');
            }
            await deleteFile(item.originalPath);
            outcome.deleted.push(item);
        } catch (error) {
            outcome.failed.push({ item, reason: String(error) });
        }
    }
    return outcome;
}
