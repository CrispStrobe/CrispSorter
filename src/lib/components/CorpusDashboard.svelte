<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { onMount } from 'svelte';
    import { Loader2, BarChart3, FileText, Languages, Calendar, Tag, HardDrive, Network } from 'lucide-svelte';

    interface CorpusStats {
        total_docs: number;
        total_chunks: number;
        ext_distribution: [string, number][];
        lang_distribution: [string, number][];
        year_histogram: [number, number][];
        top_tags: [string, number][];
        total_size_bytes: number;
    }

    let stats = $state<CorpusStats | null>(null);
    let loading = $state(false);
    let error = $state('');
    let selectedYear = $state<number | null>(null);

    // Clustering
    interface Cluster { id: number; name: string; doc_count: number; top_terms: string[]; sample_titles: string[]; member_doc_ids: string[]; }
    let clusters = $state<Cluster[]>([]);
    let clusterLoading = $state(false);
    let clusterK = $state(5);

    let labelLoading = $state(false);

    async function loadClusters() {
        clusterLoading = true;
        try {
            clusters = await invoke<Cluster[]>('index_cluster_documents', { k: clusterK });
        } catch (e: any) {
            console.warn('Clustering failed:', e);
            clusters = [];
        }
        clusterLoading = false;
    }

    async function labelClustersWithLlm() {
        if (clusters.length === 0) return;
        labelLoading = true;
        try {
            // Use the configured LLM URL from settings (Ollama default)
            const { getSetting } = await import('$lib/store');
            const url = await getSetting('llmUrl', 'http://localhost:11434/v1') as string;
            const model = await getSetting('llmModel', 'llama3') as string;
            const labels = await invoke<string[]>('index_label_clusters', {
                clusters, llmUrl: url, llmModel: model,
            });
            // Apply labels to clusters
            for (let i = 0; i < Math.min(labels.length, clusters.length); i++) {
                if (labels[i]) clusters[i].name = labels[i];
            }
            clusters = [...clusters]; // trigger reactivity
        } catch (e: any) {
            console.warn('LLM labelling failed:', e);
        }
        labelLoading = false;
    }

    function formatBytes(bytes: number): string {
        if (bytes < 1024) return `${bytes} B`;
        if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
        if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
        return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
    }

    function maxCount(items: [string, number][] | [number, number][]): number {
        if (!items.length) return 1;
        return Math.max(...items.map(([, c]) => c));
    }

    async function loadStats() {
        loading = true;
        error = '';
        try {
            stats = await invoke<CorpusStats>('index_corpus_stats');
        } catch (e: any) {
            error = String(e?.message ?? e);
        } finally {
            loading = false;
        }
    }

    // Knowledge graph
    interface GraphNode { id: string; label: string; group: string; doc_count: number; }
    interface GraphEdge { source: string; target: string; weight: number; }
    interface EntityGraph { nodes: GraphNode[]; edges: GraphEdge[]; }
    let graph = $state<EntityGraph | null>(null);
    let graphLoading = $state(false);

    async function loadGraph() {
        graphLoading = true;
        try {
            graph = await invoke<EntityGraph>('index_entity_graph', { minCooccurrence: 2, maxNodes: 50 });
        } catch { graph = null; }
        graphLoading = false;
    }

    onMount(() => { loadStats(); });
</script>

