//! `operow-cli`: the headless side of Operow. Runs test modules, simulates a
//! project without a GUI, converts and exports bus logs and sends UDS
//! requests.

mod diag_cmd;
mod log_cmd;
mod run_cmd;
mod test_cmd;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

const EXAMPLES: &str = "\
Examples:
  operow-cli test examples/gateway.operow.json --junit out/junit.xml --report out/
  operow-cli test project.operow.json --filter 'test_cycle*' --jobs 4 --fail-fast
  operow-cli run examples/gateway.operow.json --duration 10s --log trace.blf
  operow-cli replay trace.blf --export csv --out trace.csv --dbc examples/sample.dbc
  operow-cli convert trace.blf trace.asc
  operow-cli diag examples/diag_demo.operow.json --node Engine --req \"22 F1 90\"

Exit codes of `test`: 0 all passed (skips are fine), 1 a test failed,
2 an error (project, script or usage).";

#[derive(Parser)]
#[command(name = "operow-cli", version, about = "Headless Operow: test, simulate, replay and diagnose", after_help = EXAMPLES)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the Rhai test modules of a project
    Test(TestArgs),
    /// Run the simulation headless for a stretch of virtual time
    Run(RunArgs),
    /// Export a bus log as CSV (with decoded signals), ASC or BLF
    Replay(ReplayArgs),
    /// Convert a log between ASC and BLF (by file extension)
    Convert(ConvertArgs),
    /// Send one UDS request to a node and print the response
    Diag(DiagArgs),
}

#[derive(Args)]
pub struct TestArgs {
    /// Project file (.operow.json)
    pub project: PathBuf,
    /// Only run cases (or modules) matching this glob; `module::case` also works
    #[arg(long)]
    pub filter: Option<String>,
    /// Write report.html into this directory
    #[arg(long, value_name = "DIR")]
    pub report: Option<PathBuf>,
    /// Write a JUnit XML report
    #[arg(long, value_name = "FILE")]
    pub junit: Option<PathBuf>,
    /// Write the full report as JSON
    #[arg(long, value_name = "FILE")]
    pub json: Option<PathBuf>,
    /// Stop after the first failing case
    #[arg(long)]
    pub fail_fast: bool,
    /// Run this many modules in parallel
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub jobs: usize,
    /// Seed of the simulation's random generator
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    /// Virtual time one test function may take
    #[arg(long, default_value_t = 60_000, value_name = "MS")]
    pub timeout_ms: u64,
    /// Print only failures and the summary
    #[arg(long, short)]
    pub quiet: bool,
}

#[derive(Args)]
pub struct RunArgs {
    /// Project file (.operow.json)
    pub project: PathBuf,
    /// Virtual time to simulate: 500ms, 10s, 2m (a bare number is seconds)
    #[arg(long)]
    pub duration: String,
    /// Write every bus event to this log (.asc or .blf)
    #[arg(long, value_name = "FILE")]
    pub log: Option<PathBuf>,
    /// Pace the simulation to the wall clock
    #[arg(long)]
    pub realtime: bool,
    /// Seed of the simulation's random generator
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
}

#[derive(Args)]
pub struct ReplayArgs {
    /// Log to read (.asc or .blf)
    pub log: PathBuf,
    /// Output format
    #[arg(long, value_enum)]
    pub export: log_cmd::Format,
    /// Output file
    #[arg(long, value_name = "FILE")]
    pub out: PathBuf,
    /// Take the DBCs of this project; bus N (in project order) is channel N
    #[arg(long, value_name = "PROJECT")]
    pub project: Option<PathBuf>,
    /// Decode signals with this DBC (CSV only)
    #[arg(long, value_name = "DBC")]
    pub dbc: Option<PathBuf>,
    /// Channel the --dbc applies to; all channels when omitted
    #[arg(long, value_name = "N")]
    pub dbc_bus_channel: Option<u8>,
}

#[derive(Args)]
pub struct ConvertArgs {
    /// Input log (.asc or .blf)
    pub input: PathBuf,
    /// Output log; the extension picks the format
    pub output: PathBuf,
}

#[derive(Args)]
pub struct DiagArgs {
    /// Project file (.operow.json)
    pub project: PathBuf,
    /// Node with a diagnostics configuration
    #[arg(long)]
    pub node: String,
    /// Request bytes in hex, e.g. "22 F1 90"
    #[arg(long)]
    pub req: String,
    /// Virtual time to run before sending the request
    #[arg(long, default_value_t = 100, value_name = "MS")]
    pub warmup_ms: u64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Test(a) => test_cmd::run(&a),
        Command::Run(a) => run_cmd::run(&a),
        Command::Replay(a) => log_cmd::replay(&a),
        Command::Convert(a) => log_cmd::convert(&a),
        Command::Diag(a) => diag_cmd::run(&a),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
