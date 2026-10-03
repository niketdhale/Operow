//! Vector ASCII (`.asc`) reader and writer for classic CAN, CAN FD and
//! error frames.

use std::io::{self, BufRead, Write};

use operow_core::{CanFrame, Direction, Timestamp, dlc_to_len};

use crate::date::AscDate;
use crate::record::{LogError, LogRecord, LogWriter, RecordKind};

/// Writes a hex, absolute-timestamp ASC file. Timestamps are the record
/// times as seconds.
pub struct AscWriter<W: Write> {
    out: W,
    finished: bool,
}

impl<W: Write> AscWriter<W> {
    /// Write the header and `Begin Triggerblock`.
    pub fn new(mut out: W, start: AscDate) -> io::Result<Self> {
        let date = start.asc_string();
        writeln!(out, "date {date}")?;
        writeln!(out, "base hex  timestamps absolute")?;
        writeln!(out, "internal events logged")?;
        writeln!(out, "Begin Triggerblock {date}")?;
        writeln!(out, "   0.000000 Start of measurement")?;
        Ok(AscWriter {
            out,
            finished: false,
        })
    }

    fn write_footer(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        writeln!(self.out, "End TriggerBlock")?;
        self.out.flush()
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    /// Consume the writer, returning the underlying output (no footer).
    pub fn into_inner(self) -> W {
        self.out
    }
}

fn dir_str(d: Direction) -> &'static str {
    match d {
        Direction::Tx => "Tx",
        Direction::Rx => "Rx",
    }
}

fn id_str(f: &CanFrame) -> String {
    format!("{:X}{}", f.id, if f.extended { "x" } else { "" })
}

impl<W: Write> LogWriter for AscWriter<W> {
    fn write(&mut self, r: &LogRecord) -> io::Result<()> {
        let t = r.time.as_secs_f64();
        match r.kind {
            RecordKind::ErrorFrame => writeln!(self.out, "{t:11.6} {}  ErrorFrame", r.channel),
            RecordKind::Frame(f) if !f.fd => {
                write!(
                    self.out,
                    "{t:11.6} {}  {:<15} {}   d {}",
                    r.channel,
                    id_str(&f),
                    dir_str(r.dir),
                    f.dlc
                )?;
                for b in f.payload() {
                    write!(self.out, " {b:02X}")?;
                }
                writeln!(self.out)
            }
            RecordKind::Frame(f) => {
                write!(
                    self.out,
                    "{t:11.6} CANFD {:>3} {:<2} {:>11}{:35} {} 0 {:X} {:2}",
                    r.channel,
                    dir_str(r.dir),
                    id_str(&f),
                    "",
                    u8::from(f.brs),
                    f.dlc_code(),
                    f.dlc
                )?;
                for b in f.payload() {
                    write!(self.out, " {b:02X}")?;
                }
                // Duration, length, flags, crc and the bit timing fields.
                writeln!(self.out, " 0 0 0 0 0 0 0 0")
            }
        }
    }

    fn finish(mut self) -> io::Result<()> {
        self.write_footer()
    }
}

/// Parses an ASC file line by line. Unknown lines (comments, header,
/// `Start of measurement`, statistics, ...) are skipped.
pub struct AscReader<R: BufRead> {
    input: R,
    line_no: usize,
    hex: bool,
    relative: bool,
    /// Running time for relative timestamps, in nanoseconds.
    acc_ns: u64,
    buf: String,
}

impl<R: BufRead> AscReader<R> {
    pub fn new(input: R) -> Self {
        AscReader {
            input,
            line_no: 0,
            hex: true,
            relative: false,
            acc_ns: 0,
            buf: String::new(),
        }
    }

    fn err(&self, msg: impl Into<String>) -> LogError {
        LogError::Parse {
            line: self.line_no,
            msg: msg.into(),
        }
    }

    fn num(&self, tok: &str, what: &str) -> Result<u32, LogError> {
        let radix = if self.hex { 16 } else { 10 };
        u32::from_str_radix(tok, radix).map_err(|_| self.err(format!("bad {what} \"{tok}\"")))
    }

    fn time(&mut self, tok: &str) -> Result<Timestamp, LogError> {
        let secs: f64 = tok
            .parse()
            .map_err(|_| self.err(format!("bad timestamp \"{tok}\"")))?;
        if !secs.is_finite() || secs < 0.0 {
            return Err(self.err(format!("bad timestamp \"{tok}\"")));
        }
        let ns = (secs * 1e9).round() as u64;
        if self.relative {
            self.acc_ns += ns;
            Ok(Timestamp(self.acc_ns))
        } else {
            Ok(Timestamp(ns))
        }
    }

