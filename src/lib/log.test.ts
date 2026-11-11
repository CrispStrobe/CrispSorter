import { beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { flog, frontendLogs, setLogVerbosity } from './log';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(), isTauri: vi.fn().mockReturnValue(false) }));
beforeEach(() => {
  vi.mocked(isTauri).mockReturnValue(false);
  vi.mocked(invoke).mockReset().mockResolvedValue(undefined);
  for (const method of ['info', 'warn', 'error', 'debug'] as const) vi.spyOn(console, method).mockImplementation(() => {});
});

describe('app log severity and native synchronization', () => {
  it.each([
    ['silent', ['error']], ['error', ['warn', 'error']],
    ['info', ['info', 'warn', 'error']], ['debug', ['info', 'warn', 'error', 'debug']],
  ] as const)('keeps the advertised levels at %s', async (verbosity, expected) => {
    setLogVerbosity(verbosity);
    const prefix = `threshold-${verbosity}-`;
    for (const level of ['info', 'warn', 'error', 'debug'] as const) flog(level, `${prefix}${level}`);
    expect(get(frontendLogs).filter(entry => entry.msg.startsWith(prefix)).map(entry => entry.level)).toEqual(expected);
    await Promise.resolve();
  });

  it('serializes native threshold changes without forwarding frontend log entries', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    setLogVerbosity('error'); setLogVerbosity('debug');
    await vi.waitFor(() => expect(vi.mocked(invoke).mock.calls.slice(-2)).toEqual([
      ['set_log_verbosity', { level: 'error' }], ['set_log_verbosity', { level: 'debug' }],
    ]));
    flog('info', 'one frontend receipt');
    expect(vi.mocked(invoke).mock.calls.every(([command]) => command === 'set_log_verbosity')).toBe(true);
  });
});
