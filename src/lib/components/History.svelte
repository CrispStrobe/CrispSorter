<script lang="ts">
    import { onMount, tick } from 'svelte';
    import { getSetting, saveSetting } from '../store';
    import type { BatchSession } from '../types';
    import { batchManager } from '../batch/store.svelte';
    import { reviewDrafts } from '../batch/reviewDrafts.svelte';
    import { validateSessions, type BatchReplacementSummary } from '../batch/sessionTransfer';
    import { i18n } from '../i18n.svelte';
    import { Clock, Play, Trash2, Calendar, FileText, Upload, Download, RefreshCw } from 'lucide-svelte';

    let { onResumeBatch } = $props<{ onResumeBatch: () => void }>();
    type Operation = { kind: 'import' | 'resume' | 'export' | 'delete'; id?: string };
    let sessions = $state<BatchSession[]>([]);
    let loading = $state(true);
    let loadError = $state('');
    let operation = $state<Operation | null>(null);
    let failedOperation = $state<Operation | null>(null);
    let operationError = $state('');
    let success = $state('');
    let confirmation = $state<BatchReplacementSummary | null>(null);
    let dialog: HTMLDialogElement;
    let cancelButton = $state<HTMLButtonElement>();
    let replaceButton = $state<HTMLButtonElement>();
    let resolveConfirmation: ((accepted: boolean) => void) | null = null;
    let returnFocus: HTMLElement | null = null;
    let disposed = false;

    const copy = $derived(i18n.lang === 'de' ? {
        refresh: 'Verlauf aktualisieren', loading: 'Verlauf wird geladen…', loadFailed: 'Verlauf konnte nicht geladen werden.',
        loadHint: 'Prüfe den Zugriff auf die Einstellungen und versuche es erneut.', retry: 'Erneut versuchen',
        busy: 'Vorgang läuft…', busyHint: 'Warte, bis der aktuelle Stapelvorgang abgeschlossen ist.',
        restoreHint: 'Stelle zuerst den gespeicherten Stapel über den Hinweis oben wieder her.',
        operationFailed: 'Der Vorgang konnte nicht abgeschlossen werden.',
        replacementFailed: 'Der aktuelle Stapel und seine Metadatenentwürfe bleiben erhalten. Prüfe die Datei oder den Speicherzugriff und versuche es erneut.',
        exportFailed: 'Der Export ist nicht abgeschlossen. Prüfe den Zielordner und versuche es erneut.',
        deleteFailed: 'Der Verlaufseintrag konnte nicht gelöscht werden. Versuche es erneut.',
        exported: 'Der Stapel wurde mit dem gespeicherten extrahierten Text exportiert.', deleted: 'Verlaufseintrag gelöscht.',
        exportHint: 'Exportiert den aktuellen Stapel mit extrahiertem Text. Noch nicht angewendete Metadatenentwürfe bleiben in dieser App.',
        replaceTitle: 'Aktuellen Stapel ersetzen?', source: 'Quelle', cancel: 'Abbrechen', replace: 'Stapel ersetzen',
        replaceHint: 'Die Quelldateien bleiben auf dem Datenträger. Entwürfe für entfernte Dateien werden erst nach erfolgreichem Ersetzen verworfen.',
        backupHint: 'Exportiere den aktuellen Stapel vorher, wenn du ihn behalten möchtest.',
        active: 'Aktiv', completed: 'Abgeschlossen', delete: 'Gespeicherte Sitzung löschen', emptyHint: 'Importiere einen zuvor exportierten Stapel oder arbeite mit deinem aktuellen Stapel weiter.',
    } : {
        refresh: 'Refresh history', loading: 'Loading history…', loadFailed: 'History could not be loaded.',
        loadHint: 'Check access to your settings and try again.', retry: 'Retry',
        busy: 'Working…', busyHint: 'Wait for the current batch operation to finish.',
        restoreHint: 'Restore the saved batch using the notice above before importing or exporting.',
        operationFailed: 'The operation could not be completed.',
        replacementFailed: 'Your current batch and its metadata drafts are preserved. Check the file or storage access and try again.',
        exportFailed: 'The export did not finish. Check the destination folder and try again.',
        deleteFailed: 'The history entry could not be deleted. Try again.',
        exported: 'The batch was exported with its saved extracted text.', deleted: 'History entry deleted.',
        exportHint: 'Exports the current batch with extracted text. Unapplied metadata drafts stay in this app.',
        replaceTitle: 'Replace current batch?', source: 'Source', cancel: 'Cancel', replace: 'Replace batch',
        replaceHint: 'Your source files stay on disk. Drafts for removed files are discarded after replacement succeeds.',
        backupHint: 'Export the current batch first if you need to keep it.',
        active: 'Active', completed: 'Completed', delete: 'Delete saved session', emptyHint: 'Import a previously exported batch or continue working with your current batch.',
    });
    const batchBusy = $derived(batchManager.isProcessing || batchManager.isExecuting || batchManager.isUpdatingBatch ||
        batchManager.isCleaningDuplicates || !!reviewDrafts.applyingId);
    const actionsDisabled = $derived(!!operation || batchBusy || !batchManager.sessionReady);

    function fileCount(count: number) {
        return `${count} ${i18n.lang === 'de' ? (count === 1 ? 'Datei' : 'Dateien') : (count === 1 ? 'file' : 'files')}`;
    }

    function sessionDate(session: BatchSession) {
        return new Date(session.startTime).toLocaleString(i18n.lang);
    }

    onMount(() => {
        void refreshHistory();
        return () => {
            disposed = true;
            finishConfirmation(false);
        };
    });

    async function refreshHistory() {
        loading = true;
        loadError = '';
        try {
            const saved = validateSessions(await getSetting('sessions', {}));
            if (!disposed) sessions = saved;
        } catch (error) {
            if (!disposed) loadError = String(error);
        } finally {
            if (!disposed) loading = false;
        }
    }

    async function confirmReplacement(summary: BatchReplacementSummary): Promise<boolean> {
        if (disposed) return false;
        confirmation = summary;
        const answer = new Promise<boolean>(resolve => { resolveConfirmation = resolve; });
        await tick();
        if (disposed) { finishConfirmation(false); return answer; }
        dialog.showModal();
        cancelButton?.focus();
        return answer;
    }

    function finishConfirmation(accepted: boolean) {
        const resolve = resolveConfirmation;
        resolveConfirmation = null;
        confirmation = null;
        if (dialog?.open) dialog.close();
        resolve?.(accepted);
    }

    function confirmationKey(event: KeyboardEvent) {
        if (event.key !== 'Tab' || event.isComposing || event.ctrlKey || event.metaKey || event.altKey) return;
        if ((!event.shiftKey && document.activeElement === replaceButton) ||
            (event.shiftKey && document.activeElement === cancelButton)) {
            event.preventDefault();
            (event.shiftKey ? replaceButton : cancelButton)?.focus();
        }
    }

    async function runOperation(next: Operation) {
        if (actionsDisabled || disposed) return;
        returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
        operation = next;
        operationError = '';
        failedOperation = null;
        success = '';
        let replaced = false;
        try {
            if (next.kind === 'import') replaced = await batchManager.importBatch(confirmReplacement);
            else if (next.kind === 'resume') replaced = await batchManager.loadSession(next.id!, confirmReplacement);
            else if (next.kind === 'export') {
                if (await batchManager.exportBatch() && !disposed) success = copy.exported;
            } else {
                const session = sessions.find(entry => entry.id === next.id);
                if (!confirm(`${i18n.t.history.delete_confirm}\n${session ? sessionDate(session) : next.id}`)) return;
                const saved = await getSetting('sessions', {});
                validateSessions(saved);
                const remaining = { ...saved };
                delete remaining[next.id!];
                await saveSetting('sessions', remaining);
                if (!disposed) { success = copy.deleted; await refreshHistory(); }
            }
        } catch (error) {
            if (!disposed) { operationError = String(error); failedOperation = next; }
        } finally {
            if (!disposed) {
                operation = null;
                await tick();
                if (returnFocus?.isConnected) returnFocus.focus();
                returnFocus = null;
            }
        }
        if (replaced && !disposed) onResumeBatch();
    }
