/** Literal-first query construction. The native parser remains authoritative. */
export interface QueryBuilderInput {
    all: string;
    any: string;
    phrase: string;
    excluded: string;
    field: '' | 'title' | 'body' | 'headings' | 'translated';
    nearLeft: string;
    nearRight: string;
    distance: number | null;
    ordered: boolean;
}

export type QueryBuilderError = 'operands' | 'distance' | 'length' | null;
export const quoteQueryLiteral = (value: string): string => '"' + value.replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"';
const words = (value: string): string[] => value.trim().split(/\s+/u).filter(Boolean).map(quoteQueryLiteral);

export function buildQuery(input: QueryBuilderInput): { query: string; error: QueryBuilderError } {
    const left = input.nearLeft.trim(), right = input.nearRight.trim();
    if (Boolean(left) !== Boolean(right)) return { query: '', error: 'operands' };
    if ((left || right) && (input.distance == null || !Number.isInteger(input.distance) || input.distance < 0 || input.distance > 1024)) {
        return { query: '', error: 'distance' };
    }
    const clauses = words(input.all);
    if (input.any.trim()) clauses.push('(' + words(input.any).join(' OR ') + ')');
    if (input.phrase.trim()) clauses.push(quoteQueryLiteral(input.phrase.trim()));
    if (left && right) clauses.push(`(${quoteQueryLiteral(left)} ${input.ordered ? 'pre' : 'w'}/${input.distance} ${quoteQueryLiteral(right)})`);
    clauses.push(...words(input.excluded).map(word => 'NOT ' + word));
    const value = clauses.join(' AND ');
    const query = value && input.field ? `${input.field}:(${value})` : value;
    return new TextEncoder().encode(query).length > 8192 ? { query: '', error: 'length' } : { query, error: null };
}
