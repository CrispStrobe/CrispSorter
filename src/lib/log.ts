import { writable } from 'svelte/store';

export interface FrontendLogEntry {
    ts: number;
    level: 'info' | 'warn' | 'error';
    msg: string;
}

function createFrontendLogStore() {
    const { subscribe, update } = writable<FrontendLogEntry[]>([]);
    function push(level: 'info' | 'warn' | 'error', msg: string) {
        const entry: FrontendLogEntry = { ts: Date.now() / 1000, level, msg };
        update(logs => {
            const next = [...logs, entry];
            return next.length > 500 ? next.slice(-500) : next;
        });
    }
    return { subscribe, push };
}

export const frontendLogs = createFrontendLogStore();

export function flog(level: 'info' | 'warn' | 'error', msg: string) {
    console[level](`[app:${level}] ${msg}`);
    frontendLogs.push(level, msg);
}

export function logInfo(msg: string)  { flog('info',  msg); }
export function logWarn(msg: string)  { flog('warn',  msg); }
export function logError(msg: string) { flog('error', msg); }
