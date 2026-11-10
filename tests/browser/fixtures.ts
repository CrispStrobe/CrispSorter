// Explicit IPC/provider fixtures; native and model qualification are separate.
import type { Page } from '@playwright/test';

export async function fixtures(page: Page) {
    await page.addInitScript(() => {
        const root = window as any;
        const body = 'Unrelated opening. '.repeat(350) + 'The cobalt warranty expires in November 2031. ' + 'Unrelated appendix. '.repeat(350);
        const items = [
            { id: 'pending', originalPath: '/fixtures/scan.png', originalName: 'Pending scan.png', extension: 'png', size: 1024, modifiedAt: 1, status: 'queued', isAccepted: false },
            { id: 'warranty', originalPath: '/fixtures/warranty.txt', originalName: 'Warranty.txt', extension: 'txt', size: body.length, modifiedAt: 1, status: 'review', isAccepted: false, suggestedTitle: 'Warranty', suggestedAuthor: 'Acme' },
        ];
        if (new URLSearchParams(location.search).has('fixtureSort')) {
            Object.assign(items[1], { isAccepted: true, targetPath: '/fixtures/sorted/warranty.txt' });
        }
        const settings: Record<string, any> = {
            language: 'en', providers: [{ id: 'ollama', name: 'Ollama (Local)', baseUrl: 'http://localhost:11434/v1', apiKey: 'ollama', models: ['fixture-model'], selectedModel: 'fixture-model', isConfigured: true }],
            activeProviderId: 'ollama', localModels: ['fixture-model'], llmContextLimit: 4096,
            experimentalCrispEmbed: { endpoint: 'http://127.0.0.1:18080', moedern: false, decisions: true },
        };
        root.__fixtureCalls = [];
        root.__fixturePrompts = [];
        root.__fixtureItems = items;
        root.__fixtureEmptyEvidence = false;
        root.__fixtureRegionEmpty = false;
        const result = { doc_id: 'warranty', filename: 'Warranty.txt', location_uri: '/fixtures/warranty.txt', chunk_index: 4, score: 0.8, source_hash: 'a'.repeat(64), snippet: 'Cobalt warranty' };
        root.__fixtureResults = [result];
        root.__fixtureSortGate = false;
        let fields = [{ id: 'vendor-1', label: 'vendor', original_value: 'Acme Incorporated', normalized_value: 'Acme Incorporated', model: 'fixture-ner', confidence: 0.85, start_char: 12, end_char: 29, page: null, anchor_method: 'first_exact_match', review_status: 'suggested', revision: 0 }];
        let rid = 1;
        const callbacks = new Map<number, Function>();
        const requests = new Map<number, any>();
        const capabilities = {
            platform: 'linux', desktop: true, mobile: false, sidecars: true,
            dev_tools: false, aitoolkit: false, cloud_backup: false,
            local_llm: false, launch_local_servers: false,
            ocr: true, ocr_tiers: ['crispembed'], pdf_render: true, pdf_zpdf: true,
            asr: false, tts: false, tts_watermarked: false,
            translate_align: false, translate_nmt: false,
            drive_filen_native: false, drive_internxt_native: false, drive_subprocess: false,
            fuse: false, audio_glint: false, audio_ffmpeg: false, direct_print: false, share_sheet: false,
            flags: ['build:ocr', 'build:desktop'],
        };
        // Exercise the shipped mobile feature set, not just a narrow desktop
        // viewport. Real simulator/device runs are a separate qualification.
        const fixturePlatform = new URLSearchParams(location.search).get('fixturePlatform');
        if (fixturePlatform === 'ios' || fixturePlatform === 'android') {
            Object.assign(capabilities, {
                platform: fixturePlatform, desktop: false, mobile: true, sidecars: false,
                // Android can have the pure-Rust ocrs models installed without
                // having the CrispEmbed-only workbench engine.
                ocr: fixturePlatform === 'android',
                ocr_tiers: fixturePlatform === 'android' ? ['ocrs'] : [],
                pdf_render: false, pdf_zpdf: false,
                flags: fixturePlatform === 'android' ? ['build:ocr'] : [],
            });
        }
        root.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
        root.__TAURI_INTERNALS__ = {
            metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
            transformCallback: (fn: Function) => { const id = rid++; callbacks.set(id, fn); return id; },
            unregisterCallback: (id: number) => callbacks.delete(id),
            convertFileSrc: () => 'data:image/svg+xml,' + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="500" height="700"><rect width="500" height="700" fill="white"/><text x="20" y="40">Warranty expires in November 2031.</text></svg>'),
            invoke: async (command: string, args: any = {}) => {
                root.__fixtureCalls.push({ command, args });
                if (command === 'plugin:store|load') return 1;
                if (command === 'plugin:store|get') return [settings[args.key], args.key in settings];
                if (command === 'plugin:store|set') { settings[args.key] = args.value; return; }
                if (command === 'plugin:dialog|open') return '/fixtures/scan.pdf';
                if (command === 'plugin:dialog|save') return '/fixtures/document-fields.csv';
                if (command === 'index_search') return root.__fixtureResults;
                if (command === 'index_query_documents') return { rows: [result], next_cursor: null };
                if (command === 'index_get_fields') return args.docId === 'warranty' ? fields : [];
                if (command === 'index_review_field') {
                    if (args.expectedRevision !== fields[0].revision) throw new Error('Stale revision');
                    fields = [{ ...fields[0], normalized_value: args.normalizedValue, review_status: args.reviewStatus, revision: fields[0].revision + 1 }];
                    return fields;
                }
                if (command === 'verify_sort_field_source') {
                    if (root.__fixtureSortGate) return new Promise(resolve => { root.__fixtureSortResolve = () => { root.__fixtureSortGate = false; resolve(null); }; });
                    return;
                }
                if (command === 'crispembed_classify_document') return { value: args.task === 'collection' ? 'Books' : 'Invoice', confidence: 0.6, model: 'fixture-systemone', input_excerpt: 'Library excerpt for classification.' };
                if (command === 'plugin:fs|write_text_file') return;
                if (command === 'plugin:event|listen') return rid++;
                if (command === 'plugin:path|join') return args.paths.join('/');
                if (command === 'plugin:path|resolve_directory') return '/fixtures';
                if (command === 'plugin:fs|read_file' || command === 'plugin:fs|read_text_file') return Array.from(new TextEncoder().encode(body));
                if (command === 'plugin:fs|stat') return { size: body.length, isFile: true, isDirectory: false, isSymlink: false };
                if (command === 'build_capabilities') return capabilities;
                if (command === 'intended_purpose_status') return { acknowledged: true, version: 1, statement: '', accepted_at_unix: 1 };
                if (command === 'batch_session_is_migrated') return true;
                if (command === 'batch_session_load') return items;
                if (command === 'batch_session_get_extracted_text') return args.itemId === 'warranty' ? body : '';
                if (command === 'batch_session_upsert_item') return;
                if (command === 'folder_access_probe') return { writability: { state: 'writable' }, canRequest: false, shouldAsk: false };
                if (command === 'execute_batch') return { warranty: root.__fixtureSortError ? { success: false, error: 'DESTINATION_EXISTS' } : { success: true } };
                if (command === 'index_is_ready') return true;
                if (command === 'index_filename_only_count') return 7;
                if (command === 'index_stats') return { doc_count: 2, chunk_count: 4, total_rows: 4 };
                if (command === 'index_get_config') return { enabled: true, mode: 'Hybrid', backend_type: 'Local', use_vector: true };
                if (command === 'sync_status') return { pending_count: 0, last_push_ts: null, remote_online: false };
                if (command === 'index_retrieve_passages') return root.__fixtureEmptyEvidence ? [] : [{ document: { doc_id: 'warranty', filename: 'Warranty.txt', location_uri: '/fixtures/warranty.txt', chunk_index: 4, score: 0.8 }, passage: { text: 'The cobalt warranty expires in November 2031.', start_char: 4000, end_char: 4045, offset_unit: 'utf8_bytes' } }];
                if (command === 'ocr_doc_open') return { count: 2, pages: ['/fixtures/page1.png', '/fixtures/page2.png'] };
                if (command === 'ocr_page_regions') return { width: 500, height: 700, regions: args.region && root.__fixtureRegionEmpty ? [] : [{ text: args.region ? 'Corrected crop text.' : 'Warranty expires in November 2031.', x: 10, y: 20, w: 350, h: 40, confidence: args.pagePath.endsWith('page2.png') ? 0.9 : 0.2, char_conf: [] }] };
                if (command === 'plugin:http|fetch') {
                    const id = rid++;
                    const config = args.clientConfig;
                    let response = { data: [{ id: 'fixture-model' }], models: [{ name: 'fixture-model' }] } as any;
                    if (config.url.includes('/chat/completions')) {
                        const payload = JSON.parse(new TextDecoder().decode(new Uint8Array(config.data)));
                        root.__fixturePrompts.push(payload);
                        response = { choices: [{ message: { content: 'The cobalt warranty expires in November 2031. [S1]' } }] };
                    }
                    requests.set(id, { url: config.url, bytes: Array.from(new TextEncoder().encode(JSON.stringify(response))), read: false });
                    return id;
                }
                if (command === 'plugin:http|fetch_send') return { status: 200, statusText: 'OK', url: requests.get(args.rid).url, headers: [['content-type', 'application/json']], rid: args.rid };
                if (command === 'plugin:http|fetch_read_body') {
                    const request = requests.get(args.rid);
                    if (request.read) return [1];
                    request.read = true; return [...request.bytes, 0];
                }
                if (command.endsWith('_list') || command.endsWith('_list_sessions') || command === 'index_list_documents') return [];
                return null;
            },
        };
    });
}
