"""Strict structures for retained iOS draft records; unknown fields fail closed."""
import json
from datetime import datetime
import re
from urllib.parse import urlsplit

from mobile_native_provenance import BASE_NOTICES, SHEEN_NOTICES, SHEEN_SYMBOLS


def keys(value, expected):
    if not isinstance(value, dict) or set(value) != set(expected):
        raise ValueError('Unexpected retained record fields')


def literal(actual, expected):
    if actual != expected or type(actual) is not type(expected):
        raise ValueError('Unexpected retained record value')


def text(value, pattern=r'[^\x00-\x1f]{1,4096}'):
    if not isinstance(value, str) or not re.fullmatch(pattern, value):
        raise ValueError('Unexpected retained text value')


def hash_value(value, length=64):
    text(value, '[0-9a-f]{' + str(length) + '}')


def count(value):
    if type(value) is not int or not 0 <= value <= 2**31:
        raise ValueError('Unexpected retained integer')


def date(value):
    text(value, r'\d{4}-\d\d-\d\dT[0-9:.]+(?:Z|\+00:00)')
    if datetime.fromisoformat(value.replace('Z', '+00:00')).tzinfo is None:
        raise ValueError('Missing retained timestamp timezone')


def sized_hash(value):
    keys(value, ('bytes', 'sha256'))
    count(value['bytes'])
    hash_value(value['sha256'])


def preparation(value, description):
    keys(value, ('schema', 'target', 'app_source', 'production_source', 'version',
                 'runtime_lock', 'runtime_lock_sha256', 'profile', 'qualification'))
    literal(value['schema'], 'crispsorter-signed-ocr-draft-preparation-v1')
    literal(value['profile'], description)
    literal(value['qualification'], 'Not established; build preparation only')
    literal(value['target'], 'ios-arm64')
    hash_value(value['app_source'], 40)
    hash_value(value['production_source'], 40)
    hash_value(value['runtime_lock_sha256'])
    literal(value['runtime_lock'], 'scripts/ocr-mobile-runtime-lock.json')
    text(value['version'], r'\d+\.\d+\.\d+(?:-[a-z0-9.-]+)?')


def layout(value):
    keys(value, ('schema', 'app_source', 'app_tree', 'application_source_clean',
                 'tracked_source_clean', 'generated_tracked_files', 'cargo_lock_sha256',
                 'cargo_manifest_sha256', 'dependency_sources', 'checked_dependencies',
                 'mobile_qualification_lock_matches_release', 'scope'))
    literal(value['schema'], 'crispsorter-mobile-release-layout-v1')
    for field in ('app_source', 'app_tree'):
        hash_value(value[field], 40)
    for field in ('cargo_lock_sha256', 'cargo_manifest_sha256'):
        hash_value(value[field])
    for field in ('application_source_clean', 'mobile_qualification_lock_matches_release'):
        literal(value[field], True)
    generated = value['generated_tracked_files']
    if not isinstance(generated, list) or len(generated) > 1:
        raise ValueError('Unexpected generated layout inputs')
    literal(value['tracked_source_clean'], not bool(generated))
    for entry in generated:
        keys(entry, ('path', 'bytes', 'sha256', 'committed_sha256', 'generated_at', 'counts'))
        literal(entry['path'], 'static/licenses.json')
        count(entry['bytes'])
        hash_value(entry['sha256'])
        hash_value(entry['committed_sha256'])
        date(entry['generated_at'])
        license_counts(entry['counts'])
    keys(value['dependency_sources'], ('CrispEmbed', 'CrispASR', 'crisp-docx'))
    for identity in value['dependency_sources'].values():
        hash_value(identity, 40)
    checked = value['checked_dependencies']
    if not isinstance(checked, list) or not 1 <= len(checked) <= 100:
        raise ValueError('Unexpected retained dependency list')
    for item in checked:
        text(item, r'[a-z0-9_-]{1,100}')
    literal(value['scope'], 'Clean source layout and dependency identity only; no native build or behavior qualification')


def native(value, lock):
    keys(value, ('target', 'source_commit', 'ggml_commit', 'artifacts', 'native_provenance'))
    literal(value['target'], 'ios-arm64')
    hash_value(value['source_commit'], 40)
    hash_value(value['ggml_commit'], 40)
    keys(value['artifacts'], ('libcrispembed.a',))
    hash_value(value['artifacts']['libcrispembed.a'])
    proof = value['native_provenance']
    keys(proof, ('schema_version', 'components', 'notices'))
    literal(proof['schema_version'], 1)
    components = proof['components']
    keys(components, lock.get('native_components', {}))
    if set(components) - {'sheenbidi'}:
        raise ValueError('Unexpected retained native component')
    notices = dict(BASE_NOTICES)
    if 'sheenbidi' in components:
        notices.update(SHEEN_NOTICES)
        component = components['sheenbidi']
        keys(component, ('source_path', 'source_tree', 'upstream_commit', 'artifacts', 'linkage'))
        literal(component['source_path'], 'vendor/sheenbidi')
        hash_value(component['source_tree'], 40)
        hash_value(component['upstream_commit'], 40)
        keys(component['artifacts'], ('libcrispembed.a',))
        artifact = component['artifacts']['libcrispembed.a']
        keys(artifact, ('sha256', 'defined_symbols'))
        literal(artifact['sha256'], value['artifacts']['libcrispembed.a'])
        literal(artifact['defined_symbols'], list(SHEEN_SYMBOLS))
        literal(component['linkage'], 'Unity C implementation in the primary native library')
    keys(proof['notices'], notices)
    for name, (repository, path) in notices.items():
        entry = proof['notices'][name]
        keys(entry, ('source_repository', 'source_path', 'git_blob', 'bytes', 'sha256'))
        literal(entry['source_repository'], repository)
        literal(entry['source_path'], path)
        hash_value(entry['git_blob'], 40)
        count(entry['bytes'])
        hash_value(entry['sha256'])


