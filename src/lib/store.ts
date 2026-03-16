import { load } from '@tauri-apps/plugin-store';

const STORE_PATH = 'settings.json';

export async function getStore() {
    return await load(STORE_PATH, { defaults: {}, autoSave: true });
}

export async function saveSetting(key: string, value: any) {
    const store = await getStore();
    await store.set(key, value);
    await store.save();
}

export async function getSetting(key: string, defaultValue: any = null) {
    const store = await getStore();
    const val = await store.get(key);
    return val !== undefined ? val : defaultValue;
}
