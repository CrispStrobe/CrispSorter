/**
 * Catalog scanning with an explicit decision about incomplete scans.
 *
 * `catalog_scan_dir_report` returns every file it could read plus the
 * entries it could not (permission-denied folders, files that vanished
 * mid-scan, unreadable timestamps). A CAF file cannot record that it is
 * incomplete, so a partial catalog is only saved after the user agreed.
 */

export interface ScanIssue {
    path: string;
    kind: 'traversal' | 'metadata' | 'modified_time' | 'hash';
    message: string;
}

export interface ScanReport<Index> {
    index: Index;
    issues: ScanIssue[];
    suppressed_issues: number;
    skipped_symlinks: number;
    skipped_special_entries: number;
    excluded_by_size: number;
}

export type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
export type Confirm = (message: string) => Promise<boolean>;

export interface PartialScanText {
    /** Placeholders: `{count}`, `{root}`, `{examples}`. */
    confirm_partial: string;
}

/** Number of issue paths quoted in the confirmation prompt. */
export const SHOWN_ISSUES = 5;

export function issueCount(report: ScanReport<unknown>): number {
    return report.issues.length + report.suppressed_issues;
}

export function partialScanMessage(report: ScanReport<unknown>, root: string, text: PartialScanText): string {
    const count = issueCount(report);
    const shown = report.issues.slice(0, SHOWN_ISSUES).map((i) => `• ${i.path}: ${i.message}`);
    if (count > shown.length) shown.push(`• … +${count - shown.length}`);
    return text.confirm_partial
        .replace('{count}', String(count))
        .replace('{root}', root)
        .replace('{examples}', shown.join('\n'));
}

/**
 * Scan `root`. Returns the index to save, or `null` when the scan was
 * incomplete and the user declined to save the partial result.
 */
export async function scanForCatalog<Index>(
    invoke: Invoke,
    confirm: Confirm,
    root: string,
    text: PartialScanText,
    options: { hashAlgo?: string | null; maxSizeBytes?: number | null } = {},
): Promise<{ index: Index; complete: boolean; issues: number } | null> {
    const report = await invoke<ScanReport<Index>>('catalog_scan_dir_report', {
        root,
        hashAlgo: options.hashAlgo ?? null,
        maxSizeBytes: options.maxSizeBytes ?? null,
    });
    const issues = issueCount(report);
    if (issues === 0) return { index: report.index, complete: true, issues };
    if (!(await confirm(partialScanMessage(report, root, text)))) return null;
    return { index: report.index, complete: false, issues };
}
