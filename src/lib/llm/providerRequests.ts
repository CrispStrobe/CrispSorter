/** Immutable request inputs keep a delayed secret lookup tied to its recipient. */
export interface ProviderRequestConfig {
    readonly id: string;
    readonly baseUrl: string;
    readonly apiKey: string;
    readonly selectedModel: string;
}

export type ProviderRequestKind = 'models' | 'connection' | 'balance';

export interface ProviderRequest {
    readonly kind: ProviderRequestKind;
    readonly generation: number;
    readonly config: ProviderRequestConfig;
}

export function providerConfigurationMatches(
    expected: ProviderRequestConfig,
    actual: ProviderRequestConfig | undefined,
    includeModel = false,
): boolean {
    return !!actual && expected.id === actual.id
        && expected.baseUrl === actual.baseUrl && expected.apiKey === actual.apiKey
        && (!includeModel || expected.selectedModel === actual.selectedModel);
}

/** Each provider and operation owns its pending request and displayed result. */
export class ProviderRequests {
    private generation = 0;
    private latest = new Map<string, number>();
    private disposed = false;

    begin(kind: ProviderRequestKind, provider: ProviderRequestConfig): ProviderRequest {
        const config = Object.freeze({
            id: provider.id, baseUrl: provider.baseUrl,
            apiKey: provider.apiKey, selectedModel: provider.selectedModel,
        });
        const request = Object.freeze({ kind, generation: ++this.generation, config });
        this.latest.set(this.key(request), request.generation);
        return request;
    }

    isCurrent(request: ProviderRequest, provider: ProviderRequestConfig | undefined): boolean {
        return !this.disposed
            && this.latest.get(this.key(request)) === request.generation
            && providerConfigurationMatches(request.config, provider, request.kind === 'connection');
    }

    invalidate(providerId: string, kind?: ProviderRequestKind): void {
        for (const operation of kind ? [kind] : ['models', 'connection', 'balance']) {
            this.latest.delete(`${operation}:${providerId}`);
        }
    }

    async run<T>(
        request: ProviderRequest,
        currentProvider: () => ProviderRequestConfig | undefined,
        resolveKey: (reference: string) => Promise<string>,
        operation: (config: ProviderRequestConfig, key: string) => Promise<T>,
    ): Promise<{ current: true; value: T } | { current: false }> {
        if (!this.isCurrent(request, currentProvider())) return { current: false };
        const key = await resolveKey(request.config.apiKey);
        // An edited endpoint or credential invalidates the operation before
        // network dispatch. A panel switch alone does not change its recipient.
        if (!this.isCurrent(request, currentProvider())) return { current: false };
        const value = await operation(request.config, key);
        return this.isCurrent(request, currentProvider())
            ? { current: true, value } : { current: false };
    }

    dispose(): void {
        this.disposed = true;
        this.latest.clear();
    }

    private key(request: ProviderRequest): string {
        return `${request.kind}:${request.config.id}`;
    }
}

/** Native IPC can reject with strings or records instead of Error objects. */
export function providerErrorMessage(error: unknown, fallback: string): string {
    let message: unknown = error instanceof Error ? error.message : error;
    if (message && typeof message === 'object' && 'message' in message) message = message.message;
    if (typeof message === 'string') return message.trim() || fallback;
    try {
        const detail = message == null ? '' : JSON.stringify(message);
        return detail && detail !== '{}' ? detail : fallback;
    } catch {
        return fallback;
    }
}
