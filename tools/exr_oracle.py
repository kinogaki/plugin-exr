#!/usr/bin/env python3
"""OpenEXR oracle for the OpenEXR plugin's codec, run locally (Python OpenEXR and numpy in a
venv; never a dependency of the library).

  exr_oracle.py fixtures     write tests/fixtures/openexr/: files
                             written by OpenEXR itself in every compression
                             the codec reads, scanline and tiled, a
                             multi-part file, from the pattern below (which
                             the Rust tests compute too)
  exr_oracle.py check DIR    read each DIR/*.exr the Rust tests wrote
                             (KINOGAKI_EXR_ORACLE_DIR) with OpenEXR and
                             compare every channel with the pattern

The pattern: channel c of a W x H part, pixel (x, y):
  float  ((x*7 + y*13 + c*29) % 509) / 64 - 2 + ((x*y + c) % 17) / 1024
  half   the float, rounded to half
  uint   (x * 2654435761 ^ (y * 40503 + c)) mod 2^32
Channels: A (half), B (float), G (half), R (half), Z (float), id (uint).
"""
import sys
from pathlib import Path

import numpy as np
import OpenEXR

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'tests/fixtures/openexr'
CHANNELS = [('A', 'half'), ('B', 'float'), ('G', 'half'), ('R', 'half'), ('Z', 'float'), ('id', 'uint')]
COMPRESSIONS = {
    'none': OpenEXR.NO_COMPRESSION, 'rle': OpenEXR.RLE_COMPRESSION, 'zips': OpenEXR.ZIPS_COMPRESSION,
    'zip': OpenEXR.ZIP_COMPRESSION, 'piz': OpenEXR.PIZ_COMPRESSION, 'pxr24': OpenEXR.PXR24_COMPRESSION,
}


def pattern(width, height, c, kind):
    y, x = np.mgrid[0:height, 0:width].astype(np.int64)
    if kind == 'uint':
        return ((x * 2654435761) ^ (y * 40503 + c)).astype(np.uint64).astype(np.uint32)
    value = ((x * 7 + y * 13 + c * 29) % 509) / 64.0 - 2.0 + ((x * y + c) % 17) / 1024.0
    return value.astype(np.float32 if kind == 'float' else np.float16)


def channels(width, height):
    return {name: pattern(width, height, c, kind) for c, (name, kind) in enumerate(CHANNELS)}


def tiles(width, height):
    description = OpenEXR.TileDescription()
    description.xSize, description.ySize, description.mode = width, height, OpenEXR.ONE_LEVEL
    return description


def fixtures():
    OUT.mkdir(parents=True, exist_ok=True)
    count = 0
    for name, compression in COMPRESSIONS.items():
        header = {'compression': compression, 'type': OpenEXR.scanlineimage}
        OpenEXR.File(header, channels(67, 45)).write(str(OUT / f'scanline-{name}.exr'))
        count += 1
    for name in ['none', 'zip', 'piz', 'rle']:
        header = {'compression': COMPRESSIONS[name], 'type': OpenEXR.tiledimage,
                  'tiles': tiles(16, 12)}
        OpenEXR.File(header, channels(67, 45)).write(str(OUT / f'tiled-{name}.exr'))
        count += 1
    # Enough distinct words (over 16 384) for PIZ's 16-bit wavelet.
    wide = {name: data for name, data in channels(160, 120).items() if name in ('R', 'id')}
    OpenEXR.File({'compression': OpenEXR.PIZ_COMPRESSION, 'type': OpenEXR.scanlineimage}, wide).write(str(OUT / 'piz-wide.exr'))
    count += 1
    first = OpenEXR.Part({'compression': OpenEXR.ZIP_COMPRESSION, 'type': OpenEXR.scanlineimage}, channels(40, 30), 'beauty')
    second = OpenEXR.Part({'compression': OpenEXR.PIZ_COMPRESSION, 'type': OpenEXR.tiledimage,
                           'tiles': tiles(8, 8)}, channels(40, 30), 'depth')
    OpenEXR.File([first, second]).write(str(OUT / 'multipart.exr'))
    count += 1
    print(f'{count} files')


def check(directory):
    count = mismatches = 0
    for path in sorted(Path(directory).glob('*.exr')):
        image = OpenEXR.File(str(path), separate_channels=True)
        for part in image.parts:
            for c, (name, kind) in enumerate(CHANNELS):
                if name not in part.channels:
                    continue
                got = part.channels[name].pixels
                height, width = got.shape
                want = pattern(width, height, c, kind)
                lossy = 'pxr24' in path.name and kind == 'float'
                same = np.allclose(got, want, rtol=2 ** -15, atol=0) if lossy else np.array_equal(got.view(np.uint8), want.view(np.uint8))
                if not same:
                    print(f'MISMATCH {path.name} {part.name()} {name}')
                    mismatches += 1
                count += 1
    print(f'{count} channels read by OpenEXR {OpenEXR.OPENEXR_VERSION}, {mismatches} mismatches')
    if mismatches:
        raise SystemExit(1)


if __name__ == '__main__':
    if sys.argv[1:2] == ['fixtures']:
        fixtures()
    elif sys.argv[1:2] == ['check'] and len(sys.argv) == 3:
        check(sys.argv[2])
    else:
        raise SystemExit(__doc__)
