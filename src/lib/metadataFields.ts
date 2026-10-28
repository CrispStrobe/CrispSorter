export interface MetadataField {
    id: string;
    label: string;
    original_value: string;
    normalized_value: string;
    model: string;
    confidence: number | null;
    start_char: number | null;
    end_char: number | null;
    page: number | null;
    anchor_method: string;
    review_status: 'suggested' | 'accepted' | 'corrected' | 'rejected';
    revision: number;
}

export function approvedValues(fields: MetadataField[]): Record<string, string> {
    return Object.fromEntries(fields.filter(f => f.review_status === 'accepted' || f.review_status === 'corrected')
        .map(f => [f.label.toLowerCase(), f.normalized_value]));
}

export function applyFieldTemplate(template: string, fields: Record<string, string>): string {
    return template.replace(/\{Field:([^}]+)\}/gi, (_, name: string) => {
        const value = fields[name.trim().toLowerCase()];
        if (!value) throw new Error(`Review and approve the "${name}" field before sorting with this template.`);
        let component = value.replace(/[\u0000-\u001f\\/:*?"<>|]/g, '_').trim().replace(/[. ]+$/g, '').slice(0, 100);
        if (!component || /^\.+$/.test(component)) component = '_';
        if (/^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(component)) component = '_' + component;
        return component;
    });
}

export function fieldsCsv(fields: MetadataField[]): string {
    const cell = (value: unknown) => {
        let text = value == null ? '' : String(value);
        if (/^[\s]*[=+@-]/.test(text)) text = "'" + text;
        return '"' + text.replace(/"/g, '""') + '"';
    };
    const keys = ['label', 'original_value', 'normalized_value', 'model', 'confidence', 'page', 'start_char', 'end_char', 'anchor_method', 'review_status'] as const;
    return [keys.map(cell).join(','), ...fields.map(f => keys.map(k => cell(f[k])).join(','))].join('\r\n');
}
