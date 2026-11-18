//! Hardware tests. Run only with the analyzer attached and the user's OK:
//!   cargo test --test hw -- --ignored --test-threads=1

use std::sync::atomic::AtomicBool;

use la1010::capture::{Setup, stream};
use la1010::measure::Measure;
use la1010::stream::Deinterleaver;

#[test]
#[ignore = "needs the LA1010 attached"]
fn identifies_a2() {
    let o = la1010::usb::open(None).unwrap();
    assert_eq!(o.id.revision, "a2");
    assert_eq!(o.id.manufactured.as_deref(), Some("2024-03"));
}

#[test]
#[ignore = "needs a 1 kHz square on CH0 (RTC1002 P0) and the user's OK"]
fn square_on_ch0_at_1mhz() {
    let o = la1010::usb::open(None).unwrap();
    let mut d = Deinterleaver::new(1);
    let mut m = Measure::new(1, 1e6);
    let mut edges = Vec::new();
    let stop = AtomicBool::new(false);
    let setup = Setup {
        mask: 1,
        divider: 800,
        rate: 1e6,
        threshold: 1.4,
    };
    stream(&o.usb, &setup, &stop, |b| {
        edges.clear();
        d.push(b, &mut edges);
        m.feed(&edges);
        Ok(d.samples() < 1_000_000)
    })
    .unwrap();
    let s = &m.finish()[0];
    assert!((s.freq_hz.unwrap() - 1000.0).abs() < 0.5, "{s:?}");
    assert!((s.min_pulse_s.unwrap() - 500e-6).abs() <= 1.5e-6, "{s:?}");
    assert!((s.max_pulse_s.unwrap() - 500e-6).abs() <= 1.5e-6, "{s:?}");
}
