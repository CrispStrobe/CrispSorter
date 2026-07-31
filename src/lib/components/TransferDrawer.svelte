<script lang="ts">
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { ChevronDown, ChevronUp, Download, Upload, X } from 'lucide-svelte';

    type TransferState =
        | 'queued'
        | 'active'
        | 'done'
        | 'cancelled'
        | { retrying: { attempt: number } }
        | { failed: { error: string } };

    type TransferProgress = {
        job_id: number;
        direction: 'upload' | 'download';
        drive_id: string;
        remote_path: string;
        bytes_done: number;
        bytes_total: number | null;
        state: TransferState;
    };

    let jobs = $state<TransferProgress[]>([]);
    let expanded = $state(false);
    let busy = $state<number | null>(null);
    let previous = new Map<number, { bytes: number; at: number }>();
    let speeds = new Map<number, number>();

    const stateName = (state: TransferState): string => {
        if (typeof state === 'string') return state;
        if ('retrying' in state) return `retrying (${state.retrying.attempt})`;
        return 'failed';
    };

    const stateLabel = (state: TransferState): string => ({
        queued: 'Queued', active: 'Transferring', done: 'Done',
        cancelled: 'Cancelled',
    }[stateName(state)] ?? stateName(state));

    function formatBytes(value: number | null | undefined): string {
        if (value == null) return '—';
        if (value < 1024) return `${value} B`;
        const units = ['KB', 'MB', 'GB', 'TB'];
        let amount = value;
        let unit = 0;
        while (amount >= 1024 && unit < units.length - 1) {
            amount /= 1024;
            unit += 1;
        }
        return `${amount.toFixed(amount >= 100 ? 0 : 1)} ${units[unit]}`;
    }

    function speed(job: TransferProgress): number {
        return speeds.get(job.job_id) ?? 0;
    }

    function percent(job: TransferProgress): number {
        if (!job.bytes_total) return 0;
        return Math.min(100, Math.round((job.bytes_done / job.bytes_total) * 100));
    }

    async function refresh() {
        try {
            const next = await invoke<TransferProgress[]>('transfer_queue_status');
            const now = performance.now();
            for (const job of next) {
                const old = previous.get(job.job_id);
                if (old && now > old.at && job.bytes_done >= old.bytes) {
                    speeds.set(job.job_id, ((job.bytes_done - old.bytes) * 1000) / (now - old.at));
                }
                previous.set(job.job_id, { bytes: job.bytes_done, at: now });
            }
            jobs = next;
        } catch {
            // Browser preview and mobile builds may not expose Tauri commands.
        }
    }

    async function cancel(jobId: number) {
        busy = jobId;
        try { await invoke('transfer_queue_cancel', { jobId }); await refresh(); }
        finally { busy = null; }
    }

    onMount(() => {
        refresh();
        const timer = setInterval(refresh, 1000);
        return () => clearInterval(timer);
    });

    const visibleJobs = $derived(jobs.filter((job) => {
        const name = stateName(job.state);
        return expanded || (name !== 'done' && name !== 'cancelled');
    }));
    const activeCount = $derived(jobs.filter((job) => {
        const name = stateName(job.state);
        return name === 'queued' || name === 'active' || name === 'retrying';
    }).length);
</script>