    fn id(&self, tok: &str) -> Result<(u32, bool), LogError> {
        let (digits, ext) = match tok.strip_suffix(['x', 'X']) {
            Some(d) if !self.hex || !d.is_empty() => (d, true),
            _ => (tok, false),
        };
        Ok((self.num(digits, "id")?, ext))
    }

    fn data(&self, toks: &[&str], len: usize) -> Result<Vec<u8>, LogError> {
        if toks.len() < len {
            return Err(self.err(format!("expected {len} data byte(s), found {}", toks.len())));
        }
        toks[..len]
            .iter()
            .map(|t| {
                self.num(t, "data byte").and_then(|v| {
                    u8::try_from(v).map_err(|_| self.err(format!("bad data byte \"{t}\"")))
                })
            })
            .collect()
    }

    fn parse_directive(&mut self, toks: &[&str]) -> bool {
        match toks[0] {
            "base" => {
                for w in toks.windows(2) {
                    match (w[0], w[1]) {
                        ("base", "hex") => self.hex = true,
                        ("base", "dec") => self.hex = false,
                        ("timestamps", "absolute") => self.relative = false,
                        ("timestamps", "relative") => self.relative = true,
                        _ => {}
                    }
                }
                true
            }
            "date" | "internal" | "no" | "Begin" | "End" | "//" | "version" => true,
            t => t.starts_with("//"),
        }
    }

    fn parse_line(&mut self, line: &str) -> Result<Option<LogRecord>, LogError> {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.is_empty() || self.parse_directive(&toks) {
            return Ok(None);
        }
        if toks.len() < 3 || toks[0].parse::<f64>().is_err() {
            return Ok(None);
        }
        if toks[1] == "CANFD" {
            return self.parse_fd(&toks);
        }
        let Ok(channel) = toks[1].parse::<u8>() else {
            return Ok(None);
        };
        if toks[2] == "ErrorFrame" {
            let time = self.time(toks[0])?;
            return Ok(Some(LogRecord {
                time,
                channel,
                dir: Direction::Rx,
                kind: RecordKind::ErrorFrame,
            }));
        }
        let Some(dir) = toks.get(3).and_then(|d| parse_dir(d)) else {
            return Ok(None);
        };
        if toks.get(4).copied() != Some("d") {
            // Remote frames and other kinds are not supported.
            return Ok(None);
        }
        let time = self.time(toks[0])?;
        let (id, extended) = self.id(toks[2])?;
        let dlc = self.num(toks.get(5).ok_or_else(|| self.err("missing DLC"))?, "DLC")?;
        let len = dlc.min(8) as usize;
        let data = self.data(&toks[6..], len)?;
        let frame = CanFrame::new(id, extended, &data).map_err(|e| self.err(e.to_string()))?;
        Ok(Some(LogRecord {
            time,
            channel,
            dir,
            kind: RecordKind::Frame(frame),
        }))
    }

    /// `time CANFD ch dir id [name] brs esi dlc len data...`
    fn parse_fd(&mut self, toks: &[&str]) -> Result<Option<LogRecord>, LogError> {
        let Some(channel) = toks.get(2).and_then(|c| c.parse::<u8>().ok()) else {
            return Ok(None);
        };
        let Some(dir) = toks.get(3).and_then(|d| parse_dir(d)) else {
            return Ok(None);
        };
        let time = self.time(toks[0])?;
        let (id, extended) = self.id(toks.get(4).ok_or_else(|| self.err("missing id"))?)?;
        let mut i = 5;
        // An optional symbolic name sits between the id and BRS.
        if toks.get(i).is_some_and(|t| !matches!(*t, "0" | "1")) {
            i += 1;
        }
        let field = |k: usize, what: &str| -> Result<u32, LogError> {
            let t = toks
                .get(i + k)
                .ok_or_else(|| self.err(format!("missing {what}")))?;
            self.num(t, what)
        };
        let brs = field(0, "BRS")? != 0;
        let _esi = field(1, "ESI")?;
        let dlc_code = field(2, "DLC")?;
        // The data length is always decimal; the DLC code follows the base.
        let len_tok = toks
            .get(i + 3)
            .ok_or_else(|| self.err("missing data length"))?;
        let len: usize = len_tok
            .parse()
            .map_err(|_| self.err(format!("bad data length \"{len_tok}\"")))?;
        if dlc_code > 15 || dlc_to_len(dlc_code as u8) != len {
            return Err(self.err(format!("DLC {dlc_code} does not match data length {len}")));
        }
        let data = self.data(&toks[(i + 4).min(toks.len())..], len)?;
        let frame =
            CanFrame::new_fd(id, extended, brs, &data).map_err(|e| self.err(e.to_string()))?;
        Ok(Some(LogRecord {
            time,
            channel,
            dir,
            kind: RecordKind::Frame(frame),
        }))
    }
}

