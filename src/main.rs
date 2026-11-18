mod commands;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use la1010::error::Error;
use la1010::output;

const EXAMPLES: &str = "\
Examples:
  la1010 firmware fetch                          download the vendor firmware (once)
  la1010 info                                    identify the analyzer
  la1010 measure -c 0-3                          frequency / duty / pulses on CH0-3 for 1 s
  la1010 capture -d 2s --decode uart:rx=0,baud=115200
  la1010 capture --decode i2c:scl=1,sda=0 --trigger 0:fall -o boot.sr
  la1010 monitor --decode usb:dp=2,dm=3 --until GET_DESCRIPTOR --json
  la1010 decode boot.sr --decode i2c:scl=1,sda=0
  la1010 decoders                                decoders and their options

Exit codes: 0 ok, 1 usage, 2 device/firmware/IO, 3 trigger/--until/--count not reached";

#[derive(Parser)]
#[command(
    name = "la1010",
    version,
    about = "Capture and decode digital signals with a Kingst LA1010",
    after_help = EXAMPLES
)]
pub struct Cli {
    /// NDJSON on stdout, JSON errors on stderr
    #[arg(long, global = true)]
    pub json: bool,
    /// Firmware directory (default: $LA1010_FIRMWARE_DIR, $XDG_DATA_HOME/la1010, ~/.local/share/la1010)
    #[arg(long, global = true, value_name = "DIR")]
    pub firmware_dir: Option<PathBuf>,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Identify the analyzer (loads firmware into its RAM if needed)
    Info,
    /// Obtain the vendor firmware (not bundled)
    Firmware {
        #[command(subcommand)]
        cmd: FwCmd,
    },
    /// Per-channel frequency, duty cycle and pulse widths
    Measure(MeasureArgs),
    /// Capture for a fixed length; optional trigger, decoders and output file
    Capture(CaptureArgs),
    /// Decode live until --timeout, --count or --until
    Monitor(MonitorArgs),
    /// Decode a saved .sr or .vcd file
    Decode(DecodeArgs),
    /// List protocol decoders and their options
    Decoders {
        /// Show one decoder
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum FwCmd {
    /// Download KingstVIS for Linux from qdkingst.com and install the LA1010 files
    Fetch {
        /// Install directory
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        #[arg(long, hide = true, default_value = la1010::firmware::VENDOR_URL)]
        url: String,
    },
    /// Install from a KingstVIS Linux tarball, unpacked directory or binary
    Extract {
        path: PathBuf,
        /// Install directory
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
    },
}

#[derive(Args)]
pub struct Acq {
    /// Channels, e.g. 0-3,8 (default: decoder/trigger channels, else 0-15)
    #[arg(short, long)]
    pub channels: Option<String>,
    /// Sample rate, e.g. 100M, 24M, 1M (default: from decoders, else 10M)
    #[arg(short, long)]
    pub rate: Option<String>,
    /// Input threshold in volts (-4.0..4.0)
    #[arg(short, long, default_value_t = 1.4, allow_negative_numbers = true)]
    pub threshold: f64,
}

#[derive(Args)]
pub struct MeasureArgs {
    #[command(flatten)]
    pub acq: Acq,
    /// Duration, e.g. 500ms, 2s
    #[arg(short, long, default_value = "1s")]
    pub duration: String,
}

#[derive(Args)]
pub struct CaptureArgs {
    #[command(flatten)]
    pub acq: Acq,
    /// Capture length as time (default 1s)
    #[arg(short, long, conflicts_with = "samples")]
    pub duration: Option<String>,
    /// Capture length in samples
    #[arg(short = 'n', long)]
    pub samples: Option<u64>,
    /// Software trigger, e.g. 0:rise, 3:fall,1:high
    #[arg(long)]
    pub trigger: Option<String>,
    /// Percent of the capture kept before the trigger
    #[arg(long, default_value_t = 10)]
    pub pre: u8,
    /// Give up waiting for the trigger after this long
    #[arg(long, default_value = "10s")]
    pub timeout: String,
    /// Save samples: .sr (PulseView/sigrok), .vcd or .csv
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Decoder spec, repeatable, e.g. uart:rx=0,baud=115200
    #[arg(long = "decode", value_name = "SPEC")]
    pub decode: Vec<String>,
}

#[derive(Args)]
pub struct MonitorArgs {
    #[command(flatten)]
    pub acq: Acq,
    /// Decoder spec, repeatable
    #[arg(long = "decode", value_name = "SPEC", required = true)]
    pub decode: Vec<String>,
    /// Stop after this long (0 = run until Ctrl-C)
    #[arg(long, default_value = "10s")]
    pub timeout: String,
    /// Stop after N decoded events
    #[arg(long)]
    pub count: Option<u64>,
    /// Stop after a line containing TEXT
    #[arg(long, value_name = "TEXT")]
    pub until: Option<String>,
}

#[derive(Args)]
pub struct DecodeArgs {
    /// .sr or .vcd file
    pub file: PathBuf,
    /// Decoder spec, repeatable
    #[arg(long = "decode", value_name = "SPEC", required = true)]
    pub decode: Vec<String>,
    /// Stop after N decoded events
    #[arg(long)]
    pub count: Option<u64>,
    /// Stop after a line containing TEXT
    #[arg(long, value_name = "TEXT")]
    pub until: Option<String>,
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let code = if e.use_stderr() { 1 } else { 0 };
            let _ = e.print();
            std::process::exit(code);
        }
    };
    let json = cli.json;
    let code = match commands::run(cli) {
        Ok(code) => code,
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            if json {
                eprintln!("{}", output::error_json(&e));
            } else {
                eprintln!("la1010: {e}");
            }
            e.exit_code()
        }
    };
    std::process::exit(code);
}
