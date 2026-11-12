import { platform } from '../capabilities';

/** Android postMessage cannot transport raw bytes. Avoid its JSON number array
 * expansion; callers enforce their byte limit before creating this payload. */
export function ocrBinaryPayload(bytes: Uint8Array): Uint8Array | { base64: string } {
    const os = platform();
    if (os !== 'android' && os !== 'unknown') return bytes;
    // Chunks divisible by three concatenate without intermediate padding and
    // avoid argument-count limits and a full-size binary string copy.
    const chunks: string[] = [];
    for (let offset = 0; offset < bytes.byteLength; offset += 24 * 1024) {
        chunks.push(btoa(String.fromCharCode(...bytes.subarray(offset, offset + 24 * 1024))));
    }
    return { base64: chunks.join('') };
}
