# The OpenEXR codec

`src/exr/` reads and writes OpenEXR files into and from parts of channel planes, with Cryptomatte 1.2 in `src/exr/cryptomatte/`. It knows nothing of the SDK but the process's workers its chunks decompress and compress on (`parallel.rs`).

## Coverage

- Scanline and tiled parts (one level), multi-part files, read and written; increasing, decreasing or random line order read (writes are increasing).
- Compression, read and written: none, RLE, ZIPS, ZIP, PIZ, PXR24.
- UINT, HALF and FLOAT channels at any data window (negative origins too), the `pLinear` flag, long names.
- Every header attribute kept: the structural ones typed, the rest raw in file order, string attributes readable and settable.
- Conversion to and from lib-image's interleaved `Image`.

Cryptomatte 1.2 (`exr::cryptomatte`): layer discovery, layers of FLOAT quadruples (R/G/B/A in three spellings), mattes of names or exact IDs across every rank with inversion, picking by ID and name, manifests embedded or in sidecar files (lexically checked relative paths, 64 MiB limit), manifest writing, and the weighted-sample builder writing a layer (manifest embedded or sidecar).

Not supported: deep data, mipmap and ripmap levels, subsampled channels (x/y sampling other than 1: luminance-chroma files), B44/B44A, DWAA/DWAB, HTJ2K and ZSTD.

## Files

| File | Responsibility |
| --- | --- |
| `mod.rs` | `Exr`, `Part`, `Header`, `Channel`, `ChannelSamples` (U32, F16 bits, F32 planes), `Window`, `Tiles`, `Compression`, options |
| `error.rs` | `Error`: `Corrupt { what, at }`, `Unsupported`, `Invalid`, `TooLarge`, `Io` |
| `header.rs` | magic and version flags, attributes, the channel list, the interpreted attributes (everything else kept raw, in order), header writing |
| `read.rs` | headers of every part, offset tables, chunk validation (part number, y or tile coordinates), decoding in bands of rows on the workers straight into the channel planes |
| `write.rs` | validation, headers, offset tables, chunks built and compressed on the workers, written in increasing y |
| `image.rs` | `Part::to_image` / `from_image` (RGB(A), Y(A) or a lone channel), sample conversions |
| `parallel.rs` | an ordered map on the process's workers |
| `compression/mod.rs` | chunk codec dispatch; raw-size chunks are stored raw |
| `compression/zip.rs` | the byte predictor ZIP, ZIPS and RLE share (split halves, deltas); the running sum eight bytes at a time |
| `compression/rle.rs` | run-length coding |
| `compression/piz.rs` | PIZ: value bitmap and dense renumbering, the 14/16-bit Haar wavelet, canonical Huffman over up to 58-bit codes (14-bit lookup table) with a run symbol; the encoder builds real Huffman codes, as OpenEXR does |
| `compression/pxr24.rs` | PXR24: floats to 24 bits as OpenEXR rounds them, horizontal deltas, byte planes, zlib |
| `cryptomatte/mod.rs` | layer discovery, `Cryptomatte` (ranks as ID/coverage planes moved out of the part, manifest), mattes summed in f64 on the workers, picking, `Matte`, sidecar path checks and reading |
| `cryptomatte/hash.rs` | MurmurHash3_x86_32 (seed 0), the ID bit remapping, metadata keys |
| `cryptomatte/manifest.rs` | the bounded strict JSON manifest parser and writer, name and ID indexes |
| `cryptomatte/builder.rs` | weighted samples per pixel and ID, ranking by weight, the layer's part and file (manifest embedded or sidecar) |

A part's channels are planes over the data window. Reading splits every plane into bands (a scanline chunk, or a row of tiles), and each band's task owns its rows of every plane: chunks decode in parallel with no copy after. Writing does the inverse per chunk. PIZ and PXR24 follow OpenEXR's algorithms (BSD-3-Clause, [LICENSE-OpenEXR](../LICENSE-OpenEXR)); every other part follows the OpenEXR file layout documentation.

## Tests and oracles

- `tests/openexr.rs`: 12 files written by OpenEXR 3.5 itself (`tools/exr_oracle.py fixtures`: every compression scanline, four tiled, PIZ with over 16 384 distinct words, a multi-part file) read to exactly the pattern the oracle wrote (PXR24 floats to 24 bits); every compression round-trips scanline and tiled at 1×1, 67×45 and 160×120 on several worker counts; multi-part, negative data windows, string attributes and names past 31 bytes round-trip; every truncation of six files fails and 1 800 mutated files never panic. `tools/exr_oracle.py check DIR` reads the files those tests write (`KINOGAKI_EXR_ORACLE_DIR=DIR cargo test --test openexr`) with OpenEXR.
- `tests/cryptomatte.rs`: the specification's hash vectors; builder cases (ranking, duplicate selections, inversion, background, truncation without renormalizing); file and sidecar round trips, refused sidecar paths; and Psyop's three sample files (five layers, one sidecar manifest, unusual names): all 425 manifest names' mattes match OpenEXR's channels (`tests/fixtures/cryptomatte/expected.txt`, from `tools/cryptomatte_oracle.py expected`; `check DIR` compares every pixel).
- `tests/format.rs` reads a committed subset of openexr-images as OpenEXR reads them (`tools/images_oracle.py`).

The oracles run locally (OpenEXR's Python bindings and numpy in a venv) and are never dependencies.
