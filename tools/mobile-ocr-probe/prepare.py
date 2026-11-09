#!/usr/bin/env python3
"""Prepare synthetic text and hash-checked models for native mobile CI.

Weights are test inputs, not bundled app resources. The application installer
uses the paired registry independently. No API token or private source is used.
"""
import argparse
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
               'fixture': make_text_fixture(args.output, args.font)}
    (args.output / 'inputs.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
