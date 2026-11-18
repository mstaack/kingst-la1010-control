//! Software trigger and capture window. The LA1010 streams without hardware
//! trigger, so the host selects the window and rebases sample numbers to it.

use std::collections::VecDeque;

use crate::error::{Error, Result};
use crate::stream::Edge;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Rise,
    Fall,
    High,
    Low,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger(pub Vec<(u8, Cond)>);

impl Trigger {
    /// "0:rise", "3:fall,1:high". At most one rise/fall condition.
    pub fn parse(s: &str) -> Result<Self> {
        let mut conds = Vec::new();
        for part in s.split(',') {
            let (ch, c) = part.split_once(':').ok_or_else(|| {
                Error::Usage(format!("trigger '{part}': expected CH:rise|fall|high|low"))
            })?;
            let ch: u8 = ch
                .trim()
                .parse()
                .ok()
                .filter(|c| *c < 16)
                .ok_or_else(|| Error::Usage(format!("trigger channel '{ch}' must be 0-15")))?;
            let cond = match c.trim() {
                "rise" | "r" => Cond::Rise,
                "fall" | "f" => Cond::Fall,
                "high" | "1" => Cond::High,
                "low" | "0" => Cond::Low,
                o => {
                    return Err(Error::Usage(format!(
                        "trigger condition '{o}': use rise, fall, high or low"
                    )));
                }
            };
            conds.push((ch, cond));
        }
        if conds
            .iter()
            .filter(|(_, c)| matches!(c, Cond::Rise | Cond::Fall))
            .count()
            > 1
        {
            return Err(Error::Usage(
                "trigger: at most one rise/fall condition".into(),
            ));
        }
        Ok(Trigger(conds))
    }

    pub fn mask(&self) -> u16 {
        self.0.iter().fold(0, |m, (ch, _)| m | 1 << ch)
    }

    fn levels_ok(&self, levels: u16) -> bool {
        self.0.iter().all(|&(ch, c)| match c {
            Cond::High => levels >> ch & 1 == 1,
            Cond::Low => levels >> ch & 1 == 0,
            Cond::Rise | Cond::Fall => true,
        })
    }

    fn fires(&self, group: &[Edge], before: u16, after: u16, first: bool) -> bool {
        if !self.levels_ok(after) {
            return false;
        }
        match self
            .0
            .iter()
            .find(|(_, c)| matches!(c, Cond::Rise | Cond::Fall))
        {
            Some(&(ch, c)) => {
                !first
                    && group
                        .iter()
                        .any(|e| e.ch == ch && e.level == (c == Cond::Rise))
            }
            None => first || !self.levels_ok(before),
        }
    }
}

fn set(levels: u16, ch: u8, level: bool) -> u16 {
    if level {
        levels | 1 << ch
    } else {
        levels & !(1 << ch)
    }
}

pub struct Window {
    trigger: Option<Trigger>,
    mask: u16,
    pre: u64,
    len: u64,
    levels: u16,
    hist: VecDeque<Edge>,
    hist_levels: u16,
    start: Option<u64>,
    trig: Option<u64>,
    seen_first: bool,
}

impl Window {
    /// `pre`: samples kept before the trigger; `len`: total output samples
    /// (`u64::MAX` = unbounded). Without trigger the window starts at sample 0.
    pub fn new(mask: u16, trigger: Option<Trigger>, pre: u64, len: u64) -> Self {
        let start = if trigger.is_none() { Some(0) } else { None };
        Window {
            trigger,
            mask,
            pre,
            len,
            levels: 0,
            hist: VecDeque::new(),
            hist_levels: 0,
            start,
            trig: None,
            seen_first: false,
        }
    }

    pub fn triggered(&self) -> bool {
        self.start.is_some()
    }

    /// Trigger position in output samples.
    pub fn trigger_sample(&self) -> Option<u64> {
        Some(self.trig? - self.start?)
    }

    pub fn output_len(&self, now: u64) -> u64 {
        self.start
            .map_or(0, |s| now.saturating_sub(s).min(self.len))
    }