fn parse_dir(s: &str) -> Option<Direction> {
    match s {
        "Tx" | "TxRq" => Some(Direction::Tx),
        "Rx" => Some(Direction::Rx),
        _ => None,
    }
}

impl<R: BufRead> Iterator for AscReader<R> {
    type Item = Result<LogRecord, LogError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            self.buf.clear();
            match self.input.read_line(&mut self.buf) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(e) => return Some(Err(e.into())),
            }
            self.line_no += 1;
            let line = std::mem::take(&mut self.buf);
            let res = self.parse_line(&line);
            self.buf = line;
            match res {
                Ok(Some(r)) => return Some(Ok(r)),
                Ok(None) => {}
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(t_ns: u64, ch: u8, dir: Direction, f: CanFrame) -> LogRecord {
        LogRecord {
            time: Timestamp(t_ns),
            channel: ch,
            dir,
            kind: RecordKind::Frame(f),
        }
    }

    fn read_all(s: &str) -> Vec<LogRecord> {
        AscReader::new(s.as_bytes())
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn sample_records() -> Vec<LogRecord> {
        let fd64: Vec<u8> = (0..64).collect();
        vec![
            rec(
                10_270_000,
                1,
                Direction::Tx,
                CanFrame::new(0x100, false, &[0x30, 0xD4, 0x7F, 0xA9, 1, 1, 0, 0]).unwrap(),
            ),
            rec(
                20_000_000,
                2,
                Direction::Rx,
                CanFrame::new(0x1ABC_DEF0, true, &[1, 2]).unwrap(),
            ),
            rec(
                30_000_000,
                1,
                Direction::Rx,
                CanFrame::new(0x7FF, false, &[]).unwrap(),
            ),
            rec(
                40_000_000,
                3,
                Direction::Tx,
                CanFrame::new_fd(0x123, false, true, &[9; 12]).unwrap(),
            ),
            rec(
                50_000_000,
                3,
                Direction::Tx,
                CanFrame::new_fd(0x1FF0_0000, true, false, &[0xAA; 5]).unwrap(),
            ),
            rec(
                60_000_000,
                4,
                Direction::Rx,
                CanFrame::new_fd(0x77, false, true, &fd64).unwrap(),
            ),
            LogRecord {
                time: Timestamp(123_456_000),
                channel: 1,
                dir: Direction::Rx,
                kind: RecordKind::ErrorFrame,
            },
        ]
    }

    #[test]
    fn round_trip() {
        let recs = sample_records();
        let mut w = AscWriter::new(Vec::new(), AscDate::from_unix_ms(0)).unwrap();
        for r in &recs {
            w.write(r).unwrap();
        }
        w.write_footer().unwrap();
        let text = String::from_utf8(w.into_inner()).unwrap();
        assert!(text.starts_with("date Thu Jan 01 12:00:00.000 am 1970\nbase hex"));
        assert!(text.trim_end().ends_with("End TriggerBlock"));
        assert_eq!(read_all(&text), recs);
    }

    #[test]
    fn line_format() {
        let mut w = AscWriter::new(Vec::new(), AscDate::from_unix_ms(0)).unwrap();
        w.write(&sample_records()[0]).unwrap();
        w.write(&sample_records()[1]).unwrap();
        let text = String::from_utf8(w.into_inner()).unwrap();
        assert!(text.contains("   0.010270 1  100             Tx   d 8 30 D4 7F A9 01 01 00 00\n"));
        assert!(text.contains("   0.020000 2  1ABCDEF0x       Rx   d 2 01 02\n"));
    }

    #[test]
    fn parses_vector_snippet() {
        let asc = "\
date Tue Sep 9 11:30:14.032 am 2025
base hex  timestamps absolute
internal events logged
// version 9.0.0
Begin Triggerblock Tue Sep 9 11:30:14.032 am 2025
   0.000000 Start of measurement
   0.010270 1  100             Tx   d 8 30 D4 7F A9 01 01 00 00  Length = 0 BitCount = 0 ID = 256
   0.012000 1  18FEF100x       Rx   d 3 FF 00 1A
   0.015000 2  7E8   Rx  d 4 01 02 03 04
   0.020000 1  ErrorFrame
   0.021000 1 Statistic: D 10 R 0 XD 0 XR 0 E 0 O 0 B 0.00%
   0.030000 CANFD   1 Rx        1FF  EngineData                1 1 9 12 01 02 03 04 05 06 07 08 09 0A 0B 0C 80000 90 4000 0 0 0 0 0
   0.031000 CANFD   2 Tx   12345678x                          0 0 3  3 AA BB CC 0 0 0 0 0 0 0 0
End TriggerBlock
";
        let r = read_all(asc);
        assert_eq!(r.len(), 6);
        let RecordKind::Frame(f) = r[0].kind else {
            panic!()
        };
        assert_eq!((f.id, f.dlc, r[0].time.0), (0x100, 8, 10_270_000));
        let RecordKind::Frame(f) = r[1].kind else {
            panic!()
        };
        assert_eq!(
            (f.id, f.extended, r[1].dir),
            (0x18FE_F100, true, Direction::Rx)
        );
        assert_eq!(f.payload(), [0xFF, 0x00, 0x1A]);
        assert_eq!(r[2].channel, 2);
        assert_eq!(r[3].kind, RecordKind::ErrorFrame);
        let RecordKind::Frame(f) = r[4].kind else {
            panic!()
        };
        assert!(f.fd && f.brs && !f.extended);
        assert_eq!((f.id, f.dlc), (0x1FF, 12));
        assert_eq!(f.data[11], 0x0C);
        let RecordKind::Frame(f) = r[5].kind else {
            panic!()
        };
        assert!(f.fd && !f.brs && f.extended);
        assert_eq!((f.id, f.payload()), (0x1234_5678, &[0xAA, 0xBB, 0xCC][..]));
    }

    #[test]
    fn relative_timestamps_accumulate() {
        let asc = "base hex  timestamps relative\n\
                   0.5 1 100 Tx d 1 01\n\
                   0.25 1 100 Tx d 1 02\n\
                   0.125 1 ErrorFrame\n";
        let t: Vec<u64> = read_all(asc).iter().map(|r| r.time.0).collect();
        assert_eq!(t, [500_000_000, 750_000_000, 875_000_000]);
    }

    #[test]
    fn decimal_base() {
        let asc = "base dec  timestamps absolute\n\
                   1.000000 1 256 Rx d 3 10 255 0\n\
                   2.000000 CANFD 1 Tx 291x 1 0 9 12 1 2 3 4 5 6 7 8 9 10 11 12\n";
        let r = read_all(asc);
        let RecordKind::Frame(f) = r[0].kind else {
            panic!()
        };
        assert_eq!((f.id, f.payload()), (256, &[10, 255, 0][..]));
        let RecordKind::Frame(f) = r[1].kind else {
            panic!()
        };
        assert_eq!((f.id, f.extended, f.dlc), (291, true, 12));
        assert_eq!(f.data[11], 12);
    }

    #[test]
    fn errors_carry_line_numbers() {
        let asc = "base hex\n// c\n0.1 1 100 Tx d 2 01 02\n0.2 1 100 Tx d 3 01 02\n";
        let mut it = AscReader::new(asc.as_bytes());
        assert!(it.next().unwrap().is_ok());
        let e = it.next().unwrap().unwrap_err();
        assert!(matches!(e, LogError::Parse { line: 4, .. }), "{e}");
        assert!(e.to_string().starts_with("line 4:"));

        let bad_id = "0.1 1 XYZ Tx d 0\n";
        let e = AscReader::new(bad_id.as_bytes())
            .next()
            .unwrap()
            .unwrap_err();
        assert!(matches!(e, LogError::Parse { line: 1, .. }));
        let too_big = "0.1 1 800 Tx d 0\n";
        assert!(AscReader::new(too_big.as_bytes()).next().unwrap().is_err());
    }
}
