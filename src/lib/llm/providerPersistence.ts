import { llmProviderAccount, makeSentinel, sentinelAccount } from '../secrets';

interface ProviderKeyDraft {
    id: string;
    apiKey: string;
}

/** A partial keychain write must never fall back to plaintext settings. */
export async function prepareProviderSecrets<T extends ProviderKeyDraft>(
    submitted: readonly T[],
    store: (items: Array<[string, string]>) => Promise<string[]>,
): Promise<{ providers: T[]; migrated: number }> {
    const snapshot = submitted.map(provider => ({ ...provider }));
    const items: Array<[string, string]> = [];
    for (const provider of snapshot) {
        const key = provider.apiKey;
        if (typeof key !== 'string') throw new Error(`Invalid API key setting for ${provider.id}.`);
        if (!key || sentinelAccount(key) || ['ollama', 'no-key', 'local'].includes(key)) continue;
        items.push([llmProviderAccount(provider.id), key]);
    }
    if (items.length === 0) return { providers: snapshot, migrated: 0 };
    const stored = new Set(await store(items));
    const missing = items.filter(([account]) => !stored.has(account)).map(([account]) => account);
    if (missing.length) {
        throw new Error(`Secret storage did not save ${missing.join(', ')}. Provider settings were not written. Choose or unlock a secret store, then retry.`);
    }
    const migrated = new Set(items.map(([account]) => account));
    return {
        providers: snapshot.map(provider => migrated.has(llmProviderAccount(provider.id))
            ? { ...provider, apiKey: makeSentinel(llmProviderAccount(provider.id)) } : provider),
        migrated: items.length,
    };
}

/** Apply only a committed key reference; retain edits made during the save. */
export function reconcileProviderSecrets<T extends ProviderKeyDraft>(
    current: readonly T[], submitted: readonly T[], stored: readonly T[],
): T[] {
    return current.map(provider => {
        const before = submitted.find(candidate => candidate.id === provider.id);
        const after = stored.find(candidate => candidate.id === provider.id);
        return before && after && provider.apiKey === before.apiKey && after.apiKey !== before.apiKey
            ? { ...provider, apiKey: after.apiKey } : provider;
    });
}

/** Serialize vault, settings, and runtime-key updates, including retries. */
export class ProviderSaveQueue {
    private tail: Promise<void> = Promise.resolve();

    run<T>(operation: () => Promise<T>): Promise<T> {
        const result = this.tail.then(operation);
        this.tail = result.then(() => undefined, () => undefined);
        return result;
    }
}
