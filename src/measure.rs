//! Per-channel frequency, duty cycle and pulse statistics.

use crate::output::Event;
use crate::stream::Edge;
use crate::units::format_time;

#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    pub ch: u8,
    pub edges: u64,
    pub freq_hz: Option<f64>,
    pub duty: Option<f64>,
    pub min_pulse_s: Option<f64>,
    pub max_pulse_s: Option<f64>,
    /// Static level when the channel never changed; None if it toggled or saw no data.
    pub level: Option<bool>,
}

#[derive(Clone, Copy, Default)]
struct St {
    level: Option<bool>,
    last: Option<u64>,
    edges: u64,
    first_rise: Option<u64>,
    last_rise: Option<u64>,
    rises: u64,
    high_since: Option<u64>,
    pending_high: u64,
    high: u64,
    min_pulse: Option<u64>,
    max_pulse: Option<u64>,
}

pub struct Measure {
    mask: u16,
    rate: f64,
    st: [St; 16],
}

impl Measure {
    pub fn new(mask: u16, rate: f64) -> Self {
        Measure {
            mask,
            rate,
            st: [St::default(); 16],
        }
    }

    pub fn feed(&mut self, edges: &[Edge]) {
        for e in edges {
            let s = &mut self.st[e.ch as usize];
            let Some(prev) = s.level else {
                s.level = Some(e.level);
                continue;
            };
            if prev == e.level {
                continue;
            }
            s.edges += 1;
            if let Some(l) = s.last {
                let d = e.sample - l;
                s.min_pulse = Some(s.min_pulse.map_or(d, |m| m.min(d)));
                s.max_pulse = Some(s.max_pulse.map_or(d, |m| m.max(d)));
            }
            s.last = Some(e.sample);
            if e.level {
                if s.first_rise.is_none() {
                    s.first_rise = Some(e.sample);
                } else {
                    s.high = s.pending_high;
                }
                s.last_rise = Some(e.sample);
                s.rises += 1;
                s.high_since = Some(e.sample);
            } else if let Some(h) = s.high_since.take() {
                s.pending_high += e.sample - h;
            }
            s.level = Some(e.level);
        }
    }

    pub fn finish(&self) -> Vec<Stats> {
        (0..16u8)
            .filter(|c| self.mask >> c & 1 == 1)
            .map(|ch| {
                let s = &self.st[ch as usize];
                let span = match (s.first_rise, s.last_rise) {
                    (Some(a), Some(b)) if s.rises >= 2 => Some((b - a) as f64),
                    _ => None,
                };
                Stats {
                    ch,
                    edges: s.edges,
                    freq_hz: span.map(|sp| (s.rises - 1) as f64 * self.rate / sp),
                    duty: span.map(|sp| s.high as f64 / sp),
                    min_pulse_s: s.min_pulse.map(|p| p as f64 / self.rate),
                    max_pulse_s: s.max_pulse.map(|p| p as f64 / self.rate),
                    level: if s.edges == 0 { s.level } else { None },
                }
            })
            .collect()
    }

    pub fn events(stats: &[Stats]) -> Vec<Event> {
        stats
            .iter()
            .map(|s| {
                let text = match (s.edges, s.level) {
                    (0, Some(l)) => {
                        format!("ch{} static {}", s.ch, if l { "high" } else { "low" })
                    }
                    (0, None) => format!("ch{} no data", s.ch),
                    _ => {
                        let mut t = String::new();
                        if let (Some(f), Some(d)) = (s.freq_hz, s.duty) {
                            t = format!("{f:.3} Hz duty {:.1}% ", d * 100.0);
                        }
                        let p = |v: Option<f64>| v.map_or("-".into(), format_time);
                        format!(
                            "ch{} {t}edges {} pulse min {} max {}",
                            s.ch,
                            s.edges,
                            p(s.min_pulse_s),
                            p(s.max_pulse_s)
                        )
                    }
                };
                Event::new(0, "measure", "channel", text)
                    .with("ch", s.ch)
                    .with("edges", s.edges)
                    .with("freq_hz", s.freq_hz)
                    .with("duty", s.duty)
                    .with("min_pulse_s", s.min_pulse_s)
                    .with("max_pulse_s", s.max_pulse_s)
                    .with("level", s.level)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(ch: u8, period: u64, high: u64, n: u64) -> Vec<Edge> {
        let mut v = vec![Edge {
            sample: 0,
            ch,
            level: false,
        }];
        for k in 0..n {
            v.push(Edge {
                sample: 100 + k * period,
                ch,
                level: true,
            });
            v.push(Edge {
                sample: 100 + k * period + high,
                ch,
                level: false,
            });
        }
        v
    }

    #[test]
    fn square_wave_stats() {
        let mut m = Measure::new(0b11, 1e6);
        let mut edges = square(0, 1000, 300, 10);
        edges.push(Edge {
            sample: 0,
            ch: 1,
            level: true,
        });
        edges.sort_by_key(|e| (e.sample, e.ch));
        m.feed(&edges);
        let s = m.finish();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].edges, 20);
        assert!((s[0].freq_hz.unwrap() - 1000.0).abs() < 1e-9);
        assert!((s[0].duty.unwrap() - 0.3).abs() < 1e-9);
        assert!((s[0].min_pulse_s.unwrap() - 300e-6).abs() < 1e-12);
        assert!((s[0].max_pulse_s.unwrap() - 700e-6).abs() < 1e-12);
        assert_eq!(s[0].level, None);
        assert_eq!(s[1].edges, 0);
        assert_eq!(s[1].level, Some(true));
        assert_eq!(s[1].freq_hz, None);
    }

    #[test]
    fn batch_split_is_invariant() {
        let edges = square(0, 50, 25, 20);
        let mut a = Measure::new(1, 1e6);
        a.feed(&edges);
        let mut b = Measure::new(1, 1e6);
        for e in &edges {
            b.feed(std::slice::from_ref(e));
        }
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn events_text() {
        let mut m = Measure::new(0b1001, 1e6);
        m.feed(&square(0, 1000, 500, 3));
        let ev = Measure::events(&m.finish());
        assert!(ev[0].text.starts_with("ch0 1000.000 Hz duty 50.0% edges 6"));
        assert_eq!(ev[1].text, "ch3 no data");
    }
}
