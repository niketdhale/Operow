use thiserror::Error;

use crate::dtc::{Dtc, dtc_to_string, status_bit_names};
use crate::nrc::Nrc;

/// Decoding failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UdsError {
    #[error("empty message")]
    Empty,
    #[error("{0} message is too short or malformed")]
    Malformed(&'static str),
}

pub type Result<T> = std::result::Result<T, UdsError>;

/// Sub-function of RoutineControl (0x31).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineSub {
    Start,
    Stop,
    Results,
}

impl RoutineSub {
    pub fn to_u8(self) -> u8 {
        match self {
            RoutineSub::Start => 1,
            RoutineSub::Stop => 2,
            RoutineSub::Results => 3,
        }
    }

    pub fn from_u8(v: u8) -> Option<RoutineSub> {
        match v {
            1 => Some(RoutineSub::Start),
            2 => Some(RoutineSub::Stop),
            3 => Some(RoutineSub::Results),
            _ => None,
        }
    }
}

/// Name of a service id (request SID), if known.
pub fn service_name(sid: u8) -> Option<&'static str> {
    Some(match sid {
        0x10 => "DiagnosticSessionControl",
        0x11 => "EcuReset",
        0x14 => "ClearDiagnosticInformation",
        0x19 => "ReadDtcInformation",
        0x22 => "ReadDataByIdentifier",
        0x27 => "SecurityAccess",
        0x2E => "WriteDataByIdentifier",
        0x31 => "RoutineControl",
        0x3E => "TesterPresent",
        _ => return None,
    })
}

fn sid_label(sid: u8) -> String {
    match service_name(sid) {
        Some(n) => n.to_string(),
        None => format!("service 0x{sid:02X}"),
    }
}

/// A UDS request. Sub-function bytes (`DiagnosticSessionControl`,
/// `EcuReset`, the security levels, `ReadDtcInformation::sub`) are the wire
/// value and may include the suppress-positive-response bit 0x80.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    DiagnosticSessionControl(u8),
    EcuReset(u8),
    /// Odd security level (0x01, 0x03, ...).
    SecurityAccessRequestSeed(u8),
    /// `level` is the sub-function as sent: the even value (seed level + 1).
    SecurityAccessSendKey {
        level: u8,
        key: Vec<u8>,
    },
    ReadDataByIdentifier(Vec<u16>),
    WriteDataByIdentifier {
        did: u16,
        data: Vec<u8>,
    },
    RoutineControl {
        sub: RoutineSub,
        rid: u16,
        data: Vec<u8>,
    },
    /// `sub` 0x01 (count by mask), 0x02 (by status mask), 0x0A (supported);
    /// `mask` is the status mask byte, absent for 0x0A.
    ReadDtcInformation {
        sub: u8,
        mask: Option<u8>,
    },
    /// 24-bit group of DTC.
    ClearDiagnosticInformation(u32),
    TesterPresent {
        suppress: bool,
    },
    /// Any other message, unchanged.
    Raw(Vec<u8>),
}

impl Request {
    pub fn sid(&self) -> u8 {
        match self {
            Request::DiagnosticSessionControl(_) => 0x10,
            Request::EcuReset(_) => 0x11,
            Request::SecurityAccessRequestSeed(_) | Request::SecurityAccessSendKey { .. } => 0x27,
            Request::ReadDataByIdentifier(_) => 0x22,
            Request::WriteDataByIdentifier { .. } => 0x2E,
            Request::RoutineControl { .. } => 0x31,
            Request::ReadDtcInformation { .. } => 0x19,
            Request::ClearDiagnosticInformation(_) => 0x14,
            Request::TesterPresent { .. } => 0x3E,
            Request::Raw(d) => d.first().copied().unwrap_or(0),
        }
    }

    /// The sub-function with the suppress bit removed, for services that
    /// have one.
    pub fn sub_function(&self) -> Option<u8> {
        Some(match self {
            Request::DiagnosticSessionControl(s) | Request::EcuReset(s) => s & 0x7F,
            Request::SecurityAccessRequestSeed(l) => l & 0x7F,
            Request::SecurityAccessSendKey { level, .. } => level & 0x7F,
            Request::ReadDtcInformation { sub, .. } => sub & 0x7F,
            Request::RoutineControl { sub, .. } => sub.to_u8(),
            Request::TesterPresent { .. } => 0,
            _ => return None,
        })
    }