<div class="dashboard">
    {#if loading}
        <div class="center"><Loader2 size={24} class="spin" /> Loading corpus statistics…</div>
    {:else if error}
        <div class="center error">{error}</div>
    {:else if stats}
        <!-- Summary cards -->
        <div class="summary-grid">
            <div class="stat-card">
                <div class="stat-icon"><FileText size={20} /></div>
                <div class="stat-value">{stats.total_docs.toLocaleString()}</div>
                <div class="stat-label">Documents</div>
            </div>
            <div class="stat-card">
                <div class="stat-icon"><BarChart3 size={20} /></div>
                <div class="stat-value">{stats.total_chunks.toLocaleString()}</div>
                <div class="stat-label">Chunks</div>
            </div>
            <div class="stat-card">
                <div class="stat-icon"><HardDrive size={20} /></div>
                <div class="stat-value">{formatBytes(stats.total_size_bytes)}</div>
                <div class="stat-label">Total Size</div>
            </div>
            <div class="stat-card">
                <div class="stat-icon"><Languages size={20} /></div>
                <div class="stat-value">{stats.lang_distribution.length}</div>
                <div class="stat-label">Languages</div>
            </div>
        </div>

        <!-- Distribution panels -->
        <div class="panels">
            <!-- File types -->
            {#if stats.ext_distribution.length > 0}
                <section class="panel">
                    <h3><FileText size={14} /> File Types</h3>
                    <div class="bar-chart">
                        {#each stats.ext_distribution.slice(0, 15) as [ext, count]}
                            <div class="bar-row">
                                <span class="bar-label">.{ext}</span>
                                <div class="bar-track">
                                    <div class="bar-fill ext-fill" style="width:{(count / maxCount(stats.ext_distribution)) * 100}%"></div>
                                </div>
                                <span class="bar-count">{count}</span>
                            </div>
                        {/each}
                    </div>
                </section>
            {/if}

            <!-- Languages -->
            {#if stats.lang_distribution.length > 0}
                <section class="panel">
                    <h3><Languages size={14} /> Languages</h3>
                    <div class="bar-chart">
                        {#each stats.lang_distribution.slice(0, 10) as [lang, count]}
                            <div class="bar-row">
                                <span class="bar-label">{lang || '?'}</span>
                                <div class="bar-track">
                                    <div class="bar-fill lang-fill" style="width:{(count / maxCount(stats.lang_distribution)) * 100}%"></div>
                                </div>
                                <span class="bar-count">{count}</span>
                            </div>
                        {/each}
                    </div>
                </section>
            {/if}

            <!-- Document Timeline — full-width year histogram -->
            {#if stats.year_histogram.length > 0}
                <section class="panel timeline-panel">
                    <h3><Calendar size={14} /> Document Timeline</h3>
                    <div class="year-chart">
                        {#each stats.year_histogram as [year, count]}
                            <button
                                class="year-bar"
                                class:selected={selectedYear === year}
                                title="{year}: {count} document{count !== 1 ? 's' : ''}"
                                onclick={() => selectedYear = selectedYear === year ? null : year}
                            >
                                <div class="year-fill" style="height:{(count / maxCount(stats.year_histogram)) * 100}%"></div>
                                <span class="year-label">{year}</span>
                                <span class="year-count">{count}</span>
                            </button>
                        {/each}
                    </div>
                    {#if selectedYear}
                        <div class="timeline-filter-hint">
                            Showing year {selectedYear} — use this in Search with filter Year min/max = {selectedYear}
                        </div>
                    {/if}
                </section>
            {/if}

            <!-- Top tags -->
            {#if stats.top_tags.length > 0}
                <section class="panel">
                    <h3><Tag size={14} /> Top Tags</h3>
                    <div class="tag-list">
                        {#each stats.top_tags.slice(0, 20) as [tag, count]}
                            <span class="tag-chip" title="{count} documents">
                                {tag} <em>{count}</em>
                            </span>
                        {/each}
                    </div>
                </section>
            {/if}

            <!-- Topical clusters -->
            <section class="panel">
                <h3>
                    <Network size={14} /> Topical Clusters
                    <span class="cluster-controls">
                        <label class="k-label">k=<input type="number" bind:value={clusterK} min="2" max="50" class="k-input" /></label>
                        <button class="cluster-btn" onclick={loadClusters} disabled={clusterLoading}>
                            {#if clusterLoading}<Loader2 size={12} class="spin" />{:else}Cluster{/if}
                        </button>
                        {#if clusters.length > 0}
                            <button class="cluster-btn" onclick={labelClustersWithLlm} disabled={labelLoading} title="Generate labels with LLM">
                                {#if labelLoading}<Loader2 size={12} class="spin" />{:else}AI Label{/if}
                            </button>
                        {/if}
                    </span>
                </h3>
                {#if clusters.length > 0}
                    <div class="cluster-list">
                        {#each clusters as c (c.id)}
                            <div class="cluster-card">
                                <div class="cluster-header">
                                    <strong>{c.name}</strong>
                                    <span class="cluster-count">{c.doc_count} docs</span>
                                </div>
                                {#if c.sample_titles.length > 0}
                                    <ul class="cluster-titles">
                                        {#each c.sample_titles as t}
                                            <li>{t}</li>
                                        {/each}
                                    </ul>
                                {/if}
                            </div>
                        {/each}
                    </div>
                {:else if !clusterLoading}
                    <p class="muted">Click "Cluster" to group documents by embedding similarity.</p>
                {/if}
            </section>

            <!-- Knowledge graph -->
            <section class="panel">
                <h3>
                    <Network size={14} /> Entity Graph
                    <span class="cluster-controls">
                        <button class="cluster-btn" onclick={loadGraph} disabled={graphLoading}>
                            {#if graphLoading}<Loader2 size={12} class="spin" />{:else}Build{/if}
                        </button>
                    </span>
                </h3>
                {#if graph && graph.nodes.length > 0}
                    <div class="graph-info">
                        {graph.nodes.length} entities, {graph.edges.length} co-occurrences
                    </div>
                    <div class="cluster-list">
                        {#each graph.nodes.slice(0, 20) as node (node.id)}
                            <div class="cluster-card">
                                <div class="cluster-header">
                                    <strong>{node.label}</strong>
                                    <span class="cluster-count">{node.group} · {node.doc_count} docs</span>
                                </div>
                            </div>
                        {/each}
                    </div>
                {:else if !graphLoading}
                    <p class="muted">Click "Build" to generate entity co-occurrence graph from NER tags.</p>
                {/if}
            </section>
        </div>

        <button class="refresh-btn" onclick={loadStats} disabled={loading}>
            Refresh
        </button>
    {:else}
        <div class="center">No statistics available. Enable the index first.</div>
    {/if}
</div>

<style>
    .dashboard {
        padding: 16px;
        overflow-y: auto;
        max-height: 100%;
    }
    .center {
        display: flex;
        align-items: center;
        justify-content: center;
        gap: 8px;
        padding: 40px;
        color: #a1a1aa;
    }
    .error { color: #ef4444; }

    .summary-grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
        gap: 12px;
        margin-bottom: 20px;
    }
    .stat-card {
        background: #18181b;
        border: 1px solid #27272a;
        border-radius: 8px;
        padding: 16px;
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 4px;
    }
    .stat-icon { color: #3b82f6; }
    .stat-value {
        font-size: 1.5rem;
        font-weight: 700;
        color: #fafafa;
        font-variant-numeric: tabular-nums;
    }
    .stat-label {
        font-size: 0.75rem;
        color: #71717a;
        text-transform: uppercase;
        letter-spacing: 0.05em;
    }

    .panels {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
        gap: 16px;
    }
    .panel {
        background: #18181b;
        border: 1px solid #27272a;
        border-radius: 8px;
        padding: 14px;
    }
    .panel h3 {
        display: flex;
        align-items: center;
        gap: 6px;
        margin: 0 0 12px;
        font-size: 0.85rem;
        font-weight: 600;
        color: #d4d4d8;
    }

    .bar-chart { display: flex; flex-direction: column; gap: 4px; }
    .bar-row { display: flex; align-items: center; gap: 8px; }
    .bar-label {
        width: 60px;
        text-align: right;
        font-size: 0.75rem;
        color: #a1a1aa;
        font-family: ui-monospace, monospace;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }
    .bar-track {
        flex: 1;
        height: 14px;
        background: #27272a;
        border-radius: 3px;
        overflow: hidden;
    }
    .bar-fill {
        height: 100%;
        border-radius: 3px;
        transition: width 0.3s ease;
    }
    .ext-fill { background: #3b82f6; }
    .lang-fill { background: #8b5cf6; }
    .bar-count {
        width: 40px;
        text-align: right;
        font-size: 0.7rem;
        color: #71717a;
        font-variant-numeric: tabular-nums;
    }

    .year-chart {
        display: flex;
        align-items: flex-end;
        gap: 2px;
        height: 100px;
        padding-top: 4px;
    }
    .timeline-panel { grid-column: 1 / -1; }
    .year-bar {
        flex: 1;
        display: flex;
        flex-direction: column;
        align-items: center;
        height: 100%;
        min-width: 18px;
        background: none;
        border: 1px solid transparent;
        border-radius: 4px;
        cursor: pointer;
        padding: 2px 1px;
        transition: border-color 0.15s, background 0.15s;
    }
    .year-bar:hover { background: #1e293b; border-color: #334155; }
    .year-bar.selected { background: rgba(34, 197, 94, 0.15); border-color: #22c55e; }
    .year-fill {
        width: 100%;
        background: #22c55e;
        border-radius: 2px 2px 0 0;
        margin-top: auto;
        transition: height 0.3s ease;
    }
    .year-bar.selected .year-fill { background: #4ade80; }
    .year-label {
        font-size: 0.55rem;
        color: #71717a;
        margin-top: 2px;
        writing-mode: vertical-lr;
        text-orientation: mixed;
    }
    .year-count {
        font-size: 0.5rem;
        color: #52525b;
        margin-bottom: 2px;
    }
    .timeline-filter-hint {
        margin-top: 8px;
        font-size: 0.75rem;
        color: #64748b;
        text-align: center;
    }

    .tag-list {
        display: flex;
        flex-wrap: wrap;
        gap: 6px;
    }
    .tag-chip {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        padding: 3px 8px;
        background: #27272a;
        border-radius: 12px;
        font-size: 0.75rem;
        color: #d4d4d8;
    }
    .tag-chip em {
        font-style: normal;
        color: #71717a;
        font-size: 0.65rem;
    }

    /* Clustering */
    .cluster-controls { display: inline-flex; align-items: center; gap: 6px; margin-left: auto; }
    .k-label { font-size: 0.72rem; color: #a1a1aa; font-weight: 400; display: inline-flex; align-items: center; gap: 2px; }
    .k-input { width: 38px; padding: 2px 4px; background: #18181b; border: 1px solid #3f3f46; border-radius: 3px; color: #d4d4d8; font-size: 0.72rem; text-align: center; }
    .cluster-btn {
        padding: 3px 10px; background: #1d4ed8; border: 1px solid #2563eb; border-radius: 4px;
        color: #fff; font-size: 0.72rem; cursor: pointer; display: inline-flex; align-items: center; gap: 4px;
    }
    .cluster-btn:disabled { opacity: 0.5; cursor: default; }
    .cluster-list { display: flex; flex-direction: column; gap: 6px; }
    .cluster-card {
        background: #18181b; border: 1px solid #27272a; border-radius: 6px; padding: 8px 10px;
    }
    .cluster-header { display: flex; justify-content: space-between; align-items: baseline; }
    .cluster-header strong { font-size: 0.82rem; color: #e4e4e7; }
    .cluster-count { font-size: 0.7rem; color: #71717a; }
    .cluster-titles { margin: 4px 0 0; padding-left: 16px; font-size: 0.72rem; color: #a1a1aa; }
    .cluster-titles li { margin: 1px 0; }
    .muted { color: #52525b; font-size: 0.75rem; margin: 4px 0; }

    .refresh-btn {
        margin-top: 16px;
        padding: 6px 16px;
        background: #27272a;
        border: 1px solid #3f3f46;
        border-radius: 6px;
        color: #d4d4d8;
        cursor: pointer;
        font-size: 0.8rem;
    }
    .refresh-btn:hover { background: #3f3f46; }
    .refresh-btn:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
