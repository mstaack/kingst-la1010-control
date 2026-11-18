//! Throughput check (run in release): the capture path for a dense 2 ch x 100 MHz stream
//! (deinterleave, decode, write .sr) must be well faster than real time, or the capture loop
//! stalls the device.
//!   cargo test --release --test perf -- --ignored

use std::time::Instant;

use la1010::decode::{Spec, build};
use la1010::export;
use la1010::output::Printer;
use la1010::pipeline::Pipeline;
use la1010::stream::Deinterleaver;
use la1010::window::Window;

/// One 50 ms transfer: ch0 toggles every 8 samples, ch1 every 9.
fn transfer(first_word: u64, words: u64) -> Vec<u8> {
    let mut raw = Vec::with_capacity(words as usize * 4);
    for w in first_word..first_word + words {
        for period in [8u64, 9] {
            let word = (0..16).fold(0u16, |acc, i| {
                acc | (u16::from((w * 16 + i) / period % 2 == 1) << i)
            });
            raw.extend_from_slice(&word.to_le_bytes());
        }
    }
    raw
}

#[test]
#[ignore = "timing test; run with --release"]
fn sr_capture_2ch_100mhz_faster_than_realtime() {
    let dir = tempfile::tempdir().unwrap();
    let rate = 100e6;
    let words = 5_000_000 / 16; // 50 ms per transfer, as the capture loop delivers them
    let transfers: Vec<Vec<u8>> = (0..20).map(|t| transfer(t * words, words)).collect();
    let decoder = build(&Spec::parse("uart:rx=0,baud=115200").unwrap(), rate).unwrap();
    let t0 = Instant::now();
    let mut p = Pipeline::new(
        Window::new(0b11, None, 0, u64::MAX),
        vec![decoder],
        Printer::new(std::io::sink(), false, rate),
    )
    .writer(export::create(&dir.path().join("dense.sr"), 0b11, rate).unwrap());
    let mut d = Deinterleaver::new(0b11);
    let mut edges = Vec::new();
    for raw in &transfers {
        edges.clear();
        d.push(raw, &mut edges);
        assert!(p.push(&edges, d.samples()).unwrap());
    }
    p.finish(d.samples()).unwrap();
    let dt = t0.elapsed().as_secs_f64();
    assert_eq!(d.samples(), 100_000_000);
    assert!(dt < 0.5, "1 s of 2 ch x 100 MHz took {dt:.2} s to capture");
}
