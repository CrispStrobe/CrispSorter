"""Build a source-bound OCR probe without the unrelated index/UI dependencies.

This compiles verbatim production functions/types and the production live test.
It is not a full application build; release qualification exercises the actual
packaged executable separately. No inference implementation is mocked.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import tomllib


def block(source, pattern):
    match = re.search(pattern, source, re.MULTILINE)
    if not match:
        raise ValueError('Production source no longer matches: ' + pattern)
    start = source.rfind('\n', 0, match.start()) + 1
    first = source[start:source.index('\n', start)]
    indent = first[:len(first) - len(first.lstrip())]
    end = re.search('^' + re.escape(indent) + r'\}\s*$', source[match.end():], re.MULTILINE)
    if not end:
        raise ValueError('Missing production block terminator')
    return source[start:match.end() + end.end()] + '\n'


def generate(app, embed, destination, panel=False):
    app, embed, destination = app.resolve(), embed.resolve(), destination.resolve()
    if destination.parent != app:
        raise ValueError('Probe must sit beside tests/ so the original fixture path is unchanged')
    destination.mkdir(parents=True, exist_ok=True)
    src = app / 'src-tauri/src/extractors'
    config = (src / 'mod.rs').read_text()
    begin = config.index('#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]\npub struct OcrPipelineConfig')
    config = config[begin:config.index('/// How far the anydoc converter', begin)]
    production = (src / 'ocr_crispembed.rs').read_text()
    fragments = {'configuration_types_and_defaults': config}
    for name in ['vlm_engine_id', 'vlm_default_model', 'resolve', 'build_pipeline',
                 'cached_pipeline', 'effective_confidence', 'ocr_regions_detailed']:
        fragments[name] = block(production, r'^(?:pub )?fn ' + name + r'\b')
    fragments['RegionConf'] = '#[derive(Debug, Clone)]\n' + block(production, r'^pub struct RegionConf\b')
    for name in ['DEFAULT_DET_MODEL', 'DEFAULT_REC_MODEL', 'NAFNET_MODEL']:
        fragments[name] = re.search(r'^const ' + name + r'[^;]+;', production, re.MULTILINE).group() + '\n'
    fragments['OCR_ORCH'] = re.search(r'^static OCR_ORCH:[\s\S]+?OnceLock::new\(\);', production, re.MULTILINE).group() + '\n'
    fragments['live_test'] = block(production, r'^    fn ocr_task_presets_live\b')
    for name in ['context_cache', 'ocr_presets']:
        fragments[name] = (src / (name + '.rs')).read_text()
    functions = ''.join(value for name, value in fragments.items()
                        if name not in ['configuration_types_and_defaults', 'context_cache', 'ocr_presets', 'live_test'])
    rust = 'pub mod extractors {\n' + config
    for name in ['context_cache', 'ocr_presets']:
        rust += '#[path = ' + json.dumps(str(src / (name + '.rs'))) + '] pub mod ' + name + ';\n'
    rust += 'pub mod ocr_crispembed {\nuse anyhow::{Result, Context};\nuse std::{path::Path, sync::Mutex};\n'
    rust += functions + '\n#[cfg(test)] mod tests { use super::*;\n#[test]\n#[ignore]\n' + fragments['live_test'] + '\n}\n}\n}\n'
    (destination / 'lib.rs').write_text(rust)
    lock = tomllib.loads((app / 'Cargo.lock').read_text())
    dependencies = []
    for name in ['anyhow', 'serde', 'serde_json', 'sha2']:
        candidates = [p['version'] for p in lock['package'] if p['name'] == name]
        version = next((v for v in candidates if name != 'sha2' or v.startswith('0.10.')), None)
        if not version:
            raise ValueError('Missing locked dependency ' + name)
        options = '{ version = "=' + version + '"' + (', features = ["derive"]' if name == 'serde' else '') + ' }'
        dependencies.append(name + ' = ' + options)
    dependencies.append('crispembed = { path = ' + json.dumps(str(embed / 'crispembed')) + ' }')
    manifest = '''[package]
name = "crispsorter-ocr-qualification"
version = "0.0.0"
edition = "2021"
[lib]
path = "lib.rs"
[workspace]
[features]
default = ["crispembed"]
crispembed = []
[dependencies]
''' + '\n'.join(dependencies) + '\n'
    if panel:
        manifest += '\n[[bin]]\nname = "ocr-case"\npath = "case.rs"\n'
        shutil.copyfile(app / 'tools/quality/ocr_case.rs', destination / 'case.rs')
    (destination / 'Cargo.toml').write_text(manifest)
    shutil.copyfile(app / 'Cargo.lock', destination / 'Cargo.lock')
    receipt = {'scope': 'Exact production OCR functions, configuration and live test; excludes unrelated application modules',
               'fragments_sha256': {name: hashlib.sha256(value.encode()).hexdigest() for name, value in fragments.items()}}
    (destination / 'source-fragments.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--embed', type=Path, required=True)
    parser.add_argument('--destination', type=Path, required=True)
    parser.add_argument('--panel', action='store_true')
    args = parser.parse_args()
    print(json.dumps(generate(args.app, args.embed, args.destination, args.panel), indent=2))