def ort(value, files):
    keys(value, ('schema_version', 'kind', 'target', 'version', 'api_version', 'rust_binding',
                 'archive', 'details', 'files', 'lock_sha256', 'recorded_at'))
    literal(value['schema_version'], 1)
    literal(value['kind'], 'ocr-mobile-ort-runtime')
    literal(value['target'], 'ios-arm64')
    text(value['version'], r'\d+\.\d+\.\d+')
    text(value['rust_binding'], r'\d+\.\d+\.\d+-rc\.\d+')
    count(value['api_version'])
    keys(value['archive'], ('url', 'filename', 'bytes', 'sha256'))
    text(value['archive']['url'], r'https://[A-Za-z0-9./_-]+')
    text(value['archive']['filename'], r'[A-Za-z0-9._-]+')
    count(value['archive']['bytes'])
    hash_value(value['archive']['sha256'])
    keys(value['details'], ('framework_identifier', 'architectures', 'static', 'minimum_os_version'))
    literal(value['details']['framework_identifier'], 'ios-arm64')
    literal(value['details']['architectures'], ['arm64'])
    literal(value['details']['static'], True)
    text(value['details']['minimum_os_version'], r'\d+\.\d+(?:\.\d+)?')
    keys(value['files'], files)
    for entry in value['files'].values():
        sized_hash(entry)
    hash_value(value['lock_sha256'])
    date(value['recorded_at'])


def license_counts(value):
    if not isinstance(value, dict) or set(value) not in ({'frontend', 'backend', 'total'}, {'frontend', 'backend', 'native', 'total'}):
        raise ValueError('Unexpected retained license counts')
    for number in value.values():
        count(number)


def licenses(value):
    keys(value, ('generatedAt', 'counts', 'licenses'))
    date(value['generatedAt'])
    license_counts(value['counts'])
    if not isinstance(value['licenses'], list) or len(value['licenses']) > 20000:
        raise ValueError('Unexpected retained license inventory')
    for item in value['licenses']:
        keys(item, ('name', 'version', 'license', 'author', 'link', 'source'))
        for field, string in item.items():
            text(string, r'[^\x00-\x1f]{0,4096}' if field == 'author' else r'[^\x00-\x1f]{1,4096}')
        if item['source'] not in ('Frontend', 'Backend', 'Native'):
            raise ValueError('Unexpected retained license source')
        link = urlsplit(item['link'])
        if link.username or link.password or link.query or link.scheme not in ('http', 'https'):
            raise ValueError('Retained public license URL cannot contain credentials/query')


def cargo_summary(value, original_sha):
    # Arbitrary Cargo graph/workspace/package metadata is deliberately omitted.
    fields = ('schema_version', 'kind', 'source_commit', 'cargo_lock_sha256',
              'application_manifest_sha256', 'workspace_manifest_sha256', 'target',
              'features', 'default_features', 'cargo_version', 'backend_count', 'metadata_sha256')
    result = {key: value[key] for key in fields}
    literal(result['schema_version'], 1)
    literal(result['kind'], 'locked-native-license-metadata')
    hash_value(result['source_commit'], 40)
    for name in ('cargo_lock_sha256', 'application_manifest_sha256', 'workspace_manifest_sha256', 'metadata_sha256'):
        hash_value(result[name])
    literal(result['target'], 'aarch64-apple-ios')
    literal(result['features'], ['ocr-mobile', 'tauri/custom-protocol'])
    literal(result['default_features'], False)
    text(result['cargo_version'], r'cargo \d+\.\d+\.\d+(?:-[a-z0-9.]+)? \([0-9a-f]+ \d{4}-\d\d-\d\d\)')
    count(result['backend_count'])
    return {'schema': 'crispsorter-minimized-cargo-binding-v1', 'original_snapshot_sha256': original_sha,
            'original_graph_retained': False, 'binding': result}


def decode(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('Duplicate retained JSON field')
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=unique)


def validated_ios_metadata(project, staging, description, files):
    paths = {'preparation.json': 'preparation.json', 'layout.json': 'layout.json',
             'runtime-build.json': 'runtime/runtime-build.json', 'ort-runtime.json': 'ort/ort-runtime.json'}
    raw, parsed = {}, {}
    for name, relative in paths.items():
        path = staging / relative
        if path.is_symlink() or path.resolve() != path or not path.is_file() or not 0 < path.stat().st_size <= 8*1024**2:
            raise ValueError('Unsafe retained iOS metadata record')
        raw[name] = path.read_bytes()
        parsed[name] = decode(raw[name])
    preparation(parsed['preparation.json'], description)
    layout(parsed['layout.json'])
    native(parsed['runtime-build.json'], decode((project / 'scripts/ocr-mobile-runtime-lock.json').read_bytes()))
    ort(parsed['ort-runtime.json'], files)
    if (staging / 'runtime/candidate-reuse.json').exists():
        raise ValueError('Production-pinned signed draft cannot add candidate reuse metadata')
    return raw
