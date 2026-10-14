//! Reproducible, warm-cache decode benchmark: decode_bench <photo> [runs].
use fastcull::{
    arw::ArwInfo,
    imaging::{Level, decode_level},
};
use std::{path::PathBuf, time::Instant};
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().expect("photo path"));
    let runs: usize = args
        .next()
        .and_then(|s| s.to_str()?.parse().ok())
        .unwrap_or(7);
    anyhow::ensure!((1..=1000).contains(&runs), "runs must be 1..1000");
    let t = Instant::now();
    let info = ArwInfo::read(&path)?;
    println!("metadata_ms={:.3}", t.elapsed().as_secs_f64() * 1000.);
    for level in [Level::Thumb, Level::Fit(2560), Level::Full] {
        let mut times = Vec::new();
        for run in 0..=runs {
            let t = Instant::now();
            let pixels = decode_level(&path, &info, level)?;
            let ms = t.elapsed().as_secs_f64() * 1000.;
            println!(
                "{level:?} run={run} ms={ms:.3} size={}x{}",
                pixels.width, pixels.height
            );
            std::hint::black_box(&pixels);
            if run > 0 {
                times.push(ms);
            }
        }
        times.sort_by(f64::total_cmp);
        println!("BENCH {level:?} median_ms={:.3}", times[runs / 2]);
    }
    Ok(())
}
