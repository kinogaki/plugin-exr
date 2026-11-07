//! Hostile EXRs, each made here and read as File ▸ Import reads one (on its
//! 2 MiB stack, within a budget): a clean error or warning, quickly.
use kinogaki_exr_format::OpenExr;
use kinogaki_exr_format::exr::cryptomatte::Builder;
use kinogaki_exr_format::exr::{
    self, Channel, ChannelSamples, Compression, Exr, Header, Part, WriteOptions,
};
use kinogaki_studio_api::format::ReadLimits;
use kinogaki_studio_api::{Error, FileFormat, Imported, Input};
use kinogaki_studio_api_testing::read_hostile;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a read of a hostile file may take, in a debug build.
const QUICK: Duration = Duration::from_secs(10);

/// A budget of 64 MiB in all.
fn small() -> ReadLimits {
    ReadLimits {
        allocation: 64 << 20,
        total: 64 << 20,
        ..ReadLimits::default()
    }
}

fn read(input: &Input<'_>, limits: ReadLimits) -> Result<Imported, Error> {
    let (read, took) = read_hostile(&OpenExr, input, &OpenExr.options(), limits);
    assert!(took < QUICK, "{took:?}");
    read
}

fn read_bytes(bytes: &[u8], limits: ReadLimits) -> Result<Imported, Error> {
    read(&Input::memory("hostile.exr", bytes), limits)
}

/// An attribute as a header holds it.
fn attribute(out: &mut Vec<u8>, name: &str, kind: &str, value: &[u8]) {
    for text in [name, kind] {
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    }
    out.extend_from_slice(&(value.len() as i32).to_le_bytes());
    out.extend_from_slice(value);
}

/// A scanline file of one part: `channels` HALF channels over the window
/// `min..=max`, compressed as `compression` (its code), with `extra`
/// attributes; then `chunks` offsets, every one `offset`, and `tail`.
fn scanline(
    channels: usize,
    (min, max): ([i32; 2], [i32; 2]),
    compression: u8,
    extra: &[(String, &str, Vec<u8>)],
    chunks: usize,
    tail: &[u8],
) -> Vec<u8> {
    let mut file = 20_000_630u32.to_le_bytes().to_vec();
    file.extend_from_slice(&2u32.to_le_bytes());
    let mut list = Vec::new();
    for k in 0..channels {
        list.extend_from_slice(format!("c{k:05}\0").as_bytes());
        list.extend_from_slice(&1i32.to_le_bytes());
        list.extend_from_slice(&[0, 0, 0, 0]);
        list.extend_from_slice(&1i32.to_le_bytes());
        list.extend_from_slice(&1i32.to_le_bytes());
    }
    list.push(0);
    attribute(&mut file, "channels", "chlist", &list);
    attribute(&mut file, "compression", "compression", &[compression]);
    let window: Vec<u8> = [min[0], min[1], max[0], max[1]]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    attribute(&mut file, "dataWindow", "box2i", &window);
    attribute(&mut file, "displayWindow", "box2i", &window);
    attribute(&mut file, "lineOrder", "lineOrder", &[0]);
    for (name, kind, value) in extra {
        attribute(&mut file, name, kind, value);
    }
    file.push(0);
    let offset = (file.len() + chunks * 8) as u64;
    for _ in 0..chunks {
        file.extend_from_slice(&offset.to_le_bytes());
    }
    file.extend_from_slice(tail);
    file
}

/// A zlib stream cut after its one dynamic block's header, whose codes
/// are literal 0 (`0`) and the end (`1`): past its end a decoder's zero bits
/// read as literals, which once decoded on to whatever size was claimed.
fn cut_stream() -> Vec<u8> {
    let mut bits: Vec<bool> = Vec::new();
    let mut put = |value: u32, count: u32| bits.extend((0..count).map(|k| value >> k & 1 == 1));
    put(1, 1);
    put(2, 2);
    put(0, 5);
    put(0, 5);
    put(14, 4);
    for symbol in [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1] {
        put(u32::from(matches!(symbol, 1 | 18)), 3);
    }
    put(0, 1);
    put(1, 1);
    put(127, 7);
    put(1, 1);
    put(106, 7);
    put(0, 1);
    put(0, 1);
    let mut stream = vec![0x78, 0x01];
    stream.extend(
        bits.chunks(8).map(|byte| {
            (byte.iter().enumerate()).fold(0u8, |b, (k, &bit)| b | (u8::from(bit) << k))
        }),
    );
    stream
}

#[test]
fn parts_far_apart_are_refused_before_their_joined_window_is_made() {
    // Two one-pixel parts 2²⁰ apart: their joined window once asked for
    // 192 GiB.
    let pixel = |name: &str, at: i32| {
        let mut header = Header::new(1, 1, Compression::None);
        header.data_window = exr::Window {
            x_min: at,
            y_min: at,
            x_max: at,
            y_max: at,
        };
        header.display_window = header.data_window;
        header.name = Some(name.to_owned());
        Part {
            header,
            channels: vec![Channel {
                name: "R".into(),
                samples: ChannelSamples::F16(vec![0x3C00]),
                linear: false,
            }],
        }
    };
    let file = Exr {
        parts: vec![pixel("a", 0), pixel("b", 1 << 20)],
    };
    let bytes = exr::write(&file, &WriteOptions::default()).unwrap();
    let why = read_bytes(&bytes, ReadLimits::default()).unwrap_err();
    assert!(why.message().contains("together span"), "{why}");
}

