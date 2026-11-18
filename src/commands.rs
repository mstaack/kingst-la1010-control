//! Subcommand implementations.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use la1010::capture::{self, Setup};
use la1010::decode::{self, Decoder, Spec};
use la1010::error::{Error, Result};
use la1010::measure::Measure;
use la1010::output::Printer;
use la1010::pipeline::{Outcome, Pipeline};
use la1010::stream::Deinterleaver;
use la1010::window::{Trigger, Window};
use la1010::{device, export, firmware, units, usb};
use serde_json::json;

use crate::{Acq, CaptureArgs, Cli, Cmd, DecodeArgs, FwCmd, MeasureArgs, MonitorArgs};

static STOP: AtomicBool = AtomicBool::new(false);

pub fn run(cli: Cli) -> Result<i32> {
    let fw = cli.firmware_dir.as_deref();
    let json = cli.json;
    match cli.cmd {
        Cmd::Info => info(fw, json),
        Cmd::Firmware {
            cmd: FwCmd::Fetch { out, url },
        } => install(&fetch(&url, json)?, out.as_deref(), fw, json),
        Cmd::Firmware {
            cmd: FwCmd::Extract { path, out },
        } => install(
            &firmware::extract_from_path(&path)?,
            out.as_deref(),
            fw,
            json,
        ),
        Cmd::Measure(a) => measure(fw, json, &a),
        Cmd::Capture(a) => capture_cmd(fw, json, &a),
        Cmd::Monitor(a) => monitor(fw, json, &a),
        Cmd::Decode(a) => decode_file(json, &a),
        Cmd::Decoders { name } => decoders(name.as_deref(), json),
    }
}

struct Plan {
    mask: u16,
    rate: f64,
    divider: u16,
    specs: Vec<Spec>,
    trigger: Option<Trigger>,
}

fn parse_specs(decode: &[String]) -> Result<Vec<Spec>> {
    decode.iter().map(|s| Spec::parse(s)).collect()
}

fn plan(acq: &Acq, decode: &[String], trigger: Option<&str>) -> Result<Plan> {
    let specs = parse_specs(decode)?;
    let trigger = trigger.map(Trigger::parse).transpose()?;
    let mut need = 0u16;
    for s in &specs {
        need |= s.channel_mask()?;
    }
    if let Some(t) = &trigger {
        need |= t.mask();
    }
    let mask = match &acq.channels {
        Some(c) => {
            let m = units::parse_channels(c)?;
            if need & !m != 0 {
                return Err(Error::Usage(format!(
                    "-c {c} does not include the decoder/trigger channels (mask 0x{need:04x})"
                )));
            }
            m
        }
        None if need != 0 => need,
        None => 0xffff,
    };
    let want = match &acq.rate {
        Some(r) => units::parse_rate(r)?,
        None if specs.is_empty() => 10e6,
        None => specs
            .iter()
            .map(decode::recommended_rate)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .fold(0.0, f64::max),
    };
    let (rate, divider) = units::snap_rate(want)?;
    if (rate - want).abs() / want > 1e-3 {
        eprintln!(
            "la1010: rate {} -> {} (800 MHz / {divider})",
            units::fmt_hz(want),
            units::fmt_hz(rate)
        );
    }
    let bps = rate * f64::from(mask.count_ones());
    if bps > units::VERIFIED_BPS {
        eprintln!(
            "la1010: warning: {:.0} Mbit/s exceeds the verified lossless {:.0} Mbit/s; an overrun aborts the capture",
            bps / 1e6,
            units::VERIFIED_BPS / 1e6
        );
    }
    Ok(Plan {
        mask,
        rate,
        divider,
        specs,
        trigger,
    })
}

fn decoders_for(specs: &[Spec], rate: f64) -> Result<Vec<Box<dyn Decoder>>> {
    specs.iter().map(|s| decode::build(s, rate)).collect()
}

fn samples(d: &str, rate: f64) -> Result<u64> {
    Ok((units::parse_duration(d)?.as_secs_f64() * rate).round() as u64)
}

fn stdout_printer(json: bool, rate: f64) -> Printer<io::StdoutLock<'static>> {
    Printer::new(io::stdout().lock(), json, rate)
}

