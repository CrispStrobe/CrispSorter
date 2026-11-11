import type { SearchMode } from './recipe';
import type { TextRange } from './inspection';

/** Byte offsets in this receipt refer to its exact stored passage text. */
export type EvidenceKind = 'term' | 'fuzzy' | 'wildcard' | 'phrase' | 'proximity';
export type ClauseKind = 'term' | 'phrase' | 'proximity' | 'and' | 'or' | 'not' | 'field' | 'all';
export type EvidenceField = 'title' | 'body' | 'headings' | 'translated';
export type QueryIntent = 'advanced' | 'natural';
export interface EvidenceSpan { start: number; end: number; kind: EvidenceKind; clause: string }
export interface ExplainClause {
    kind: ClauseKind;
    label: string;
    matched: boolean;
    field?: string;
    matched_fields?: EvidenceField[];
    children: ExplainClause[];
}
export interface DisplayedPassage {
    text: string;
    start_char: number | null;
    end_char: number | null;
    offset_unit: string;
    truncated: boolean;
    doc_id?: string;
    owner_id?: string;
    chunk_index?: number;
    source_hash?: string | null;
    indexed_at?: number | null;
}
export interface ExplanationRequest {
    docId: string;
    ownerId: string;
    chunkIndex: number;
    query: string;
    mode: SearchMode;
    queryIntent: QueryIntent;
    fuzzy: boolean;
    synonyms: boolean;
}
export interface PassageExplanation {
    explanation_version: 1;
    query: string;
    lexical_query: string;
    query_intent: QueryIntent;
    mode: SearchMode;
    fuzzy: boolean;
    synonyms: boolean;
    passage: DisplayedPassage & { doc_id: string; owner_id: string; chunk_index: number };
    lexical_document_match: boolean | null;
    semantic_evidence: boolean;
    clauses: ExplainClause | null;
    spans: EvidenceSpan[];
    spans_truncated: boolean;
    scope: 'document_query_with_passage_spans';
    notes: string[];
}
/** UTF-16 ranges for DOM slicing, retaining every overlapping proof label. */
export interface EvidenceRange extends TextRange { evidence: { kind: EvidenceKind; clause: string }[] }
export interface ClauseRow { clause: ExplainClause; depth: number; key: string }

const MAX_PASSAGE_BYTES = 256 * 1024;
const MAX_SPANS = 256;
const kinds = new Set<unknown>(['term', 'fuzzy', 'wildcard', 'phrase', 'proximity']);
const clauseKinds = new Set<unknown>(['term', 'phrase', 'proximity', 'and', 'or', 'not', 'field', 'all']);
const fields = new Set<unknown>(['title', 'body', 'headings', 'translated']);
function fail(message: string): never { throw new Error(message); }
const object = (value: unknown): Record<string, unknown> => value != null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown> : fail('The query evidence response is invalid.');
const bytes = (value: string): number => new TextEncoder().encode(value).byteLength;
const boundedText = (value: unknown, maximum: number): value is string => typeof value === 'string'
    && bytes(value) <= maximum && !value.includes('\0');
const integer = (value: unknown): value is number => Number.isSafeInteger(value) && Number(value) >= 0;

/**
 * Convert only actual Unicode boundaries. A byte inside a multi-byte character
 * must never become a source mark or an offset into the next character.
 */
