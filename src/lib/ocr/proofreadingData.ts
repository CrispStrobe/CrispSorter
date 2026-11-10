/** User-authored proofreading preferences; none of these are OCR confidence. */
export interface VocabularyEntry {
    id: string; text: string; language: string;
    kind: 'word' | 'protected'; caseSensitive: boolean;
}
export interface CorrectionRule {
    id: string; name: string; from: string; to: string; language: string;
    caseSensitive: boolean; wholeWord: boolean; enabled: boolean; example?: string;
}
export interface ProofreadingSelection {
    pageId: string; regionId: string; start: number; end: number; text: string;
}
export interface IgnoredOccurrence extends ProofreadingSelection {
    imagePath: string; sourceText: string;
}
export interface ProofreadingData {
    version: 1; language: string; entries: VocabularyEntry[];
    rules: CorrectionRule[]; ignored: IgnoredOccurrence[];
}

const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const plain = (value: unknown, maximum: number): value is string => typeof value === 'string' &&
    value.length <= maximum && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f]/u.test(value);
const uniqueId = (): string => globalThis.crypto?.randomUUID?.() ?? `proof-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
export const isUtf16Boundary = (text: string, offset: number): boolean => offset === 0 || offset === text.length ||
    !(text.charCodeAt(offset-1) >= 0xd800 && text.charCodeAt(offset-1) <= 0xdbff &&
      text.charCodeAt(offset) >= 0xdc00 && text.charCodeAt(offset) <= 0xdfff);

export function normalizeProofreadingLanguage(value: string): string {
    const aliases: Record<string, string> = { auto:'und', eng:'en', deu:'de', ger:'de', fra:'fr', fre:'fr',
        spa:'es', ita:'it', por:'pt', nld:'nl', dut:'nl', rus:'ru', ara:'ar', jpn:'ja', kor:'ko', chi_sim:'zh-Hans', chi_tra:'zh-Hant' };
    const language = aliases[value.trim().toLowerCase()] ?? value.trim();
    try {
        if (!language || language.length > 40) throw new Error();
        return Intl.getCanonicalLocales(language)[0];
    } catch { throw new Error('Choose a language code such as en, de, or und (unspecified).'); }
}

export function emptyProofreadingData(language = 'und'): ProofreadingData {
    return { version:1, language:normalizeProofreadingLanguage(language), entries:[], rules:[], ignored:[] };
}

/** Missing legacy data is empty; malformed supplied data is never overwritten. */
export function readProofreadingData(value: unknown): ProofreadingData {
    if (value === undefined || value === null) return emptyProofreadingData();
    const invalid = (detail: string): never => { throw new Error(`Invalid proofreading data: ${detail}. The saved data was preserved.`); };
    if (!object(value) || value.version !== 1 || typeof value.language !== 'string' ||
        !Array.isArray(value.entries) || value.entries.length > 10000 ||
        !Array.isArray(value.rules) || value.rules.length > 1000 ||
        (value.ignored !== undefined && (!Array.isArray(value.ignored) || value.ignored.length > 200))) invalid('unsupported version or size');
    const input = value as unknown as ProofreadingData;
    const ids = new Set<string>();
    const identity = (value: unknown): value is string => {
        if (!plain(value, 128) || !value.trim() || ids.has(value)) return false;
        ids.add(value); return true;
    };
    const language = normalizeProofreadingLanguage(input.language);
    const entries = input.entries.map((entry): VocabularyEntry => {
        if (!object(entry) || !identity(entry.id) || !plain(entry.text, 256) || !entry.text.trim() ||
            /[\r\n\t]/u.test(entry.text) || typeof entry.language !== 'string' ||
            !['word','protected'].includes(entry.kind) || typeof entry.caseSensitive !== 'boolean') invalid('invalid vocabulary entry');
        return { id:entry.id, text:entry.text, language:normalizeProofreadingLanguage(entry.language),
            kind:entry.kind, caseSensitive:entry.caseSensitive };
    });
    const rules = input.rules.map((rule): CorrectionRule => {
        if (!object(rule) || !identity(rule.id) || !plain(rule.name, 160) || !rule.name.trim() ||
            !plain(rule.from, 1000) || !rule.from.length || !plain(rule.to, 10000) || rule.from === rule.to ||
            typeof rule.language !== 'string' || typeof rule.caseSensitive !== 'boolean' ||
            typeof rule.wholeWord !== 'boolean' || typeof rule.enabled !== 'boolean' ||
            (rule.example !== undefined && !plain(rule.example, 4000))) invalid('invalid literal correction rule');
        return { id:rule.id, name:rule.name, from:rule.from, to:rule.to,
            language:normalizeProofreadingLanguage(rule.language), caseSensitive:rule.caseSensitive,
            wholeWord:rule.wholeWord, enabled:rule.enabled, ...(rule.example !== undefined ? {example:rule.example} : {}) };
    });
    const ignored = (input.ignored ?? []).map((entry): IgnoredOccurrence => {
        if (!object(entry) || !plain(entry.pageId, 128) || !entry.pageId || !plain(entry.regionId, 128) || !entry.regionId ||
            !plain(entry.imagePath, 10000) || !entry.imagePath || !plain(entry.sourceText, 1000000) ||
            !plain(entry.text, 10000) || !entry.text || !Number.isInteger(entry.start) || !Number.isInteger(entry.end) ||
            entry.start < 0 || entry.end <= entry.start || entry.end > entry.sourceText.length ||
            !isUtf16Boundary(entry.sourceText,entry.start) || !isUtf16Boundary(entry.sourceText,entry.end) ||
            entry.sourceText.slice(entry.start, entry.end) !== entry.text) invalid('invalid ignored occurrence');
        return { pageId:entry.pageId, regionId:entry.regionId, start:entry.start, end:entry.end,
            text:entry.text, imagePath:entry.imagePath, sourceText:entry.sourceText };
    });
    return {version:1, language, entries, rules, ignored};
}

export function createVocabularyEntry(text: string, language: string, kind: 'word'|'protected' = 'word', caseSensitive = true): VocabularyEntry {
    const entry = { id:uniqueId(), text:text.trim(), language, kind, caseSensitive };
    return readProofreadingData({...emptyProofreadingData(), entries:[entry]}).entries[0];
}

export function createCorrectionRule(value: Omit<CorrectionRule,'id'>): CorrectionRule {
    return readProofreadingData({...emptyProofreadingData(), rules:[{...value,id:uniqueId()}]}).rules[0];
}

/** Import merges ownership-local data; IDs are regenerated only on a collision. */
export function mergeProofreadingData(current: ProofreadingData, incoming: ProofreadingData): ProofreadingData {
    const out = readProofreadingData(current), next = readProofreadingData(incoming);
    const ids = new Set([...out.entries, ...out.rules].map(item => item.id));
    for (const entry of next.entries) {
        if (out.entries.some(item => item.text.normalize('NFC') === entry.text.normalize('NFC') &&
            item.language === entry.language && item.kind === entry.kind && item.caseSensitive === entry.caseSensitive)) continue;
        const copy = {...entry,id:ids.has(entry.id) ? uniqueId() : entry.id};
        ids.add(copy.id); out.entries.push(copy);
    }
    for (const rule of next.rules) {
        if (out.rules.some(item => item.from === rule.from && item.to === rule.to && item.language === rule.language &&
            item.caseSensitive === rule.caseSensitive && item.wholeWord === rule.wholeWord && item.enabled === rule.enabled)) continue;
        const copy = {...rule,id:ids.has(rule.id) ? uniqueId() : rule.id};
        ids.add(copy.id); out.rules.push(copy);
    }
    // Occurrence ignores belong to one project/image/text version and are not
    // portable dictionary entries. Existing decisions remain, imported ones do not.
    return readProofreadingData(out);
}

export function exportProofreadingData(data: ProofreadingData): string {
    return JSON.stringify({...clone(readProofreadingData(data)), ignored:[]}, null, 2) + '\n';
}
