#!/usr/bin/env python3
"""openexr-images oracle for the EXR format, run locally (Python OpenEXR
and numpy in a venv; never a dependency of the plugin).

  images_oracle.py     writes tests/fixtures/openexr-images/expected.txt:
                       for each file OpenEXR reads, a line per part
                       (`part`, its data window inclusive) and one per
                       channel (`channel`, its name, type and the FNV-1a
                       64-bit hash of its samples' little-endian bytes,
                       rows from the top); `error` for a file of channels
                       the format refuses (subsampled luminance-chroma,
                       several resolutions).
"""
from pathlib import Path

import numpy as np
import OpenEXR

ROOT = Path(__file__).resolve().parents[1]
IMAGES = ROOT / 'tests/fixtures/openexr-images'
REFUSED = {'Rec709_YC.exr', 'ColorCodedLevels.exr'}


def fnv(data: bytes) -> str:
    """FNV-1a, 64 bits, as hex: what the Rust test computes too."""
    value = 0xcbf29ce484222325
    for byte in data:
        value = ((value ^ byte) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return f'{value:016x}'


def main() -> None:
    lines = []
    for path in sorted(IMAGES.glob('*.exr')):
        if path.name in REFUSED:
            lines.append(f'{path.name} error')
            continue
        with OpenEXR.File(str(path), separate_channels=True) as file:
            for part in file.parts:
                (x0, y0), (x1, y1) = part.header['dataWindow']
                lines.append(f'{path.name} part {x0} {y0} {x1} {y1}')
                for name in sorted(part.channels):
                    pixels = np.ascontiguousarray(part.channels[name].pixels)
                    kind = {np.float16: 'half', np.float32: 'float', np.uint32: 'uint'}[pixels.dtype.type]
                    data = pixels.astype(pixels.dtype.newbyteorder('<')).tobytes()
                    lines.append(f'{path.name} channel {name} {kind} {fnv(data)}')
    (IMAGES / 'expected.txt').write_text('\n'.join(lines) + '\n')


if __name__ == '__main__':
    main()
