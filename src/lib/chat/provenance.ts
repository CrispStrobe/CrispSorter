/** Application-authored origin records, not signed credentials or a text watermark. */
export interface GeneratedChatAnswer {
    text: string;
    provenance: {
        schema_version: 1;
        application: 'CrispSorter';
        origin: 'ai-generated';
        operation: 'chat';
        generated_at: string;
        provider: string;
        provider_kind: 'configured-connection';
        requested_model?: string;
    };
}

/** Local model paths and URL credentials do not belong in a portable record. */
function portableModel(model: string): string | undefined {
    const value = model.trim();
    return value && !/^(?:[~\\/]|[a-z]:[\\/]|[a-z][a-z\d+.-]*:)/i.test(value)
        ? value : undefined;
}

export function recordChatAnswer(text: string, provider: string, model: string, generatedAt = new Date().toISOString()): GeneratedChatAnswer {
    const modelId = ['llamacpp', 'mlx', 'mistralrs', 'ort', 'webllm'].includes(provider) ? undefined : portableModel(model);
    return {
        text,
        provenance: {
            schema_version: 1, application: 'CrispSorter', origin: 'ai-generated',
            operation: 'chat', generated_at: generatedAt, provider, provider_kind: 'configured-connection',
            ...(modelId ? { requested_model: modelId } : {}),
        },
    };
}

/** Explicitly marked copy; the answer itself stays untouched in the app. */
export function markedChatCopy(answer: GeneratedChatAnswer): string {
    return `${answer.text}\n\n---\nAI-generated with CrispSorter. Review for accuracy.\n`;
}

/** Keep each assertion next to its actual output; do not include prompts or document paths. */
export function exportChatAnswers(answers: readonly GeneratedChatAnswer[]): string {
    return JSON.stringify({
        schema: 'crisp-ai-output-record', schema_version: 1,
        application: 'CrispSorter',
        marking: 'application-authored provenance; not a watermark or signed credential',
        outputs: answers.map(({ text, provenance }) => ({ text, provenance })),
    }, null, 2);
}
