import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flog } from './log';
import { beginOperation, runOperation } from './operationLog';

vi.mock('./log', () => ({ flog: vi.fn() }));
beforeEach(() => { vi.mocked(flog).mockReset(); });

describe('critical operation receipts', () => {
  it('correlates stages and emits one terminal outcome despite a late completion', () => {
    const operation = beginOperation('ocr.recognition', { engine: 'qwen2vl', bytes: 42 });
    operation.stage('dispatching');
    operation.cancel({ nativeContinues: true });
    operation.complete({ characters: 100 });
    operation.fail(new Error('late native failure'));
    const messages = vi.mocked(flog).mock.calls.map(([, message]) => message);
    expect(messages).toHaveLength(3);
    expect(messages.every(message => message.includes(`operation_id=${operation.id}`))).toBe(true);
    expect(messages[1]).toContain('phase=stage stage=dispatching');
    expect(messages[2]).toContain('phase=cancelled');
    expect(messages[2]).toContain('nativeContinues=true');
  });

  it('keeps document/query/config/error payloads and unsafe identifiers out of logs', () => {
    const privateText = 'private document or query';
    const operation = beginOperation('ocr.recognition', {
      engine: 'https://host.invalid/?token=secret', language: privateText,
      bytes: 0, cpuBudget: false, config: { prompt: privateText }, query: privateText,
      path: '/private/document', url: 'https://host.invalid', password: 'secret',
    } as Parameters<typeof beginOperation>[1]);
    operation.fail(new Error(`${privateText} bearer secret https://host.invalid`));
    const messages = vi.mocked(flog).mock.calls.map(([, message]) => message).join('\n');
    expect(messages).toContain('engine=unknown');
    expect(messages).toContain('bytes=0');
    expect(messages).toContain('cpuBudget=false');
    for (const value of [privateText, '/private', 'https:', 'secret', 'prompt=', 'query=']) expect(messages).not.toContain(value);
    expect(vi.mocked(flog).mock.calls.at(-1)?.[0]).toBe('error');
  });

  it('does not mistake an error mentioning cancellation for an actual abort', () => {
    beginOperation('search.export').fail(new Error('Could not read cancelled-file.csv'));
    expect(vi.mocked(flog).mock.calls.at(-1)?.[1]).toContain('phase=failed');
    beginOperation('search.export').fail(new DOMException('Stopped', 'AbortError'));
    expect(vi.mocked(flog).mock.calls.at(-1)?.[1]).toContain('phase=cancelled');
  });

  it('never changes an operation outcome when the log sink throws', () => {
    vi.mocked(flog).mockImplementation(() => { throw new Error('observer failed'); });
    expect(() => { const operation = beginOperation('ocr.checkpoint'); operation.complete({ saved: true }); }).not.toThrow();
  });

  it('retains a successful publication receipt when optional receipt formatting fails', async () => {
    const published = { saved_path: '/private/output.txt', bytes: 12 };
    await expect(runOperation('ocr.export', {}, async () => published,
      () => { throw new Error('Optional response metadata absent'); })).resolves.toBe(published);
    expect(vi.mocked(flog).mock.calls.at(-1)?.[1]).toContain('phase=completed');
  });
});
