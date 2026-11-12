import { relocateDriveSearchUri } from './browser';
import type { DuplicateMutationAudit } from './duplicateAudit';
import type { ContextPanel } from './panels';

/** Complete a provider move against its originating group, even after navigation. */
export async function completeDuplicateMove(deps: {
    current(): ContextPanel | null;
    changed(panel: ContextPanel): void;
    move(driveId: string, source: string, destination: string): Promise<unknown>;
    record(entry: DuplicateMutationAudit): void;
}, operation: {
    groupId: string;
    driveId: string;
    source: string;
    destination: string;
    audit: DuplicateMutationAudit;
}): Promise<void> {
    const { groupId, driveId, source, destination, audit } = operation;
    await deps.move(driveId, source, destination);
    // Success belongs in the audit independently of which panel is now visible.
    deps.record({ ...audit, at: Date.now() });
    const panel = deps.current();
    if (panel?.source.kind !== 'DuplicateGroup' || panel.source.groupId !== groupId) return;
    const items = panel.source.items.map(item => ({
        ...item,
        path: relocateDriveSearchUri(item.path, driveId, source, destination),
    }));
    // Use the current group's state so intervening decisions are preserved.
    deps.changed({ ...panel, source: { ...panel.source, items } });
}
