"""Generate a self-authored, single-image HEIC input for native decoder tests.

Offline fixture utility only. Requires Pillow and FFmpeg with libx265; neither
tool is an application dependency. The committed bytes and digest, rather than
the encoder installation, are the test input shared by Android and iOS.
"""

import argparse
from pathlib import Path
import struct
import subprocess
import tempfile


def u16(value):
    return struct.pack(">H", value)


def u32(value):
    return struct.pack(">I", value)


def box(kind, payload):
    return u32(len(payload) + 8) + kind.encode("ascii") + payload


def full(kind, payload, version=0):
    return box(kind, bytes([version, 0, 0, 0]) + payload)


def heic_from_single_sample(mp4):
    """Wrap our one HEVC Main sample with the HEIF primary-item properties.

    Container layouts were checked against libheif's item/property writer:
    https://github.com/strukturag/libheif/blob/master/libheif/box.cc
    """
    position = mp4.index(b"hvcC") - 4
    config_size = int.from_bytes(mp4[position:position + 4], "big")
    config = mp4[position:position + config_size]
    assert len(config) == config_size and (config[9] & 31) == 1, "HEVC Main is required"
    offset, samples = 0, []
    while offset < len(mp4):
        length = int.from_bytes(mp4[offset:offset + 4], "big")
        assert 8 <= length <= len(mp4) - offset
        if mp4[offset + 4:offset + 8] == b"mdat":
            samples.append(mp4[offset + 8:offset + length])
        offset += length
    assert len(samples) == 1 and samples[0]
    sample = samples[0]
    ftyp = box("ftyp", b"heic" + u32(0) + b"mif1heic")

    def metadata(sample_offset):
        handler = full("hdlr", u32(0) + b"pict" + bytes(12) + b"CrispSorter fixture\0")
        primary = full("pitm", u16(1))
        location = full("iloc", bytes([0x44, 0]) + u16(1) + u16(1) + u16(0)
                        + u16(1) + u32(sample_offset) + u32(len(sample)))
        item = full("infe", u16(1) + u16(0) + b"hvc1Primary\0", version=2)
        information = full("iinf", u16(1) + item)
        properties = box("ipco", config + full("ispe", u32(120) + u32(80))
                         + full("pixi", bytes([3, 8, 8, 8])))
        associations = full("ipma", u32(1) + u16(1) + bytes([3, 0x81, 0x82, 0x03]))
        return full("meta", handler + primary + location + information
                    + box("iprp", properties + associations))

    meta = metadata(len(ftyp) + len(metadata(0)) + 8)
    return ftyp + meta + box("mdat", sample)


def main():
    from PIL import Image

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="crisp-heic-fixture-") as directory:
        work = Path(directory)
        image = Image.new("RGB", (120, 80))
        pixels = image.load()
        palette = [(230, 30, 30), (30, 190, 50), (30, 60, 220), (240, 220, 30)]
        for y in range(80):
            for x in range(120):
                pixels[x, y] = palette[(2 if y >= 40 else 0) + (1 if x >= 60 else 0)]
        image.save(work / "colors.png")
        subprocess.run([
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(work / "colors.png"),
            "-frames:v", "1", "-c:v", "libx265", "-pix_fmt", "yuv420p", "-profile:v", "main",
            "-preset", "medium", "-crf", "18", "-x265-params",
            "pools=none:frame-threads=1:asm=0:bframes=0:keyint=30:log-level=error",
            "-tag:v", "hvc1", str(work / "colors.mp4"),
        ], check=True)
        args.output.write_bytes(heic_from_single_sample((work / "colors.mp4").read_bytes()))


if __name__ == "__main__":
    main()
