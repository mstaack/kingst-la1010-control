//! Protocol decoders. Each consumes sorted edges (see `stream::Edge`) and emits events.

pub mod i2c;
pub mod spi;
pub mod uart;
pub mod usb;

use std::str::FromStr;

use crate::error::{Error, Result};
use crate::output::Event;
use crate::stream::Edge;
use crate::units::{MAX_RATE, parse_si};

pub trait Decoder {
    fn feed(&mut self, edges: &[Edge], out: &mut Vec<Event>);
    /// `end`: last sample of the capture (exclusive).
    fn finish(&mut self, end: u64, out: &mut Vec<Event>);
}

pub struct OptInfo {
    pub name: &'static str,
    pub help: &'static str,
    pub default: Option<&'static str>,
    pub required: bool,
    pub channel: bool,
}

pub struct Info {
    pub name: &'static str,
    pub help: &'static str,
    pub opts: &'static [OptInfo],
}

pub fn catalog() -> Vec<&'static Info> {
    vec![&uart::INFO, &i2c::INFO, &spi::INFO, &usb::INFO]
}

pub fn info(name: &str) -> Result<&'static Info> {
    catalog()
        .into_iter()
        .find(|i| i.name == name)
        .ok_or_else(|| {
            let names: Vec<_> = catalog().iter().map(|i| i.name).collect();
            Error::Usage(format!(
                "unknown decoder '{name}' (available: {})",
                names.join(", ")
            ))
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub name: String,
    pub opts: Vec<(String, String)>,
}

impl Spec {
    /// "uart:rx=0,baud=115200,invert". Validated against the catalog.
    pub fn parse(s: &str) -> Result<Spec> {
        let (name, rest) = s.split_once(':').unwrap_or((s, ""));
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Usage(format!(
                "decoder spec '{s}': missing name (example: uart:rx=0,baud=115200)"
            )));
        }
        let opts = rest
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once('=') {
                Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
                None => (p.to_string(), String::new()),
            })
            .collect();
        let spec = Spec {
            name: name.to_string(),
            opts,
        };
        spec.check(info(&spec.name)?)?;
        Ok(spec)
    }

    fn check(&self, info: &Info) -> Result<()> {
        for (k, _) in &self.opts {
            if !info.opts.iter().any(|o| o.name == k) {
                let known: Vec<_> = info.opts.iter().map(|o| o.name).collect();
                return Err(self.usage(format!(
                    "unknown option '{k}' (options: {})",
                    known.join(", ")
                )));
            }
        }
        for o in info.opts.iter().filter(|o| o.required) {
            if self.get(o.name).is_none() {
                return Err(self.usage(format!("missing required option {}=", o.name)));
            }
        }
        for o in info.opts.iter().filter(|o| o.channel) {
            self.ch(o.name)?;
        }
        Ok(())
    }

    pub fn usage(&self, msg: impl std::fmt::Display) -> Error {
        Error::Usage(format!("{}: {msg}", self.name))
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.opts
            .iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    pub fn flag(&self, k: &str) -> bool {
        matches!(self.get(k), Some("" | "1" | "true" | "yes"))
    }

    pub fn ch(&self, k: &str) -> Result<Option<u8>> {
        self.get(k)
            .map(|v| {
                v.parse::<u8>()
                    .ok()
                    .filter(|c| *c < 16)
                    .ok_or_else(|| self.usage(format!("{k}={v} is not a channel 0-15")))
            })
            .transpose()
    }

    pub fn num<T: FromStr>(&self, k: &str, default: T) -> Result<T> {
        match self.get(k) {
            None => Ok(default),
            Some(v) => v
                .parse()
                .map_err(|_| self.usage(format!("invalid {k}={v}"))),
        }
    }

    pub fn channel_mask(&self) -> Result<u16> {
        let mut m = 0;
        for o in info(&self.name)?.opts.iter().filter(|o| o.channel) {
            if let Some(c) = self.ch(o.name)? {
                m |= 1 << c;
            }
        }
        Ok(m)
    }
}

pub fn build(spec: &Spec, rate: f64) -> Result<Box<dyn Decoder>> {
    Ok(match spec.name.as_str() {
        "uart" => Box::new(uart::Uart::new(spec, rate)?),
        "i2c" => Box::new(i2c::I2c::new(spec)?),
        "spi" => Box::new(spi::Spi::new(spec)?),
        "usb" => Box::new(usb::Usb::new(spec, rate)?),
        n => return Err(Error::Usage(format!("unknown decoder '{n}'"))),
    })
}

