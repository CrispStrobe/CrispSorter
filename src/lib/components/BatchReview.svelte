<script lang="ts">
    import { batchManager } from '../batch/store';
    import { open } from '@tauri-apps/plugin-dialog';
    import { 
        Play, Trash2, Check, X, FileSearch, 
        Loader2, Eye, Edit, Rocket, CheckSquare, 
        Square, Brain, Type, Download, Upload 
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
        if (confirm(`Move ${count} files to their sorted locations?`)) {
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
                <FileSearch size={18} /> Add Files
            </button>
            
            <div class="mode-toggle">
                <button 
                    class="toggle-btn" 
                    class:active={!batchManager.isMetadataExtractionEnabled}
                    onclick={() => batchManager.isMetadataExtractionEnabled = false}
                    title="Extract Text Only (.txt)"
                >
                    <Type size={16} /> Text Only
                </button>
                <button 
                    class="toggle-btn" 
                    class:active={batchManager.isMetadataExtractionEnabled}
                    onclick={() => batchManager.isMetadataExtractionEnabled = true}
                    title="AI Sort (Extract + Metadata + Move)"
                >
                    <Brain size={16} /> AI Sort
                </button>
            </div>

            <button class="action-btn success" onclick={startProcessing} disabled={batchManager.isProcessing}>
                {#if batchManager.isProcessing}
                    <Loader2 class="loader-spin" size={18} /> Processing...
                {:else}
                    <Play size={18} /> Start Batch
                {/if}
            </button>
            
            <div class="divider"></div>

            <button class="action-btn" onclick={acceptAll} title="Accept All Ready">
                <CheckSquare size={18} /> Accept All
            </button>
            
            <button class="action-btn rocket-btn" 
                    onclick={executeSorting} 
                    disabled={batchManager.items.filter(i => i.isAccepted).length === 0}>
                <Rocket size={18} /> Execute Sorting
            </button>
        </div>
        <div class="right-actions">
            <button class="action-btn" onclick={() => batchManager.importBatch()} title="Import Batch JSON">
                <Upload size={18} /> Import
            </button>
            <button class="action-btn" onclick={() => batchManager.exportBatch()} title="Export Batch JSON">
                <Download size={18} /> Export
            </button>
            <button class="action-btn danger" onclick={() => batchManager.clear()}>
                <Trash2 size={18} /> Clear All
            </button>
        </div>
    </div>

    <div class="main-split">
        <div class="table-container">
            <table class="dense-table">
                <thead>
                    <tr>
                        <th width="40"><button class="ghost-btn" onclick={clearAccepted}><Square size={14} /></button></th>
                        <th width="100">Status</th>
                        <th>Original File Name</th>
                        <th>Suggested Title</th>
                        <th>Suggested Author</th>
                        <th width="80">Actions</th>
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
                            <td colspan="6" class="empty-row">No files added. Use "Add Files" to begin.</td>
                        </tr>
                    {/if}
                </tbody>
            </table>
        </div>

        {#if selectedItem}
            <div class="detail-pane">
                <div class="detail-header">
                    <h3><Eye size={18} /> Item Details</h3>
                    <button class="close-btn" onclick={() => selectedItemId = null}>×</button>
                </div>
                <div class="detail-content">
                    {#if selectedItem.errorMessage}
                        <div class="error-box">
                            <strong>Error:</strong> {selectedItem.errorMessage}
                        </div>
                    {/if}

                    <div class="detail-section">
                        <h4>Target Path</h4>
                        <div class="path-preview">
                            {selectedItem.targetPath || "Will be calculated after extraction."}
                        </div>
                    </div>

                    <div class="detail-section">
                        <h4>Extracted Text (Preview)</h4>
                        <div class="text-preview">
                            {selectedItem.extractedText || "Processing required to view text."}
                        </div>
                    </div>
                    
                    <div class="detail-section">
                        <h4>Edit Metadata</h4>
                        <div class="edit-fields">
                            <label>Title <input type="text" bind:value={selectedItem.suggestedTitle} /></label>
                            <label>Author <input type="text" bind:value={selectedItem.suggestedAuthor} /></label>
                            <label>Year <input type="text" bind:value={selectedItem.suggestedYear} /></label>
                        </div>
                    </div>
                </div>
            </div>
        {/if}
    </div>
</div>

<style>
    .batch-container { display: flex; flex-direction: column; height: 100%; background: #fff; overflow: hidden; }
    .toolbar { padding: 12px 20px; background: #f8f9fa; border-bottom: 1px solid #dee2e6; display: flex; justify-content: space-between; align-items: center; }
    .left-actions, .right-actions { display: flex; align-items: center; gap: 10px; }
    .mode-toggle { display: flex; background: #e2e8f0; padding: 4px; border-radius: 8px; gap: 4px; margin-right: 10px; }
    .toggle-btn { display: flex; align-items: center; gap: 6px; padding: 4px 12px; border: none; background: transparent; border-radius: 6px; font-size: 0.75rem; font-weight: 600; color: #64748b; cursor: pointer; transition: all 0.2s; }
    .toggle-btn.active { background: white; color: #3b82f6; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }
    .divider { width: 1px; height: 24px; background: #dee2e6; margin: 0 5px; }
    .action-btn { display: flex; align-items: center; gap: 8px; padding: 6px 14px; border: 1px solid #ced4da; background: white; border-radius: 4px; cursor: pointer; font-size: 0.875rem; font-weight: 600; }
    .action-btn.primary { background: #3b82f6; color: white; border-color: #3b82f6; }
    .action-btn.success { background: #10b981; color: white; border-color: #10b981; }
    .action-btn.danger { background: #ef4444; color: white; border-color: #ef4444; }
    .action-btn.rocket-btn { background: #8b5cf6; color: white; border-color: #8b5cf6; }
    .main-split { display: flex; flex: 1; overflow: hidden; }
    .table-container { flex: 1; overflow-y: auto; }
    .dense-table { width: 100%; border-collapse: collapse; font-size: 0.875rem; table-layout: fixed; }
    .dense-table th { position: sticky; top: 0; z-index: 10; background: #f8f9fa; padding: 10px 12px; text-align: left; border-bottom: 2px solid #e9ecef; color: #64748b; font-weight: 600; }
    .dense-table td { padding: 6px 12px; border-bottom: 1px solid #f1f5f9; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .dense-table tr:hover { background: #f8fafc; }
    .dense-table tr.selected { background: #eff6ff; }
    .dense-table input[type="text"] { width: 100%; border: 1px solid transparent; background: transparent; padding: 4px 8px; border-radius: 4px; font-size: 0.875rem; }
    .status-badge { padding: 3px 8px; border-radius: 20px; background: #f1f5f9; font-size: 0.75rem; font-weight: 600; color: #64748b; }
    .status-active { background: #dbeafe; color: #1e40af; }
    .status-success { background: #dcfce7; color: #166534; }
    .status-error { background: #fee2e2; color: #991b1b; }
    .icon-btn { background: transparent; border: none; cursor: pointer; padding: 4px; }
    .detail-pane { width: 450px; background: #fff; border-left: 1px solid #e2e8f0; display: flex; flex-direction: column; }
    .detail-header { padding: 16px 20px; border-bottom: 1px solid #e2e8f0; display: flex; justify-content: space-between; align-items: center; }
    .detail-content { padding: 20px; overflow-y: auto; display: flex; flex-direction: column; gap: 24px; }
    .path-preview, .text-preview { background: #f8fafc; border: 1px solid #e2e8f0; padding: 12px; font-family: monospace; font-size: 0.75rem; border-radius: 6px; }
    .edit-fields label { display: block; font-size: 0.875rem; margin-bottom: 12px; }
    .edit-fields input { width: 100%; margin-top: 4px; padding: 8px 12px; border: 1px solid #cbd5e1; border-radius: 6px; }
    .loader-spin { animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
    @media (prefers-color-scheme: dark) {
        .batch-container { background: #09090b; }
        .toolbar { background: #18181b; border-color: #27272a; }
        .mode-toggle { background: #27272a; }
        .toggle-btn.active { background: #3f3f46; color: white; }
        .action-btn { background: #27272a; border-color: #3f3f46; }
        .dense-table th { background: #18181b; border-color: #27272a; }
        .dense-table td { border-color: #1e1e1e; color: #e2e8f0; }
        .dense-table tr:hover { background: #1e293b; }
        .dense-table tr.selected { background: #1e3a8a; }
        .detail-pane { background: #0f172a; border-color: #1e293b; }
        .path-preview, .text-preview { background: #020617; border-color: #1e293b; color: #94a3b8; }
        .edit-fields input { background: #1e293b; border-color: #334155; color: white; }
    }
</style>
