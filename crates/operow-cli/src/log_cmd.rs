//! `operow-cli replay` and `operow-cli convert`: log conversion and export.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use clap::ValueEnum;
use operow_dbc::Database;
use operow_log::{AscDate, AscWriter, BlfWriter, LogRecord, LogWriter, RecordKind, open_log};
use operow_test::Project;

use crate::{ConvertArgs, ReplayArgs};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Csv,
    Asc,
    Blf,
}

/// A log writer of either format.
pub enum AnyWriter {
    Asc(AscWriter<BufWriter<File>>),
    Blf(BlfWriter<BufWriter<File>>),
}

impl AnyWriter {
    pub fn create(path: &Path) -> Result<AnyWriter, String> {
        let blf = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("blf") => true,
            Some("asc") => false,
            _ => return Err(format!("{}: the log must be .asc or .blf", path.display())),
        };
        Self::with_format(path, blf)
    }

    fn with_format(path: &Path, blf: bool) -> Result<AnyWriter, String> {
        let file =
            File::create(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        let out = BufWriter::new(file);
        let date = AscDate::now();
        let w = if blf {
            BlfWriter::new(out, date).map(AnyWriter::Blf)
        } else {
            AscWriter::new(out, date).map(AnyWriter::Asc)
        };
        w.map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    pub fn write(&mut self, r: &LogRecord) -> io::Result<()> {
        match self {
            AnyWriter::Asc(w) => w.write(r),
            AnyWriter::Blf(w) => w.write(r),
        }
    }

    pub fn finish(self) -> io::Result<()> {
        match self {
            AnyWriter::Asc(w) => w.finish(),
            AnyWriter::Blf(w) => w.finish(),
        }
    }
}

fn convert_log(input: &Path, output: &Path, blf: Option<bool>) -> Result<usize, String> {
    let reader = open_log(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let mut w = match blf {
        Some(b) => AnyWriter::with_format(output, b)?,
        None => AnyWriter::create(output)?,
    };
    let mut n = 0;
    for r in reader {
        let r = r.map_err(|e| format!("{}: {e}", input.display()))?;
        w.write(&r)
            .map_err(|e| format!("cannot write {}: {e}", output.display()))?;
        n += 1;
    }
    w.finish()
        .map_err(|e| format!("cannot write {}: {e}", output.display()))?;
    Ok(n)
}

pub fn convert(a: &ConvertArgs) -> Result<ExitCode, String> {
    let n = convert_log(&a.input, &a.output, None)?;
    println!(
        "{n} records: {} -> {}",
        a.input.display(),
        a.output.display()
    );
    Ok(ExitCode::SUCCESS)
}

pub fn replay(a: &ReplayArgs) -> Result<ExitCode, String> {
    let n = match a.export {
        Format::Asc => convert_log(&a.log, &a.out, Some(false))?,
        Format::Blf => convert_log(&a.log, &a.out, Some(true))?,
        Format::Csv => export_csv(a)?,
    };
    println!("{n} records: {} -> {}", a.log.display(), a.out.display());
    Ok(ExitCode::SUCCESS)
}

/// DBCs by channel: from the project (bus N in project order is channel N)
/// and from `--dbc`.
fn databases(a: &ReplayArgs) -> Result<HashMap<u8, Arc<Database>>, String> {
    let mut by_channel = HashMap::new();
    if let Some(p) = &a.project {
        let project = Project::load(p).map_err(|e| e.to_string())?;
        for (i, bus) in project.topology.buses.iter().enumerate() {
            if let Some(db) = project.dbcs.by_bus.get(&bus.id) {
                by_channel.insert((i + 1).min(255) as u8, db.clone());
            }
        }
    }
    if let Some(path) = &a.dbc {
        let db = Arc::new(operow_project::load_file(path)?);
        match a.dbc_bus_channel {
            Some(ch) => {
                by_channel.insert(ch, db);
            }
            None => {
                // Every channel of the log.
                for r in open_log(&a.log).map_err(|e| format!("{}: {e}", a.log.display()))? {
                    let r = r.map_err(|e| format!("{}: {e}", a.log.display()))?;
                    by_channel.entry(r.channel).or_insert_with(|| db.clone());
                }
            }
        }
    }
    Ok(by_channel)
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn export_csv(a: &ReplayArgs) -> Result<usize, String> {
    let dbs = databases(a)?;
    let open = || open_log(&a.log).map_err(|e| format!("{}: {e}", a.log.display()));

    // Signal columns: the signals of every message that occurs in the log.
    let mut seen: HashSet<(u8, u32, bool)> = HashSet::new();
    if !dbs.is_empty() {
        for r in open()? {
            let r = r.map_err(|e| format!("{}: {e}", a.log.display()))?;
            if let RecordKind::Frame(f) = r.kind {
                seen.insert((r.channel, f.id, f.extended));
            }
        }
    }
    let mut channels: Vec<u8> = dbs.keys().copied().collect();
    channels.sort();
    let mut columns: Vec<String> = Vec::new();
    for ch in channels {
        for m in &dbs[&ch].messages {
            if seen.contains(&(ch, m.id, m.extended)) {
                for s in &m.signals {
                    let col = format!("{}.{}", m.name, s.name);
                    if !columns.contains(&col) {
                        columns.push(col);
                    }
                }
            }
        }
    }
    let index: HashMap<&str, usize> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| (c.as_str(), i))
        .collect();

    let file =
        File::create(&a.out).map_err(|e| format!("cannot create {}: {e}", a.out.display()))?;
    let mut out = BufWriter::new(file);
    let werr = |e: io::Error| format!("cannot write {}: {e}", a.out.display());
    let mut header = String::from("time_s,channel,dir,id_hex,ext,fd,dlc,data_hex");
    for c in &columns {
        header.push(',');
        header.push_str(&csv_field(c));
    }
    writeln!(out, "{header}").map_err(werr)?;

    let mut n = 0;
    for r in open()? {
        let r = r.map_err(|e| format!("{}: {e}", a.log.display()))?;
        let dir = format!("{:?}", r.dir);
        let mut cells = vec![String::new(); columns.len()];
        let fixed = match r.kind {
            RecordKind::Frame(f) => {
                if let Some(m) = dbs
                    .get(&r.channel)
                    .and_then(|db| db.message(f.id, f.extended))
                {
                    for (name, v) in m.decode(&f) {
                        if let Some(&i) = index.get(format!("{}.{name}", m.name).as_str()) {
                            cells[i] = format!("{v}");
                        }
                    }
                }
                let data: Vec<String> = f.payload().iter().map(|b| format!("{b:02X}")).collect();
                format!(
                    "{:.6},{},{dir},{:X},{},{},{},{}",
                    r.time.as_secs_f64(),
                    r.channel,
                    f.id,
                    u8::from(f.extended),
                    u8::from(f.fd),
                    f.dlc,
                    data.join(" ")
                )
            }
            RecordKind::ErrorFrame => {
                format!("{:.6},{},{dir},ERROR,,,,", r.time.as_secs_f64(), r.channel)
            }
        };
        let mut line = fixed;
        for c in &cells {
            line.push(',');
            line.push_str(c);
        }
        writeln!(out, "{line}").map_err(werr)?;
        n += 1;
    }
    out.flush().map_err(werr)?;
    Ok(n)
}