export function evidenceRanges(text: string, spans: readonly EvidenceSpan[]): EvidenceRange[] {
    if (spans.length > MAX_SPANS) fail('The query evidence exceeds the passage span limit.');
    const requested = new Set<number>();
    for (const span of spans) {
        if (!integer(span.start) || !integer(span.end) || span.end <= span.start
            || span.end > MAX_PASSAGE_BYTES || !kinds.has(span.kind) || !boundedText(span.clause, 512)) {
            fail('The query evidence contains invalid source offsets.');
        }
        requested.add(span.start); requested.add(span.end);
    }
    const boundaries = new Map<number, number>();
    let byteOffset = 0, stringOffset = 0;
    if (requested.has(0)) boundaries.set(0, 0);
    for (const character of text) {
        const code = character.codePointAt(0)!;
        if (code >= 0xd800 && code <= 0xdfff) fail('The stored passage contains invalid Unicode.');
        byteOffset += code <= 0x7f ? 1 : code <= 0x7ff ? 2 : code <= 0xffff ? 3 : 4;
        stringOffset += character.length;
        if (byteOffset > MAX_PASSAGE_BYTES) fail('The stored passage exceeds the evidence text limit.');
        if (requested.has(byteOffset)) boundaries.set(byteOffset, stringOffset);
    }
    if (boundaries.size !== requested.size) fail('The query evidence does not align with the stored Unicode text.');
    const ordered = spans.map(span => ({ start: boundaries.get(span.start)!, end: boundaries.get(span.end)!,
        evidence: [{ kind: span.kind, clause: span.clause }] })).sort((a, b) => a.start - b.start || a.end - b.end);
    const merged: EvidenceRange[] = [];
    for (const range of ordered) {
        const previous = merged.at(-1);
        if (previous && range.start < previous.end) {
            previous.end = Math.max(previous.end, range.end);
            for (const item of range.evidence) if (!previous.evidence.some(value => value.kind === item.kind && value.clause === item.clause)) {
                previous.evidence.push(item);
            }
        } else merged.push(range);
    }
    return merged;
}

/** Reject mismatched query/source receipts before any native span is displayed. */
export function readPassageExplanation(value: unknown, request: ExplanationRequest, displayed: DisplayedPassage): PassageExplanation {
    const response = object(value);
    if (response.explanation_version !== 1 || response.scope !== 'document_query_with_passage_spans'
        || response.query !== request.query || response.query_intent !== request.queryIntent
        || response.mode !== request.mode || response.fuzzy !== request.fuzzy || response.synonyms !== request.synonyms
        || !boundedText(response.lexical_query, 8192) || (request.mode === 'vector') !== (response.lexical_query === '')) {
        fail('The query evidence describes a different search or unsupported response version.');
    }
    const passage = object(response.passage);
    if (passage.doc_id !== request.docId || passage.owner_id !== request.ownerId || passage.chunk_index !== request.chunkIndex
        || (displayed.doc_id != null && displayed.doc_id !== request.docId)
        || (displayed.owner_id != null && displayed.owner_id !== request.ownerId)
        || (displayed.chunk_index != null && displayed.chunk_index !== request.chunkIndex)) {
        fail('The query evidence belongs to a different owner, document, or passage.');
    }
    if (typeof passage.text !== 'string' || bytes(passage.text) > MAX_PASSAGE_BYTES || passage.text !== displayed.text
        || passage.offset_unit !== 'utf8_bytes' || displayed.offset_unit !== 'utf8_bytes'
        || passage.start_char !== displayed.start_char || passage.end_char !== displayed.end_char
        || typeof passage.truncated !== 'boolean' || passage.truncated !== displayed.truncated
        || (passage.source_hash ?? null) !== (displayed.source_hash ?? null)
        || (passage.indexed_at ?? null) !== (displayed.indexed_at ?? null)) {
        fail('The stored passage changed while its query evidence was loading. Reload the passage to inspect current evidence.');
    }
    if ((response.lexical_document_match !== null && typeof response.lexical_document_match !== 'boolean')
        || response.semantic_evidence !== (request.mode !== 'text') || typeof response.spans_truncated !== 'boolean'
        || !Array.isArray(response.spans) || response.spans.length > MAX_SPANS
        || !Array.isArray(response.notes) || response.notes.length > 32
        || !response.notes.every(note => boundedText(note, 8192))) {
        fail('The query evidence contains invalid match or coverage information.');
    }
    let nodes = 0;
    const readClause = (value: unknown, depth: number): ExplainClause => {
        if (++nodes > 1024 || depth > 64) fail('The query evidence exceeds the clause limit.');
        const clause = object(value);
        if (!clauseKinds.has(clause.kind) || !boundedText(clause.label, 512) || typeof clause.matched !== 'boolean'
            || (clause.field != null && !fields.has(clause.field)) || !Array.isArray(clause.children)) {
            fail('The query evidence contains an invalid clause.');
        }
        if (clause.matched_fields != null && (!Array.isArray(clause.matched_fields) || clause.matched_fields.length > 4
            || !clause.matched_fields.every(field => fields.has(field)) || new Set(clause.matched_fields).size !== clause.matched_fields.length
            || (!clause.matched && clause.matched_fields.length > 0))) {
            fail('The query evidence contains invalid matching text fields.');
        }
        const children = clause.children.map(child => readClause(child, depth + 1));
        const valid = clause.kind === 'and' ? children.length > 0 && clause.matched === children.every(child => child.matched)
            : clause.kind === 'or' ? children.length > 0 && clause.matched === children.some(child => child.matched)
            : clause.kind === 'not' ? children.length === 1 && clause.matched !== children[0].matched
            : clause.kind === 'field' ? children.length === 1 && fields.has(clause.field) && clause.matched === children[0].matched
            : clause.kind === 'all' ? children.length === 0 && clause.matched : children.length === 0;
        if (!valid) fail('The query evidence has inconsistent clause semantics.');
        return { kind: clause.kind as ClauseKind, label: clause.label, matched: clause.matched,
            ...(typeof clause.field === 'string' ? { field: clause.field } : {}),
            ...(Array.isArray(clause.matched_fields) ? { matched_fields: [...clause.matched_fields] as EvidenceField[] } : {}),
            children };
    };
    const clauses = response.clauses === null ? null : readClause(response.clauses, 0);
    const spans: EvidenceSpan[] = response.spans.map(value => {
        const span = object(value);
        if (!integer(span.start) || !integer(span.end) || !kinds.has(span.kind) || !boundedText(span.clause, 512)) {
            fail('The query evidence contains an invalid passage span.');
        }
        return { start: span.start, end: span.end, kind: span.kind as EvidenceKind, clause: span.clause };
    });
    evidenceRanges(displayed.text, spans);
    if (request.mode === 'vector') {
        if (response.lexical_query !== '' || response.lexical_document_match !== null || clauses !== null
            || spans.length !== 0 || response.spans_truncated || !response.semantic_evidence) {
            fail('Semantic query evidence must not claim literal matching spans.');
        }
    } else if (clauses === null || response.lexical_document_match !== clauses.matched
        || (response.lexical_document_match !== true && (spans.length > 0 || response.spans_truncated))) {
        fail('The query evidence has inconsistent lexical match information.');
    }
    return {
        explanation_version: 1, query: request.query, lexical_query: response.lexical_query,
        query_intent: request.queryIntent, mode: request.mode, fuzzy: request.fuzzy, synonyms: request.synonyms,
        passage: { ...displayed, doc_id: request.docId, owner_id: request.ownerId, chunk_index: request.chunkIndex },
        lexical_document_match: response.lexical_document_match as boolean | null,
        semantic_evidence: response.semantic_evidence, clauses, spans,
        spans_truncated: response.spans_truncated, scope: 'document_query_with_passage_spans',
        notes: [...response.notes] as string[],
    };
}

