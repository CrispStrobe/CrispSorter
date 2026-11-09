/** Licence facts for one registry model, as returned by the
 *  `embedder_model_license_info` Tauri command. */
export interface ModelLicenseInfo {
    name: string;
    /** Reviewed licence label, e.g. "Tongyi Qianwen Research License". */
    label: string;
    /** CrispEmbed's licence tag, e.g. "tongyi-qianwen-research". May be empty. */
    spdx: string;
    /** Upstream model card, where the licence terms are published. May be empty. */
    model_card_url: string;
    description: string;
    requires_consent: boolean;
}

export interface LicenseConsentStrings {
    /** Placeholders: {model}, {license}. */
    registry_license_confirm: string;
    /** Placeholder: {url}. */
    registry_license_terms: string;
    registry_license_terms_unknown: string;
    /** Placeholder: {license}. */
    registry_license_title: string;
}

function fill(template: string, values: Record<string, string>): string {
    return template.replace(/\{(\w+)\}/g, (m, k: string) => (k in values ? values[k] : m));
}

/** Build the generic registry-download consent dialog for exactly the
 *  selected model: its name, its licence, and where its terms are. */
export function buildLicenseConsentDialog(
    strings: LicenseConsentStrings,
    model: string,
    info: Pick<ModelLicenseInfo, 'label' | 'spdx' | 'model_card_url'>,
): { title: string; message: string } {
    const license = info.label || info.spdx || '?';
    const body = fill(strings.registry_license_confirm, { model, license });
    const terms = info.model_card_url
        ? fill(strings.registry_license_terms, { url: info.model_card_url })
        : strings.registry_license_terms_unknown;
    return {
        title: fill(strings.registry_license_title, { license }),
        message: `${body}\n\n${terms}`,
    };
}
