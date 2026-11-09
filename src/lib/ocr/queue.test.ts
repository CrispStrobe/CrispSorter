import { describe, expect, it, vi } from 'vitest';
import { SequentialQueue, type QueueState } from './queue';

function gate<T>() {
    let resolve!: (value: T) => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<T>((yes, no) => { resolve=yes; reject=no; });
    return { promise, resolve, reject };
}
// The processing segment defers startup by a microtask, then awaits onStart.
async function started() { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); }

describe('sequential OCR queue', () => {
    it('claims a run synchronously and serializes even asynchronous result handlers', async () => {
        const resultGate=gate<void>();
        let active=0,maxActive=0;
        const events:string[]=[];
        const queue=new SequentialQueue<number>({
            onStart:job=>{events.push(`start:${job.id}`);},
            onResult:async(job,result)=>{events.push(`result:${job.id}:${result}`);if(job.id==='one')await resultGate.promise;},
        });
        const run=(id:number)=>async()=>{active++;maxActive=Math.max(maxActive,active);await Promise.resolve();active--;return id;};
        expect(queue.start([{id:'one',run:run(1)},{id:'two',run:run(2)}])).toBe(true);
        expect(queue.canStart).toBe(false);
        expect(queue.start([{id:'third',run:run(3)}])).toBe(false);
        await started();await started();
        expect(events).toEqual(['start:one','result:one:1']);
        resultGate.resolve();
        await queue.whenSettled();
        expect(events).toEqual(['start:one','result:one:1','start:two','result:two:2']);
        expect(maxActive).toBe(1);
        expect(queue.state).toEqual({phase:'completed',total:2,completed:2,failed:0,currentId:null,remainingIds:[]});
        expect(queue.canStart).toBe(true);
    });

    it('pauses after the active page, retains pending jobs, and resumes the same run', async () => {
        const first=gate<number>();
        const second=vi.fn(async()=>2);
        const result=vi.fn();
        const queue=new SequentialQueue<number>({onResult:result});
        queue.start([{id:'one',run:()=>first.promise},{id:'two',run:second}]);
        await started();
        queue.pause();
        expect(queue.state.phase).toBe('pausing');
        expect(queue.state.currentId).toBe('one');
        first.resolve(1);
        await queue.whenSettled();
        expect(queue.state).toEqual({phase:'paused',total:2,completed:1,failed:0,currentId:null,remainingIds:['two']});
        expect(result).toHaveBeenCalledTimes(1);
        expect(second).not.toHaveBeenCalled();
        expect(queue.start([])).toBe(false);
        expect(queue.resume()).toBe(true);
        await queue.whenSettled();
        expect(second).toHaveBeenCalledTimes(1);
        expect(queue.state.phase).toBe('completed');
        expect(queue.state.completed).toBe(2);
    });

    it('cancels between pages and applies the in-flight result without losing pending IDs', async () => {
        const first=gate<string>();
        const result=vi.fn();
        const second=vi.fn(async()=>'second');
        const queue=new SequentialQueue<string>({onResult:result});
        queue.start([{id:'one',run:()=>first.promise},{id:'two',run:second}]);
        await started();
        queue.cancel();
        expect(queue.state.phase).toBe('cancelling');
        expect(queue.canStart).toBe(false);
        first.resolve('first');
        await queue.whenSettled();
        expect(queue.state.phase).toBe('cancelled');
        expect(queue.state.remainingIds).toEqual(['two']);
        expect(queue.state.completed).toBe(1);
        expect(second).not.toHaveBeenCalled();
        expect(result).toHaveBeenCalledWith(expect.objectContaining({id:'one'}),'first');
        expect(queue.resume()).toBe(false);
        expect(queue.start([{id:'two',run:second}])).toBe(true);
        await queue.whenSettled();
        expect(queue.state.completed).toBe(1);
    });

    it('reports individual failures and processes subsequent pages', async () => {
        const errors=vi.fn();
        const results=vi.fn();
        const queue=new SequentialQueue<number>({onResult:results,onError:errors});
        queue.start([
            {id:'bad',run:async()=>{throw new Error('Unreadable page');}},
            {id:'good',run:async()=>42},
        ]);
        await queue.whenSettled();
        expect(errors).toHaveBeenCalledWith(expect.objectContaining({id:'bad'}),expect.objectContaining({message:'Unreadable page'}));
        expect(results).toHaveBeenCalledWith(expect.objectContaining({id:'good'}),42);
        expect(queue.state.completed).toBe(1);
        expect(queue.state.failed).toBe(1);
        expect(queue.state.phase).toBe('completed');
    });

    it('invalidates obsolete results while retaining the native-call lock until they settle', async () => {
        const first=gate<number>();
        const result=vi.fn();
        const errors=vi.fn();
        const second=vi.fn(async()=>2);
        const queue=new SequentialQueue<number>({onResult:result,onError:errors});
        queue.start([{id:'old',run:()=>first.promise},{id:'pending',run:second}]);
        await started();
        queue.invalidate();
        expect(queue.start([{id:'new',run:second}])).toBe(false);
        first.resolve(1);
        await queue.whenSettled();
        expect(result).not.toHaveBeenCalled();
        expect(errors).not.toHaveBeenCalled();
        expect(second).not.toHaveBeenCalled();
        expect(queue.state.remainingIds).toEqual(['old','pending']);
        expect(queue.state.phase).toBe('cancelled');
        expect(queue.start([{id:'new',run:second}])).toBe(true);
        await queue.whenSettled();
        expect(result).toHaveBeenCalledOnce();
        expect(result).toHaveBeenCalledWith(expect.objectContaining({id:'new'}),2);
    });

    it('suppresses obsolete errors too and rechecks page identity before applying results', async () => {
        const first=gate<number>();
        let current=true;
        const result=vi.fn(),error=vi.fn();
        const queue=new SequentialQueue<number>({isCurrent:()=>current,onResult:result,onError:error});
        queue.start([{id:'one',run:()=>first.promise}]);
        await started();
        current=false;
        first.reject(new Error('An obsolete request failed'));
        await queue.whenSettled();
        expect(result).not.toHaveBeenCalled();
        expect(error).not.toHaveBeenCalled();
        expect(queue.state.failed).toBe(0);
        expect(queue.state.remainingIds).toEqual(['one']);
    });

    it('respects synchronous start/resume gates and does not invoke an obsolete queued page', async () => {
        let allowed=false;
        const run=vi.fn(async()=>1);
        const queue=new SequentialQueue<number>({canStart:()=>allowed,isCurrent:()=>false});
        expect(queue.canStart).toBe(false);
        expect(queue.start([{id:'one',run}])).toBe(false);
        expect(queue.state.phase).toBe('idle');
        allowed=true;
        expect(queue.start([{id:'one',run}])).toBe(true);
        await queue.whenSettled();
        expect(run).not.toHaveBeenCalled();
        expect(queue.state.phase).toBe('cancelled');
        expect(queue.state.remainingIds).toEqual(['one']);
    });

    it('can pause before the first native call, then cancel without changing any page', async () => {
        const run=vi.fn(async()=>1);
        const queue=new SequentialQueue<number>();
        queue.start([{id:'one',run}]);
        queue.pause();
        await queue.whenSettled();
        expect(run).not.toHaveBeenCalled();
        expect(queue.state.phase).toBe('paused');
        queue.cancel();
        expect(queue.state.phase).toBe('cancelled');
        expect(queue.state.remainingIds).toEqual(['one']);
        expect(queue.canStart).toBe(true);
    });

    it('stops after a result checkpoint fails without misreporting successful recognition', async () => {
        const applied:number[]=[];
        const errors=vi.fn();
        const second=vi.fn(async()=>2);
        const checkpointError=vi.fn();
        const queue=new SequentialQueue<number>({
            onState:()=>{throw new Error('Cannot render state');},
            onResult:(_job,value)=>{applied.push(value);throw new Error('Disk full');},
            onError:errors,onCheckpointError:checkpointError,
        });
        queue.start([{id:'one',run:async()=>1},{id:'two',run:second}]);
        await queue.whenSettled();
        expect(applied).toEqual([1]);
        expect(errors).not.toHaveBeenCalled();
        expect(second).not.toHaveBeenCalled();
        expect(checkpointError).toHaveBeenCalledWith(expect.objectContaining({message:'Disk full'}),expect.objectContaining({id:'one'}),'result');
        expect(queue.state).toEqual({phase:'cancelled',total:2,completed:0,failed:0,currentId:null,remainingIds:['one','two']});
        expect(queue.canStart).toBe(true);
    });

    it('never starts native work when its durable start checkpoint fails', async () => {
        const first=vi.fn(async()=>1),second=vi.fn(async()=>2),errors=vi.fn();
        const checkpointError=vi.fn(()=>{throw new Error('Detached UI');});
        const queue=new SequentialQueue<number>({onStart:()=>{throw new Error('Revision conflict');},onError:errors,onCheckpointError:checkpointError});
        queue.start([{id:'one',run:first},{id:'two',run:second}]);
        await queue.whenSettled();
        expect(first).not.toHaveBeenCalled();
        expect(second).not.toHaveBeenCalled();
        expect(errors).not.toHaveBeenCalled();
        expect(checkpointError).toHaveBeenCalledWith(expect.objectContaining({message:'Revision conflict'}),expect.objectContaining({id:'one'}),'start');
        expect(queue.state.remainingIds).toEqual(['one','two']);
        expect(queue.state.phase).toBe('cancelled');
    });

    it('suppresses a late checkpoint failure after the project generation changes', async () => {
        const saving=gate<void>(),run=vi.fn(async()=>1),checkpointError=vi.fn();
        const queue=new SequentialQueue<number>({onStart:()=>saving.promise,onCheckpointError:checkpointError});
        queue.start([{id:'obsolete-page',run}]);
        await started();
        queue.invalidate();
        expect(queue.canStart).toBe(false);
        saving.reject(new Error('The old project could not save'));
        await queue.whenSettled();
        expect(run).not.toHaveBeenCalled();
        expect(checkpointError).not.toHaveBeenCalled();
        expect(queue.state.remainingIds).toEqual(['obsolete-page']);
        expect(queue.state.phase).toBe('cancelled');
    });

    it('stops when saving a recognition failure fails instead of losing its retry evidence', async () => {
        const second=vi.fn(async()=>2),checkpointError=vi.fn();
        const queue=new SequentialQueue<number>({onError:()=>{throw new Error('Storage unavailable');},onCheckpointError:checkpointError});
        queue.start([{id:'one',run:async()=>{throw new Error('Unreadable image');}},{id:'two',run:second}]);
        await queue.whenSettled();
        expect(second).not.toHaveBeenCalled();
        expect(queue.state.failed).toBe(1);
        expect(queue.state.remainingIds).toEqual(['one','two']);
        expect(checkpointError).toHaveBeenCalledWith(expect.objectContaining({message:'Storage unavailable'}),expect.objectContaining({id:'one'}),'error');
    });

    it.each(['pause','cancel'] as const)('honors %s while the start checkpoint is in flight', async action => {
        const checkpoint=gate<void>(),run=vi.fn(async()=>1);
        const queue=new SequentialQueue<number>({onStart:()=>checkpoint.promise});
        queue.start([{id:'one',run}]);
        await started();
        queue[action]();
        checkpoint.resolve();
        await queue.whenSettled();
        expect(run).not.toHaveBeenCalled();
        expect(queue.state.remainingIds).toEqual(['one']);
        expect(queue.state.phase).toBe(action==='pause'?'paused':'cancelled');
        if(action==='pause') {
            expect(queue.resume()).toBe(true);
            await queue.whenSettled();
            expect(run).toHaveBeenCalledTimes(1);
        }
    });

    it('does not expose mutable internal state or allow synchronous observer reentrancy', async () => {
        let queue!:SequentialQueue<number>;
        const reentrant:boolean[]=[];
        queue=new SequentialQueue<number>({onState:(state)=>{
            if(state.phase==='running') reentrant.push(queue.start([]));
            (state as QueueState).remainingIds.length=0;
        }});
        queue.start([{id:'one',run:async()=>1}]);
        expect(queue.state.remainingIds).toEqual(['one']);
        await queue.whenSettled();
        expect(reentrant.every(result=>result===false)).toBe(true);
        expect(queue.state.completed).toBe(1);
    });

    it('rejects duplicate job IDs without claiming a run and finishes an empty batch', async () => {
        const queue=new SequentialQueue<number>();
        const job={id:'one',run:async()=>1};
        expect(()=>queue.start([job,job])).toThrow(/unique/);
        expect(queue.canStart).toBe(true);
        expect(queue.start([])).toBe(true);
        await queue.whenSettled();
        expect(queue.state).toEqual({phase:'completed',total:0,completed:0,failed:0,currentId:null,remainingIds:[]});
    });
});
