//! Vector binary logging format (`.blf`) reader and writer for classic CAN,
//! CAN FD and error frames.
//!
//! A file is a `LOGG` header followed by `LOBJ` objects. Nearly all data sits
//! in zlib-compressed `LOG_CONTAINER` objects whose concatenated payload is a
//! stream of further objects; an object may straddle two containers.

use std::io::{self, Read, Seek, SeekFrom, Write};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use operow_core::{CanFrame, Direction, Timestamp};

use crate::date::AscDate;
use crate::record::{LogError, LogRecord, LogWriter, RecordKind};

pub(crate) const BLF_SIGNATURE: [u8; 4] = *b"LOGG";
const OBJ_SIGNATURE: [u8; 4] = *b"LOBJ";

const FILE_HEADER_SIZE: usize = 144;
/// Signature, header size, header version, object size and type.
const BASE_HEADER_SIZE: usize = 16;
/// Base header plus flags, client index, object version and timestamp.
const HEADER_V1_SIZE: usize = 32;
/// Container header: base header plus method, reserved and sizes.
const CONTAINER_HEADER_SIZE: usize = 32;
/// Refuse absurd object sizes instead of allocating them.
const MAX_OBJECT_SIZE: usize = 1 << 28;
/// Uncompressed bytes collected before the writer emits a container.
const CONTAINER_TARGET: usize = 128 * 1024;

const CAN_MESSAGE: u32 = 1;
const CAN_ERROR: u32 = 2;
const LOG_CONTAINER: u32 = 10;
const CAN_ERROR_EXT: u32 = 73;
const CAN_MESSAGE2: u32 = 86;
const CAN_FD_MESSAGE: u32 = 100;
const CAN_FD_MESSAGE_64: u32 = 101;
const CAN_FD_ERROR_64: u32 = 104;

const FLAGS_10_US: u32 = 1;
const FLAGS_1_NS: u32 = 2;

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

fn err(msg: impl Into<String>) -> LogError {
    LogError::Blf(msg.into())
}

fn channel(c: u16) -> u8 {
    u8::try_from(c).unwrap_or(u8::MAX)
}

/// Streams records out of a BLF file.
pub struct BlfReader<R: Read> {
    input: R,
    /// Decompressed object stream not yet parsed; `buf[pos..]` is pending.
    buf: Vec<u8>,
    pos: usize,
    /// The input is exhausted (or failed).
    done: bool,
}

impl<R: Read> BlfReader<R> {
    /// Read and check the file header.
    pub fn new(mut input: R) -> Result<Self, LogError> {
        let mut head = [0u8; 8];
        input.read_exact(&mut head).map_err(truncated)?;
        if head[..4] != BLF_SIGNATURE {
            return Err(err("not a BLF file (missing LOGG signature)"));
        }
        let size = u32_at(&head, 4) as usize;
        if !(8..=MAX_OBJECT_SIZE).contains(&size) {
            return Err(err(format!("bad file header size {size}")));
        }
        // The rest of the header (versions, sizes, times) is not needed.
        input
            .read_exact(&mut vec![0u8; size - 8])
            .map_err(truncated)?;
        Ok(BlfReader {
            input,
            buf: Vec::new(),
            pos: 0,
            done: false,
        })
    }

