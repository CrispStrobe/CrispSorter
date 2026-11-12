/** Format binary-scaled transfer counters and bytes per second consistently. */
export function formatTransferBytes(value: number | null | undefined): string {
    if (value == null) return '—';
    if (value < 1024) return `${value} B`;
    const units = ['KB', 'MB', 'GB', 'TB'];
    let amount = value / 1024;
    let unit = 0;
    while (amount >= 1024 && unit < units.length - 1) {
        amount /= 1024;
        unit += 1;
    }
    return `${amount.toFixed(amount >= 100 ? 0 : 1)} ${units[unit]}`;
}
