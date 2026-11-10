import { ProjectAutosave } from './autosave';
import { readProofreadingData, type ProofreadingData } from './proofreadingData';

export interface ProofreadingSaveStatus {
    state: 'saved'|'pending'|'saving'|'error'; error: string|null;
}

/** A single serialized writer keeps the latest preference edit through failures. */
export function createProofreadingWriter(save: (data: ProofreadingData) => Promise<void>, onStatus: (status: ProofreadingSaveStatus) => void) {
    const writer = new ProjectAutosave<ProofreadingData>(async (data, revision) => {
        await save(data);
        return {revision:revision+1,saved_at:Date.now(),path:'ocrProofreadingV1'};
    }, status => onStatus({state:status.state,error:status.error ?? null}), 0, 250);
    const flush = async (): Promise<boolean> => {
        try { await writer.flush(); return true; } catch { return false; }
    };
    return {
        schedule(data: ProofreadingData): void { writer.schedule(readProofreadingData(data)); },
        flush,
        retry:flush,
        hasUnsavedChanges:():boolean => writer.hasUnsavedChanges(),
        close:async ():Promise<boolean> => { try { await writer.close(); return true; } catch { return false; } },
    };
}
