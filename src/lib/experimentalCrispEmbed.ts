import { writable } from 'svelte/store';
import { getSetting, saveSetting } from './store';
export interface ExperimentalConfig { endpoint: string; moedern: boolean; decisions: boolean }
const defaults: ExperimentalConfig = { endpoint: '', moedern: false, decisions: false };
export const experimentalCrispEmbed = writable<ExperimentalConfig>(defaults);
export async function loadExperimentalCrispEmbed() {
    const raw = await getSetting('experimentalCrispEmbed', defaults) as Partial<ExperimentalConfig>;
    const config = { endpoint: typeof raw?.endpoint === 'string' ? raw.endpoint : '', moedern: raw?.moedern === true, decisions: raw?.decisions === true };
    experimentalCrispEmbed.set(config);
    return config;
}
export async function saveExperimentalCrispEmbed(config: ExperimentalConfig) {
    await saveSetting('experimentalCrispEmbed', config);
    experimentalCrispEmbed.set(config);
}
