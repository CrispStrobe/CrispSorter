import { initEpubFile } from '@lingo-reader/epub-parser';

export interface ExtractionResult {
    text: string;
    markdownText?: string;
    headings?: string[];
    metadata?: Record<string, any>;
}

export interface ExtractionOptions {
    forceOCR?: boolean;
    signal?: AbortSignal;
    onProgress?: (page: number, total: number) => void;
    maxChars?: number;
    maxPages?: number;
}

export async function extractEpub(
    arrayBuffer: ArrayBuffer, 
    fileName: string, 
    options: ExtractionOptions = {}
): Promise<ExtractionResult> {
    console.log(`[EpubExtractor] Starting extraction for ${fileName}, buffer size: ${arrayBuffer.byteLength}`);
    
    try {
        // In the browser/Tauri frontend, we can create a File from ArrayBuffer
        const file = new File([arrayBuffer], fileName, { type: 'application/epub+zip' });
        const epub = await initEpubFile(file);
        
        const spine = epub.getSpine();
        const metadata = epub.getMetadata();
        
        let fullText = '';
        let markdownParts: string[] = [];
        let allHeadings: string[] = [];

        console.log(`[EpubExtractor] Spine items: ${spine.length}`);

        const totalChapters = spine.length;
        for (let i = 0; i < totalChapters; i++) {
            const item = spine[i];
            
            // Abort signal check
            if (options.signal?.aborted) {
                console.log(`[EpubExtractor] Aborted at chapter ${i + 1}/${totalChapters}`);
                break;
            }

            // Progress callback (treating chapters as "pages" for EPUB)
            options.onProgress?.(i + 1, totalChapters);

            // Early stop: maxPages (chapters) limit
            if (options.maxPages && (i + 1) > options.maxPages) {
                console.log(`[EpubExtractor] Reached maxPages (${options.maxPages}), stopping early.`);
                break;
            }

            try {
                const { html } = await epub.loadChapter(item.id);
                if (!html) continue;

                const parser = new DOMParser();
                const doc = parser.parseFromString(html, 'text/html');

                // Extract plain text
                const chapterText = doc.body.textContent || '';
                fullText += chapterText + '\n\n';

                // Simple HTML to Markdown-ish conversion for headings
                const nodes = doc.body.querySelectorAll('*');
                nodes.forEach(node => {
                    const tagName = node.tagName.toLowerCase();
                    if (/^h[1-6]$/.test(tagName)) {
                        const level = parseInt(tagName[1]);
                        const headingText = (node.textContent || '').trim();
                        if (headingText) {
                            markdownParts.push(`${'#'.repeat(level)} ${headingText}`);
                            allHeadings.push(headingText);
                        }
                    } else if (tagName === 'p') {
                        const pText = (node.textContent || '').trim();
                        if (pText) {
                            markdownParts.push(pText);
                        }
                    }
                });
                markdownParts.push('\n'); 

                // Early stop: maxChars already reached?
                if (options.maxChars && fullText.length >= options.maxChars) {
                    console.log(`[EpubExtractor] Reached maxChars (${options.maxChars}), stopping early.`);
                    break;
                }

            } catch (chapterErr) {
                console.warn(`[EpubExtractor] Failed to load chapter ${item.id}:`, chapterErr);
            }
        }

        const result: ExtractionResult = {
            text: fullText.trim(),
            markdownText: markdownParts.join('\n\n').trim(),
            headings: allHeadings,
            metadata: metadata
        };

        console.log(`[EpubExtractor] Done — text: ${result.text.length} chars, headings: ${result.headings?.length}`);
        
        if ((epub as any).destroy) {
            (epub as any).destroy();
        }

        return result;

    } catch (error: any) {
        console.error('[EpubExtractor] CRITICAL ERROR:', error);
        throw new Error(`EPUB extraction failed: ${error.message || String(error)}`);
    }
}
