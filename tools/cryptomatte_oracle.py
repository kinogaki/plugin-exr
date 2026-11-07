#!/usr/bin/env python3
"""Cryptomatte oracle for the OpenEXR plugin's codec, run locally (Python OpenEXR and
numpy; never a dependency of the library). Mattes come straight from the
channels as OpenEXR reads them, as Psyop's specification defines them:
for each rank (ID, coverage) pair, the coverage where the ID's float bits
equal the name's (manifest hash, exponent 0/255 remapped), summed, clipped.

  cryptomatte_oracle.py expected    write tests/fixtures/
                                    cryptomatte/expected.txt: for every
                                    layer of every Psyop fixture and every
                                    manifest name (by its index in the
                                    manifest), the matte's nonzero pixel
                                    count and sum, tab-separated
  cryptomatte_oracle.py check DIR   compare every pixel of the mattes the
                                    Rust tests wrote (KINOGAKI_CRYPTOMATTE_
                                    ORACLE_DIR: FILE~LAYER~INDEX.exr, the
                                    INDEX-th manifest name)
"""
import json
import re
import sys
from pathlib import Path

import numpy as np
import OpenEXR

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / 'tests/fixtures/cryptomatte'
PAIR = {'R': 'G', 'r': 'g', 'red': 'green', 'B': 'A', 'b': 'a', 'blue': 'alpha'}


def layers(path):
    image = OpenEXR.File(str(path), separate_channels=True)
    header, channels = image.header(), image.channels()
    for key, value in header.items():
        if not (key.startswith('cryptomatte/') and key.endswith('/name')):
            continue
        prefix = key[:-4]
        if prefix + 'manifest' in header:
            manifest = json.loads(header[prefix + 'manifest'])
        else:
            manifest = json.loads((path.parent / header[prefix + 'manif_file']).read_text())
        yield value, manifest, channels


def matte(layer, channels, hash_hex):
    bits = int(hash_hex, 16)
    if (bits >> 23) & 255 in (0, 255):
        bits ^= 1 << 23
    total = None
    for name, channel in channels.items():
        if not re.fullmatch(re.escape(layer) + r'\d\d+\.(R|r|red|B|b|blue)', name):
            continue
        partner = channels[name.rsplit('.', 1)[0] + '.' + PAIR[name.rsplit('.', 1)[1]]].pixels
        part = np.where(channel.pixels.view(np.uint32) == bits, partner, 0).astype(np.float64)
        total = part if total is None else total + part
    return np.clip(total, 0, 1)


def expected():
    lines = []
    for path in sorted(FIXTURES.glob('*.exr')):
        for layer, manifest, channels in layers(path):
            for index, hash_hex in enumerate(manifest.values()):
                m = matte(layer, channels, hash_hex)
                lines.append(f'{path.name}\t{layer}\t{index}\t{int((m > 0).sum())}\t{float(m.sum())!r}')
    (FIXTURES / 'expected.txt').write_text('\n'.join(lines) + '\n', encoding='utf-8')
    print(f'{len(lines)} mattes')


def check(directory):
    count = 0
    for out in sorted(Path(directory).glob('*.exr')):
        name, layer, index = out.stem.split('~')
        for found, manifest, channels in layers(FIXTURES / name):
            if found != layer:
                continue
            want = matte(layer, channels, list(manifest.values())[int(index)])
            got = OpenEXR.File(str(out), separate_channels=True).channels()['Y'].pixels
            np.testing.assert_allclose(got, want, rtol=0, atol=6e-8)
            count += 1
    print(f'PASS {count} full mattes against OpenEXR\'s reading of the Psyop fixtures')


if __name__ == '__main__':
    if sys.argv[1:2] == ['expected']:
        expected()
    elif sys.argv[1:2] == ['check'] and len(sys.argv) == 3:
        check(sys.argv[2])
    else:
        raise SystemExit(__doc__)
