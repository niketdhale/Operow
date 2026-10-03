use std::io;

use operow_core::{CanFrame, Direction, Timestamp};
use thiserror::Error;

/// What a [`LogRecord`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Frame(CanFrame),
    ErrorFrame,
}

/// One line of a bus log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogRecord {
    pub time: Timestamp,
    /// 1-based channel number.
    pub channel: u8,
    pub dir: Direction,
    pub kind: RecordKind,
}

#[derive(Debug, Error)]
pub enum LogError {
    #[error("line {line}: {msg}")]
    Parse { line: usize, msg: String },
    #[error("BLF: {0}")]
    Blf(String),
    #[error("read error: {0}")]
    Io(#[from] io::Error),
}

/// Writes records to a log file.
pub trait LogWriter {
    fn write(&mut self, r: &LogRecord) -> io::Result<()>;
    /// Write the footer and flush.
    fn finish(self) -> io::Result<()>
    where
        Self: Sized;
}

/// Reads records back; implemented by every matching iterator.
pub trait LogReader: Iterator<Item = Result<LogRecord, LogError>> {}

impl<T: Iterator<Item = Result<LogRecord, LogError>>> LogReader for T {}