/// Sample rate to use when the user gives none.
pub fn recommended_rate(spec: &Spec) -> Result<f64> {
    let r = match spec.name.as_str() {
        "uart" => match spec.get("baud") {
            None | Some("auto") => 10e6,
            Some(b) => 16.0 * parse_si(b).ok_or_else(|| spec.usage(format!("invalid baud={b}")))?,
        },
        "i2c" => 10e6,
        "spi" => 25e6,
        "usb" => {
            if matches!(spec.get("speed"), Some("ls" | "low")) {
                24e6
            } else {
                100e6
            }
        }
        n => return Err(Error::Usage(format!("unknown decoder '{n}'"))),
    };
    Ok(r.clamp(1e6, MAX_RATE))
}

/// Line levels with all edges of one sample applied together.
#[derive(Default)]
pub struct Lines {
    levels: u16,
    known: u16,
}

pub struct Step {
    pub sample: u64,
    pub before: u16,
    pub after: u16,
    /// Channels whose level was known before this sample.
    pub known: u16,
    /// Channels with an edge at this sample.
    pub touched: u16,
}

impl Step {
    pub fn high(&self, ch: u8) -> bool {
        self.after >> ch & 1 == 1
    }
    pub fn was_high(&self, ch: u8) -> bool {
        self.before >> ch & 1 == 1
    }
    pub fn was_known(&self, ch: u8) -> bool {
        self.known >> ch & 1 == 1
    }
    pub fn touched(&self, ch: u8) -> bool {
        self.touched >> ch & 1 == 1
    }
    /// First level ever seen for `ch`.
    pub fn first(&self, ch: u8) -> bool {
        self.touched(ch) && !self.was_known(ch)
    }
    pub fn rose(&self, ch: u8) -> bool {
        self.was_known(ch) && !self.was_high(ch) && self.high(ch)
    }
    pub fn fell(&self, ch: u8) -> bool {
        self.was_known(ch) && self.was_high(ch) && !self.high(ch)
    }
}

impl Lines {
    pub fn steps(&mut self, edges: &[Edge], mut f: impl FnMut(&Step)) {
        let mut i = 0;
        while i < edges.len() {
            let sample = edges[i].sample;
            let mut st = Step {
                sample,
                before: self.levels,
                after: self.levels,
                known: self.known,
                touched: 0,
            };
            while i < edges.len() && edges[i].sample == sample {
                let e = edges[i];
                let bit = 1u16 << e.ch;
                st.after = if e.level {
                    st.after | bit
                } else {
                    st.after & !bit
                };
                st.touched |= bit;
                i += 1;
            }
            self.levels = st.after;
            self.known |= st.touched;
            f(&st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_parsing() {
        let s = Spec::parse("uart:rx=2,tx=5,invert").unwrap();
        assert_eq!(s.get("rx"), Some("2"));
        assert!(s.flag("invert"));
        assert_eq!(s.channel_mask().unwrap(), 0b10_0100);
        assert_eq!(Spec::parse("uart:rx=0,foo=1").unwrap_err().exit_code(), 1);
        assert!(
            Spec::parse("nope:x=1")
                .unwrap_err()
                .to_string()
                .contains("available: uart")
        );
        assert!(Spec::parse("uart:rx=16").is_err());
        assert!(Spec::parse(":rx=1").is_err());
    }

    #[test]
    fn recommended_rates() {
        assert_eq!(
            recommended_rate(&Spec::parse("uart:rx=0").unwrap()).unwrap(),
            10e6
        );
        assert_eq!(
            recommended_rate(&Spec::parse("uart:rx=0,baud=115200").unwrap()).unwrap(),
            1_843_200.0
        );
        assert_eq!(
            recommended_rate(&Spec::parse("uart:rx=0,baud=12M").unwrap()).unwrap(),
            100e6
        );
    }

    #[test]
    fn lines_group_by_sample() {
        let edges = [
            Edge {
                sample: 0,
                ch: 0,
                level: true,
            },
            Edge {
                sample: 0,
                ch: 1,
                level: true,
            },
            Edge {
                sample: 5,
                ch: 0,
                level: false,
            },
            Edge {
                sample: 5,
                ch: 1,
                level: false,
            },
        ];
        let mut steps = Vec::new();
        Lines::default().steps(&edges, |s| {
            steps.push((s.sample, s.first(0), s.fell(0), s.fell(1), s.was_high(1)))
        });
        assert_eq!(
            steps,
            vec![(0, true, false, false, false), (5, false, true, true, true)]
        );
    }
}
