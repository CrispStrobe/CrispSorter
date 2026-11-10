import { describe, expect, it, vi } from 'vitest';
import { createProofreadingWriter, type ProofreadingSaveStatus } from './proofreadingStore';
import { emptyProofreadingData, createVocabularyEntry, type ProofreadingData } from './proofreadingData';

describe('user proofreading preference writer',()=>{
    it('serializes writes and retains the latest snapshot while an older save is active',async()=>{
        let release!:()=>void;
        const data=emptyProofreadingData('en'),saved:string[][]=[];
        const writer=createProofreadingWriter(async snapshot=>{
            saved.push(snapshot.entries.map(entry=>entry.text));
            if(saved.length===1) await new Promise<void>(resolve=>release=resolve);
        },()=>{});
        data.entries.push(createVocabularyEntry('Invoice','en'));writer.schedule(data);
        const active=writer.flush();
        data.entries.push(createVocabularyEntry('Total','en'));writer.schedule(data);
        data.entries.push(createVocabularyEntry('unscheduled','en'));
        // The shared autosave installs its write lock and starts the first
        // write one microtask later; wait until that write is actually active.
        await vi.waitFor(()=>expect(saved).toHaveLength(1));
        release();expect(await active).toBe(true);
        expect(saved).toEqual([['Invoice'],['Invoice','Total']]);
        expect(writer.hasUnsavedChanges()).toBe(false);
    });
    it('retains failed data and retries it rather than reporting success or saving empty data',async()=>{
        let failed=true;const statuses:ProofreadingSaveStatus[]=[];
        const save=vi.fn(async(_data:ProofreadingData)=>{if(failed)throw new Error('disk unavailable');});
        const writer=createProofreadingWriter(save,status=>statuses.push(status));
        const data=emptyProofreadingData('en');data.entries.push(createVocabularyEntry('Invoice','en'));
        writer.schedule(data);expect(await writer.flush()).toBe(false);
        expect(statuses.at(-1)).toEqual({state:'error',error:'disk unavailable'});
        expect(writer.hasUnsavedChanges()).toBe(true);
        failed=false;expect(await writer.retry()).toBe(true);
        expect(save.mock.calls[0][0]).toEqual(data);expect(save.mock.calls[1][0]).toEqual(data);
        expect(statuses.at(-1)).toEqual({state:'saved',error:null});
    });
    it('rejects invalid updates before invoking persistent storage',async()=>{
        const save=vi.fn(async()=>{}),writer=createProofreadingWriter(save,()=>{});
        expect(()=>writer.schedule({...emptyProofreadingData(),version:2} as never)).toThrow(/version/);
        expect(save).not.toHaveBeenCalled();expect(await writer.close()).toBe(true);
    });
});