{#if jobs.length > 0}
    <section class="transfer-drawer" aria-label="Cloud transfers">
        <button class="drawer-header" onclick={() => expanded = !expanded} aria-expanded={expanded}>
            <span class="drawer-title"><span class="status-dot" class:busy={activeCount > 0}></span>Transfers</span>
            <span class="drawer-summary">{activeCount} active · {jobs.length} total</span>
            {#if expanded}<ChevronDown size={16} />{:else}<ChevronUp size={16} />{/if}
        </button>

        {#if visibleJobs.length > 0}
            <div class="job-list">
                {#each visibleJobs as job (job.job_id)}
                    <article class="job" class:terminal={stateName(job.state) === 'done'}>
                        <div class="job-icon" aria-hidden="true">
                            {#if job.direction === 'upload'}<Upload size={14} />{:else}<Download size={14} />{/if}
                        </div>
                        <div class="job-body">
                            <div class="job-line">
                                <span class="job-path" title={job.remote_path}>{job.remote_path}</span>
                                <span class="job-state state-{stateName(job.state)}">{stateLabel(job.state)}</span>
                            </div>
                            <div class="progress-track"><div class="progress-value" style={`width: ${percent(job)}%`}></div></div>
                            <div class="job-meta">
                                <span>{formatBytes(job.bytes_done)}{job.bytes_total != null ? ` / ${formatBytes(job.bytes_total)}` : ''}</span>
                                {#if speed(job) > 0}<span>{formatBytes(speed(job))}/s</span>{/if}
                                <span>#{job.job_id}</span>
                            </div>
                        </div>
                        {#if stateName(job.state) === 'queued' || stateName(job.state) === 'active' || stateName(job.state) === 'retrying'}
                            <button class="cancel" disabled={busy === job.job_id} onclick={() => cancel(job.job_id)} title="Cancel transfer" aria-label="Cancel transfer">
                                <X size={14} />
                            </button>
                        {/if}
                    </article>
                {/each}
            </div>
        {/if}
    </section>
{/if}

<style>
    .transfer-drawer { position: fixed; z-index: 80; right: 18px; bottom: 18px; width: min(440px, calc(100vw - 36px)); color: #f4f4f5; background: #18181b; border: 1px solid #3f3f46; border-radius: 10px; box-shadow: 0 12px 36px #0008; overflow: hidden; }
    .drawer-header { width: 100%; display: flex; align-items: center; gap: 9px; padding: 10px 12px; border: 0; color: inherit; background: #27272a; cursor: pointer; text-align: left; }
    .drawer-title { font-weight: 600; font-size: .82rem; display: flex; align-items: center; gap: 7px; }
    .drawer-summary { color: #a1a1aa; font-size: .72rem; margin-left: auto; }
    .status-dot { width: 7px; height: 7px; border-radius: 50%; background: #71717a; }
    .status-dot.busy { background: #22c55e; box-shadow: 0 0 8px #22c55e; }
    .job-list { max-height: 300px; overflow: auto; padding: 4px 0; }
    .job { display: flex; align-items: center; gap: 9px; padding: 9px 11px; border-bottom: 1px solid #27272a; }
    .job:last-child { border-bottom: 0; }
    .job-icon { color: #a78bfa; flex: 0 0 auto; }
    .job-body { min-width: 0; flex: 1; }
    .job-line, .job-meta { display: flex; align-items: center; gap: 8px; }
    .job-path { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: .76rem; flex: 1; }
    .job-state { font-size: .65rem; text-transform: capitalize; color: #a1a1aa; white-space: nowrap; }
    .state-active { color: #4ade80; } .state-retrying { color: #fbbf24; } .state-failed { color: #f87171; }
    .progress-track { height: 4px; margin: 6px 0 4px; border-radius: 3px; background: #3f3f46; overflow: hidden; }
    .progress-value { height: 100%; border-radius: inherit; background: #8b5cf6; transition: width .25s ease; }
    .job-meta { color: #71717a; font-size: .64rem; }
    .job-meta span:last-child { margin-left: auto; }
    .cancel { display: grid; place-items: center; border: 0; border-radius: 5px; padding: 5px; color: #a1a1aa; background: transparent; cursor: pointer; }
    .cancel:hover { color: #f87171; background: #3f1f2a; }
    .cancel:disabled { opacity: .5; cursor: wait; }
    @media (max-width: 767px) { .transfer-drawer { right: 8px; bottom: calc(54px + env(safe-area-inset-bottom, 0px)); width: calc(100vw - 16px); } }
</style>
