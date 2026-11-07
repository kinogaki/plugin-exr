//! Times Cryptomatte, for docs/BENCHMARKS.md:
//! `cryptomatte_bench make OUT.exr` writes a 3840×2160 layer of 6 ranks
//! (`CryptoObject`, 2 000 objects in overlapping tiles);
//! `cryptomatte_bench time FILE [repeats]` times opening it, a matte of
//! one name and of ten, and 100 000 picks.
use kinogaki_exr_format::exr::Compression;
use kinogaki_exr_format::exr::cryptomatte::{Builder, Cryptomatte};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = std::path::Path::new(&args[2]);
    if args[1] == "make" {
        let (width, height) = (3840, 2160);
        let mut builder = Builder::new(width, height, "CryptoObject", 3).unwrap();
        let names: Vec<String> = (0..2000).map(|i| format!("/scene/object{i}")).collect();
        for y in 0..height {
            for x in 0..width {
                let a = (x / 37 + (y / 29) * 104) % 2000;
                let b = (x / 53 + (y / 41) * 73 + 7) % 2000;
                let c = (x * 7 + y * 3) % 2000;
                let edge = ((x % 37) as f64 / 37.0).min(0.9);
                builder
                    .add_sample(
                        x,
                        y,
                        &[&names[a], &names[b], &names[c]],
                        &[0.9 - edge * 0.5, edge * 0.5, 0.05],
                        1.0,
                    )
                    .unwrap();
            }
        }
        builder.save(path, Compression::Zip, (0, 0), None).unwrap();
        return;
    }
    let repeats: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5);
    let best = |f: &mut dyn FnMut()| {
        let mut best = f64::MAX;
        for _ in 0..repeats {
            let start = Instant::now();
            f();
            best = best.min(start.elapsed().as_secs_f64() * 1e3);
        }
        best
    };
    let mut layer = None;
    let open = best(&mut || layer = Some(Cryptomatte::open(path, "CryptoObject", true).unwrap()));
    let layer = layer.unwrap();
    let one = best(&mut || {
        drop(std::hint::black_box(
            layer.extract(&["/scene/object5"], false).unwrap(),
        ))
    });
    let ten: Vec<String> = (0..10)
        .map(|i| format!("/scene/object{}", i * 101))
        .collect();
    let ten: Vec<&str> = ten.iter().map(String::as_str).collect();
    let many = best(&mut || drop(std::hint::black_box(layer.extract(&ten, false).unwrap())));
    let ids = best(&mut || {
        for i in 0..100_000usize {
            std::hint::black_box(layer.pick(i * 37 % 3840, i * 11 % 2160, 0).unwrap());
        }
    });
    let picks = best(&mut || {
        for i in 0..100_000usize {
            std::hint::black_box(layer.pick_name(i * 37 % 3840, i * 11 % 2160, 0).unwrap());
        }
    });
    println!(
        "open {open:.1} ms, extract one {one:.1} ms, extract ten {many:.1} ms, 100000 pick {ids:.1} ms, 100000 pick_name {picks:.1} ms"
    );
}
