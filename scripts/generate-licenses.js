import { execSync } from 'child_process';
import { writeFileSync, mkdirSync, existsSync } from 'fs';
import path from 'path';

const STATIC_DIR = path.resolve('static');
const OUTPUT_FILE = path.join(STATIC_DIR, 'licenses.json');

console.log('Generating license reports...');

if (!existsSync(STATIC_DIR)) {
    mkdirSync(STATIC_DIR);
}

try {
    // 1. Get NPM licenses
    console.log('- Scanning NPM dependencies...');
    const npmOutput = execSync('npx license-report --output=json --only=prod').toString();
    const npmData = JSON.parse(npmOutput);
    const npmLicenses = npmData.map(dep => ({
        name: dep.name,
        version: dep.installedVersion,
        license: dep.licenseType,
        author: dep.author,
        link: `https://www.npmjs.com/package/${dep.name}`,
        source: 'Frontend'
    }));

    // 2. Get Rust licenses
    console.log('- Scanning Rust dependencies...');
    const rustOutput = execSync('cd src-tauri && cargo-license --json').toString();
    const rustData = JSON.parse(rustOutput);
    const rustLicenses = rustData.map(dep => {
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
            source: 'Backend'
        };
    });

    // 3. Combine and Save
    const combined = [...npmLicenses, ...rustLicenses].sort((a, b) => a.name.localeCompare(b.name));
    
    writeFileSync(OUTPUT_FILE, JSON.stringify(combined, null, 2));
    console.log(`Successfully generated ${combined.length} licenses at ${OUTPUT_FILE}`);

} catch (error) {
    console.error('Failed to generate licenses:', error);
    process.exit(1);
}