    /// Whether the suppress-positive-response bit is set.
    pub fn suppress_positive_response(&self) -> bool {
        match self {
            Request::DiagnosticSessionControl(s) | Request::EcuReset(s) => s & 0x80 != 0,
            Request::SecurityAccessRequestSeed(l) => l & 0x80 != 0,
            Request::SecurityAccessSendKey { level, .. } => level & 0x80 != 0,
            Request::ReadDtcInformation { sub, .. } => sub & 0x80 != 0,
            Request::TesterPresent { suppress } => *suppress,
            _ => false,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            Request::DiagnosticSessionControl(s) => vec![0x10, *s],
            Request::EcuReset(s) => vec![0x11, *s],
            Request::SecurityAccessRequestSeed(l) => vec![0x27, *l],
            Request::SecurityAccessSendKey { level, key } => {
                let mut v = vec![0x27, *level];
                v.extend_from_slice(key);
                v
            }
            Request::ReadDataByIdentifier(dids) => {
                let mut v = vec![0x22];
                for d in dids {
                    v.extend_from_slice(&d.to_be_bytes());
                }
                v
            }
            Request::WriteDataByIdentifier { did, data } => {
                let mut v = vec![0x2E];
                v.extend_from_slice(&did.to_be_bytes());
                v.extend_from_slice(data);
                v
            }
            Request::RoutineControl { sub, rid, data } => {
                let mut v = vec![0x31, sub.to_u8()];
                v.extend_from_slice(&rid.to_be_bytes());
                v.extend_from_slice(data);
                v
            }
            Request::ReadDtcInformation { sub, mask } => {
                let mut v = vec![0x19, *sub];
                v.extend(*mask);
                v
            }
            Request::ClearDiagnosticInformation(g) => {
                let b = g.to_be_bytes();
                vec![0x14, b[1], b[2], b[3]]
            }
            Request::TesterPresent { suppress } => vec![0x3E, if *suppress { 0x80 } else { 0 }],
            Request::Raw(d) => d.clone(),
        }
    }

    /// Decode a request. Unknown service ids and unknown sub-functions of
    /// known services give `Raw`; known services with a malformed body give
    /// an error.
    pub fn decode(b: &[u8]) -> Result<Request> {
        let (&sid, p) = b.split_first().ok_or(UdsError::Empty)?;
        let bad = |what| Err(UdsError::Malformed(what));
        Ok(match sid {
            0x10 => match p {
                [s] => Request::DiagnosticSessionControl(*s),
                _ => return bad("DiagnosticSessionControl"),
            },
            0x11 => match p {
                [s] => Request::EcuReset(*s),
                _ => return bad("EcuReset"),
            },
            0x27 => match p.split_first() {
                Some((&l, rest)) if (l & 0x7F) % 2 == 1 => {
                    let _ = rest; // optional security data record is ignored
                    Request::SecurityAccessRequestSeed(l)
                }
                Some((&l, rest)) => Request::SecurityAccessSendKey {
                    level: l,
                    key: rest.to_vec(),
                },
                None => return bad("SecurityAccess"),
            },
            0x22 => {
                if p.is_empty() || p.len() % 2 != 0 {
                    return bad("ReadDataByIdentifier");
                }
                Request::ReadDataByIdentifier(
                    p.chunks(2)
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect(),
                )
            }
            0x2E => match p {
                [h, l, data @ ..] if !data.is_empty() => Request::WriteDataByIdentifier {
                    did: u16::from_be_bytes([*h, *l]),
                    data: data.to_vec(),
                },
                _ => return bad("WriteDataByIdentifier"),
            },
            0x31 => match p {
                [s, h, l, data @ ..] => match RoutineSub::from_u8(s & 0x7F) {
                    Some(sub) => Request::RoutineControl {
                        sub,
                        rid: u16::from_be_bytes([*h, *l]),
                        data: data.to_vec(),
                    },
                    None => Request::Raw(b.to_vec()),
                },
                _ => return bad("RoutineControl"),
            },
            0x19 => match p {
                [s] if s & 0x7F == 0x0A => Request::ReadDtcInformation {
                    sub: *s,
                    mask: None,
                },
                [s, m] => Request::ReadDtcInformation {
                    sub: *s,
                    mask: Some(*m),
                },
                _ => return bad("ReadDtcInformation"),
            },
            0x14 => match p {
                [a, b, c] => {
                    Request::ClearDiagnosticInformation(u32::from_be_bytes([0, *a, *b, *c]))
                }
                _ => return bad("ClearDiagnosticInformation"),
            },
            0x3E => match p {
                [s] if s & 0x7F == 0 => Request::TesterPresent {
                    suppress: s & 0x80 != 0,
                },
                [_] => Request::Raw(b.to_vec()),
                _ => return bad("TesterPresent"),
            },
            _ => Request::Raw(b.to_vec()),
        })
    }
}

