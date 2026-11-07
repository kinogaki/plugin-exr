//! Times the codec reading and writing on as many threads as `THREADS`
//! says (the tests' workers; unset or 0, every processor):
//! `exr_bench read FILE [repeats]` and
//! `exr_bench write FILE [repeats] none|rle|zips|zip|piz|pxr24 [fast|small]`
//! (writing the file's own parts). Prints the best of the runs.

use kinogaki_exr_format::exr::{Compression, Level, ReadOptions, WriteOptions};
use kinogaki_studio_api_testing::workers;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let number =
        |i: usize, fallback: usize| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(fallback);
    let data = std::fs::read(&args[2]).expect("file");
    let threads = std::env::var("THREADS").ok().and_then(|t| t.parse().ok());
    workers::install(threads.unwrap_or(0));
    let repeats = number(3, 5);
    let mut best = f64::MAX;
    match args[1].as_str() {
        "read" => {
            let options = ReadOptions::default();
            for _ in 0..repeats {
                let start = Instant::now();
                let exr = kinogaki_exr_format::exr::read(&data, &options).unwrap();
                best = best.min(start.elapsed().as_secs_f64() * 1e3);
                std::hint::black_box(exr);
            }
            println!("read {}: best {best:.1} ms of {repeats}", args[2]);
        }
        "write" => {
            let mut exr = kinogaki_exr_format::exr::read(&data, &ReadOptions::default()).unwrap();
            let compression = match args[4].as_str() {
                "none" => Compression::None,
                "rle" => Compression::Rle,
                "zips" => Compression::Zips,
                "zip" => Compression::Zip,
                "piz" => Compression::Piz,
                _ => Compression::Pxr24,
            };
            for part in &mut exr.parts {
                part.header.compression = compression;
            }
            let level = match args.get(5).map(String::as_str) {
                Some("fast") => Level::Fast,
                Some("small") => Level::Small,
                _ => Level::Balanced,
            };
            let options = WriteOptions { level };
            let mut size = 0;
            for _ in 0..repeats {
                let start = Instant::now();
                let bytes = kinogaki_exr_format::exr::write(&exr, &options).unwrap();
                best = best.min(start.elapsed().as_secs_f64() * 1e3);
                size = bytes.len();
            }
            println!(
                "write {} as {}: best {best:.1} ms of {repeats}, {size} bytes",
                args[2], args[4]
            );
        }
        _ => panic!("read or write"),
    }
}
