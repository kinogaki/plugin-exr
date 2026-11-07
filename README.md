# OpenEXR

OpenEXR, with Cryptomatte, as a format plugin for [Kinogaki Studio](https://github.com/kinogaki): an EXR file reads as one standard Image of the public [studio-sdk](https://github.com/kinogaki/studio-sdk), every part's channels its layers, and an Image writes as one EXR. The codec is this crate's own, `src/exr/` ([docs/CODEC.md](docs/CODEC.md)): scanline and tiled, multi-part, every lossless compression and PXR24, HALF, FLOAT and UINT channels, every attribute, Cryptomatte 1.2, its chunks on the process's workers. The format converts no color: the host resolves what a file says of its colors.

The library crate is `kinogaki-exr-format`; `plugin/` (`kinogaki-plugin-exr`) is the dylib Studio loads, `formats/exr.dylib`. Package `kinogaki.exr`, format `kinogaki.format.exr`.

## Reading

A multi-part file reads as one Image (the parts' data windows joined, the first part's display window and pixel aspect):
- a channel `<layer>.<name>` is `<layer>`'s; one named alone is its part's layer: the main one (`""`) in a file of one part, else the layer of the output its part is named after (`beauty` is `""`, `albedo` is `albedo`). A layer holds at most four channels: past the colors, a main layer's `Z` is `depth` and another channel a layer of its own. Colors come first in a layer (`R G B`, then `X Y Z`, the rest by name, `A` last);
- samples stay as stored (half, float, integer); a layer mixing them is read as floats;
- a layer is color when its part says its space (`colorspace` as a name, else `chromaticities` as primaries; the tag), or, in a file saying none, when its channels are colors (`R G B A`, `Y`); Cryptomatte ranks and integers are data; colors are premultiplied, as OpenEXR has them;
- every header attribute is the Image's metadata (a later part's differing one under `<part>/<name>`), Cryptomatte's keys among them; a name an attribute's can't be is held escaped and written back as it was (`ColorSpace/Blue Primary` as `_ColorSpace/Blue:20Primary`: `_`, then each character a name can't hold, and each `:`, as `:` and its bytes in hex); a manifest in a sidecar file (`manif_file`) is read from beside the file as its `manifest`, through the SDK's `format::files::Named` (in the file's folder or below, a regular file, each file once, 64 MiB of manifests in all), and said left unresolved for a file in memory.

A file is read as the SDK's policy for untrusted files says (Studio's `docs/plugins/FORMAT.md`): a part's size and the parts' joined window are checked against the read budget's pixels (2²⁸ by default) before anything is made of them, the planes and every layer's tiles are charged to the read's budget before they are allocated, and every chunk polls the read's cancellation.

## Writing

Option `parts`: a part a layer (`layers`, the default), named by its output (`""` is `beauty`), a Cryptomatte output's preview and rank sets one part with its four keys (a sidecar's manifest written in the file); or one part of `<layer>.<name>` channels (`one`). Option `compression`: none, RLE, ZIPS, ZIP (the default), PIZ, PXR24. A color layer's part carries its tag: `colorspace` its space's name, `chromaticities` its primaries or the Image's. Every other attribute of the metadata goes on the first part, as the EXR attribute its type is. Half, float and integer samples are written as held, 8 and 16-bit ones as floats. An ICC profile, attributes no EXR attribute holds and further images are left out, each said once.

## Code

| File | Role |
|---|---|
| `src/lib.rs` | The package and the format |
| `src/read.rs` | An EXR's parts to one Image's layers |
| `src/write.rs` | One Image to an EXR's parts, and the writer's options |
| `src/metadata.rs` | Header attributes to the Image's metadata and back, Cryptomatte's sidecar manifests |
| `src/tiles.rs` | Channel planes to an Image's tiles on the workers, and back |
| `src/exr/` | The codec and Cryptomatte, knowing no SDK but the process's workers and the read budget ([docs/CODEC.md](docs/CODEC.md)) |

## Tests and data

- `tests/openexr.rs`, `tests/cryptomatte.rs`: the codec against OpenEXR's own files and Psyop's Cryptomatte samples ([docs/CODEC.md](docs/CODEC.md#tests-and-oracles)).
- `tests/hostile.rs`: hostile files made in code, read as Import reads them, each a clean error or warning within seconds: parts far apart, 4,096 channels in 40,000 bands, windows whose chunk counts overflow, 100,000 attributes, a cut ZIP chunk, a part past a small budget, sidecars that are a pipe, a device or a link out of the folder.
- `tests/format.rs`: a committed subset of openexr-images reads as OpenEXR reads it (`tools/images_oracle.py`), luminance-chroma and multi-resolution files refused naming the file; a render's frame (beauty, albedo, depth, object ids, Cryptomatte with its manifest) written a part an output and as one part, and read back as itself; parts of other windows joined, the same on any number of workers; a sidecar manifest read beside its file; damaged and truncated files fail without panicking.
- `examples/exr_bench.rs`, `examples/cryptomatte_bench.rs` time the codec.

Data, each with its license: `tests/fixtures/openexr` written by OpenEXR's Python bindings from a synthetic pattern (`tools/exr_oracle.py`); `tests/fixtures/openexr-images` seven files of the openexr-images repository (github.com/AcademySoftwareFoundation/openexr-images), BSD 3-Clause, Copyright (c) 2004, Industrial Light & Magic ([LICENSE](tests/fixtures/openexr-images/LICENSE)); `tests/fixtures/cryptomatte` three of Psyop's Cryptomatte sample images (github.com/Psyop/Cryptomatte, revision 968d5e4b6171e29ba5f89d554117132a164e747e, `sample_images/debug_images/`), BSD 3-Clause, Copyright (c) 2014-2016 Psyop Media Company, LLC ([LICENSE](tests/fixtures/cryptomatte/LICENSE)). The oracles run locally (OpenEXR's Python bindings and numpy) and are never dependencies.

PIZ and PXR24 implement OpenEXR's algorithms, written from its description and behaviour; OpenEXR's BSD-3-Clause notice is retained in [LICENSE-OpenEXR](LICENSE-OpenEXR) (Copyright Contributors to the OpenEXR Project).

## Building

The format builds from this repository, the public studio-sdk and lib-image (its interleaved `Image` and DEFLATE), checked out side by side as Studio's tree lays them out (`sdk/`, `libs/lib-image/` and `plugins/exr/`), with the compiler the SDK pins (`sdk/rust-toolchain.toml`):

```sh
cd plugins/exr
cargo test
```

`plugin/` links Studio's shared library (`kinogaki_shared`), so it is built in Studio's tree, or against a Studio release's plugin kit as the SDK's guides describe. Studio's tree also runs the image formats' workflows (`workflows/`).

## License

Dual-licensed under the MIT license ([LICENSE-MIT](LICENSE-MIT)) and the Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), at your option; the OpenEXR notice above applies to PIZ and PXR24.
