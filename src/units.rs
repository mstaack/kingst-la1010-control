use std::time::Duration;

use crate::error::{Error, Result};

/// FPGA base clock; sample rate = BASE_CLOCK / divider.
pub const BASE_CLOCK: f64 = 800e6;
/// LA1010 maximum sample rate.
pub const MAX_RATE: f64 = 100e6;
/// Highest rate x channels product verified lossless on hardware (bits/s).
pub const VERIFIED_BPS: f64 = 320e6;

/// Parse a positive number with optional k/M/G suffix.
pub fn parse_si(s: &str) -> Option<f64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1e3),
        'M' => (&s[..s.len() - 1], 1e6),
        'G' => (&s[..s.len() - 1], 1e9),
        _ => (s, 1.0),
    };
    num.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v * mult)
}

pub fn parse_rate(s: &str) -> Result<f64> {
    let t = s.trim();
    let t = t
        .strip_suffix("Hz")
        .or_else(|| t.strip_suffix("hz"))
        .unwrap_or(t);
    parse_si(t)
        .ok_or_else(|| Error::Usage(format!("invalid rate '{s}' (examples: 100M, 24M, 500k)")))
}

/// Snap a requested rate to BASE_CLOCK / n. Returns (rate, divider).
pub fn snap_rate(want: f64) -> Result<(f64, u16)> {
    if want > MAX_RATE * 1.0001 {
        return Err(Error::Usage(format!(
            "rate {} exceeds the LA1010 maximum of 100 MHz",
            fmt_hz(want)
        )));
    }
    let n = (BASE_CLOCK / want).round();
    if n > f64::from(u16::MAX) {
        return Err(Error::Usage(format!(
            "rate {} is below the minimum of {}",
            fmt_hz(want),
            fmt_hz(BASE_CLOCK / f64::from(u16::MAX))
        )));
    }
    let n = (n as u16).max(8);
    Ok((BASE_CLOCK / f64::from(n), n))
}

pub fn parse_duration(s: &str) -> Result<Duration> {
    let t = s.trim();
    let bad = || {
        Error::Usage(format!(
            "invalid duration '{s}' (examples: 500ms, 2s, 10us)"
        ))
    };
    let (num, scale) = if let Some(n) = t.strip_suffix("ns") {
        (n, 1e-9)
    } else if let Some(n) = t.strip_suffix("us") {
        (n, 1e-6)
    } else if let Some(n) = t.strip_suffix("ms") {
        (n, 1e-3)
    } else if let Some(n) = t.strip_suffix("min") {
        (n, 60.0)
    } else if let Some(n) = t.strip_suffix('s') {
        (n, 1.0)
    } else {
        (t, 1.0)
    };
    let v: f64 = num.trim().parse().map_err(|_| bad())?;
    if !v.is_finite() || v < 0.0 {
        return Err(bad());
    }
    Ok(Duration::from_nanos((v * scale * 1e9).round() as u64))
}

/// Parse "0-3,8" into a channel bitmask.
pub fn parse_channels(s: &str) -> Result<u16> {
    let bad = || {
        Error::Usage(format!(
            "invalid channel list '{s}' (examples: 0-3, 0,2,5, 0-15)"
        ))
    };
    let ch = |x: &str| {
        x.trim()
            .parse::<u8>()
            .ok()
            .filter(|c| *c < 16)
            .ok_or_else(bad)
    };
    let mut mask = 0u16;
    for part in s.split(',') {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (ch(a)?, ch(b)?),
            None => (ch(part)?, ch(part)?),
        };
        if a > b {
            return Err(bad());
        }
        for c in a..=b {
            mask |= 1 << c;
        }
    }
    Ok(mask)
}

/// "12.345678ms" below one second, "1.000123s" above.
pub fn format_time(t: f64) -> String {
    if t.abs() < 1.0 {
        format!("{:.6}ms", t * 1e3)
    } else {
        format!("{t:.6}s")
    }
}

pub fn fmt_hz(hz: f64) -> String {
    if hz >= 1e6 {
        format!("{:.3} MHz", hz / 1e6)
    } else if hz >= 1e3 {
        format!("{:.3} kHz", hz / 1e3)
    } else {
        format!("{hz:.3} Hz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn si_and_rates() {
        assert_eq!(parse_si("100M"), Some(100e6));
        assert_eq!(parse_si("1.5k"), Some(1500.0));
        assert_eq!(parse_si("115200"), Some(115200.0));
        assert_eq!(parse_si("x"), None);
        assert_eq!(parse_rate("24MHz").unwrap(), 24e6);
        assert_eq!(snap_rate(100e6).unwrap(), (100e6, 8));
        assert_eq!(snap_rate(10e6).unwrap(), (10e6, 80));
        let (r, d) = snap_rate(33e6).unwrap();
        assert_eq!(d, 24);
        assert!((r - 33_333_333.333).abs() < 1.0);
        assert_eq!(snap_rate(101e6).unwrap_err().exit_code(), 1);
        assert_eq!(snap_rate(10e3).unwrap_err().exit_code(), 1);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse_duration("10us").unwrap(), Duration::from_micros(10));
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert!(parse_duration("5 parsecs").is_err());
    }

    #[test]
    fn channels() {
        assert_eq!(parse_channels("0-3,8").unwrap(), 0b1_0000_1111);
        assert_eq!(parse_channels("15").unwrap(), 0x8000);
        assert!(parse_channels("16").is_err());
        assert!(parse_channels("3-1").is_err());
        assert!(parse_channels("").is_err());
    }

    #[test]
    fn time_format() {
        assert_eq!(format_time(0.012345678), "12.345678ms");
        assert_eq!(format_time(1.000123), "1.000123s");
        assert_eq!(format_time(-0.000001), "-0.001000ms");
    }
}