</script>

<section class="history-container" aria-labelledby="history-title">
    <div class="header">
        <div class="title-area">
            <h1 id="history-title">{i18n.t.history.title}</h1>
            <p>{i18n.t.history.subtitle}</p>
        </div>
        <div class="header-actions">
            <button class="action-btn" onclick={() => runOperation({ kind: 'import' })} disabled={actionsDisabled}>
                <Upload size={18} aria-hidden="true" /> {i18n.t.history.import}
            </button>
            <button class="action-btn" onclick={() => runOperation({ kind: 'export' })} disabled={actionsDisabled} aria-describedby="history-export-hint">
                <Download size={18} aria-hidden="true" /> {i18n.t.history.export}
            </button>
            <button class="action-btn" onclick={refreshHistory} disabled={loading || !!operation}>
                <RefreshCw size={18} aria-hidden="true" /> {copy.refresh}
            </button>
        </div>
    </div>
    <p id="history-export-hint" class="help-text">{copy.exportHint}</p>
    {#if !batchManager.sessionReady}<p class="help-text" role="status">{copy.restoreHint}</p>
    {:else if batchBusy && !operation}<p class="help-text" role="status">{copy.busyHint}</p>{/if}
    {#if loading}<p class="notice" role="status">{copy.loading}</p>{/if}
    {#if operation}<p class="notice" role="status" aria-live="polite">{copy.busy}</p>{/if}
    {#if success}<p class="notice success" role="status">{success}</p>{/if}
    {#if loadError}
        <div class="notice error" role="alert">
            <strong>{copy.loadFailed}</strong><p>{copy.loadHint}</p><p class="error-detail">{loadError}</p>
            <button class="action-btn" onclick={refreshHistory} disabled={loading || !!operation}>{copy.retry}</button>
        </div>
    {/if}
    {#if operationError && failedOperation}
        <div class="notice error" role="alert">
            <strong>{copy.operationFailed}</strong>
            <p>{failedOperation.kind === 'export' ? copy.exportFailed : failedOperation.kind === 'delete' ? copy.deleteFailed : copy.replacementFailed}</p>
            <p class="error-detail">{operationError}</p>
            <button class="action-btn" disabled={actionsDisabled} onclick={() => failedOperation && runOperation(failedOperation)}>{copy.retry}</button>
        </div>
    {/if}

    <div class="sessions-list" aria-busy={loading}>
        {#each sessions as session (session.id)}
            <article class="session-card" aria-label={sessionDate(session)}>
                <div class="session-info">
                    <div class="session-main">
                        <Clock size={16} aria-hidden="true" />
                        <strong>{sessionDate(session)}</strong>
                        <span class="badge" class:completed={session.status === 'completed'} class:paused={session.status === 'paused'}>
                            {session.status === 'paused' ? i18n.t.history.paused : session.status === 'completed' ? copy.completed : copy.active}
                        </span>
                    </div>
                    <div class="session-details">
                        <span><FileText size={14} aria-hidden="true" /> {fileCount(session.items.length)}</span>
                        <span title={session.id}><Calendar size={14} aria-hidden="true" /> ID: {session.id.substring(0, 8)}</span>
                    </div>
                </div>
                <div class="session-actions">
                    <button class="resume-btn" onclick={() => runOperation({ kind: 'resume', id: session.id })} disabled={actionsDisabled || loading}>
                        <Play size={16} aria-hidden="true" /> {i18n.t.history.resume}
                    </button>
                    <button class="delete-btn" aria-label={`${copy.delete}: ${sessionDate(session)}`} title={copy.delete}
                        onclick={() => runOperation({ kind: 'delete', id: session.id })} disabled={actionsDisabled || loading}>
                        <Trash2 size={16} aria-hidden="true" />
                    </button>
                </div>
            </article>
        {:else}
            {#if !loading && !loadError}
                <div class="empty-state"><Clock size={48} aria-hidden="true" /><p>{i18n.t.history.empty}</p><p>{copy.emptyHint}</p></div>
            {/if}
        {/each}
    </div>
</section>

<dialog class="replacement-dialog" bind:this={dialog} aria-labelledby="replacement-title" aria-describedby="replacement-summary replacement-hint"
    oncancel={(event) => { event.preventDefault(); finishConfirmation(false); }} onkeydown={confirmationKey}>
    {#if confirmation}
        <h2 id="replacement-title">{copy.replaceTitle}</h2>
        <p id="replacement-summary">{i18n.lang === 'de'
            ? `${fileCount(confirmation.currentCount)} im aktuellen Stapel durch ${fileCount(confirmation.incomingCount)} aus dieser Quelle ersetzen?`
            : `Replace ${fileCount(confirmation.currentCount)} in the current batch with ${fileCount(confirmation.incomingCount)} from this source?`}</p>
        <p class="replacement-source"><strong>{copy.source}:</strong> {confirmation.source}</p>
        {#if confirmation.currentDraftCount}
            <p>{i18n.lang === 'de'
                ? `Der aktuelle Stapel enthält ${confirmation.currentDraftCount} ${confirmation.currentDraftCount === 1 ? 'noch nicht angewendeten Metadatenentwurf' : 'noch nicht angewendete Metadatenentwürfe'}.`
                : `The current batch has ${confirmation.currentDraftCount} unapplied metadata ${confirmation.currentDraftCount === 1 ? 'draft' : 'drafts'}.`}</p>
        {/if}
        <p id="replacement-hint">{copy.replaceHint}</p><p>{copy.backupHint}</p>
        <div class="dialog-actions">
            <button class="action-btn" bind:this={cancelButton} onclick={() => finishConfirmation(false)}>{copy.cancel}</button>
            <button class="resume-btn" bind:this={replaceButton} onclick={() => finishConfirmation(true)}>{copy.replace}</button>
        </div>
    {/if}
</dialog>

<style>
    .history-container { box-sizing: border-box; padding: 32px; width: 100%; min-width: 0; height: 100%; overflow-y: auto; background: #09090b; color: #fafafa; }
    .header { margin-bottom: 16px; display: flex; flex-wrap: wrap; gap: 20px; justify-content: space-between; align-items: flex-start; }
    .title-area { min-width: 0; }
    h1 { font-size: 1.875rem; font-weight: 700; margin: 0 0 8px; }
    p { color: #a1a1aa; margin: 0; line-height: 1.5; overflow-wrap: anywhere; }
    .header-actions, .session-actions, .dialog-actions { display: flex; flex-wrap: wrap; gap: 10px; }
    .action-btn, .resume-btn, .delete-btn { display: inline-flex; align-items: center; justify-content: center; gap: 8px; min-height: 40px; padding: 8px 14px; border: 1px solid #3f3f46; border-radius: 8px; color: #e4e4e7; background: #18181b; cursor: pointer; font-weight: 600; }
    .action-btn:hover:not(:disabled) { background: #27272a; }
    button:disabled { opacity: 0.5; cursor: not-allowed; }
    button:focus-visible { outline: 2px solid #93c5fd; outline-offset: 3px; }
    .help-text { max-width: 800px; font-size: 0.85rem; margin-bottom: 16px; }
    .notice { max-width: 800px; box-sizing: border-box; padding: 14px; margin: 16px 0; border: 1px solid #3f3f46; border-radius: 8px; }
    .notice.error { background: #2a171b; border-color: #9f3454; display: flex; flex-direction: column; align-items: flex-start; gap: 10px; }
    .error-detail { font-size: 0.8rem; color: #fecdd3; }
    .notice.success { color: #bbf7d0; border-color: #166534; }
    .sessions-list { display: flex; flex-direction: column; gap: 16px; max-width: 800px; }
    .session-card { background: #18181b; border: 1px solid #27272a; border-radius: 12px; padding: 20px; display: flex; flex-wrap: wrap; gap: 16px; justify-content: space-between; align-items: center; min-width: 0; }
    .session-info { display: flex; flex-direction: column; gap: 10px; min-width: 0; }
    .session-main { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; font-size: 1rem; }
    .session-main strong { overflow-wrap: anywhere; }
    .badge { font-size: 0.75rem; padding: 2px 8px; background: #3f3f46; color: #d4d4d8; border-radius: 9999px; text-transform: uppercase; font-weight: 600; }
    .badge.completed { background: #166534; color: #dcfce7; }
    .badge.paused { background: #450a0a; color: #fca5a5; }
    .session-details { display: flex; flex-wrap: wrap; gap: 8px 20px; font-size: 0.875rem; color: #a1a1aa; }
    .session-details span { display: flex; align-items: center; gap: 4px; }
    .resume-btn { background: #2563eb; color: white; border-color: #3b82f6; }
    .delete-btn { background: #450a0a; color: #fecaca; border-color: #7f1d1d; min-width: 40px; padding: 8px; }
    .empty-state { display: flex; flex-direction: column; align-items: center; gap: 12px; text-align: center; padding: 40px 20px; color: #71717a; }
    .replacement-dialog { box-sizing: border-box; width: min(540px, calc(100vw - 32px)); max-height: calc(100dvh - 32px); padding: 24px; background: #18181b; color: #fafafa; border: 1px solid #52525b; border-radius: 12px; overflow-y: auto; }
    .replacement-dialog::backdrop { background: #09090bb8; }
    .replacement-dialog h2 { margin: 0 0 16px; font-size: 1.25rem; }
    .replacement-dialog p { margin-bottom: 12px; }
    .replacement-source { color: #e4e4e7; }
    .dialog-actions { justify-content: flex-end; margin-top: 24px; }
    @media (max-width: 600px) {
        .history-container { padding: 16px; }
        .header-actions { gap: 8px; }
        .action-btn, .resume-btn, .delete-btn { min-height: 44px; }
        .session-card { padding: 16px; }
        .session-actions { width: 100%; }
        .resume-btn { flex: 1; }
        .replacement-dialog { padding: 20px; }
        .dialog-actions { flex-direction: column-reverse; }
        .dialog-actions button { width: 100%; }
    }
</style>
