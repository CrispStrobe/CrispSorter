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
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}, options?: { headers?: Record<string, string> }) => {
            if (command === 'drive_native_support') return {filen: false, internxt: false};
            if (command === 'drive_list') return structuredClone(root.__smbDrives);
            if (command === 'drive_credentials_status') return {has_username: true, has_password: true};
            if (command === 'drive_create' || command === 'drive_update') {
                root.__smbWrites.push(structuredClone(args));
                const saved = {id: args.id ?? 'nas', label: args.label, kind: args.kind, path: args.path, smb: args.smb};
                root.__smbDrives = [saved];
                return saved;
            }
            return original(command, args, options);
        };
    });
    await page.getByRole('button', {name: 'Library', exact: true}).first().click();
    await page.locator('.tab-bar').getByRole('button', {name: /^Sources/}).click();
    await page.getByRole('button', {name: 'Cloud-Ordner', exact: true}).click();
    const dialog = page.locator('.drive-dialog');
    await dialog.getByLabel('Typ', {exact: true}).selectOption('smb');
    await dialog.getByLabel('Label', {exact: true}).fill('NAS');
    await dialog.getByLabel('SMB URL', {exact: true}).fill('smb://nas/books');
    await expect(dialog.getByLabel('SMB-Transport', {exact: true})).toHaveValue('mounted');
    await dialog.getByRole('button', {name: 'Anlegen', exact: true}).click();
    await expect(dialog).toContainText('Choose the existing SMB mount folder.');
    expect(await page.evaluate(() => (window as any).__smbWrites.length)).toBe(0);
    await dialog.getByLabel('SMB-Mount-Ordner', {exact: true}).fill('/Volumes/Books');
    await dialog.getByRole('button', {name: 'Anlegen', exact: true}).click();
    await expect.poll(() => page.evaluate(() => (window as any).__smbWrites.length)).toBe(1);
    await page.getByRole('button', {name: 'Cloud-Ordner', exact: true}).click();
    await dialog.getByTitle('Bearbeiten', {exact: true}).click();
    await dialog.getByLabel('SMB-Transport', {exact: true}).selectOption('rust');
    await expect(dialog.getByLabel('SMB-Verschlüsselung erforderlich (Rust)', {exact: true})).toBeEnabled();
    await dialog.getByRole('button', {name: 'Speichern', exact: true}).click();
    await expect.poll(() => page.evaluate(() => (window as any).__smbWrites.length)).toBe(2);
    const writes = await page.evaluate(() => (window as any).__smbWrites);
    expect(writes[0].smb).toEqual({backend: 'mounted', mount_path: '/Volumes/Books', require_encryption: true});
    expect(writes[1]).toMatchObject({id: 'nas', username: null, password: null, smb: {backend: 'rust', mount_path: '/Volumes/Books', require_encryption: true}});
    await page.getByRole('button', {name: 'Cloud-Ordner', exact: true}).click();
    await dialog.getByTitle('Bearbeiten', {exact: true}).click();
    await expect(dialog.getByLabel('SMB-Transport', {exact: true})).toHaveValue('rust');
    await expect(dialog.getByLabel('SMB-Mount-Ordner', {exact: true})).toHaveValue('/Volumes/Books');
});
