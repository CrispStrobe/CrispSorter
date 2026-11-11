"""Build a source-bound OCR probe without the unrelated index/UI dependencies.

This compiles the production OCR modules, configuration and live tests.
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


def declared_block(source, declaration):
    """Keep the declaration's real derive/serde attributes with its body."""
    return block(source, r'^(?:#\[[^\n]+\]\n)*' + declaration)


def generate(app, embed, destination, panel=False, worker=False):
    app, embed, destination = app.resolve(), embed.resolve(), destination.resolve()
    if destination.parent != app:
        raise ValueError('Probe must sit beside tests/ so the original fixture path is unchanged')
    destination.mkdir(parents=True, exist_ok=True)
    src = app / 'src-tauri/src/extractors'
    sources = {}
    fragments = {}

    def read(relative):
        text = (app / relative).read_text(encoding='utf-8')
        sources[relative] = text
        return text

    def module(name, relative):
        read(relative)
        return '#[path = ' + json.dumps((app / relative).as_posix()) + '] pub mod ' + name + ';\n'

    production = read('src-tauri/src/extractors/mod.rs')
    begin = re.search(r'^#\[derive\([^\n]+\)\]\npub enum OcrTier\b', production, re.MULTILINE)
    if not begin:
        raise ValueError('Missing production OCR option declarations')
    fragments['configuration_types_and_defaults'] = production[begin.start():production.index('/// P30.2 — Extract text', begin.start())]
    fragments['ExtractedDocument'] = declared_block(production, r'pub struct ExtractedDocument\b')
    # Include the real single-image dispatcher, preparation and layout loop.
    # The worker's document/review modes must not be replaced with probe mocks.
    begin = production.index('static LAYOUT_DET:')
    fragments['image_dispatch'] = production[begin:production.index('/// One shared budget', begin)]
    fragments['operation_logging_tests'] = '#[cfg(test)]\n' + block(production, r'^mod operation_logging_tests\b')
    fragments['ExtractedMember'] = declared_block(read('src-tauri/src/extractors/container.rs'), r'pub struct ExtractedMember\b')
    fragments['AudioMetadata'] = declared_block(read('src-tauri/src/audio/probe.rs'), r'pub struct AudioMetadata\b')
    fragments['ExifSummary'] = declared_block(read('src-tauri/src/images/exif.rs'), r'pub struct ExifSummary\b')
    fragments['OcrRegion'] = declared_block(read('src-tauri/src/extractors/ocr_render.rs'), r'pub struct OcrRegion\b')
    fragments['resolve_model'] = '#[cfg(feature = "crispembed")]\n' + block(read('src-tauri/src/model_provisioning.rs'), r'^pub fn resolve_model\b')

    rust = module('operation_log', 'src-tauri/src/operation_log.rs')
    rust += module('ocr_word_evidence', 'src-tauri/src/ocr_word_evidence.rs')
    rust += 'pub mod index {\n' + module('license_consent', 'src-tauri/src/index/license_consent.rs') + '}\n'
    rust += 'pub mod model_provisioning { use std::path::Path;\n' + fragments['resolve_model'] + '}\n'
    # The probe has no GUI/ring buffer. Keep the real verbosity policy and
    # typed worker transport, with diagnostic output restricted to stderr.
    rust += '''pub fn app_log(level: &str, message: String) {
    use std::io::Write;
    if operation_log::enabled(level) {
        let _ = writeln!(std::io::stderr().lock(), "[{level}] {message}");
    }
}
'''
    rust += 'pub mod audio { pub mod probe {\n' + fragments['AudioMetadata'] + '} }\n'
    rust += 'pub mod images { pub mod exif { use serde::{Deserialize, Serialize};\n' + fragments['ExifSummary'] + '} }\n'
    rust += 'pub mod extractors {\nuse anyhow::{Context, Result}; use std::path::Path;\nuse crate::operation_log::OperationLog; use serde_json::{json, Value};\n'
    rust += fragments['configuration_types_and_defaults'] + fragments['ExtractedDocument']
    rust += 'pub mod container { use super::ExtractedDocument; use crisp_index_protocol::container::ContainerEntry;\n' + fragments['ExtractedMember'] + '}\n'
    rust += 'pub mod ocr_render {\n' + fragments['OcrRegion'] + '}\n'
    for name in ['context_cache', 'image_input', 'layout', 'ocr_crispembed', 'ocr_ocrs',
                 'ocr_paddle', 'ocr_policy', 'ocr_presets', 'ocr_worker', 'page_source']:
        rust += module(name, 'src-tauri/src/extractors/' + name + '.rs')
    rust += fragments['image_dispatch'] + fragments['operation_logging_tests'] + '}\n'
    (destination / 'lib.rs').write_text(rust)
    lock = tomllib.loads((app / 'Cargo.lock').read_text())
    app_dependencies = tomllib.loads((app / 'src-tauri/Cargo.toml').read_text())['dependencies']
    dependencies = []
    for name in ['anyhow', 'serde', 'serde_json', 'sha2', 'tempfile', 'uuid', 'image',
                 'kamadak-exif', 'tiff', 'ocrs', 'rten']:
        declaration = app_dependencies[name]
        requirement = declaration if isinstance(declaration, str) else declaration['version']
        required = requirement.lstrip('=^~')
        candidates = [p['version'] for p in lock['package'] if p['name'] == name
                      and (p['version'] == required or p['version'].startswith(required + '.'))]
        version = next(iter(candidates), None)
        if not version:
            raise ValueError('Missing locked dependency ' + name)
        extra = {'serde': ', features = ["derive"]', 'uuid': ', features = ["v4"]',
                 'image': ', default-features = false, features = ["png", "jpeg", "tiff", "webp", "bmp", "gif"]'}.get(name, '')
        options = '{ version = "=' + version + '"' + extra + ' }'
        dependencies.append(name + ' = ' + options)
    dependencies.append('crisp-index-protocol = { path = ' + json.dumps((app / 'crisp-index-protocol').as_posix()) + ' }')
    dependencies.append('crispembed = { path = ' + json.dumps((embed / 'crispembed').as_posix()) + ', optional = true }')
    dependencies.append('crispembed-sys = { path = ' + json.dumps((embed / 'crispembed-sys').as_posix()) + ', optional = true }')
    manifest = '''[package]
name = "crispsorter-ocr-qualification"
version = "0.0.0"
edition = "2021"
[lib]
path = "lib.rs"
[workspace]
[features]
default = ["crispembed", "sidecars", "crispembed-license-api"]
crispembed = ["dep:crispembed", "dep:crispembed-sys"]
crispembed-license-api = ["crispembed"]
ocr-review-diagnostics = ["crispembed"]
ocr-photo-tools = ["crispembed"]
sidecars = []
desktop-mas = []
[dependencies]
''' + '\n'.join(dependencies) + '\n'
    if panel:
        manifest += '\n[[bin]]\nname = "ocr-case"\npath = "case.rs"\n'
        shutil.copyfile(app / 'tools/quality/ocr_case.rs', destination / 'case.rs')
    if worker:
        manifest += '\n[[bin]]\nname = "ocr-worker-probe"\npath = "worker_probe.rs"\n'
        shutil.copyfile(app / 'tools/quality/ocr_worker_probe.rs', destination / 'worker_probe.rs')
    (destination / 'Cargo.toml').write_text(manifest)
    shutil.copyfile(app / 'Cargo.lock', destination / 'Cargo.lock')
    receipt = {'scope': 'Exact production OCR modules, policy, image preparation, worker modes and live tests; excludes GUI/index orchestration and document renderers',
               'native_execution': 'Generation only; compile, protocol and real-model runs are separate qualification steps',
               'fragments_sha256': {name: hashlib.sha256(value.encode()).hexdigest() for name, value in fragments.items()},
               'source_files_sha256': {name: hashlib.sha256(value.encode()).hexdigest() for name, value in sources.items()}}
    (destination / 'source-fragments.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--embed', type=Path, required=True)
    parser.add_argument('--destination', type=Path, required=True)
    parser.add_argument('--panel', action='store_true')
    parser.add_argument('--worker', action='store_true')
    args = parser.parse_args()
    print(json.dumps(generate(args.app, args.embed, args.destination, args.panel, args.worker), indent=2))