    /// Feed absolute edges; `now` = samples processed so far. Returns true when complete.
    pub fn push(&mut self, edges: &[Edge], now: u64, out: &mut Vec<Edge>) -> bool {
        let mut i = 0;
        while i < edges.len() {
            if let Some(s) = self.start {
                for e in &edges[i..] {
                    if e.sample - s >= self.len {
                        break;
                    }
                    out.push(Edge {
                        sample: e.sample - s,
                        ..*e
                    });
                }
                break;
            }
            let sample = edges[i].sample;
            let j = edges[i..]
                .iter()
                .position(|e| e.sample != sample)
                .map_or(edges.len(), |k| i + k);
            let group = &edges[i..j];
            let before = self.levels;
            for e in group {
                self.levels = set(self.levels, e.ch, e.level);
                self.hist.push_back(*e);
            }
            let first = !self.seen_first;
            self.seen_first = true;
            let cutoff = sample.saturating_sub(self.pre);
            while let Some(f) = self.hist.front().copied() {
                if f.sample >= cutoff {
                    break;
                }
                self.hist_levels = set(self.hist_levels, f.ch, f.level);
                self.hist.pop_front();
            }
            let fires = self
                .trigger
                .as_ref()
                .is_some_and(|t| t.fires(group, before, self.levels, first));
            if fires {
                self.start = Some(cutoff);
                self.trig = Some(sample);
                let at_cutoff = self.hist.iter().take_while(|e| e.sample == cutoff).count();
                let mut lv = self.hist_levels;
                for e in self.hist.iter().take(at_cutoff) {
                    lv = set(lv, e.ch, e.level);
                }
                for ch in (0..16u8).filter(|c| self.mask >> c & 1 == 1) {
                    out.push(Edge {
                        sample: 0,
                        ch,
                        level: lv >> ch & 1 == 1,
                    });
                }
                for e in self.hist.drain(..).skip(at_cutoff) {
                    out.push(Edge {
                        sample: e.sample - cutoff,
                        ..e
                    });
                }
            }
            i = j;
        }
        self.start
            .is_some_and(|s| now.saturating_sub(s) >= self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(sample: u64, ch: u8, level: bool) -> Edge {
        Edge { sample, ch, level }
    }

    #[test]
    fn parse_trigger() {
        assert_eq!(
            Trigger::parse("0:rise").unwrap(),
            Trigger(vec![(0, Cond::Rise)])
        );
        assert_eq!(Trigger::parse("3:fall,1:high").unwrap().mask(), 0b1010);
        assert!(Trigger::parse("0:rise,1:fall").is_err());
        assert!(Trigger::parse("16:rise").is_err());
        assert!(Trigger::parse("0:wiggle").is_err());
        assert!(Trigger::parse("rise").is_err());
    }

    #[test]
    fn untriggered_window_limits_length() {
        let mut w = Window::new(1, None, 0, 100);
        let mut out = Vec::new();
        assert!(!w.push(
            &[e(0, 0, false), e(50, 0, true), e(150, 0, false)],
            99,
            &mut out
        ));
        assert_eq!(out, vec![e(0, 0, false), e(50, 0, true)]);
        assert!(w.push(&[], 160, &mut out));
        assert_eq!(w.output_len(160), 100);
        assert_eq!(w.trigger_sample(), None);
    }

    #[test]
    fn rise_trigger_with_pretrigger() {
        let mut w = Window::new(0b11, Some(Trigger::parse("0:rise").unwrap()), 10, 100);
        let mut out = Vec::new();
        let input = [
            e(0, 0, false),
            e(0, 1, false),
            e(25, 1, true),
            e(30, 0, true),
            e(40, 1, false),
        ];
        assert!(!w.push(&input, 50, &mut out));
        assert_eq!(
            out,
            vec![
                e(0, 0, false),
                e(0, 1, false),
                e(5, 1, true),
                e(10, 0, true),
                e(20, 1, false)
            ]
        );
        assert_eq!(w.trigger_sample(), Some(10));
        assert!(w.push(&[], 120, &mut out));
        assert_eq!(w.output_len(120), 100);
    }

    #[test]
    fn edge_trigger_ignores_initial_levels() {
        let mut w = Window::new(1, Some(Trigger::parse("0:rise").unwrap()), 0, 10);
        let mut out = Vec::new();
        w.push(&[e(0, 0, true)], 16, &mut out);
        assert!(!w.triggered());
        assert!(out.is_empty());
    }

    #[test]
    fn level_trigger_fires_when_condition_becomes_true() {
        let mut w = Window::new(0b11, Some(Trigger::parse("0:high,1:low").unwrap()), 0, 10);
        let mut out = Vec::new();
        w.push(
            &[e(0, 0, false), e(0, 1, false), e(7, 0, true)],
            16,
            &mut out,
        );
        assert_eq!(w.trigger_sample(), Some(0));
        assert_eq!(out, vec![e(0, 0, true), e(0, 1, false)]);
    }

    #[test]
    fn never_triggered_reports_zero_length() {
        let mut w = Window::new(1, Some(Trigger::parse("0:fall").unwrap()), 5, 10);
        let mut out = Vec::new();
        assert!(!w.push(&[e(0, 0, false)], 1000, &mut out));
        assert!(!w.triggered());
        assert_eq!(w.output_len(1000), 0);
    }
}
