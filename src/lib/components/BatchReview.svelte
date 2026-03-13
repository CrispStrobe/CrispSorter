<script lang="ts">
    import { batchManager } from '../batch/store.svelte';
    import { i18n } from '../i18n.svelte';
    import { open } from '@tauri-apps/plugin-dialog';
    import { 
        Play, Trash2, Check, X, FileSearch, 
        Loader2, Eye, Edit, Rocket, CheckSquare, 
        Square, Brain, Type
    } from 'lucide-svelte';

    let selectedItemId = $state<string | null>(null);
    let selectedItem = $derived(batchManager.items.find(i => i.id === selectedItemId));

    async function handleAddFiles() {
        const selected = await open({
            multiple: true,
            filters: [{ name: 'Documents', extensions: ['pdf', 'docx', 'txt', 'md'] }]
        });
        
        if (Array.isArray(selected)) {
            selected.forEach(path => {
                const name = path.split(/[\\/]/).pop() || '';
                batchManager.addItem(path, name);
            });
        }
    }

    function toggleSelect(id: string) {
        selectedItemId = selectedItemId === id ? null : id;
    }

    async function startProcessing() {
        await batchManager.processAll();
    }

    async function executeSorting() {
        const count = batchManager.items.filter(i => i.isAccepted).length;
        if (confirm(i18n.t.batch.confirm_move.replace('{count}', count.toString()))) {
            await batchManager.executeBatch();
        }
    }

    function acceptAll() {
        batchManager.items.forEach(i => {
            if (i.status === 'review') i.isAccepted = true;
        });
    }

    function clearAccepted() {
        batchManager.items.forEach(i => i.isAccepted = false);
    }
</script>