    /// Read the next top-level object into the pending buffer. `Ok(false)`
    /// at a clean end of file.
    fn fill(&mut self) -> Result<bool, LogError> {
        let mut head = [0u8; BASE_HEADER_SIZE];
        let mut got = 0;
        while got < head.len() {
            match self.input.read(&mut head[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        if got == 0 {
            return Ok(false);
        }
        if got < head.len() {
            // Zero padding after the last object is not an error.
            if head[..got].iter().all(|&b| b == 0) {
                return Ok(false);
            }
            return Err(err("truncated object header"));
        }
        if head[..4] != OBJ_SIGNATURE {
            return Err(err("bad object signature"));
        }
        let size = u32_at(&head, 8) as usize;
        let kind = u32_at(&head, 12);
        if !(BASE_HEADER_SIZE..=MAX_OBJECT_SIZE).contains(&size) {
            return Err(err(format!("bad object size {size}")));
        }
        let mut body = vec![0u8; size - BASE_HEADER_SIZE];
        self.input.read_exact(&mut body).map_err(truncated)?;
        // Vector pads every object with `size % 4` zero bytes (not up to a
        // 4-byte boundary, despite appearances); a missing
        // final pad is harmless.
        let pad = size % 4;
        let _ = self.input.read_exact(&mut [0u8; 3][..pad]);
        self.compact();
        if kind == LOG_CONTAINER {
            if body.len() < CONTAINER_HEADER_SIZE - BASE_HEADER_SIZE {
                return Err(err("truncated container header"));
            }
            let method = u16_at(&body, 0);
            let data = &body[CONTAINER_HEADER_SIZE - BASE_HEADER_SIZE..];
            match method {
                0 => self.buf.extend_from_slice(data),
                2 => {
                    ZlibDecoder::new(data)
                        .read_to_end(&mut self.buf)
                        .map_err(|e| err(format!("bad compressed container: {e}")))?;
                }
                m => return Err(err(format!("unsupported compression method {m}"))),
            }
        } else {
            // A bare object outside any container.
            self.buf.extend_from_slice(&head);
            self.buf.extend_from_slice(&body);
            self.buf.resize(self.buf.len() + pad, 0);
        }
        Ok(true)
    }

    fn compact(&mut self) {
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }

    /// Parse one complete object from the pending buffer: `None` when more
    /// data is needed, `Some(None)` for a skipped object.
    fn take_object(&mut self) -> Result<Option<Option<LogRecord>>, LogError> {
        let rest = &self.buf[self.pos..];
        if rest.len() < BASE_HEADER_SIZE {
            return Ok(None);
        }
        if rest[..4] != OBJ_SIGNATURE {
            return Err(err("bad object signature inside container"));
        }
        let header_size = u16_at(rest, 4) as usize;
        let size = u32_at(rest, 8) as usize;
        let kind = u32_at(rest, 12);
        if !(BASE_HEADER_SIZE..=MAX_OBJECT_SIZE).contains(&size) {
            return Err(err(format!("bad object size {size}")));
        }
        let total = size + size % 4;
        if rest.len() < size {
            return Ok(None);
        }
        let obj = &rest[..size];
        let record = decode(obj, header_size, kind)?;
        self.pos += total.min(rest.len());
        Ok(Some(record))
    }
}

fn truncated(e: io::Error) -> LogError {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        err("truncated file or object")
    } else {
        e.into()
    }
}

/// Decode one object; `None` for types that are not bus traffic.
fn decode(obj: &[u8], header_size: usize, kind: u32) -> Result<Option<LogRecord>, LogError> {
    if !matches!(
        kind,
        CAN_MESSAGE
            | CAN_MESSAGE2
            | CAN_FD_MESSAGE
            | CAN_FD_MESSAGE_64
            | CAN_ERROR
            | CAN_ERROR_EXT
            | CAN_FD_ERROR_64
    ) {
        return Ok(None);
    }
    if header_size < HEADER_V1_SIZE || header_size > obj.len() {
        return Err(err(format!(
            "bad header size {header_size} for type {kind}"
        )));
    }
    let flags = u32_at(obj, 16);
    let raw = u64_at(obj, 24);
    let ns = match flags {
        FLAGS_10_US => raw.saturating_mul(10_000),
        FLAGS_1_NS => raw,
        f => return Err(err(format!("unknown timestamp unit {f}"))),
    };
    let time = Timestamp(ns);
    let p = &obj[header_size..];
    let need = |n: usize| {
        if p.len() < n {
            Err(err(format!("object type {kind} too short")))
        } else {
            Ok(())
        }
    };
    let frame = |dir: Direction, ch: u8, f: Result<CanFrame, operow_core::FrameError>| {
        f.map(|f| LogRecord {
            time,
            channel: ch,
            dir,
            kind: RecordKind::Frame(f),
        })
        .map_err(|e| err(e.to_string()))
    };
    let split_id = |raw: u32| (raw & 0x1FFF_FFFF, raw & 0x8000_0000 != 0);
    let rec = match kind {
        CAN_MESSAGE | CAN_MESSAGE2 => {
            need(16)?;
            let f = p[2];
            if f & 0x80 != 0 {
                // Remote frame.
                return Ok(None);
            }
            let (id, ext) = split_id(u32_at(p, 4));
            let len = p[3].min(8) as usize;
            let dir = if f & 1 != 0 {
                Direction::Tx
            } else {
                Direction::Rx
            };
            frame(
                dir,
                channel(u16_at(p, 0)),
                CanFrame::new(
                    id & if ext { 0x1FFF_FFFF } else { 0x7FF },
                    ext,
                    &p[8..8 + len],
                ),
            )?
        }
        CAN_FD_MESSAGE => {
            need(84)?;
            let f = p[2];
            let (id, ext) = split_id(u32_at(p, 4));
            let fd_flags = p[13];
            let valid = (p[14] as usize).min(64);
            let dir = if f & 1 != 0 {
                Direction::Tx
            } else {
                Direction::Rx
            };
            let data = &p[20..20 + valid];
            let built = if fd_flags & 1 != 0 {
                CanFrame::new_fd(id, ext, fd_flags & 2 != 0, data)
            } else {
                CanFrame::new(id, ext, &data[..valid.min(8)])
            };
            frame(dir, channel(u16_at(p, 0)), built)?
        }
        CAN_FD_MESSAGE_64 => {
            need(40)?;
            let valid = (p[2] as usize).min(64);
            need(40 + valid)?;
            let (id, ext) = split_id(u32_at(p, 4));
            let flags = u32_at(p, 12);
            let dir = if p[34] != 0 {
                Direction::Tx
            } else {
                Direction::Rx
            };
            let data = &p[40..40 + valid];
            let built = if flags & (1 << 12) != 0 {
                CanFrame::new_fd(id, ext, flags & (1 << 13) != 0, data)
            } else {
                CanFrame::new(id, ext, &data[..valid.min(8)])
            };
            frame(dir, p[0], built)?
        }
        CAN_ERROR | CAN_ERROR_EXT => {
            need(2)?;
            error_record(time, channel(u16_at(p, 0)))
        }
        _ => {
            need(1)?;
            error_record(time, p[0])
        }
    };
    Ok(Some(rec))
}

fn error_record(time: Timestamp, channel: u8) -> LogRecord {
    LogRecord {
        time,
        channel,
        dir: Direction::Rx,
        kind: RecordKind::ErrorFrame,
    }
}

impl<R: Read> Iterator for BlfReader<R> {
    type Item = Result<LogRecord, LogError>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            match self.take_object() {
                Ok(Some(Some(r))) => return Some(Ok(r)),
                Ok(Some(None)) => continue,
                Ok(None) => {}
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
            match self.fill() {
                Ok(true) => {}
                Ok(false) => {
                    self.done = true;
                    // Leftover bytes beyond zero padding are a cut object.
                    if self.buf[self.pos..].iter().any(|&b| b != 0) {
                        return Some(Err(err("truncated object")));
                    }
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
        None
    }
}

/// Writes a BLF file with zlib-compressed containers. The output must be
/// seekable: the header's sizes and object count are patched on finish.
pub struct BlfWriter<W: Write + Seek> {
    out: W,
    start: AscDate,
    /// Uncompressed objects waiting for the next container.
    pending: Vec<u8>,
    objects: u32,
    /// Total uncompressed object bytes written.
    uncompressed: u64,
    /// Bytes written to `out` so far, header included.
    file_size: u64,
}

impl<W: Write + Seek> BlfWriter<W> {
    /// Write a provisional file header; `start` is the measurement start.
    pub fn new(mut out: W, start: AscDate) -> io::Result<Self> {
        out.write_all(&file_header(&start, 0, 0, 0))?;
        Ok(BlfWriter {
            out,
            start,
            pending: Vec::new(),
            objects: 0,
            uncompressed: 0,
            file_size: FILE_HEADER_SIZE as u64,
        })
    }

    fn push_object(&mut self, kind: u32, version: u16, time: Timestamp, payload: &[u8]) {
        let size = HEADER_V1_SIZE + payload.len();
        let p = &mut self.pending;
        p.extend_from_slice(&OBJ_SIGNATURE);
        p.extend_from_slice(&(HEADER_V1_SIZE as u16).to_le_bytes());
        p.extend_from_slice(&1u16.to_le_bytes());
        p.extend_from_slice(&(size as u32).to_le_bytes());
        p.extend_from_slice(&kind.to_le_bytes());
        p.extend_from_slice(&FLAGS_1_NS.to_le_bytes());
        p.extend_from_slice(&0u16.to_le_bytes());
        p.extend_from_slice(&version.to_le_bytes());
        p.extend_from_slice(&time.0.to_le_bytes());
        p.extend_from_slice(payload);
        p.resize(p.len() + size % 4, 0);
        self.objects += 1;
    }

    /// Compress `pending` into one container.
    fn flush_container(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&self.pending)?;
        let packed = enc.finish()?;
        let size = CONTAINER_HEADER_SIZE + packed.len();
        let mut head = Vec::with_capacity(CONTAINER_HEADER_SIZE);
        head.extend_from_slice(&OBJ_SIGNATURE);
        head.extend_from_slice(&(BASE_HEADER_SIZE as u16).to_le_bytes());
        head.extend_from_slice(&1u16.to_le_bytes());
        head.extend_from_slice(&(size as u32).to_le_bytes());
        head.extend_from_slice(&LOG_CONTAINER.to_le_bytes());
        head.extend_from_slice(&2u16.to_le_bytes());
        head.extend_from_slice(&[0u8; 6]);
        head.extend_from_slice(&(self.pending.len() as u32).to_le_bytes());
        head.extend_from_slice(&[0u8; 4]);
        self.out.write_all(&head)?;
        self.out.write_all(&packed)?;
        let pad = size % 4;
        self.out.write_all(&[0u8; 3][..pad])?;
        self.file_size += (size + pad) as u64;
        self.uncompressed += self.pending.len() as u64;
        self.pending.clear();
        Ok(())
    }

    /// Write out buffered records and flush the output. The header counts
    /// are only final after [`LogWriter::finish`].
    pub fn flush(&mut self) -> io::Result<()> {
        self.flush_container()?;
        self.out.flush()
    }

    /// Finish the file and return the underlying output.
    pub fn finish_into_inner(mut self) -> io::Result<W> {
        self.flush_container()?;
        let head = file_header(
            &self.start,
            self.file_size,
            self.uncompressed + FILE_HEADER_SIZE as u64,
            self.objects,
        );
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&head)?;
        self.out.seek(SeekFrom::End(0))?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// The 144-byte `LOGG` header.
fn file_header(start: &AscDate, file_size: u64, uncompressed: u64, objects: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(FILE_HEADER_SIZE);
    h.extend_from_slice(&BLF_SIGNATURE);
    h.extend_from_slice(&(FILE_HEADER_SIZE as u32).to_le_bytes());
    // Application id and version, binary log version 4.1.
    h.extend_from_slice(&[0, 1, 0, 0, 4, 1, 0, 0]);
    h.extend_from_slice(&file_size.to_le_bytes());
    h.extend_from_slice(&uncompressed.to_le_bytes());
    h.extend_from_slice(&objects.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    // SYSTEMTIME for the start and (unknown, so equal) stop time.
    for _ in 0..2 {
        for v in [
            start.year as u16,
            start.month as u16,
            start.weekday as u16,
            start.day as u16,
            start.hour as u16,
            start.minute as u16,
            start.second as u16,
            start.millis as u16,
        ] {
            h.extend_from_slice(&v.to_le_bytes());
        }
    }
    h.resize(FILE_HEADER_SIZE, 0);
    h
}

impl<W: Write + Seek> LogWriter for BlfWriter<W> {
    fn write(&mut self, r: &LogRecord) -> io::Result<()> {
        let tx = r.dir == Direction::Tx;
        match r.kind {
            RecordKind::ErrorFrame => {
                // CAN_ERROR_EXT with every field but the channel zeroed.
                let mut p = [0u8; 32];
                p[..2].copy_from_slice(&u16::from(r.channel).to_le_bytes());
                self.push_object(CAN_ERROR_EXT, 0, r.time, &p);
            }
            RecordKind::Frame(f) if !f.fd => {
                let mut p = [0u8; 16];
                p[..2].copy_from_slice(&u16::from(r.channel).to_le_bytes());
                p[2] = u8::from(tx);
                p[3] = f.dlc;
                let id = f.id | if f.extended { 0x8000_0000 } else { 0 };
                p[4..8].copy_from_slice(&id.to_le_bytes());
                p[8..8 + f.dlc as usize].copy_from_slice(f.payload());
                self.push_object(CAN_MESSAGE, 0, r.time, &p);
            }
            RecordKind::Frame(f) => {
                let mut p = vec![0u8; 40];
                p[0] = r.channel;
                p[1] = f.dlc_code();
                p[2] = f.dlc;
                let id = f.id | if f.extended { 0x8000_0000 } else { 0 };
                p[4..8].copy_from_slice(&id.to_le_bytes());
                let flags = 1u32 << 12 | u32::from(f.brs) << 13;
                p[12..16].copy_from_slice(&flags.to_le_bytes());
                p[34] = u8::from(tx);
                p.extend_from_slice(f.payload());
                self.push_object(CAN_FD_MESSAGE_64, 1, r.time, &p);
            }
        }
        if self.pending.len() >= CONTAINER_TARGET {
            self.flush_container()?;
        }
        Ok(())
    }

    fn finish(self) -> io::Result<()> {
        self.finish_into_inner().map(drop)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn rec(t_ns: u64, ch: u8, dir: Direction, f: CanFrame) -> LogRecord {
        LogRecord {
            time: Timestamp(t_ns),
            channel: ch,
            dir,
            kind: RecordKind::Frame(f),
        }
    }

    fn sample() -> Vec<LogRecord> {
        let d64: Vec<u8> = (0..64).collect();
        vec![
            rec(
                1_000,
                1,
                Direction::Rx,
                CanFrame::new(0x123, false, &[1, 2, 3]).unwrap(),
            ),
            rec(
                2_500_000,
                2,
                Direction::Tx,
                CanFrame::new(0x1ABC_DEF0, true, &[0xAA; 8]).unwrap(),
            ),
            rec(
                3_000_000,
                1,
                Direction::Tx,
                CanFrame::new_fd(0x456, false, true, &[7; 12]).unwrap(),
            ),
            rec(
                4_000_000,
                3,
                Direction::Rx,
                CanFrame::new_fd(0x1234_5678, true, false, &d64).unwrap(),
            ),
            LogRecord {
                time: Timestamp(5_000_000),
                channel: 2,
                dir: Direction::Rx,
                kind: RecordKind::ErrorFrame,
            },
            rec(
                6_000_000,
                1,
                Direction::Rx,
                CanFrame::new(0x7FF, false, &[]).unwrap(),
            ),
        ]
    }

    fn write_all(recs: &[LogRecord]) -> Vec<u8> {
        let mut w = BlfWriter::new(Cursor::new(Vec::new()), AscDate::from_unix_ms(0)).unwrap();
        for r in recs {
            w.write(r).unwrap();
        }
        w.finish_into_inner().unwrap().into_inner()
    }

    fn read_all(bytes: &[u8]) -> Result<Vec<LogRecord>, LogError> {
        BlfReader::new(bytes)?.collect()
    }

    #[test]
    fn round_trip() {
        let recs = sample();
        let bytes = write_all(&recs);
        assert_eq!(&bytes[..4], b"LOGG");
        assert_eq!(u32_at(&bytes, 32), recs.len() as u32);
        assert_eq!(u64_at(&bytes, 16), bytes.len() as u64);
        assert_eq!(read_all(&bytes).unwrap(), recs);
    }

    #[test]
    fn round_trip_many_containers() {
        let recs: Vec<LogRecord> = (0..5000u32)
            .map(|i| {
                rec(
                    u64::from(i) * 1000,
                    1 + (i % 3) as u8,
                    Direction::Rx,
                    CanFrame::new_fd(i & 0x7FF, false, i % 2 == 0, &[i as u8; 64]).unwrap(),
                )
            })
            .collect();
        let bytes = write_all(&recs);
        assert_eq!(read_all(&bytes).unwrap(), recs);
    }

    /// Hand-built object with a v1 header.
    fn object(kind: u32, unit: u32, ts: u64, payload: &[u8]) -> Vec<u8> {
        let size = HEADER_V1_SIZE + payload.len();
        let mut o = Vec::new();
        o.extend_from_slice(b"LOBJ");
        o.extend_from_slice(&32u16.to_le_bytes());
        o.extend_from_slice(&1u16.to_le_bytes());
        o.extend_from_slice(&(size as u32).to_le_bytes());
        o.extend_from_slice(&kind.to_le_bytes());
        o.extend_from_slice(&unit.to_le_bytes());
        o.extend_from_slice(&[0; 4]);
        o.extend_from_slice(&ts.to_le_bytes());
        o.extend_from_slice(payload);
        o.resize(o.len() + size % 4, 0);
        o
    }

    fn can_payload(id: u32, data: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 16];
        p[0] = 1;
        p[3] = data.len() as u8;
        p[4..8].copy_from_slice(&id.to_le_bytes());
        p[8..8 + data.len()].copy_from_slice(data);
        p
    }

    fn container(method: u16, data: &[u8]) -> Vec<u8> {
        let body = if method == 2 {
            let mut e = ZlibEncoder::new(Vec::new(), Compression::fast());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        } else {
            data.to_vec()
        };
        let size = CONTAINER_HEADER_SIZE + body.len();
        let mut c = Vec::new();
        c.extend_from_slice(b"LOBJ");
        c.extend_from_slice(&16u16.to_le_bytes());
        c.extend_from_slice(&1u16.to_le_bytes());
        c.extend_from_slice(&(size as u32).to_le_bytes());
        c.extend_from_slice(&LOG_CONTAINER.to_le_bytes());
        c.extend_from_slice(&method.to_le_bytes());
        c.extend_from_slice(&[0; 6]);
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(&[0; 4]);
        c.extend_from_slice(&body);
        c.resize(c.len() + size % 4, 0);
        c
    }

    fn file(body: &[u8]) -> Vec<u8> {
        let mut f = file_header(&AscDate::from_unix_ms(0), 0, 0, 0);
        f.extend_from_slice(body);
        f
    }

    #[test]
    fn uncompressed_container_and_units() {
        let mut inner = object(CAN_MESSAGE, FLAGS_10_US, 150, &can_payload(0x10, &[9, 8]));
        inner.extend(object(
            CAN_MESSAGE,
            FLAGS_1_NS,
            777,
            &can_payload(0x11, &[]),
        ));
        let recs = read_all(&file(&container(0, &inner))).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].time, Timestamp(1_500_000));
        assert_eq!(recs[0].channel, 1);
        assert_eq!(recs[1].time, Timestamp(777));
        let RecordKind::Frame(f) = recs[0].kind else {
            panic!()
        };
        assert_eq!((f.id, f.payload()), (0x10, &[9u8, 8][..]));
    }

    #[test]
    fn object_straddles_containers_and_unknown_skipped() {
        let mut stream = object(CAN_MESSAGE, FLAGS_1_NS, 1, &can_payload(1, &[1]));
        // Unknown type 65 (app text) with an odd size.
        stream.extend(object(65, FLAGS_1_NS, 2, &[1, 2, 3, 4, 5]));
        stream.extend(object(CAN_MESSAGE, FLAGS_1_NS, 3, &can_payload(2, &[2])));
        stream.extend(object(CAN_MESSAGE, FLAGS_1_NS, 4, &can_payload(3, &[3])));
        for cut in [10, 40, 50, 70, stream.len() - 5] {
            let mut body = container(2, &stream[..cut]);
            body.extend(container(2, &stream[cut..]));
            let ids: Vec<u32> = read_all(&file(&body))
                .unwrap()
                .iter()
                .map(|r| match r.kind {
                    RecordKind::Frame(f) => f.id,
                    RecordKind::ErrorFrame => 0,
                })
                .collect();
            assert_eq!(ids, [1, 2, 3], "cut at {cut}");
        }
    }

    #[test]
    fn truncated_is_an_error() {
        let bytes = write_all(&sample());
        for cut in [3, 100, bytes.len() - 7] {
            let res = read_all(&bytes[..cut]);
            assert!(matches!(res, Err(LogError::Blf(_))), "cut {cut}: {res:?}");
        }
        // Cut inside the uncompressed stream of a container.
        let inner = object(CAN_MESSAGE, FLAGS_1_NS, 1, &can_payload(1, &[1]));
        let res = read_all(&file(&container(0, &inner[..inner.len() - 6])));
        assert!(matches!(res, Err(LogError::Blf(_))), "{res:?}");
        assert!(read_all(b"nope....").is_err());
    }

    #[test]
    fn header_only_file_is_empty() {
        assert!(read_all(&file(&[])).unwrap().is_empty());
    }
}