fn acquire<W: Write>(
    fw: Option<&Path>,
    p: &Plan,
    threshold: f64,
    mut pipe: Pipeline<W>,
    max_wait: u64,
) -> Result<Outcome> {
    let _ = ctrlc::set_handler(|| STOP.store(true, Ordering::Relaxed));
    let opened = usb::open(fw)?;
    let mut deint = Deinterleaver::new(p.mask);
    let mut edges = Vec::new();
    let setup = Setup {
        mask: p.mask,
        divider: p.divider,
        rate: p.rate,
        threshold,
    };
    let res = capture::stream(&opened.usb, &setup, &STOP, |bytes| {
        edges.clear();
        deint.push(bytes, &mut edges);
        let more = pipe.push(&edges, deint.samples())?;
        Ok(more && (pipe.triggered() || deint.samples() < max_wait))
    });
    let outcome = pipe.finish(deint.samples());
    res?;
    outcome
}

fn check_stop(o: &Outcome, until: Option<&str>, count: Option<u64>) -> Result<i32> {
    if o.hit || (until.is_none() && count.is_none()) {
        return Ok(0);
    }
    Err(Error::NoResult(match until {
        Some(u) => format!("'{u}' not seen"),
        None => format!("only {} of {} events", o.decoded, count.unwrap_or(0)),
    }))
}

fn info(fw: Option<&Path>, json: bool) -> Result<i32> {
    let o = usb::open(fw)?;
    let pins = device::pins(&o.usb)?;
    let dirs: Vec<String> = firmware::dirs(fw)
        .iter()
        .map(|d| d.display().to_string())
        .collect();
    let mut out = io::stdout().lock();
    if json {
        let v = json!({
            "model": "LA1010", "revision": o.id.revision, "magic": o.id.magic,
            "magic2": o.id.magic2, "manufactured": o.id.manufactured,
            "bitstream": o.id.bitstream, "fx2_uploaded": o.fx2_loaded,
            "fpga_uploaded": o.fpga_loaded, "pins": pins, "firmware_dirs": dirs,
        });
        writeln!(out, "{v}")?;
    } else {
        let state = |b: bool| if b { "uploaded now" } else { "already running" };
        writeln!(
            out,
            "model         LA1010 rev {} (EEPROM magic 0x{:02x}/0x{:02x})",
            o.id.revision, o.id.magic, o.id.magic2
        )?;
        writeln!(
            out,
            "manufactured  {}",
            o.id.manufactured.as_deref().unwrap_or("unknown")
        )?;
        writeln!(out, "fx2 firmware  {}", state(o.fx2_loaded))?;
        writeln!(
            out,
            "fpga          {} ({})",
            o.id.bitstream,
            state(o.fpga_loaded)
        )?;
        writeln!(out, "pins          0x{pins:04x}")?;
    }
    Ok(0)
}

/// `firmware::fetch` with a one-line progress display on stderr when it is a terminal.
fn fetch(url: &str, json: bool) -> Result<Vec<firmware::Blob>> {
    let show = !json && io::stderr().is_terminal();
    let mut last = u64::MAX;
    let res = firmware::fetch(url, firmware::IDLE_TIMEOUT, |n, total| {
        let tenth = n / 100_000;
        if !show || tenth == last {
            return;
        }
        last = tenth;
        let total = total.map_or(String::new(), |t| format!("/{:.1}", t as f64 / 1e6));
        eprint!("\rla1010: downloading {:.1}{total} MB", n as f64 / 1e6);
    });
    if show && last != u64::MAX {
        eprint!("\r\x1b[2K");
    }
    res
}

fn install(
    blobs: &[firmware::Blob],
    out: Option<&Path>,
    fw: Option<&Path>,
    json: bool,
) -> Result<i32> {
    let dir = match out {
        Some(d) => d.to_path_buf(),
        None => firmware::install_dir(fw)?,
    };
    let mut stdout = io::stdout().lock();
    for r in firmware::install(blobs, &dir)? {
        let status = match r.known {
            Some(true) => "verified",
            Some(false) => "differs from KingstVIS v3.6.6",
            None => "not in table",
        };
        if r.known == Some(false) {
            eprintln!(
                "la1010: warning: {} (size {}, crc32 {:08x}) differs from the tested KingstVIS v3.6.6 file; installed anyway",
                r.name, r.size, r.crc
            );
        }
        if json {
            let v = json!({"file": r.name, "path": r.path.display().to_string(), "size": r.size,
                           "crc32": format!("{:08x}", r.crc), "status": status});
            writeln!(stdout, "{v}")?;
        } else {
            writeln!(
                stdout,
                "{}  {} B  crc32 {:08x}  {status}",
                r.path.display(),
                r.size,
                r.crc
            )?;
        }
    }
    Ok(0)
}

