"""Inspect actual CLI PDFs with an independent parser; no recognition models."""
import json
import pathlib
import subprocess
import sys
import tempfile
from PIL import Image
from pypdf import PdfReader

binary = str(pathlib.Path(sys.argv[1]).resolve())

def invoke(*args, success=True):
    result = subprocess.run([binary, "images", "pdf", *map(str, args)], capture_output=True, text=True)
    assert (result.returncode == 0) == success, result.stderr
    return json.loads(result.stdout) if success else result

with tempfile.TemporaryDirectory() as directory:
    root = pathlib.Path(directory)
    transparent = root / "transparent.png"
    Image.new("RGBA", (40, 20), (0, 0, 0, 0)).save(transparent)
    oriented = root / "orientation.jpg"
    exif = Image.Exif()
    exif[274] = 6
    Image.new("RGB", (30, 60), "red").save(oriented, exif=exif)
    tiff = root / "renamed.data"
    Image.new("RGB", (20, 30), "black").save(tiff, format="TIFF", save_all=True,
        append_images=[Image.new("RGB", (20, 30), "white")])
    output = root / "ordered.pdf"
    report = invoke(transparent, oriented, tiff, "--out", output, "--paper", "source", "--dpi", 72)
    reader = PdfReader(output)
    assert report["pages"] == len(reader.pages) == 4
    assert [tuple(map(float, p.mediabox[2:])) for p in reader.pages] == [(40, 20), (60, 30), (20, 30), (20, 30)]
    assert list(reader.pages[0].images)[0].image.convert("RGB").getpixel((0, 0)) == (255, 255, 255)
    assert list(reader.pages[2].images)[0].image.convert("RGB").getpixel((0, 0)) == (0, 0, 0)
    assert all(not page.extract_text() for page in reader.pages)
    before = output.read_bytes()
    invoke(transparent, "--out", output, success=False)
    assert output.read_bytes() == before
    corrupt = root / "corrupt.jpg"
    corrupt.write_bytes(oriented.read_bytes()[:-2])
    invoke(transparent, corrupt, "--out", output, "--force", success=False)
    assert output.read_bytes() == before
    invoke(transparent, "--out", transparent, "--force", success=False)
    animated = root / "animated.png"
    Image.new("RGB", (20, 30), "black").save(animated, save_all=True,
        append_images=[Image.new("RGB", (20, 30), "white")])
    invoke(animated, "--out", root / "animation.pdf", success=False)
    assert not (root / "animation.pdf").exists()
    binary_output = root / "binary.pdf"
    report = invoke(oriented, "--out", binary_output, "--black-and-white", "--perspective")
    reader = PdfReader(binary_output)
    image = list(reader.pages[0].images)[0]
    assert not any(image.image.convert("L").histogram()[1:255])
    assert all(obj.get_object()["/BitsPerComponent"] == 1 for obj in reader.pages[0]["/Resources"]["/XObject"].values())
    assert report["timings"]["total_ms"] >= report["timings"]["preparation_ms"] >= report["timings"]["cleanup_ms"] >= 0
    assert not reader.pages[0].extract_text()
print("CLI contracts passed: order, all TIFF frames, EXIF, alpha, physical sizes, binary PDF, no text, animation rejection, atomic failures.")
