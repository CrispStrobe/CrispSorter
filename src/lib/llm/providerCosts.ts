import { fetch } from '@tauri-apps/plugin-http';
import type { ProviderRequestConfig } from './providerRequests';

export const COST_REPORT_PROVIDERS: readonly string[] = ['anthropic', 'openai'];
export const costKeyAccount = (id: string): string => `llm-provider-admin:${id}`;
const usd = (amount: number): string => `$${amount.toFixed(2)}`;
const apiError = (body: any, status: number): string => body?.error?.message || body?.message || `HTTP ${status}`;

/** A custom compatible endpoint's ordinary key belongs to that endpoint.
 *  Only a separately saved admin key may query the original provider there. */
export async function resolveProviderReportKey(
    config: ProviderRequestConfig,
    readAdmin: (account: string) => Promise<string | null>,
    resolveKey: (reference: string) => Promise<string>,
): Promise<{ key: string; admin: boolean }> {
    if (COST_REPORT_PROVIDERS.includes(config.id)) {
        const admin = await readAdmin(costKeyAccount(config.id));
        if (admin) return { key: admin, admin: true };
        const expectedOrigin = config.id === 'anthropic' ? 'https://api.anthropic.com' : 'https://api.openai.com';
        let origin = '';
        try { origin = new URL(config.baseUrl).origin; } catch { /* Reject an absent or malformed endpoint. */ }
        if (origin !== expectedOrigin) {
            throw new Error('Cost reports for a custom endpoint require a separate provider admin key.');
        }
    } else if (config.id !== 'openrouter') {
        throw new Error('This provider does not offer an account report.');
    }
    const key = await resolveKey(config.apiKey);
    if (!key) throw new Error('No API key is saved for this provider.');
    return { key, admin: false };
}

function amount(value: unknown): number {
    if ((typeof value !== 'number' && typeof value !== 'string')
        || (typeof value === 'string' && !value.trim()) || !Number.isFinite(Number(value))) {
        throw new Error('The provider returned an invalid cost amount; no total was calculated.');
    }
    return Number(value);
}

/** Complete paginated reports retain the captured key and stop if superseded.
 *  An incomplete page chain must never be presented as a monthly total. */
export async function fetchProviderAccountReport(
    config: ProviderRequestConfig, key: string, assertCurrent: () => void = () => {}, now = new Date(),
): Promise<string> {
    if (!key) throw new Error('No API key is saved for this provider.');
    if (config.id === 'openrouter') {
        const base = config.baseUrl.replace(/\/+$/, '');
        if (!base) throw new Error('Base URL for OpenRouter is not configured.');
        assertCurrent();
        const response = await fetch(`${base}/credits`, { headers: { Authorization: `Bearer ${key}` } });
        const body = await response.json().catch(() => null) as any;
        assertCurrent();
        if (!response.ok) throw new Error(apiError(body, response.status));
        const total = amount(body?.data?.total_credits), used = amount(body?.data?.total_usage);
        return `${usd(total - used)} left of ${usd(total)} purchased (${usd(used)} used)`;
    }
    if (!COST_REPORT_PROVIDERS.includes(config.id)) throw new Error('This provider does not offer an account report.');
    const anthropic = config.id === 'anthropic';
    const start = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1));
    const end = new Date(now); end.setUTCHours(24, 0, 0, 0);
    const timestamp = (date: Date) => date.toISOString().replace(/\.\d{3}Z$/, 'Z');
    const base = anthropic
        ? `https://api.anthropic.com/v1/organizations/cost_report?starting_at=${timestamp(start)}&ending_at=${timestamp(end)}`
        : `https://api.openai.com/v1/organization/costs?start_time=${Math.floor(start.getTime() / 1000)}&limit=31`;
    const headers: Record<string, string> = anthropic
        ? { 'x-api-key': key, 'anthropic-version': '2023-06-01' }
        : { Authorization: `Bearer ${key}` };
    const seen = new Set<string>();
    let total = 0, page: string | null = null;
    for (let guard = 0; guard < 20; guard++) {
        assertCurrent();
        const response = await fetch(base + (page ? `&page=${encodeURIComponent(page)}` : ''), { headers });
        const body = await response.json().catch(() => null) as any;
        assertCurrent();
        if (!response.ok) throw new Error(apiError(body, response.status));
        if (!Array.isArray(body?.data)) throw new Error('The provider returned an invalid cost report.');
        for (const bucket of body.data) {
            if (!Array.isArray(bucket?.results)) throw new Error('The provider returned an invalid cost bucket.');
            for (const row of bucket.results) total += amount(anthropic ? row?.amount : row?.amount?.value);
        }
        if (!Number.isFinite(total)) throw new Error('The provider cost total is out of range.');
        if (!body.has_more) {
            return `${usd(anthropic ? total / 100 : total)} spent this month (${anthropic ? 'Anthropic' : 'OpenAI'} does not report a remaining balance)`;
        }
        if (typeof body.next_page !== 'string' || !body.next_page || seen.has(body.next_page)) {
            throw new Error('The provider returned an incomplete cost report with an invalid page cursor.');
        }
        seen.add(body.next_page);
        page = body.next_page;
    }
    throw new Error('Cost report pagination exceeded its safety limit; no partial total was displayed.');
}
