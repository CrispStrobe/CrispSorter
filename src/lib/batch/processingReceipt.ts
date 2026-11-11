import type { OperationFields, OperationLog } from '../operationLog';

/** The last checkpoint owns completion. Always release controls after it
 * settles, and preserve an earlier processing error if both stages failed. */
export async function finishProcessingCheckpoint(options: {
    persist: () => Promise<unknown>;
    release: () => void;
    operation: OperationLog;
    failure?: { error: unknown };
    cancelled: boolean;
    counts: OperationFields;
}): Promise<void> {
    const { operation, counts, failure } = options;
    try {
        operation.stage('saving', counts);
        await options.persist();
        const receipt = { ...counts, saved: true };
        if (failure) operation.fail(failure.error, receipt);
        else if (options.cancelled) operation.cancel(receipt);
        else operation.complete({ ...receipt, partial: (counts.failed ?? 0) > 0 });
    } catch (error) {
        operation.stage('checkpoint_failed', { ...counts, saved: false }, 'warn');
        operation.fail(error, { ...counts, saved: false });
        if (!failure) throw error;
    } finally {
        options.release();
    }
}
