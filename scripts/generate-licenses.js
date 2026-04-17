import { execSync } from 'child_process';
import { writeFileSync, mkdirSync, existsSync } from 'fs';
import path from 'path';

const STATIC_DIR = path.resolve('static');
const OUTPUT_FILE = path.join(STATIC_DIR, 'licenses.json');
const ALLOW_MISSING = process.env.LICENSES_ALLOW_MISSING === '1';

console.log('Generating license reports...');

if (!existsSync(STATIC_DIR)) {
    mkdirSync(STATIC_DIR);
}

function runOrFail(label, cmd, options = {}) {
    try {
        return execSync(cmd, { encoding: 'utf8', ...options });
    } catch (e) {
        if (ALLOW_MISSING) {
            console.warn(
                `[licenses] Skipping ${label} — command failed and LICENSES_ALLOW_MISSING=1.`
            );
            console.warn(`[licenses] ${e.message}`);
            return null;
        }
        console.error(`[licenses] ${label} failed: ${cmd}`);
        console.error(
            label === 'cargo-license'
                ? '\n' +
                      '  Install it with:  cargo install cargo-license\n' +
                      '  (Or re-run with LICENSES_ALLOW_MISSING=1 to skip backend deps.)\n'
                : ''
        );
        throw e;
    }
}

// 1. Get NPM licenses
console.log('- Scanning NPM dependencies...');
let npmLicenses = [];
const npmOutput = runOrFail('license-report', 'npx license-report --output=json --only=prod');
if (npmOutput) {
    const npmData = JSON.parse(npmOutput);
    npmLicenses = npmData.map((dep) => ({
        name: dep.name,
        version: dep.installedVersion,
        license: dep.licenseType,
        author: dep.author,
        link: `https://www.npmjs.com/package/${dep.name}`,
        source: 'Frontend',
    }));
}

// 2. Get Rust licenses
console.log('- Scanning Rust dependencies...');
let rustLicenses = [];
const rustOutput = runOrFail(
    'cargo-license',
    'cargo-license --json',
    { cwd: path.resolve('src-tauri') }
);
if (rustOutput) {
    const rustData = JSON.parse(rustOutput);
    rustLicenses = rustData.map((dep) => {
        let author = 'Various';
        if (typeof dep.authors === 'string') {
            author = dep.authors.replace(/\|/g, ', ');
        } else if (Array.isArray(dep.authors)) {
            author = dep.authors.join(', ');
        }

        return {
            name: dep.name,
            version: dep.version,
            license: dep.license || 'Unknown',
            author: author,
            link: dep.repository || `https://crates.io/crates/${dep.name}`,
            source: 'Backend',
        };
    });
}

// 3. Combine and save. Include a generatedAt footer entry so the UI can
//    surface how fresh this list is without anyone having to stat the file.
const combined = [...npmLicenses, ...rustLicenses].sort((a, b) =>
    a.name.localeCompare(b.name)
);

const payload = {
    generatedAt: new Date().toISOString(),
    counts: {
        frontend: npmLicenses.length,
        backend: rustLicenses.length,
        total: combined.length,
    },
    licenses: combined,
};

writeFileSync(OUTPUT_FILE, JSON.stringify(payload, null, 2));
console.log(
    `Successfully generated ${combined.length} licenses ` +
        `(${npmLicenses.length} frontend, ${rustLicenses.length} backend) ` +
        `at ${OUTPUT_FILE}`
);
