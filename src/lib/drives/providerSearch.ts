import { invoke } from '@tauri-apps/api/core';

export interface ProviderSearchEntry {
    path: string;
    is_dir: boolean;
    size: number | null;
    mtime_unix: number | null;
}

export interface ProviderSearchReport {
    results: ProviderSearchEntry[];
    scanned_entries: number;
    scan_limit: number;
    scan_limit_reached: boolean;
    // Conservative: the cap was filled, without extra content reads to find
    // whether another match exists.
    results_limit_reached: boolean;
}

export function searchProvider(options: {
    driveId: string;
    root: string;
    query: string;
    maxDepth: number;
    maxResults: number;
    content: boolean;
}): Promise<ProviderSearchReport> {
    return invoke<ProviderSearchReport>('drive_search_report', options);
}

export function providerSearchNotice(report: ProviderSearchReport): string {
    const notices: string[] = [];
    if (report.scan_limit_reached) {
        notices.push(`Search stopped after scanning ${report.scanned_entries.toLocaleString()} files and folders. Results may be incomplete; search a smaller folder to continue.`);
    }
    if (report.results_limit_reached) {
        notices.push(`Result limit reached (${report.results.length.toLocaleString()}); additional matches may exist. Narrow your search to see other matches.`);
    }
    return notices.join(' ');
}
