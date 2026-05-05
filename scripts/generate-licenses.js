import { execSync } from 'child_process';
import { writeFileSync, mkdirSync, existsSync } from 'fs';
import path from 'path';

const STATIC_DIR = path.resolve('static');
const OUTPUT_FILE = path.join(STATIC_DIR, 'licenses.json');
// Default to permissive: a missing dev tool (cargo-license, license-report)
// should not break a production build. CI / release pipelines that *want*
// the build to fail when license metadata is incomplete can set
// LICENSES_REQUIRE=1.
const REQUIRE_ALL = process.env.LICENSES_REQUIRE === '1';

console.log('Generating license reports...');

if (!existsSync(STATIC_DIR)) {
    mkdirSync(STATIC_DIR);
}

/** Returns the command's stdout, or null if it could not be run. Only throws
 *  when LICENSES_REQUIRE=1. Treats "command not found" / ENOENT identically
 *  to any other non-zero exit. */
function runOrFail(label, cmd, options = {}) {
    try {
        return execSync(cmd, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], ...options });
    } catch (e) {
        const stderr = (e.stderr || '').toString().trim();
        const looksLikeMissingTool =
            e.code === 'ENOENT' ||
            /is not recognized|nicht gefunden|command not found/i.test(stderr);

        if (REQUIRE_ALL) {
            console.error(`[licenses] ${label} failed: ${cmd}`);
            if (label === 'cargo-license') {
                console.error('\n  Install it with:  cargo install cargo-license\n');
            }
            throw e;
        }

        if (looksLikeMissingTool) {
            console.warn(
                `[licenses] ${label}: tool not installed — skipping. ` +
                `(install \`cargo install ${label === 'cargo-license' ? 'cargo-license' : label}\` ` +
                `or set LICENSES_REQUIRE=1 to make this fatal.)`
            );
        } else {
            console.warn(`[licenses] ${label}: ${cmd} failed — continuing without it.`);
            if (stderr) console.warn(`[licenses] stderr: ${stderr}`);
        }
        return null;
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
