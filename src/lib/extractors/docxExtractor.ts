import mammoth from 'mammoth';

export async function extractDocx(arrayBuffer: ArrayBuffer): Promise<string> {
    console.log(`[DocxExtractor] Starting extraction, buffer size: ${arrayBuffer.byteLength}`);
    try {
        const result = await mammoth.extractRawText({ arrayBuffer: arrayBuffer });
        
        if (result.messages && result.messages.length > 0) {
            console.warn("[DocxExtractor] Mammoth messages:", result.messages);
        }
        
        console.log(`[DocxExtractor] Extraction success, text length: ${result.value.length}`);
        return result.value.trim();
    } catch (error: any) {
        console.error("[DocxExtractor] CRITICAL ERROR:", error);
        throw new Error(`DOCX extraction failed: ${error.message || String(error)}`);
    }
}
