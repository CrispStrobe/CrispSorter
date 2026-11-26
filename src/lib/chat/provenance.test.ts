import { describe, expect, it } from 'vitest';
import { exportChatAnswers, markedChatCopy, recordChatAnswer } from './provenance';

describe('portable chat output provenance', () => {
    it('binds each record to its original answer without altering JSON, XML or math', () => {
        const text = '{"title":"A & B"}\n<METADATA>one</METADATA>\n$x^2$';
        const answer = recordChatAnswer(text, 'nebius', 'Qwen/model', '2026-11-26T10:00:00.000Z');
        const exported = JSON.parse(exportChatAnswers([answer]));
        expect(exported.outputs[0].text).toBe(text);
        expect(exported.outputs[0].provenance).toMatchObject({
            origin: 'ai-generated', operation: 'chat', provider: 'nebius', requested_model: 'Qwen/model',
        });
        expect(markedChatCopy(answer)).toBe(`${text}\n\n---\nAI-generated with CrispSorter. Review for accuracy.\n`);
        expect(answer.text).toBe(text);
    });

    it('omits local paths and URL model identifiers from shared records', () => {
        for (const model of ['/Users/owner/models/private.gguf', 'C:\\Users\\owner\\model.gguf', '~/models/m.gguf', 'file:///private/m.gguf', 'https://user:secret@host/model']) {
            expect(recordChatAnswer('answer', 'llamacpp', model).provenance.requested_model).toBeUndefined();
        }
        expect(recordChatAnswer('answer', 'nebius', 'Qwen/model').provenance.requested_model).toBe('Qwen/model');
    });

    it('does not infer generated answers from arbitrary chat messages or reuse the current provider', () => {
        expect(JSON.parse(exportChatAnswers([])).outputs).toEqual([]);
        const first = recordChatAnswer('First', 'ollama', 'local-model');
        const second = recordChatAnswer('Second', 'nebius', 'remote-model');
        const outputs = JSON.parse(exportChatAnswers([first, second])).outputs;
        expect(outputs.map((output: GeneratedOutput) => output.provenance.provider)).toEqual(['ollama', 'nebius']);
        expect(JSON.stringify(outputs)).not.toMatch(/apiKey|prompt|locationUri/);
    });
});

interface GeneratedOutput { provenance: { provider: string } }
