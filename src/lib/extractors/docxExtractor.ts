import mammoth from 'mammoth';

export interface DocxExtractionResult {
    text: string;
    markdownText: string;
    headings: string[];
}

export async function extractDocx(arrayBuffer: ArrayBuffer): Promise<DocxExtractionResult> {
    console.log(`[DocxExtractor] Starting extraction, buffer size: ${arrayBuffer.byteLength}`);
    try {
        // Extract raw plain text (used for LLM prompt + embedding).
        const rawResult = await mammoth.extractRawText({ arrayBuffer });
        if (rawResult.messages?.length) {
            console.warn('[DocxExtractor] Mammoth messages (raw):', rawResult.messages);
        }

        // Convert to Markdown — preserves heading levels, bold, lists.
        // mammoth.convertToMarkdown exists at runtime but is missing from the type stubs.
        const mdResult = await (mammoth as any).convertToMarkdown({ arrayBuffer }) as { value: string; messages: any[] };
        if (mdResult.messages?.length) {
            console.warn('[DocxExtractor] Mammoth messages (md):', mdResult.messages);
        }

        const text         = rawResult.value.trim();
        const markdownText = mdResult.value.trim();

        // Extract headings from Markdown output.
        const headings = markdownText
            .split('\n')
            .filter((line: string) => /^#{1,6}\s/.test(line))
            .map((line: string) => line.replace(/^#{1,6}\s+/, '').trim())
            .filter(Boolean);

        console.log(`[DocxExtractor] Done — text: ${text.length} chars, headings: ${headings.length}`);
        return { text, markdownText, headings };

    } catch (error: any) {
        console.error('[DocxExtractor] CRITICAL ERROR:', error);
        throw new Error(`DOCX extraction failed: ${error.message || String(error)}`);
    }
}
