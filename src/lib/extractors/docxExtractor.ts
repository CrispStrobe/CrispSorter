import mammoth from 'mammoth';

export async function extractDocx(arrayBuffer: ArrayBuffer): Promise<string> {
    // mammoth expects a buffer in Node, but for universal browser usage, it also accepts arrayBuffers
    try {
        const result = await mammoth.extractRawText({ arrayBuffer: arrayBuffer });
        
        if (result.messages && result.messages.length > 0) {
            console.warn("Mammoth messages during extraction:", result.messages);
        }
        
        return result.value.trim();
    } catch (error) {
        console.error("Error extracting DOCX:", error);
        throw new Error(`DOCX extraction failed: ${error instanceof Error ? error.message : String(error)}`);
    }
}
