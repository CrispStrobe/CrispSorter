#!/usr/bin/env python3
"""Prepare synthetic text and hash-checked models for native mobile CI.

Weights are test inputs, not bundled app resources. The application installer
uses the paired registry independently. No API token or private source is used.
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import tempfile
from urllib.request import urlopen

MAX_MODEL_BYTES = 64 * 1024 * 1024
WIDTH, HEIGHT = 1400, 480
LINES = ('CRISP MOBILE OCR', 'Document 2048', 'Total 125.00')


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as source:
        while chunk := source.read(1024 * 1024):
            value.update(chunk)
    return value.hexdigest()


def fetch_model(model, output):
    name = model['filename']
    if Path(name).name != name or not name or not model['url'].startswith('https://'):
        raise ValueError('Invalid model fixture metadata')
    expected = model['sha256']
    if len(expected) != 64 or any(c not in '0123456789abcdef' for c in expected):
        raise ValueError('Model fixture requires an immutable SHA-256 pin')
    destination = output / name
    if destination.exists():
        if destination.is_symlink() or destination.stat().st_size > MAX_MODEL_BYTES or digest(destination) != expected:
            raise ValueError(f'Existing model fixture does not match its pin: {name}')
        return destination
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(prefix='.model-', dir=output, delete=False) as sink:
            temporary = Path(sink.name)
            received, checksum = 0, hashlib.sha256()
            with urlopen(model['url'], timeout=120) as source:
                if not source.geturl().startswith('https://'):
                    raise ValueError('Model fixture redirected outside HTTPS')
                while chunk := source.read(1024 * 1024):
                    received += len(chunk)
                    if received > MAX_MODEL_BYTES:
                        raise ValueError(f'Model fixture exceeds the 64 MiB limit: {name}')
                    checksum.update(chunk)
                    sink.write(chunk)
        if not received or checksum.hexdigest() != expected:
            raise ValueError(f'Model fixture SHA-256 mismatch: {name}')
        temporary.replace(destination)
        temporary = None
        return destination
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def make_text_fixture(output, font_path):
    from PIL import Image, ImageDraw, ImageFont
    image = Image.new('RGB', (WIDTH, HEIGHT), 'white')
    draw = ImageDraw.Draw(image)
    font = ImageFont.truetype(str(font_path), 64)
    for line, y in zip(LINES, (50, 180, 310)):
        draw.text((70, y), line, font=font, fill='black')
    image.save(output / 'page.ppm')
    image.save(output / 'page.png')
    (output / 'page.rgb').write_bytes(image.tobytes())
    return {'width': WIDTH, 'height': HEIGHT, 'text': '\n'.join(LINES),
            'font_sha256': digest(font_path), 'image_sha256': digest(output / 'page.ppm'),
            'rgb_sha256': digest(output / 'page.rgb'), 'png_sha256': digest(output / 'page.png')}


def quality_spec(source=None):
    source = source or Path(__file__).with_name('quality.json')
    spec = json.loads(source.read_text())
    if spec.get('schema_version') != 1 or len(spec.get('cases', [])) != 2:
        raise ValueError('Unexpected bounded native quality fixture set')
    if len(spec.get('models', [])) != 1 or spec['models'][0]['name'] != 'tesseract-deu' or not 0 < spec['models'][0]['bytes'] <= 2 * 1024 * 1024:
        raise ValueError('Native quality fixture requires one bounded German recognizer')
    names = set()
    for case in spec['cases']:
        name = case['filename']
        if Path(name).name != name or not name.endswith('.png') or name in names:
            raise ValueError('Unsafe or duplicate native quality fixture filename')
        names.add(name)
        if case['language'] not in ('eng', 'deu') or case['layout_engine'] not in (None, 'cc'):
            raise ValueError('Quality fixtures must use the explicit small-model profiles')
        if not 0 < case['width'] * case['height'] <= 2_000_000 or not 0 < len(case['lines']) <= 12:
            raise ValueError('Quality fixture dimensions or line count exceeds its bound')
        if not 0 < case['max_unicode_cer'] <= 0.25:
            raise ValueError('Quality fixture cannot weaken the Unicode CER bound')
        if any(not line['text'].strip() for line in case['lines']):
            raise ValueError('Quality fixture text must not be empty')
    return spec


def make_quality_fixtures(output, font_path):
    from PIL import Image, ImageDraw, ImageFont
    spec = quality_spec()
    prepared = []
    for case in spec['cases']:
        image = Image.new('RGB', (case['width'], case['height']), 'white')
        draw = ImageDraw.Draw(image)
        for line in case['lines']:
            font = ImageFont.truetype(str(font_path), line.get('font_size', case['font_size']))
            box = draw.textbbox((line['x'], line['y']), line['text'], font=font)
            if box[0] < 0 or box[1] < 0 or box[2] > case['width'] or box[3] > case['height']:
                raise ValueError('Native quality reference text is clipped by the selected font')
            draw.text((line['x'], line['y']), line['text'], font=font, fill='black')
        destination = output / case['filename']
        if destination.exists() or destination.is_symlink():
            raise ValueError('Choose a fresh output directory for native quality fixtures')
        image.save(destination)
        prepared.append({**case, 'text': '\n'.join(line['text'] for line in case['lines']),
                         'png_sha256': digest(destination), 'bytes': destination.stat().st_size})
    return {'spec_sha256': digest(Path(__file__).with_name('quality.json')),
            'font_sha256': digest(font_path), 'models': spec['models'], 'cases': prepared,
            'scope': spec['scope']}


def prepare_document_fixtures(output, source=None):
    """Copy reviewed native document fixtures only after checking their pins."""
    source = source or Path(__file__).resolve().parents[2] / 'tests/fixtures/mobile-documents'
    manifest_path = source / 'manifest.json'
    if not manifest_path.is_file():
        return {'available': False, 'reason': 'Native document fixtures are not present in this source revision.'}
    manifest = json.loads(manifest_path.read_text())
    if manifest.get('schema_version') != 1:
        raise ValueError('Unsupported native document fixture manifest')
    candidates = [manifest, *manifest.get('images', [])]
    prepared = []
    for item in candidates:
        filename = item['filename']
        if not filename or Path(filename).name != filename:
            raise ValueError('Unsafe native document fixture filename')
        fixture = item.get('fixture', filename)
        if not fixture or Path(fixture).name != fixture:
            raise ValueError('Unsafe native document fixture source')
        source_file = source / fixture
        if source_file.is_symlink() or source_file.stat().st_size > 48 * 1024 * 1024:
            raise ValueError('Unsafe or oversized native document fixture source')
        encoded = source_file.read_bytes()
        data = base64.b64decode(b''.join(encoded.split()), validate=True) if fixture.endswith('.b64') else encoded
        if not 0 < len(data) <= 32 * 1024 * 1024 or len(data) != item['bytes']:
            raise ValueError('Native document fixture size mismatch')
        if hashlib.sha256(data).hexdigest() != item['sha256']:
            raise ValueError('Native document fixture SHA-256 mismatch')
        prepared.append((filename, data))
    # Verify the complete set before writing. Preparation never decodes images
    # or PDF; the actual app/plugin must perform that decoding in CI.
    for filename, data in prepared:
        destination = output / filename
        if destination.is_symlink() or (destination.exists() and destination.read_bytes() != data):
            raise ValueError('Existing native document fixture differs from the pinned input')
    for filename, data in prepared:
        (output / filename).write_bytes(data)
    return {'available': True, 'manifest_sha256': digest(manifest_path), **manifest}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--font', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    models = json.loads(Path(__file__).with_name('models.json').read_text())['models']
    for model in models:
        fetch_model(model, args.output)
    receipt = {'schema_version': 1, 'synthetic_only': True, 'models': models,
               'fixture': make_text_fixture(args.output, args.font),
               'quality_fixtures': make_quality_fixtures(args.output, args.font),
               'document_fixtures': prepare_document_fixtures(args.output)}
    (args.output / 'inputs.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
