import { describe, expect, it } from 'vitest';
import { containerBreadcrumb, readContainerMember, readOpenedContainerMember, resultContainerMember } from './containers';

const locator = () => ({ version: 1, rootOwnerId: 'local', rootDocId: 'root-occurrence', rootLocationUri: '/records/mail.zip',
    rootSha256: 'a'.repeat(64), entries: [
        { kind: 'zip', index: 1, occurrence: 0, name: 'inbox.mbox', sha256: 'b'.repeat(64), size: 120 },
        { kind: 'mailbox-message', index: 4, occurrence: 1, name: 'Receipt.eml', sha256: 'c'.repeat(64), size: 80 },
        { kind: 'mime-part', index: 2, occurrence: 0, name: 'invoice.pdf', sha256: 'd'.repeat(64), size: 20 },
    ] });

describe('container source breadcrumbs', () => {
    it('preserves the root and each nested occurrence without turning names into paths', () => {
        const parsed = readContainerMember(locator());
        expect(parsed).not.toBeNull();
        expect(containerBreadcrumb(parsed!)).toBe('/records/mail.zip › inbox.mbox › Receipt.eml (occurrence 2) › invoice.pdf');
    });

    it('does not render malformed or unsafe stored metadata as a source chain', () => {
        for (const patch of [{ version: 2 }, { rootSha256: 'wrong' }, { rootLocationUri: 'crisp+member://other' }, { rootOwnerId: '' }]) {
            expect(readContainerMember({ ...locator(), ...patch })).toBeNull();
        }
        for (const name of ['../escape', 'C:/absolute', 'a\\b', 'a//b', 'a/./b', '\u202ehidden', 'line\nbreak']) {
            const invalid = locator(); invalid.entries[0].name = name;
            expect(readContainerMember(invalid)).toBeNull();
        }
        const deep = locator(); deep.entries.push(...deep.entries);
        expect(readContainerMember(deep)).toBeNull();
    });

    it('applies protocol byte bounds to multibyte names', () => {
        const invalid = locator(); invalid.entries[0].name = 'é'.repeat(1025);
        expect(readContainerMember(invalid)).toBeNull();
        invalid.entries[0].name = 'é'.repeat(1024);
        expect(readContainerMember(invalid)).not.toBeNull();
    });

    it('accepts only an owner-bound source chain and the matching native copy receipt', () => {
        const member = locator();
        const result = { doc_id: 'e'.repeat(64), owner_id: 'local', source_hash: 'd'.repeat(64),
            location_uri: `crisp+member://v1/${'f'.repeat(64)}/${'e'.repeat(64)}`, metadata_json: JSON.stringify({ container_member: member }) };
        expect(resultContainerMember(result)).toEqual(member);
        expect(resultContainerMember({ ...result, owner_id: 'another-owner' })).toBeNull();
        const opened = { path: '/app/container-cache/generated.pdf', filename: 'invoice.pdf', rootLocationUri: member.rootLocationUri,
            rootSha256: member.rootSha256,
            ownerId: result.owner_id, docId: result.doc_id, sourceHash: result.source_hash };
        expect(readOpenedContainerMember(opened, result)).toEqual(opened);
        for (const change of [{ docId: 'other' }, { ownerId: 'other' }, { sourceHash: '0'.repeat(64) },
            { rootLocationUri: '/changed/root.zip' }, { rootSha256: '0'.repeat(64) }, { path: 'relative.txt' }, { filename: '../outside.pdf' }]) {
            expect(() => readOpenedContainerMember({ ...opened, ...change }, result)).toThrow();
        }
    });
});
