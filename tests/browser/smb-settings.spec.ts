import { expect, test } from '@playwright/test';
import { fixtures } from './fixtures';

test('SMB drive form saves and reloads mounted and Rust choices through IPC', async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('.batch-container .file-name')).toHaveText('Pending scan.png');
    await page.evaluate(() => {
        const root = window as any;
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__smbWrites = [];
        root.__smbDrives = [];
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
            if (command === 'drive_native_support') return {filen: false, internxt: false};
            if (command === 'drive_list') return structuredClone(root.__smbDrives);
            if (command === 'drive_credentials_status') return {has_username: true, has_password: true};
            if (command === 'drive_create' || command === 'drive_update') {
                if (command === 'drive_update' && JSON.stringify(args.expected) !== JSON.stringify(root.__smbDrives[0])) {
                    throw new Error('Drive settings changed; reload before saving');
                }
                root.__smbWrites.push(structuredClone(args));
                const saved = {id: args.id ?? 'nas', label: args.label, kind: args.kind, path: args.path, smb: args.smb};
                root.__smbDrives = [saved];
                return saved;
            }
            return original(command, args);
        };
    });
    await page.getByRole('button', {name: 'Library', exact: true}).first().click();
    await page.locator('.tab-bar').getByRole('button', {name: /^Sources/}).click();
    await page.getByRole('button', {name: 'Cloud-Ordner', exact: true}).click();
    const dialog = page.locator('.drive-dialog');
    await expect(dialog).toBeVisible();
    await dialog.getByRole('combobox', {name: 'Typ', exact: true}).selectOption('smb');
    await dialog.getByRole('textbox', {name: 'Label', exact: true}).fill('NAS');
    await dialog.getByRole('textbox', {name: 'SMB URL', exact: true}).fill('smb://nas/books');
    await expect(dialog.getByRole('combobox', {name: 'SMB-Transport', exact: true})).toHaveValue('mounted');
    await dialog.getByRole('button', {name: 'Anlegen', exact: true}).click();
    await expect(dialog).toContainText('Choose the existing SMB mount folder.');
    expect(await page.evaluate(() => (window as any).__smbWrites.length)).toBe(0);
    await dialog.getByRole('textbox', {name: 'SMB-Mount-Ordner', exact: true}).fill('/Volumes/Books');
    await dialog.getByRole('button', {name: 'Anlegen', exact: true}).click();
    await expect.poll(() => page.evaluate(() => (window as any).__smbWrites.length)).toBe(1);
    await page.getByRole('button', {name: 'Cloud-Ordner', exact: true}).click();
    await dialog.getByTitle('Bearbeiten', {exact: true}).click();
    await dialog.getByRole('combobox', {name: 'SMB-Transport', exact: true}).selectOption('rust');
    await expect(dialog.getByRole('checkbox', {name: 'SMB-Verschlüsselung erforderlich (Rust)', exact: true})).toBeEnabled();
    await dialog.getByRole('button', {name: 'Speichern', exact: true}).click();
    await expect.poll(() => page.evaluate(() => (window as any).__smbWrites.length)).toBe(2);
    const writes = await page.evaluate(() => (window as any).__smbWrites);
    expect(writes[0].smb).toEqual({backend: 'mounted', mount_path: '/Volumes/Books', require_encryption: true});
    expect(writes[1]).toMatchObject({id: 'nas', username: null, password: null, smb: {backend: 'rust', mount_path: '/Volumes/Books', require_encryption: true}});
    await page.getByRole('button', {name: 'Cloud-Ordner', exact: true}).click();
    await dialog.getByTitle('Bearbeiten', {exact: true}).click();
    await expect(dialog.getByRole('combobox', {name: 'SMB-Transport', exact: true})).toHaveValue('rust');
    await expect(dialog.getByRole('textbox', {name: 'SMB-Mount-Ordner', exact: true})).toHaveValue('/Volumes/Books');
    // A CLI edit while the form is open must survive the stale GUI submit.
    await page.evaluate(() => { (window as any).__smbDrives[0].label = 'Changed by CLI'; });
    await dialog.getByRole('button', {name: 'Speichern', exact: true}).click();
    await expect(dialog).toContainText('Drive settings changed; reload before saving');
    expect(await page.evaluate(() => (window as any).__smbWrites.length)).toBe(2);
    expect(await page.evaluate(() => (window as any).__smbDrives[0].label)).toBe('Changed by CLI');
});
