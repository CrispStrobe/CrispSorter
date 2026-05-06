import { writable } from 'svelte/store';

/** Verbosity threshold. `silent` = errors only, `info` = the
 *  user-facing default (every successful op), `debug` = adds queue
 *  transitions / worker-pool internals / per-stage timing.
 *
 *  Mutated from Settings (Allgemein -> Log-Verbosität); read by every
 *  frontend log call site via `flog()`. The corresponding Rust-side
 *  filter lives next to `app_log!` in lib.rs.
 */
export type LogLevel = 'silent' | 'error' | 'info' | 'debug';

const LEVEL_RANK: Record<LogLevel, number> = {
    silent: 0,
    error: 1,
    info: 2,
    debug: 3,
};

let _verbosity: LogLevel = 'info';
export function setLogVerbosity(level: LogLevel) {
    _verbosity = level;
}
export function getLogVerbosity(): LogLevel {
    return _verbosity;
}

/** Returns true when a message at `msgLevel` should be shown given
 *  the current verbosity threshold. */
function shouldLog(msgLevel: 'info' | 'warn' | 'error' | 'debug'): boolean {
    if (msgLevel === 'error' || msgLevel === 'warn') {
        // Errors + warnings are gated only by the silent threshold.
        return LEVEL_RANK[_verbosity] >= LEVEL_RANK.error;
    }
    if (msgLevel === 'debug') {
        return LEVEL_RANK[_verbosity] >= LEVEL_RANK.debug;
    }
    // info
    return LEVEL_RANK[_verbosity] >= LEVEL_RANK.info;
}

export interface FrontendLogEntry {
    ts: number;
    level: 'info' | 'warn' | 'error' | 'debug';
    msg: string;
}

function createFrontendLogStore() {
    const { subscribe, update } = writable<FrontendLogEntry[]>([]);
    function push(level: 'info' | 'warn' | 'error' | 'debug', msg: string) {
        const entry: FrontendLogEntry = { ts: Date.now() / 1000, level, msg };
        update(logs => {
            const next = [...logs, entry];
            return next.length > 500 ? next.slice(-500) : next;
        });
    }
    return { subscribe, push };
}

export const frontendLogs = createFrontendLogStore();

export function flog(level: 'info' | 'warn' | 'error' | 'debug', msg: string) {
    if (!shouldLog(level)) return;
    // Map debug -> console.debug; the rest are the matching methods.
    const fn = level === 'debug' ? 'debug' : level;
    // eslint-disable-next-line no-console
    (console as any)[fn](`[app:${level}] ${msg}`);
    frontendLogs.push(level, msg);
}

export function logInfo(msg: string)  { flog('info',  msg); }
export function logWarn(msg: string)  { flog('warn',  msg); }
export function logError(msg: string) { flog('error', msg); }
/** Verbose / debug-only message. Hidden unless verbosity is `debug`.
 *  Use for queue transitions, worker-pool internals, per-stage
 *  timing -- things a user wouldn't want flooding their LogPanel by
 *  default but a developer wants when reproducing a bug. */
export function logDebug(msg: string) { flog('debug', msg); }
