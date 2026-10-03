//! Opening a log of either format.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::asc::AscReader;
use crate::blf::{BLF_SIGNATURE, BlfReader};
use crate::record::{LogError, LogReader};

/// Open `path` as ASC or BLF: by extension (`.asc`/`.blf`), falling back to
/// the `LOGG` magic bytes for any other name.
pub fn open_log(path: &Path) -> Result<Box<dyn LogReader + Send>, LogError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let blf = match ext.as_deref() {
        Some("blf") => true,
        Some("asc") => false,
        _ => {
            let mut magic = [0u8; 4];
            File::open(path)?.read_exact(&mut magic).is_ok() && magic == BLF_SIGNATURE
        }
    };
    let input = BufReader::new(File::open(path)?);
    if blf {
        Ok(Box::new(BlfReader::new(input)?))
    } else {
        Ok(Box::new(AscReader::new(input)))
    }
}
