import { beginOperation, type OperationFields, type OperationLog, type OperationName } from '../../operationLog';

export function cancelledDocumentAction(): Error {
    const error = new Error('The document changed before the action finished.');
    error.name = 'AbortError';
    return error;
}

export function requireCurrentDocument(isCurrent: () => boolean): void {
    if (!isCurrent()) throw cancelledDocumentAction();
}

interface ActionOptions {
    operation: OperationName;
    fields?: OperationFields;
    isCurrent?: () => boolean;
    /** Opening a replacement document and capability probes may supersede work. */
    concurrent?: boolean;
    /** A successful native write remains successful even if its view was replaced. */
    writesOutput?: boolean;
    onError?: (error: unknown) => void;
}

/** One shared busy boundary and one lifecycle receipt per document action.
 * The owner supplies its actual document identity; obsolete results never
 * escape to a newly opened view. No paths or raw errors enter the log. */
export function createDocumentActions(onPending: (count: number) => void) {
    let pending = 0;
    let idleWaiters: Array<() => void> = [];
    return {
        get pending() { return pending; },
        whenIdle(): Promise<void> {
            return pending === 0 ? Promise.resolve() : new Promise((resolve) => { idleWaiters.push(resolve); });
        },
        async run<T>(options: ActionOptions, task: (operation: OperationLog) => Promise<T>): Promise<T | undefined> {
            if (pending > 0 && !options.concurrent) return undefined;
            const isCurrent = options.isCurrent ?? (() => true);
            const operation = beginOperation(options.operation, options.fields);
            if (!isCurrent()) { operation.cancel(); return undefined; }
            onPending(++pending);
            try {
                const result = await task(operation);
                if (!isCurrent()) {
                    if (options.writesOutput) operation.complete({ saved: true });
                    else operation.cancel();
                    return undefined;
                }
                operation.complete(options.writesOutput ? { saved: true } : {});
                return result;
            } catch (error) {
                operation.fail(error);
                if (isCurrent() && !(error instanceof Error && error.name === 'AbortError')) options.onError?.(error);
                return undefined;
            } finally {
                onPending(--pending);
                if (pending === 0) {
                    const waiters = idleWaiters;
                    idleWaiters = [];
                    for (const resolve of waiters) resolve();
                }
            }
        },
    };
}