fn measure(fw: Option<&Path>, json: bool, a: &MeasureArgs) -> Result<i32> {
    let p = plan(&a.acq, &[], None)?;
    let total = samples(&a.duration, p.rate)?.max(1);
    let window = Window::new(p.mask, None, 0, total);
    let pipe = Pipeline::new(window, Vec::new(), stdout_printer(json, p.rate))
        .measure(Measure::new(p.mask, p.rate));
    acquire(fw, &p, a.acq.threshold, pipe, u64::MAX)?;
    Ok(0)
}

fn capture_cmd(fw: Option<&Path>, json: bool, a: &CaptureArgs) -> Result<i32> {
    let p = plan(&a.acq, &a.decode, a.trigger.as_deref())?;
    let total = match a.samples {
        Some(n) => n,
        None => samples(a.duration.as_deref().unwrap_or("1s"), p.rate)?,
    };
    if total == 0 {
        return Err(Error::Usage("capture length is zero".into()));
    }
    if a.pre > 99 {
        return Err(Error::Usage("--pre must be 0-99".into()));
    }
    let pre = total * u64::from(a.pre) / 100;
    let window = Window::new(p.mask, p.trigger.clone(), pre, total);
    let mut pipe = Pipeline::new(
        window,
        decoders_for(&p.specs, p.rate)?,
        stdout_printer(json, p.rate),
    );
    if let Some(o) = &a.output {
        pipe = pipe.writer(export::create(o, p.mask, p.rate)?);
    }
    let max_wait = if p.trigger.is_some() {
        samples(&a.timeout, p.rate)?
    } else {
        u64::MAX
    };
    let o = acquire(fw, &p, a.acq.threshold, pipe, max_wait)?;
    if p.trigger.is_some() && !o.triggered {
        return Err(Error::NoResult(format!(
            "trigger not reached within {}",
            a.timeout
        )));
    }
    Ok(0)
}

fn monitor(fw: Option<&Path>, json: bool, a: &MonitorArgs) -> Result<i32> {
    let p = plan(&a.acq, &a.decode, None)?;
    let len = match samples(&a.timeout, p.rate)? {
        0 => u64::MAX,
        n => n,
    };
    let window = Window::new(p.mask, None, 0, len);
    let pipe = Pipeline::new(
        window,
        decoders_for(&p.specs, p.rate)?,
        stdout_printer(json, p.rate),
    )
    .stop_on(a.until.clone(), a.count);
    let o = acquire(fw, &p, a.acq.threshold, pipe, u64::MAX)?;
    check_stop(&o, a.until.as_deref(), a.count)
}

fn decode_file(json: bool, a: &DecodeArgs) -> Result<i32> {
    let specs = parse_specs(&a.decode)?;
    let rec = export::read(&a.file)?;
    for s in &specs {
        if s.channel_mask()? & !rec.mask != 0 {
            return Err(s.usage(format!(
                "uses channels not present in {} (channels mask 0x{:04x})",
                a.file.display(),
                rec.mask
            )));
        }
    }
    let window = Window::new(rec.mask, None, 0, u64::MAX);
    let mut pipe = Pipeline::new(
        window,
        decoders_for(&specs, rec.rate)?,
        stdout_printer(json, rec.rate),
    )
    .stop_on(a.until.clone(), a.count);
    pipe.push(&rec.edges, rec.end)?;
    let o = pipe.finish(rec.end)?;
    check_stop(&o, a.until.as_deref(), a.count)
}

fn decoders(name: Option<&str>, json: bool) -> Result<i32> {
    let list = match name {
        Some(n) => vec![decode::info(n)?],
        None => decode::catalog(),
    };
    let mut out = io::stdout().lock();
    if json {
        let v: Vec<_> = list
            .iter()
            .map(|i| {
                let opts: Vec<_> = i
                    .opts
                    .iter()
                    .map(|o| {
                        json!({"name": o.name, "help": o.help, "default": o.default,
                               "required": o.required, "channel": o.channel})
                    })
                    .collect();
                json!({"name": i.name, "help": i.help, "options": opts})
            })
            .collect();
        writeln!(out, "{}", serde_json::Value::Array(v))?;
        return Ok(0);
    }
    for i in list {
        writeln!(out, "{}  {}", i.name, i.help)?;
        for o in i.opts {
            let mut tags = Vec::new();
            if o.required {
                tags.push("required".to_string());
            }
            if let Some(d) = o.default {
                tags.push(format!("default {d}"));
            }
            let tags = if tags.is_empty() {
                String::new()
            } else {
                format!(" ({})", tags.join(", "))
            };
            writeln!(out, "    {:<10} {}{tags}", o.name, o.help)?;
        }
    }
    writeln!(
        out,
        "\nspec syntax: NAME:opt=val,opt=val,flag   e.g. uart:rx=0,baud=115200"
    )?;
    Ok(0)
}