/// A UDS response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// Timing values in milliseconds (P2* is sent in units of 10 ms).
    DiagnosticSessionControl {
        session: u8,
        p2_ms: u16,
        p2_star_ms: u16,
    },
    EcuReset(u8),
    SecuritySeed {
        level: u8,
        seed: Vec<u8>,
    },
    SecurityKeyAccepted {
        level: u8,
    },
    /// First DID with everything that follows it. With several requested
    /// DIDs `data` also holds the later `DID + data` records, because their
    /// lengths are not self-describing.
    ReadDataByIdentifier {
        did: u16,
        data: Vec<u8>,
    },
    WriteDataByIdentifier {
        did: u16,
    },
    RoutineControl {
        sub: RoutineSub,
        rid: u16,
        status: Vec<u8>,
    },
    DtcCount {
        availability_mask: u8,
        format: u8,
        count: u16,
    },
    DtcList {
        sub: u8,
        availability_mask: u8,
        dtcs: Vec<Dtc>,
    },
    ClearDiagnosticInformation,
    TesterPresent,
    Negative {
        sid: u8,
        nrc: Nrc,
    },
    /// Any other message, unchanged.
    Raw(Vec<u8>),
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Response::DiagnosticSessionControl {
                session,
                p2_ms,
                p2_star_ms,
            } => {
                let mut v = vec![0x50, *session];
                v.extend_from_slice(&p2_ms.to_be_bytes());
                v.extend_from_slice(&(p2_star_ms / 10).to_be_bytes());
                v
            }
            Response::EcuReset(k) => vec![0x51, *k],
            Response::SecuritySeed { level, seed } => {
                let mut v = vec![0x67, *level];
                v.extend_from_slice(seed);
                v
            }
            Response::SecurityKeyAccepted { level } => vec![0x67, *level],
            Response::ReadDataByIdentifier { did, data } => {
                let mut v = vec![0x62];
                v.extend_from_slice(&did.to_be_bytes());
                v.extend_from_slice(data);
                v
            }
            Response::WriteDataByIdentifier { did } => {
                let b = did.to_be_bytes();
                vec![0x6E, b[0], b[1]]
            }
            Response::RoutineControl { sub, rid, status } => {
                let mut v = vec![0x71, sub.to_u8()];
                v.extend_from_slice(&rid.to_be_bytes());
                v.extend_from_slice(status);
                v
            }
            Response::DtcCount {
                availability_mask,
                format,
                count,
            } => {
                let c = count.to_be_bytes();
                vec![0x59, 0x01, *availability_mask, *format, c[0], c[1]]
            }
            Response::DtcList {
                sub,
                availability_mask,
                dtcs,
            } => {
                let mut v = vec![0x59, *sub, *availability_mask];
                for d in dtcs {
                    let c = d.code.to_be_bytes();
                    v.extend_from_slice(&[c[1], c[2], c[3], d.status]);
                }
                v
            }
            Response::ClearDiagnosticInformation => vec![0x54],
            Response::TesterPresent => vec![0x7E, 0x00],
            Response::Negative { sid, nrc } => vec![0x7F, *sid, nrc.to_u8()],
            Response::Raw(d) => d.clone(),
        }
    }

    pub fn decode(b: &[u8]) -> Result<Response> {
        let (&sid, p) = b.split_first().ok_or(UdsError::Empty)?;
        let bad = |what| Err(UdsError::Malformed(what));
        Ok(match sid {
            0x7F => match p {
                [s, n] => Response::Negative {
                    sid: *s,
                    nrc: Nrc::from_u8(*n),
                },
                _ => return bad("negative response"),
            },
            0x50 => match p {
                [s, a, b, c, d] => Response::DiagnosticSessionControl {
                    session: *s,
                    p2_ms: u16::from_be_bytes([*a, *b]),
                    p2_star_ms: u16::from_be_bytes([*c, *d]).saturating_mul(10),
                },
                _ => return bad("DiagnosticSessionControl response"),
            },
            0x51 => match p {
                [k, ..] => Response::EcuReset(*k),
                _ => return bad("EcuReset response"),
            },
            0x67 => match p.split_first() {
                Some((&l, rest)) if l & 1 == 1 => Response::SecuritySeed {
                    level: l,
                    seed: rest.to_vec(),
                },
                Some((&l, _)) => Response::SecurityKeyAccepted { level: l },
                None => return bad("SecurityAccess response"),
            },
            0x62 => match p {
                [h, l, data @ ..] => Response::ReadDataByIdentifier {
                    did: u16::from_be_bytes([*h, *l]),
                    data: data.to_vec(),
                },
                _ => return bad("ReadDataByIdentifier response"),
            },
            0x6E => match p {
                [h, l] => Response::WriteDataByIdentifier {
                    did: u16::from_be_bytes([*h, *l]),
                },
                _ => return bad("WriteDataByIdentifier response"),
            },
            0x71 => match p {
                [s, h, l, status @ ..] => match RoutineSub::from_u8(*s) {
                    Some(sub) => Response::RoutineControl {
                        sub,
                        rid: u16::from_be_bytes([*h, *l]),
                        status: status.to_vec(),
                    },
                    None => Response::Raw(b.to_vec()),
                },
                _ => return bad("RoutineControl response"),
            },
            0x59 => match p {
                [0x01, m, f, h, l] => Response::DtcCount {
                    availability_mask: *m,
                    format: *f,
                    count: u16::from_be_bytes([*h, *l]),
                },
                [s, m, rest @ ..] if matches!(s & 0x7F, 0x02 | 0x0A) && rest.len() % 4 == 0 => {
                    Response::DtcList {
                        sub: *s,
                        availability_mask: *m,
                        dtcs: rest
                            .chunks(4)
                            .map(|c| Dtc {
                                code: u32::from_be_bytes([0, c[0], c[1], c[2]]),
                                status: c[3],
                            })
                            .collect(),
                    }
                }
                _ => Response::Raw(b.to_vec()),
            },
            0x54 if p.is_empty() => Response::ClearDiagnosticInformation,
            0x7E => match p {
                [_] => Response::TesterPresent,
                _ => return bad("TesterPresent response"),
            },
            _ => Response::Raw(b.to_vec()),
        })
    }
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn describe_request(r: &Request) -> String {
    let sup = if r.suppress_positive_response() {
        " (suppress positive response)"
    } else {
        ""
    };
    match r {
        Request::DiagnosticSessionControl(s) => {
            format!("DiagnosticSessionControl session=0x{:02X}{sup}", s & 0x7F)
        }
        Request::EcuReset(s) => format!("EcuReset type=0x{:02X}{sup}", s & 0x7F),
        Request::SecurityAccessRequestSeed(l) => {
            format!("SecurityAccess requestSeed level=0x{:02X}{sup}", l & 0x7F)
        }
        Request::SecurityAccessSendKey { level, key } => format!(
            "SecurityAccess sendKey level=0x{:02X} key={}{sup}",
            level & 0x7F,
            hex(key)
        ),
        Request::ReadDataByIdentifier(d) => format!(
            "ReadDataByIdentifier {}",
            d.iter()
                .map(|x| format!("{x:04X}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Request::WriteDataByIdentifier { did, data } => {
            format!("WriteDataByIdentifier {did:04X} data={}", hex(data))
        }
        Request::RoutineControl { sub, rid, data } => format!(
            "RoutineControl {sub:?} rid={rid:04X}{}",
            if data.is_empty() {
                String::new()
            } else {
                format!(" data={}", hex(data))
            }
        ),
        Request::ReadDtcInformation { sub, mask } => match mask {
            Some(m) => format!(
                "ReadDtcInformation sub=0x{:02X} mask=0x{m:02X}{sup}",
                sub & 0x7F
            ),
            None => format!("ReadDtcInformation sub=0x{:02X}{sup}", sub & 0x7F),
        },
        Request::ClearDiagnosticInformation(g) => {
            format!("ClearDiagnosticInformation group=0x{g:06X}")
        }
        Request::TesterPresent { suppress } => format!(
            "TesterPresent{}",
            if *suppress {
                " (suppress positive response)"
            } else {
                ""
            }
        ),
        Request::Raw(d) => match d.first() {
            Some(&sid) => format!("{} data={}", sid_label(sid), hex(&d[1..])),
            None => "empty".to_string(),
        },
    }
}

fn describe_response(r: &Response) -> String {
    match r {
        Response::DiagnosticSessionControl {
            session,
            p2_ms,
            p2_star_ms,
        } => format!(
            "Positive DiagnosticSessionControl session=0x{session:02X} P2={p2_ms}ms P2*={p2_star_ms}ms"
        ),
        Response::EcuReset(k) => format!("Positive EcuReset type=0x{k:02X}"),
        Response::SecuritySeed { level, seed } => {
            format!(
                "Positive SecurityAccess seed level=0x{level:02X} seed={}",
                hex(seed)
            )
        }
        Response::SecurityKeyAccepted { level } => {
            format!("Positive SecurityAccess key accepted level=0x{level:02X}")
        }
        Response::ReadDataByIdentifier { did, data } => {
            format!("Positive ReadDataByIdentifier {did:04X} = {}", hex(data))
        }
        Response::WriteDataByIdentifier { did } => {
            format!("Positive WriteDataByIdentifier {did:04X}")
        }
        Response::RoutineControl { sub, rid, status } => format!(
            "Positive RoutineControl {sub:?} rid={rid:04X}{}",
            if status.is_empty() {
                String::new()
            } else {
                format!(" status={}", hex(status))
            }
        ),
        Response::DtcCount { count, .. } => format!("Positive ReadDtcInformation count={count}"),
        Response::DtcList { dtcs, .. } => format!(
            "Positive ReadDtcInformation {} DTC(s){}",
            dtcs.len(),
            dtcs.iter()
                .map(|d| format!(
                    " {} [{:02X} {}]",
                    dtc_to_string(d.code),
                    d.status,
                    status_bit_names(d.status).join(",")
                ))
                .collect::<String>()
        ),
        Response::ClearDiagnosticInformation => "Positive ClearDiagnosticInformation".to_string(),
        Response::TesterPresent => "Positive TesterPresent".to_string(),
        Response::Negative { sid, nrc } => format!(
            "Negative {} NRC 0x{:02X} {}",
            sid_label(*sid),
            nrc.to_u8(),
            nrc.name()
        ),
        Response::Raw(d) => format!("data={}", hex(d)),
    }
}

/// Human-readable one-line summary of a UDS message, for traces and
/// consoles. Undecodable bytes fall back to a hex dump.
pub fn describe(bytes: &[u8], is_request: bool) -> String {
    if is_request {
        match Request::decode(bytes) {
            Ok(r) => describe_request(&r),
            Err(UdsError::Empty) => "empty".to_string(),
            Err(e) => format!("{e}: {}", hex(bytes)),
        }
    } else {
        match Response::decode(bytes) {
            Ok(r) => describe_response(&r),
            Err(UdsError::Empty) => "empty".to_string(),
            Err(e) => format!("{e}: {}", hex(bytes)),
        }
    }
}
