//! Convert a log between ASC and BLF: `convert <in> <out>`; the formats come
//! from the file extensions.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;

use operow_log::{AscDate, AscWriter, BlfWriter, LogWriter, open_log};

fn convert(input: &Path, output: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    let out = BufWriter::new(File::create(output)?);
    let date = AscDate::now();
    let blf = output
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("blf"));
    let mut count = 0;
    if blf {
        let mut w = BlfWriter::new(out, date)?;
        for r in open_log(input)? {
            w.write(&r?)?;
            count += 1;
        }
        w.finish()?;
    } else {
        let mut w = AscWriter::new(out, date)?;
        for r in open_log(input)? {
            w.write(&r?)?;
            count += 1;
        }
        w.finish()?;
    }
    Ok(count)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: convert <in.asc|in.blf> <out.asc|out.blf>");
        return ExitCode::from(2);
    };
    match convert(Path::new(input), Path::new(output)) {
        Ok(n) => {
            let _ = writeln!(std::io::stdout(), "{n} records");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{input}: {e}");
            ExitCode::FAILURE
        }
    }
}
