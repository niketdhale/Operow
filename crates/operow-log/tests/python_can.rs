//! Interop with python-can: `fixtures/python_can.blf` was written by
//! python-can's `BLFWriter` (CAN_MESSAGE, CAN_FD_MESSAGE_64 and CAN_ERROR_EXT
//! objects). python-can numbers channels from 0 and writes channel + 1, so
//! its channel 1 reads back as 2 here. It also pads objects with `size % 4`
//! bytes, which the odd-length FD frames exercise.

use operow_core::{CanFrame, Direction, Timestamp};
use operow_log::{BlfReader, LogRecord, RecordKind};

fn rec(t_ms: u64, ch: u8, dir: Direction, f: CanFrame) -> LogRecord {
    LogRecord {
        time: Timestamp(t_ms * 1_000_000),
        channel: ch,
        dir,
        kind: RecordKind::Frame(f),
    }
}

#[test]
fn reads_a_python_can_file() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/python_can.blf"
    ))
    .unwrap();
    let got: Vec<LogRecord> = BlfReader::new(bytes.as_slice())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let d12: Vec<u8> = (0..12).collect();
    let d64: Vec<u8> = (0..64).collect();
    let want = vec![
        rec(
            0,
            2,
            Direction::Rx,
            CanFrame::new(0x123, false, &[1, 2, 3]).unwrap(),
        ),
        rec(
            10,
            2,
            Direction::Tx,
            CanFrame::new(0x1ABCDEF, true, &[9; 8]).unwrap(),
        ),
        rec(
            20,
            3,
            Direction::Tx,
            CanFrame::new_fd(0x456, false, true, &d12).unwrap(),
        ),
        rec(
            30,
            2,
            Direction::Rx,
            CanFrame::new_fd(0x1ABCDEF, true, true, &d64).unwrap(),
        ),
        rec(
            40,
            2,
            Direction::Rx,
            CanFrame::new_fd(0x10, false, false, &[5; 8]).unwrap(),
        ),
        LogRecord {
            time: Timestamp(50_000_000),
            channel: 2,
            dir: Direction::Rx,
            kind: RecordKind::ErrorFrame,
        },
        rec(
            60,
            2,
            Direction::Rx,
            CanFrame::new_fd(0x20, false, false, &[7, 8, 9]).unwrap(),
        ),
        rec(
            70,
            2,
            Direction::Tx,
            CanFrame::new_fd(0x21, false, false, &[1]).unwrap(),
        ),
        rec(
            80,
            2,
            Direction::Rx,
            CanFrame::new(0x22, false, &[]).unwrap(),
        ),
    ];
    assert_eq!(got.len(), want.len(), "{got:#?}");
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g, w);
    }
}