export function explanationClauseRows(clause: ExplainClause | null): ClauseRow[] {
    const rows: ClauseRow[] = [];
    const visit = (value: ExplainClause, depth: number, key: string) => {
        rows.push({ clause: value, depth, key });
        value.children.forEach((child, index) => visit(child, depth + 1, `${key}.${index}`));
    };
    if (clause) visit(clause, 0, '0');
    return rows;
}

export function evidenceKindLabel(kind: EvidenceKind, german = false): string {
    const labels: Record<EvidenceKind, [string, string]> = {
        term: ['Indexed term', 'Indiziertes Wort'], fuzzy: ['Fuzzy term', 'Unscharfer Worttreffer'],
        wildcard: ['Wildcard term', 'Platzhaltertreffer'], phrase: ['Exact phrase', 'Exakte Phrase'],
        proximity: ['Proximity match', 'Abstandstreffer'],
    };
    return labels[kind][german ? 1 : 0];
}

export function evidenceFieldLabel(field: EvidenceField, german = false): string {
    const labels: Record<EvidenceField, [string, string]> = {
        title: ['Title', 'Titel'], body: ['Body text', 'Dokumenttext'],
        headings: ['Headings', 'Überschriften'], translated: ['Translation', 'Übersetzung'],
    };
    return labels[field][german ? 1 : 0];
}