<div class="batch-container">
    <div class="toolbar">
        <div class="left-actions">
            <button class="action-btn primary" onclick={handleAddFiles}>
                <FileSearch size={18} /> {i18n.t.batch.add_files}
            </button>
            
            <div class="mode-toggle">
                <button 
                    class="toggle-btn" 
                    class:active={!batchManager.isMetadataExtractionEnabled}
                    onclick={() => batchManager.isMetadataExtractionEnabled = false}
                    title={i18n.t.batch.text_only}
                >
                    <Type size={16} /> {i18n.t.batch.text_only}
                </button>
                <button 
                    class="toggle-btn" 
                    class:active={batchManager.isMetadataExtractionEnabled}
                    onclick={() => batchManager.isMetadataExtractionEnabled = true}
                    title={i18n.t.batch.ai_sort}
                >
                    <Brain size={16} /> {i18n.t.batch.ai_sort}
                </button>
            </div>

            <button class="action-btn success" onclick={startProcessing} disabled={batchManager.isProcessing}>
                {#if batchManager.isProcessing}
                    <Loader2 class="loader-spin" size={18} /> {i18n.t.batch.processing}
                {:else}
                    <Play size={18} /> {i18n.t.batch.start_batch}
                {/if}
            </button>
            
            <div class="divider"></div>

            <button class="action-btn" onclick={acceptAll} title={i18n.t.batch.accept_all}>
                <CheckSquare size={18} /> {i18n.t.batch.accept_all}
            </button>
            
            <button class="action-btn rocket-btn" 
                    onclick={executeSorting} 
                    disabled={batchManager.items.filter(i => i.isAccepted).length === 0}>
                <Rocket size={18} /> {i18n.t.batch.execute}
            </button>
        </div>
        <div class="right-actions">
            <button class="action-btn danger" onclick={() => batchManager.clear()}>
                <Trash2 size={18} /> {i18n.t.batch.clear_all}
            </button>
        </div>
    </div>

    <div class="main-split">
        <div class="table-container">
            <table class="dense-table">
                <thead>
                    <tr>
                        <th width="40"><button class="ghost-btn" onclick={clearAccepted}><Square size={14} /></button></th>
                        <th width="100">{i18n.t.batch.status}</th>
                        <th>{i18n.t.batch.file_name}</th>
                        <th>{i18n.t.batch.title}</th>
                        <th>{i18n.t.batch.author}</th>
                        <th width="80">{i18n.t.batch.actions}</th>
                    </tr>
                </thead>
                <tbody>
                    {#each batchManager.items as item (item.id)}
                        <tr 
                            class:selected={selectedItemId === item.id}
                            onclick={() => toggleSelect(item.id)}
                            class:status-error={item.status === 'error'}
                            class:status-done={item.status === 'done'}
                        >
                            <td onclick={e => e.stopPropagation()}>
                                <input type="checkbox" bind:checked={item.isAccepted} disabled={item.status !== 'review' && item.status !== 'done'} />
                            </td>
                            <td>
                                <span class="status-badge" class:status-active={['extracting', 'analyzing', 'moving'].includes(item.status)} class:status-success={item.status === 'done'}>
                                    {item.status}
                                </span>
                            </td>
                            <td class="file-name" title={item.originalPath}>{item.originalName}</td>
                            <td><input type="text" bind:value={item.suggestedTitle} onclick={e => e.stopPropagation()} /></td>
                            <td><input type="text" bind:value={item.suggestedAuthor} onclick={e => e.stopPropagation()} /></td>
                            <td class="row-actions" onclick={e => e.stopPropagation()}>
                                <button class="icon-btn" onclick={() => item.isAccepted = true} title="Accept"><Check size={16} color={item.isAccepted ? "#198754" : "#adb5bd"} /></button>
                                <button class="icon-btn" onclick={() => item.isAccepted = false} title="Reject"><X size={16} color={!item.isAccepted ? "#dc3545" : "#adb5bd"} /></button>
                            </td>
                        </tr>
                    {/each}
                    {#if batchManager.items.length === 0}
                        <tr>
                            <td colspan="6" class="empty-row">{i18n.t.batch.empty}</td>
                        </tr>
                    {/if}
                </tbody>
            </table>
        </div>

        {#if selectedItem}
            <div class="detail-pane">
                <div class="detail-header">
                    <h3><Eye size={18} /> {i18n.t.batch.details}</h3>
                    <button class="close-btn" onclick={() => selectedItemId = null}>×</button>
                </div>
                <div class="detail-content">
                    {#if selectedItem.errorMessage}
                        <div class="error-box">
                            <strong>Error:</strong> {selectedItem.errorMessage}
                        </div>
                    {/if}

                    <div class="detail-section">
                        <h4>{i18n.t.batch.target_path}</h4>
                        <div class="path-preview">
                            {selectedItem.targetPath || i18n.t.batch.path_hint}
                        </div>
                    </div>

                    <div class="detail-section">
                        <h4>{i18n.t.batch.extracted_text}</h4>
                        <div class="text-preview">
                            {selectedItem.extractedText || i18n.t.batch.extract_hint}
                        </div>
                    </div>
                    
                    <div class="detail-section">
                        <h4>{i18n.t.batch.edit_metadata}</h4>
                        <div class="edit-fields">
                            <label>{i18n.t.batch.title} <input type="text" bind:value={selectedItem.suggestedTitle} /></label>
                            <label>{i18n.t.batch.author} <input type="text" bind:value={selectedItem.suggestedAuthor} /></label>
                            <label>{i18n.t.batch.year} <input type="text" bind:value={selectedItem.suggestedYear} /></label>
                        </div>
                    </div>
                </div>
            </div>
        {/if}
    </div>
</div>

<style>
    .batch-container { display: flex; flex-direction: column; height: 100%; background: #09090b; overflow: hidden; }
    .toolbar { padding: 12px 20px; background: #18181b; border-bottom: 1px solid #27272a; display: flex; justify-content: space-between; align-items: center; }
    .left-actions, .right-actions { display: flex; align-items: center; gap: 10px; }
    .mode-toggle { display: flex; background: #27272a; padding: 4px; border-radius: 8px; gap: 4px; margin-right: 10px; }
    .toggle-btn { display: flex; align-items: center; gap: 6px; padding: 4px 12px; border: none; background: transparent; border-radius: 6px; font-size: 0.75rem; font-weight: 600; color: #a1a1aa; cursor: pointer; transition: all 0.2s; }
    .toggle-btn.active { background: #3f3f46; color: white; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }
    .divider { width: 1px; height: 24px; background: #27272a; margin: 0 5px; }
    .action-btn { display: flex; align-items: center; gap: 8px; padding: 6px 14px; border: 1px solid #27272a; background: #18181b; border-radius: 6px; cursor: pointer; font-size: 0.875rem; font-weight: 600; color: #d4d4d8; }
    .action-btn:hover:not(:disabled) { background: #27272a; }
    .action-btn:disabled { opacity: 0.5; cursor: not-allowed; }
    .action-btn.primary { background: #3b82f6; color: white; border-color: #3b82f6; }
    .action-btn.success { background: #10b981; color: white; border-color: #10b981; }
    .action-btn.danger { background: #ef4444; color: white; border-color: #ef4444; }
    .action-btn.rocket-btn { background: #8b5cf6; color: white; border-color: #8b5cf6; }
    .main-split { display: flex; flex: 1; overflow: hidden; }
    .table-container { flex: 1; overflow-y: auto; }
    .dense-table { width: 100%; border-collapse: collapse; font-size: 0.875rem; table-layout: fixed; }
    .dense-table th { position: sticky; top: 0; z-index: 10; background: #18181b; padding: 10px 12px; text-align: left; border-bottom: 2px solid #27272a; color: #a1a1aa; font-weight: 600; }
    .dense-table td { padding: 6px 12px; border-bottom: 1px solid #1e1e1e; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: #e2e8f0; }
    .dense-table tr:hover { background: #1e293b; }
    .dense-table tr.selected { background: #1e3a8a; }
    .dense-table tr.status-done { background: #064e3b; }
    .dense-table input[type="text"] { width: 100%; border: 1px solid transparent; background: transparent; padding: 4px 8px; border-radius: 4px; font-size: 0.875rem; color: #f8fafc; }
    .dense-table tr:hover input[type="text"], .dense-table tr.selected input[type="text"] { background: #0f172a; border-color: #334155; }
    .status-badge { padding: 3px 8px; border-radius: 20px; background: #27272a; font-size: 0.75rem; font-weight: 600; color: #a1a1aa; }
    .status-active { background: #1e40af; color: #dbeafe; }
    .status-success { background: #166534; color: #dcfce7; }
    .status-error { background: #7f1d1d; color: #fee2e2; }
    .icon-btn { background: transparent; border: none; cursor: pointer; padding: 4px; }
    .detail-pane { width: 450px; background: #0f172a; border-left: 1px solid #1e293b; display: flex; flex-direction: column; }
    .detail-header { padding: 16px 20px; border-bottom: 1px solid #1e293b; display: flex; justify-content: space-between; align-items: center; }
    .detail-content { padding: 20px; overflow-y: auto; display: flex; flex-direction: column; gap: 24px; }
    .path-preview, .text-preview { background: #020617; border: 1px solid #1e293b; padding: 12px; font-family: monospace; font-size: 0.75rem; border-radius: 6px; color: #94a3b8; }
    .edit-fields label { display: block; font-size: 0.875rem; margin-bottom: 12px; color: #a1a1aa; }
    .edit-fields input { width: 100%; margin-top: 4px; padding: 8px 12px; border: 1px solid #334155; border-radius: 6px; background: #1e293b; color: white; }
    .error-box { background: #450a0a; border: 1px solid #7f1d1d; color: #fecaca; padding: 12px; border-radius: 6px; font-size: 0.875rem; }
    .empty-row { text-align: center; padding: 40px !important; color: #71717a; }
    .ghost-btn { background: transparent; border: none; cursor: pointer; padding: 2px; color: #71717a; }
    .loader-spin { animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
</style>
