import { mockIPC } from '@tauri-apps/api/mocks';
import { ensureThirdPartyConsent, ThirdPartyAiConsentRequired } from '../../src/lib/llm/thirdPartyConsent';

interface OverlayControl {
    outcome: 'idle' | 'pending' | 'allowed' | 'declined' | 'error';
    error: string;
    saveCount: number;
    storedConsent(): string[];
    request(providerId: string, endpoint: string): void;
}

declare global {
    interface Window { overlaySmoke: OverlayControl; }
}

/** Mock only the persistent-store IPC boundary; mount the real disclosure and
 * run the real consent gate. No model request or network transmission occurs. */
export function installOverlayConsent(): void {
    const settings = new Map<string, unknown>();
    const control: OverlayControl = {
        outcome: 'idle', error: '', saveCount: 0,
        storedConsent: () => structuredClone(settings.get('thirdPartyAiConsent') as string[] ?? []),
        request(providerId, endpoint) {
            control.outcome = 'pending'; control.error = '';
            void ensureThirdPartyConsent(providerId, endpoint).then(
                () => { control.outcome = 'allowed'; },
                error => {
                    control.outcome = error instanceof ThirdPartyAiConsentRequired ? 'declined' : 'error';
                    control.error = String(error);
                },
            );
        },
    };
    mockIPC((command, args) => {
        if (command === 'plugin:store|load') return 1;
        const payload = args as Record<string, unknown> | undefined;
        const key = String(payload?.key ?? '');
        if (command === 'plugin:store|get') return [settings.get(key), settings.has(key)];
        if (command === 'plugin:store|set') { settings.set(key, structuredClone(payload?.value)); return; }
        if (command === 'plugin:store|save') { control.saveCount++; return; }
        throw new Error(`Unexpected overlay fixture IPC: ${command}`);
    });
    window.overlaySmoke = control;
}
