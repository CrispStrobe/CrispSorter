import type { ContainerCoverage, ContainerEntry, ContainerSourceReceipt } from '../containers';
import { extractionTags, type ExtractionResult } from './index';
import { extractionIndexMetadata, type IndexMetadata } from './indexMetadata';

/** Content only: the native transaction binds member identities and locations. */
export interface ExtractedIngestContent extends IndexMetadata {
    fullText: string;
    fullTextMd?: string;
    headings: string[];
    filename?: string;
    ext?: string;
    title?: string;
    author?: string;
    year?: number;
    language?: string;
    tags: string[];
    containerCoverage?: ContainerCoverage;
}
export interface ContainerIngestContent extends ExtractedIngestContent {
    ownText?: string;
    containerSource?: ContainerSourceReceipt;
    containerCoverage?: ContainerCoverage;
    members?: { entries: ContainerEntry[]; document: ExtractedIngestContent }[];
}

function content(result: ExtractionResult, filename?: string, fallbackLanguage?: (text: string) => string): ExtractedIngestContent {
    const fullText = result.ownText ?? result.text;
    const metadata = result.metadata ?? {};
    const text = (value: unknown): string | undefined => typeof value === 'string' && value.trim() ? value : undefined;
    return {
        fullText,
        // Aggregate markdown would reintroduce every child into the root index.
        ...(result.ownText === undefined && result.markdownText ? { fullTextMd: result.markdownText } : {}),
        headings: result.headings ?? [],
        ...(filename ? { filename } : {}),
        ...(result.ext ? { ext: result.ext } : {}),
        ...(text(metadata.title) ? { title: text(metadata.title) } : {}),
        ...(text(metadata.author) ? { author: text(metadata.author) } : {}),
        ...(Number.isInteger(metadata.year) ? { year: metadata.year } : {}),
        language: text(metadata.language) ?? fallbackLanguage?.(fullText),
        tags: extractionTags(result),
        ...(result.containerCoverage ? { containerCoverage: result.containerCoverage } : {}),
        ...extractionIndexMetadata(result),
    };
}

/** One queue source expands atomically on the native side, preserving root receipts. */
export function extractionIngestContent(result: ExtractionResult, sourceHash?: string, fallbackLanguage?: (text: string) => string): ContainerIngestContent {
    const members = result.members ?? [];
    if (members.length > 0 && (!result.containerSource || result.ownText === undefined)) {
        throw new Error('Container members require a captured source receipt and separate root text.');
    }
    if (result.containerSource && sourceHash && result.containerSource.sha256.toLowerCase() !== sourceHash.toLowerCase()) {
        throw new Error('The container changed during extraction. Reindex the current source file.');
    }
    const root: ContainerIngestContent = content(result, undefined, fallbackLanguage);
    if (result.ownText !== undefined) root.ownText = result.ownText;
    if (result.containerSource) root.containerSource = result.containerSource;
    if (result.containerCoverage) root.containerCoverage = result.containerCoverage;
    if (result.members !== undefined) root.members = members.map(member => {
        if (member.document.members?.length) throw new Error('Container member expansion must be flattened.');
        const entry = member.entries.at(-1);
        if (!entry) throw new Error('Container member has no source chain.');
        const filename = entry.name.split('/').at(-1) || entry.name;
        const document = content(member.document, filename, fallbackLanguage);
        document.ext ??= filename.split('.').length > 1 ? filename.split('.').at(-1)?.toLowerCase() : 'txt';
        return { entries: member.entries, document };
    });
    return root;
}