#[test]
fn many_bands_of_many_channels_are_checked_before_their_rows() {
    // 4,096 channels in 40,000 one-line chunks, every offset the same: what
    // each band holds of every plane once took 3.5 GB of bookkeeping.
    let bytes = scanline(4096, ([0, 0], [0, 39_999]), 0, &[], 40_000, &[0; 16]);
    assert!(read_bytes(&bytes, ReadLimits::default()).is_err());
    let why = read_bytes(
        &bytes,
        ReadLimits {
            total: 16 << 20,
            ..small()
        },
    )
    .unwrap_err();
    assert!(why.message().contains("over the read's budget"), "{why}");
}

#[test]
fn a_window_whose_chunks_overflow_is_refused() {
    // 2³² × 2³² pixels in one-pixel tiles: counting the chunks overflowed.
    let mut bytes = scanline(
        1,
        ([i32::MIN, i32::MIN], [i32::MAX, i32::MAX]),
        0,
        &[],
        0,
        &[],
    );
    bytes[4..8].copy_from_slice(&(2u32 | 0x200).to_le_bytes());
    let end = bytes.len() - 1;
    let mut tiles = Vec::new();
    attribute(
        &mut tiles,
        "tiles",
        "tiledesc",
        &[1, 0, 0, 0, 1, 0, 0, 0, 0],
    );
    bytes.splice(end..end, tiles);
    let why = read_bytes(&bytes, ReadLimits::default()).unwrap_err();
    assert!(why.message().contains("hostile.exr"), "{why}");
    // As wide a scanline part: its pixels overflowed.
    let wide = scanline(4096, ([i32::MIN, 0], [i32::MAX, i32::MAX]), 0, &[], 0, &[]);
    assert!(read_bytes(&wide, ReadLimits::default()).is_err());
}

#[test]
fn a_hundred_thousand_attributes_read_in_linear_time() {
    // Each attribute once copied the metadata so far: 56 s.
    let extra: Vec<(String, &str, Vec<u8>)> = (0..100_000)
        .map(|k| (format!("a{k}"), "int", 7i32.to_le_bytes().to_vec()))
        .collect();
    // One chunk: line 0, two bytes, a half of 1.
    let chunk = [0, 0, 0, 0, 2, 0, 0, 0, 0, 0x3C];
    let bytes = scanline(1, ([0, 0], [0, 0]), 0, &extra, 1, &chunk);
    let read = read_bytes(&bytes, ReadLimits::default()).unwrap();
    let said = &read.warnings[0].message;
    assert!(said.ends_with(" and 98960 more"), "{said}");
}

#[test]
fn a_cut_zip_chunk_fails_at_its_end() {
    // One ZIP chunk of 16 lines 4,096 wide from a cut stream: once decoded
    // to its whole size in zeros (a 303-byte file reached 3.2 GB).
    let stream = cut_stream();
    let mut chunk = 0i32.to_le_bytes().to_vec();
    chunk.extend_from_slice(&(stream.len() as i32).to_le_bytes());
    chunk.extend_from_slice(&stream);
    let bytes = scanline(1, ([0, 0], [4095, 15]), 3, &[], 1, &chunk);
    let why = read_bytes(&bytes, ReadLimits::default()).unwrap_err();
    assert!(why.message().contains("does not decompress"), "{why}");
}

#[test]
fn a_part_past_the_budget_is_refused_before_its_planes() {
    let bytes = scanline(16, ([0, 0], [4095, 4095]), 0, &[], 4096, &[]);
    let why = read_bytes(&bytes, small()).unwrap_err();
    assert!(why.message().contains("over the read's budget"), "{why}");
}

/// A scratch folder of its own, gone when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Scratch {
        let name = format!("kinogaki-exr-{test}-{}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A Cryptomatte EXR at `path` whose manifest is the sidecar `sidecar`.
fn cryptomatte(path: &Path, sidecar: &str) {
    let mut builder = Builder::new(2, 1, "CryptoObject", 1).unwrap();
    builder.add_sample(0, 0, &["hero"], &[1.0], 1.0).unwrap();
    let part = builder.part((0, 0), Some(sidecar)).unwrap();
    let bytes = exr::write(&Exr { parts: vec![part] }, &WriteOptions::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_sidecar_that_is_a_pipe_or_a_device_is_never_read() {
    let scratch = Scratch::new("pipe");
    let fifo = scratch.0.join("fifo.json");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    std::os::unix::fs::symlink("/dev/zero", scratch.0.join("zero.json")).unwrap();
    for (file, sidecar) in [("pipe.exr", "fifo.json"), ("device.exr", "zero.json")] {
        let path = scratch.0.join(file);
        cryptomatte(&path, sidecar);
        let read = read(&Input::file(&path), ReadLimits::default()).unwrap();
        let said: Vec<&str> = read.warnings.iter().map(|w| w.message.as_str()).collect();
        assert!(
            said.iter()
                .any(|w| w.contains("never read") && w.ends_with(sidecar)),
            "{said:?}"
        );
    }
}

#[test]
fn a_sidecar_linked_out_of_the_folder_is_left_unresolved() {
    let scratch = Scratch::new("link");
    let inside = scratch.0.join("shot");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::write(scratch.0.join("secret.json"), b"{}").unwrap();
    std::os::unix::fs::symlink(scratch.0.join("secret.json"), inside.join("m.json")).unwrap();
    let path = inside.join("shot.exr");
    cryptomatte(&path, "m.json");
    let read = read(&Input::file(&path), ReadLimits::default()).unwrap();
    let said = &read.warnings.last().unwrap().message;
    assert!(
        said.contains("outside the folder") && said.ends_with("m.json"),
        "{said}"
    );
}
