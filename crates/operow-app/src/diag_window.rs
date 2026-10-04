//! The Diagnostics window: a UDS console (request entry, history, decoded
//! detail, saved requests, SecurityAccess helper) and a DTC view, driving
//! the engine's virtual tester.
//!
//! Responses arrive as `EngineEvent::DiagResponse` without a window id, so
//! each window remembers its outstanding requests and the app offers every
//! response to the windows in turn ([`DiagWindow::on_response`]).

use std::collections::VecDeque;

use egui_extras::{Column, TableBuilder};
use operow_core::{BusId, DidEntry, KeyAlgo};
use operow_engine::Command;
use operow_uds::{
    Dtc, Nrc, Request, Response, RoutineSub, describe, dtc_to_string, service_name,
    status_bit_names,
};
use serde::{Deserialize, Serialize};

use crate::inspector::{RED, hex_bytes, parse_hex_u32};
use crate::trace::{DiagTarget, NameLookup};
use crate::workspace::WindowId;

/// History entries kept per window.
const MAX_HISTORY: usize = 500;
const ROW_H: f32 = 34.0;

// --- parsing and request building ------------------------------------------

/// Hex bytes of a raw request: `22 F1 90`, `22F190` or `22,F1,90`.
pub fn parse_request_hex(text: &str) -> Result<Vec<u8>, String> {
    let t = text.replace(',', " ");
    let t = t.trim();
    if t.is_empty() {
        return Err("empty request".into());
    }
    let tokens: Vec<&str> = t.split_whitespace().collect();
    let pairs: Vec<String>;
    let tokens = if tokens.len() == 1 && tokens[0].len() > 2 {
        let s = tokens[0].strip_prefix("0x").unwrap_or(tokens[0]);
        if !s.len().is_multiple_of(2) || !s.is_ascii() {
            return Err(format!("odd number of hex digits in {s:?}"));
        }
        pairs = s
            .as_bytes()
            .chunks(2)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        pairs.iter().map(String::as_str).collect()
    } else {
        tokens
    };
    tokens
        .iter()
        .map(|tok| {
            if tok.len() > 2 {
                return Err(format!("{tok:?} is not a byte"));
            }
            u8::from_str_radix(tok, 16).map_err(|_| format!("{tok:?} is not hex"))
        })
        .collect()
}

fn parse_u8(text: &str, what: &str) -> Result<u8, String> {
    parse_hex_u32(text)
        .and_then(|v| u8::try_from(v).ok())
        .ok_or_else(|| format!("{what} must be a hex byte"))
}

fn parse_u16(text: &str, what: &str) -> Result<u16, String> {
    parse_hex_u32(text)
        .and_then(|v| u16::try_from(v).ok())
        .ok_or_else(|| format!("{what} must be 4 hex digits"))
}

fn parse_bytes(text: &str, what: &str) -> Result<Vec<u8>, String> {
    let t = text.replace(',', " ");
    t.split_whitespace()
        .map(|tok| {
            u8::from_str_radix(tok, 16)
                .ok()
                .filter(|_| tok.len() <= 2)
                .ok_or_else(|| format!("{what}: {tok:?} is not a hex byte"))
        })
        .collect()
}

/// DIDs separated by spaces or commas.
fn parse_dids(text: &str) -> Result<Vec<u16>, String> {
    let t = text.replace(',', " ");
    let dids: Result<Vec<u16>, String> = t
        .split_whitespace()
        .map(|tok| parse_u16(tok, "DID"))
        .collect();
    let dids = dids?;
    if dids.is_empty() {
        return Err("enter at least one DID".into());
    }
    Ok(dids)
}

/// The services of the symbolic request builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Service {
    Session,
    Reset,
    SeedRequest,
    SendKey,
    ReadDid,
    WriteDid,
    Routine,
    ReadDtc,
    ClearDtc,
    TesterPresent,
}

impl Service {
    pub const ALL: [Service; 10] = [
        Service::Session,
        Service::Reset,
        Service::SeedRequest,
        Service::SendKey,
        Service::ReadDid,
        Service::WriteDid,
        Service::Routine,
        Service::ReadDtc,
        Service::ClearDtc,
        Service::TesterPresent,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Service::Session => "10 DiagnosticSessionControl",
            Service::Reset => "11 EcuReset",
            Service::SeedRequest => "27 SecurityAccess seed",
            Service::SendKey => "27 SecurityAccess key",
            Service::ReadDid => "22 ReadDataByIdentifier",
            Service::WriteDid => "2E WriteDataByIdentifier",
            Service::Routine => "31 RoutineControl",
            Service::ReadDtc => "19 ReadDTCInformation",
            Service::ClearDtc => "14 ClearDiagnosticInformation",
            Service::TesterPresent => "3E TesterPresent",
        }
    }
}

/// Field values of the symbolic builder; every field is kept as typed so
/// switching services does not lose input.
#[derive(Debug, Clone, PartialEq)]
pub struct Builder {
    pub service: Service,
    pub session: u8,
    pub reset: u8,
    /// Security level (sub-function): odd for the seed, even for the key.
    pub level: String,
    pub key: String,
    /// Space separated DIDs of a read.
    pub dids: String,
    pub did: String,
    pub data: String,
    pub routine_sub: u8,
    pub rid: String,
    pub dtc_sub: u8,
    pub mask: String,
    pub group: String,
    pub suppress: bool,
}

impl Default for Builder {
    fn default() -> Self {
        Builder {
            service: Service::Session,
            session: 3,
            reset: 1,
            level: "01".into(),
            key: String::new(),
            dids: "F190".into(),
            did: "0100".into(),
            data: String::new(),
            routine_sub: 1,
            rid: "FF00".into(),
            dtc_sub: 2,
            mask: "FF".into(),
            group: "FFFFFF".into(),
            suppress: false,
        }
    }
}

impl Builder {
    /// The request bytes for the current fields.
    pub fn build(&self) -> Result<Vec<u8>, String> {
        let sup = if self.suppress { 0x80 } else { 0 };
        let req = match self.service {
            Service::Session => Request::DiagnosticSessionControl(self.session | sup),
            Service::Reset => Request::EcuReset(self.reset | sup),
            Service::SeedRequest => {
                Request::SecurityAccessRequestSeed(parse_u8(&self.level, "level")? | sup)
            }
            Service::SendKey => {
                let key = parse_bytes(&self.key, "key")?;
                if key.is_empty() {
                    return Err("enter the key bytes".into());
                }
                Request::SecurityAccessSendKey {
                    level: parse_u8(&self.level, "level")? | sup,
                    key,
                }
            }
            Service::ReadDid => Request::ReadDataByIdentifier(parse_dids(&self.dids)?),
            Service::WriteDid => {
                let data = parse_bytes(&self.data, "data")?;
                if data.is_empty() {
                    return Err("enter the data to write".into());
                }
                Request::WriteDataByIdentifier {
                    did: parse_u16(&self.did, "DID")?,
                    data,
                }
            }
            Service::Routine => Request::RoutineControl {
                sub: RoutineSub::from_u8(self.routine_sub).unwrap_or(RoutineSub::Start),
                rid: parse_u16(&self.rid, "routine id")?,
                data: parse_bytes(&self.data, "data")?,
            },
            Service::ReadDtc => Request::ReadDtcInformation {
                sub: self.dtc_sub | sup,
                mask: if matches!(self.dtc_sub, 1 | 2) {
                    Some(parse_u8(&self.mask, "status mask")?)
                } else {
                    None
                },
            },
            Service::ClearDtc => {
                let g = parse_hex_u32(&self.group)
                    .filter(|g| *g <= 0xFF_FFFF)
                    .ok_or("group must be up to 6 hex digits")?;
                Request::ClearDiagnosticInformation(g)
            }
            Service::TesterPresent => Request::TesterPresent {
                suppress: self.suppress,
            },
        };
        Ok(req.encode())
    }
}

// --- SecurityAccess key ----------------------------------------------------

/// The key for `seed` under a built-in algorithm; `None` for `Script`,
/// which only the node's script can compute.
pub fn compute_key(algo: &KeyAlgo, seed: &[u8]) -> Option<Vec<u8>> {
    match algo {
        KeyAlgo::XorConst(c) if c.is_empty() => Some(seed.to_vec()),
        KeyAlgo::XorConst(c) => Some(
            seed.iter()
                .enumerate()
                .map(|(i, b)| b ^ c[i % c.len()])
                .collect(),
        ),
        KeyAlgo::AddConst(k) => {
            let mut key = seed.to_vec();
            let add = k.to_be_bytes();
            let mut carry = 0u16;
            for (i, byte) in key.iter_mut().rev().enumerate() {
                let a = if i < 4 { u16::from(add[3 - i]) } else { 0 };
                let sum = u16::from(*byte) + a + carry;
                *byte = sum as u8;
                carry = sum >> 8;
            }
            Some(key)
        }
        KeyAlgo::Script => None,
    }
}

fn algo_label(algo: &KeyAlgo) -> String {
    match algo {
        KeyAlgo::XorConst(c) => format!("XorConst {}", hex_bytes(c)),
        KeyAlgo::AddConst(k) => format!("AddConst 0x{k:X}"),
        KeyAlgo::Script => "Script".to_string(),
    }
}

// --- history ---------------------------------------------------------------

/// How a request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Positive,
    /// Negative response with this code.
    Negative(u8),
    Timeout,
    /// The request could not be sent or the transport failed.
    Failed(String),
}

/// Classify the engine's answer to a request.
pub fn classify(resp: &Result<Vec<u8>, String>) -> Outcome {
    match resp {
        Ok(r) if r.first() == Some(&0x7F) => Outcome::Negative(r.get(2).copied().unwrap_or(0)),
        Ok(_) => Outcome::Positive,
        Err(e) if e.to_ascii_lowercase().contains("timeout") => Outcome::Timeout,
        Err(e) => Outcome::Failed(e.clone()),
    }
}

/// One sent request and, once known, its response.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    /// Simulation time the request was sent.
    pub t_s: f64,
    pub req: Vec<u8>,
    pub resp: Option<Result<Vec<u8>, String>>,
    pub elapsed_ms: f64,
}

impl HistoryEntry {
    pub fn outcome(&self) -> Outcome {
        self.resp.as_ref().map_or(Outcome::Pending, classify)
    }

    fn positive_response(&self) -> Option<&[u8]> {
        match &self.resp {
            Some(Ok(r)) if self.outcome() == Outcome::Positive => Some(r),
            _ => None,
        }
    }
}

/// The seed to answer: level and bytes of the newest positive seed
/// response, unless a later SecurityAccess exchange superseded it.
pub fn seed_offer(history: &[HistoryEntry]) -> Option<(u8, Vec<u8>)> {
    for e in history.iter().rev() {
        if e.req.first() != Some(&0x27) || e.resp.is_none() {
            continue;
        }
        let level = e.req.get(1)? & 0x7F;
        return match e.positive_response() {
            Some(r) if level & 1 == 1 && r.first() == Some(&0x67) && r.len() >= 2 => {
                Some((level, r[2..].to_vec()))
            }
            _ => None,
        };
    }
    None
}

/// Result of the newest `19 02` request.
#[derive(Debug, Clone, PartialEq)]
pub struct DtcReadout {
    pub t_s: f64,
    pub result: Result<Vec<Dtc>, String>,
}

pub fn last_dtc_readout(history: &[HistoryEntry]) -> Option<DtcReadout> {
    let e = history.iter().rev().find(|e| {
        e.req.first() == Some(&0x19)
            && e.req.get(1).map(|s| s & 0x7F) == Some(2)
            && e.resp.is_some()
    })?;
    let result = match e.outcome() {
        Outcome::Positive => match e.positive_response().map(Response::decode) {
            Some(Ok(Response::DtcList { dtcs, .. })) => Ok(dtcs),
            _ => Err("unexpected response".to_string()),
        },
        Outcome::Negative(n) => Err(format!("negative response {}", nrc_text(n))),
        Outcome::Timeout => Err("no response (timeout)".to_string()),
        Outcome::Failed(m) => Err(m),
        Outcome::Pending => return None,
    };
    Some(DtcReadout { t_s: e.t_s, result })
}

fn nrc_text(n: u8) -> String {
    format!("0x{n:02X} {}", Nrc::from_u8(n).name())
}

// --- decoded detail --------------------------------------------------------

/// A line of the decoded tree with its children.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub text: String,
    pub children: Vec<Node>,
}

fn leaf(text: impl Into<String>) -> Node {
    Node {
        text: text.into(),
        children: Vec::new(),
    }
}

fn branch(text: impl Into<String>, children: Vec<Node>) -> Node {
    Node {
        text: text.into(),
        children,
    }
}

/// `DE AD  |..|`: hex and printable ASCII of `data`.
pub fn hex_ascii(data: &[u8]) -> String {
    let ascii: String = data
        .iter()
        .map(|b| {
            if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '.'
            }
        })
        .collect();
    format!("{}  \"{ascii}\"", hex_bytes(data))
}

fn did_label(did: u16, dids: &[DidEntry]) -> String {
    match dids.iter().find(|d| d.did == did) {
        Some(d) if !d.name.is_empty() => format!("DID {did:04X} {}", d.name),
        _ => format!("DID {did:04X}"),
    }
}

/// Split the data of a ReadDataByIdentifier response into its DID records,
/// using the lengths of the target's DID table where known.
pub fn split_rdbi(
    requested: &[u16],
    first: u16,
    data: &[u8],
    table: &[DidEntry],
) -> Vec<(u16, Vec<u8>)> {
    let mut out = Vec::new();
    let mut rest = data;
    let mut did = first;
    let count = requested.len().max(1);
    for i in 0..count {
        if i > 0 {
            let Some(b) = rest.get(..2) else { break };
            did = u16::from_be_bytes([b[0], b[1]]);
            rest = &rest[2..];
        }
        let known = table.iter().find(|d| d.did == did).map(|d| d.data.len());
        let take = match known {
            Some(n) if i + 1 < count => n.min(rest.len()),
            _ => rest.len(),
        };
        out.push((did, rest[..take].to_vec()));
        rest = &rest[take..];
    }
    out
}

fn sub_function_node(sub: u8) -> Node {
    let mut text = format!("Sub-function: 0x{:02X}", sub & 0x7F);
    if sub & 0x80 != 0 {
        text.push_str(" (suppressPositiveResponse)");
    }
    leaf(text)
}

fn request_nodes(req: &[u8], table: &[DidEntry]) -> Vec<Node> {
    let sid = req.first().copied().unwrap_or(0);
    let name = service_name(sid).unwrap_or("unknown service");
    let mut nodes = vec![leaf(format!("Service: 0x{sid:02X} {name}"))];
    let Ok(r) = Request::decode(req) else {
        nodes.push(leaf("Malformed request"));
        return nodes;
    };
    match &r {
        Request::DiagnosticSessionControl(s) | Request::EcuReset(s) => {
            nodes.push(sub_function_node(*s));
        }
        Request::SecurityAccessRequestSeed(l) => {
            nodes.push(sub_function_node(*l));
            nodes.push(leaf(format!("Seed request, level 0x{:02X}", l & 0x7F)));
        }
        Request::SecurityAccessSendKey { level, key } => {
            nodes.push(sub_function_node(*level));
            nodes.push(leaf(format!("Key: {}", hex_bytes(key))));
        }
        Request::ReadDataByIdentifier(dids) => {
            for d in dids {
                nodes.push(leaf(did_label(*d, table)));
            }
        }
        Request::WriteDataByIdentifier { did, data } => {
            nodes.push(branch(
                did_label(*did, table),
                vec![leaf(format!("Data: {}", hex_ascii(data)))],
            ));
        }
        Request::RoutineControl { sub, rid, data } => {
            nodes.push(leaf(format!("Sub-function: {sub:?}")));
            nodes.push(leaf(format!("Routine id: {rid:04X}")));
            if !data.is_empty() {
                nodes.push(leaf(format!("Option record: {}", hex_ascii(data))));
            }
        }
        Request::ReadDtcInformation { sub, mask } => {
            nodes.push(sub_function_node(*sub));
            if let Some(m) = mask {
                nodes.push(branch(
                    format!("Status mask: 0x{m:02X}"),
                    status_bit_names(*m).into_iter().map(leaf).collect(),
                ));
            }
        }
        Request::ClearDiagnosticInformation(g) => {
            nodes.push(leaf(format!("Group of DTC: 0x{g:06X}")));
        }
        Request::TesterPresent { suppress } => {
            nodes.push(sub_function_node(if *suppress { 0x80 } else { 0 }));
        }
        Request::Raw(d) => {
            nodes.push(leaf(format!(
                "Data: {}",
                hex_ascii(d.get(1..).unwrap_or(&[]))
            )));
        }
    }
    nodes
}

fn dtc_node(d: &Dtc) -> Node {
    branch(
        format!("{}  status 0x{:02X}", dtc_to_string(d.code), d.status),
        status_bit_names(d.status).into_iter().map(leaf).collect(),
    )
}

fn response_nodes(req: &[u8], resp: &[u8], table: &[DidEntry]) -> Vec<Node> {
    let sid = resp.first().copied().unwrap_or(0);
    let Ok(r) = Response::decode(resp) else {
        return vec![leaf(format!("Malformed response 0x{sid:02X}"))];
    };
    let svc = |sid: u8| {
        format!(
            "Service: 0x{sid:02X} {}",
            service_name(sid.wrapping_sub(0x40)).unwrap_or("positive response")
        )
    };
    match r {
        Response::Negative { sid, nrc } => vec![
            leaf(format!(
                "Rejected service: 0x{sid:02X} {}",
                service_name(sid).unwrap_or("unknown service")
            )),
            leaf(format!("NRC: 0x{:02X} {}", nrc.to_u8(), nrc.name())),
        ],
        Response::DiagnosticSessionControl {
            session,
            p2_ms,
            p2_star_ms,
        } => vec![
            leaf(svc(sid)),
            leaf(format!("Session: 0x{session:02X}")),
            leaf(format!("P2: {p2_ms} ms, P2*: {p2_star_ms} ms")),
        ],
        Response::EcuReset(k) => vec![leaf(svc(sid)), leaf(format!("Reset type: 0x{k:02X}"))],
        Response::SecuritySeed { level, seed } => vec![
            leaf(svc(sid)),
            leaf(format!("Sub-function: 0x{level:02X} (seed)")),
            leaf(format!("Seed: {}", hex_bytes(&seed))),
        ],
        Response::SecurityKeyAccepted { level } => vec![
            leaf(svc(sid)),
            leaf(format!("Sub-function: 0x{level:02X} (key accepted)")),
        ],
        Response::ReadDataByIdentifier { did, data } => {
            let requested = match Request::decode(req) {
                Ok(Request::ReadDataByIdentifier(d)) => d,
                _ => Vec::new(),
            };
            let mut nodes = vec![leaf(svc(sid))];
            for (d, bytes) in split_rdbi(&requested, did, &data, table) {
                nodes.push(branch(
                    did_label(d, table),
                    vec![leaf(format!("Data: {}", hex_ascii(&bytes)))],
                ));
            }
            nodes
        }
        Response::WriteDataByIdentifier { did } => {
            vec![leaf(svc(sid)), leaf(did_label(did, table))]
        }
        Response::RoutineControl { sub, rid, status } => {
            let mut v = vec![
                leaf(svc(sid)),
                leaf(format!("Sub-function: {sub:?}")),
                leaf(format!("Routine id: {rid:04X}")),
            ];
            if !status.is_empty() {
                v.push(leaf(format!("Status record: {}", hex_ascii(&status))));
            }
            v
        }
        Response::DtcCount {
            availability_mask,
            count,
            ..
        } => vec![
            leaf(svc(sid)),
            branch(
                format!("Availability mask: 0x{availability_mask:02X}"),
                status_bit_names(availability_mask)
                    .into_iter()
                    .map(leaf)
                    .collect(),
            ),
            leaf(format!("DTC count: {count}")),
        ],
        Response::DtcList {
            sub,
            availability_mask,
            dtcs,
        } => vec![
            leaf(svc(sid)),
            sub_function_node(sub),
            leaf(format!("Availability mask: 0x{availability_mask:02X}")),
            branch(
                format!("DTCs ({})", dtcs.len()),
                dtcs.iter().map(dtc_node).collect(),
            ),
        ],
        Response::ClearDiagnosticInformation | Response::TesterPresent => vec![leaf(svc(sid))],
        Response::Raw(d) => vec![leaf(format!("Data: {}", hex_ascii(&d)))],
    }
}

/// The decoded tree of a history entry: request, then response.
pub fn detail_tree(e: &HistoryEntry, table: &[DidEntry]) -> Vec<Node> {
    let mut out = vec![branch(
        format!("Request  {}", hex_bytes(&e.req)),
        request_nodes(&e.req, table),
    )];
    out.push(match &e.resp {
        None => leaf("Response: waiting\u{2026}"),
        Some(Err(m)) => leaf(format!("No response: {m}")),
        Some(Ok(r)) => branch(
            format!("Response  {}  ({:.1} ms)", hex_bytes(r), e.elapsed_ms),
            response_nodes(&e.req, r, table),
        ),
    });
    out
}

// --- saved state -----------------------------------------------------------

/// CAN addressing of the tester towards the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetIds {
    /// `None` picks the first bus.
    pub bus: Option<BusId>,
    pub req_id: u32,
    pub resp_id: u32,
    pub functional_id: u32,
    pub extended: bool,
    pub fd: bool,
}

impl Default for TargetIds {
    fn default() -> Self {
        TargetIds {
            bus: None,
            req_id: 0x7E0,
            resp_id: 0x7E8,
            functional_id: 0x7DF,
            extended: false,
            fd: false,
        }
    }
}

/// A named request in the window's saved list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRequest {
    pub name: String,
    /// Request bytes as hex text.
    pub hex: String,
}

/// Settings of a Diagnostics window saved in the project workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagView {
    pub title: Option<String>,
    /// Name of the target ECU, when picked from the list.
    pub target: Option<String>,
    pub ids: TargetIds,
    pub functional: bool,
    pub tester_present: bool,
    pub tester_present_ms: u32,
    pub saved: Vec<SavedRequest>,
}

impl Default for DiagView {
    fn default() -> Self {
        DiagView {
            title: None,
            target: None,
            ids: TargetIds::default(),
            functional: false,
            tester_present: false,
            tester_present_ms: 2000,
            saved: Vec::new(),
        }
    }
}

// --- the window ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Console,
    Dtcs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryMode {
    Raw,
    Builder,
}

/// A queued request (the demo sequence and automatic follow-ups).
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Send(Vec<u8>),
    /// `27 <level+1> <key>` for the newest seed, using the target's algorithm.
    ComputeKey,
    /// Select the first history entry starting with these bytes.
    Select(Vec<u8>),
}

/// TesterPresent settings as last sent to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TpApplied {
    bus: BusId,
    ids: TargetIds,
    functional: bool,
    period_ms: u32,
}

pub struct DiagWindow {
    pub title: Option<String>,
    pub renaming: bool,
    rename_buf: String,
    pub target: Option<String>,
    pub ids: TargetIds,
    id_bufs: [String; 3],
    pub functional: bool,
    pub tester_present: bool,
    pub tester_present_ms: u32,
    tp_applied: Option<TpApplied>,
    tab: Tab,
    entry: EntryMode,
    raw: String,
    builder: Builder,
    pub history: Vec<HistoryEntry>,
    selected: Option<usize>,
    /// The detail pane follows the newest entry.
    follow: bool,
    pub saved: Vec<SavedRequest>,
    save_name: String,
    confirm_clear: bool,
    queue: VecDeque<Step>,
    /// A message for the user (failed to send, bad input).
    notice: Option<String>,
}

impl Default for DiagWindow {
    fn default() -> Self {
        let ids = TargetIds::default();
        DiagWindow {
            title: None,
            renaming: false,
            rename_buf: String::new(),
            target: None,
            id_bufs: id_texts(&ids),
            ids,
            functional: false,
            tester_present: false,
            tester_present_ms: 2000,
            tp_applied: None,
            tab: Tab::Console,
            entry: EntryMode::Raw,
            raw: "22 F1 90".into(),
            builder: Builder::default(),
            history: Vec::new(),
            selected: None,
            follow: true,
            saved: Vec::new(),
            save_name: String::new(),
            confirm_clear: false,
            queue: VecDeque::new(),
            notice: None,
        }
    }
}

fn id_texts(ids: &TargetIds) -> [String; 3] {
    [
        format!("{:X}", ids.req_id),
        format!("{:X}", ids.resp_id),
        format!("{:X}", ids.functional_id),
    ]
}

fn green(dark: bool) -> egui::Color32 {
    if dark {
        egui::Color32::from_rgb(0x6c, 0xd0, 0x8a)
    } else {
        egui::Color32::from_rgb(0x1a, 0x80, 0x45)
    }
}

fn chip(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    let fill = color.gamma_multiply(0.22);
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.7)))
        .corner_radius(9)
        .inner_margin(egui::Margin::symmetric(7, 1))
        .show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(text).small().color(color)).extend());
        });
}

fn status_chip_color(name: &str, dark: bool) -> egui::Color32 {
    match name {
        "testFailed" | "confirmedDTC" => {
            if dark {
                egui::Color32::from_rgb(0xff, 0x7a, 0x7a)
            } else {
                egui::Color32::from_rgb(0xc0, 0x28, 0x28)
            }
        }
        "pendingDTC" | "warningIndicatorRequested" | "testFailedThisOperationCycle" => {
            if dark {
                egui::Color32::from_rgb(0xff, 0xc0, 0x50)
            } else {
                egui::Color32::from_rgb(0xa8, 0x6a, 0x00)
            }
        }
        _ => {
            if dark {
                egui::Color32::from_rgb(0xa8, 0xb0, 0xc0)
            } else {
                egui::Color32::from_rgb(0x55, 0x5d, 0x6c)
            }
        }
    }
}

impl DiagWindow {
    pub fn view(&self) -> DiagView {
        DiagView {
            title: self.title.clone(),
            target: self.target.clone(),
            ids: self.ids.clone(),
            functional: self.functional,
            tester_present: self.tester_present,
            tester_present_ms: self.tester_present_ms,
            saved: self.saved.clone(),
        }
    }

    pub fn apply_view(&mut self, v: DiagView) {
        self.title = v.title;
        self.target = v.target;
        self.id_bufs = id_texts(&v.ids);
        self.ids = v.ids;
        self.functional = v.functional;
        self.tester_present = v.tester_present;
        self.tester_present_ms = v.tester_present_ms.max(1);
        self.saved = v.saved;
    }

    pub fn begin_rename(&mut self) {
        self.renaming = true;
        self.rename_buf = self.title.clone().unwrap_or_default();
    }

    /// Fill the addressing from a diagnostic ECU of the topology.
    pub fn select_target(&mut self, t: &DiagTarget) {
        self.target = Some(t.name.clone());
        self.ids = TargetIds {
            bus: t.cfg.bus.or(t.buses.first().copied()),
            req_id: t.cfg.req_id,
            resp_id: t.cfg.resp_id,
            functional_id: t.cfg.functional_id.unwrap_or(0x7DF),
            extended: t.cfg.extended_ids,
            fd: t.cfg.fd,
        };
        self.id_bufs = id_texts(&self.ids);
    }

    pub fn show_dtcs(&mut self) {
        self.tab = Tab::Dtcs;
    }

    /// Queue requests to send one after another once the measurement runs.
    pub fn queue_steps(&mut self, steps: impl IntoIterator<Item = Step>) {
        self.queue.extend(steps);
    }

    fn target_of<'a>(&self, names: &'a NameLookup) -> Option<&'a DiagTarget> {
        let n = self.target.as_ref()?;
        names.diag_targets.iter().find(|t| &t.name == n)
    }

    fn has_pending(&self) -> bool {
        self.history.iter().any(|e| e.resp.is_none())
    }

    /// The request the entry fields currently describe.
    fn current_request(&self) -> Result<Vec<u8>, String> {
        match self.entry {
            EntryMode::Raw => parse_request_hex(&self.raw),
            EntryMode::Builder => self.builder.build(),
        }
    }

    fn push_entry(&mut self, e: HistoryEntry) {
        self.history.push(e);
        if self.history.len() > MAX_HISTORY {
            let drop = self.history.len() - MAX_HISTORY;
            self.history.drain(..drop);
            self.selected = self.selected.and_then(|s| s.checked_sub(drop));
        }
        if self.follow {
            self.selected = Some(self.history.len() - 1);
        }
    }

    /// Record `payload` as sent and build the engine command.
    fn send(&mut self, payload: Vec<u8>, now_s: f64, names: &NameLookup) -> Option<Command> {
        if payload.is_empty() {
            return None;
        }
        let bus = self
            .ids
            .bus
            .or_else(|| self.target_of(names).and_then(|t| t.buses.first().copied()))
            .or_else(|| names.bus_names.keys().min().copied());
        let Some(bus) = bus else {
            self.notice = Some("no bus to send on".into());
            return None;
        };
        self.notice = None;
        self.push_entry(HistoryEntry {
            t_s: now_s,
            req: payload.clone(),
            resp: None,
            elapsed_ms: 0.0,
        });
        Some(Command::DiagRequest {
            tester_bus: bus,
            req_id: if self.functional {
                self.ids.functional_id
            } else {
                self.ids.req_id
            },
            resp_id: self.ids.resp_id,
            extended: self.ids.extended,
            fd: self.ids.fd,
            payload,
            functional: self.functional,
        })
    }

    /// Offer an engine response; true when this window had asked for it.
    pub fn on_response(
        &mut self,
        req: &[u8],
        resp: &Result<Vec<u8>, String>,
        elapsed_ms: f64,
    ) -> bool {
        let Some(e) = self
            .history
            .iter_mut()
            .find(|e| e.resp.is_none() && e.req == req)
        else {
            return false;
        };
        e.resp = Some(resp.clone());
        e.elapsed_ms = elapsed_ms;
        // A successful clear is followed by a fresh read of the DTCs.
        if req == [0x14, 0xFF, 0xFF, 0xFF] && classify(resp) == Outcome::Positive {
            self.queue.push_back(Step::Send(vec![0x19, 0x02, 0xFF]));
        }
        true
    }

    /// The simulation stopped: nothing in flight will be answered.
    pub fn on_stopped(&mut self) {
        for e in &mut self.history {
            if e.resp.is_none() {
                e.resp = Some(Err("simulation stopped".into()));
            }
        }
        self.tp_applied = None;
    }

    /// Per-frame work: queued requests and the TesterPresent state.
    pub fn update(&mut self, names: &NameLookup, running: bool, now_s: f64) -> Vec<Command> {
        let mut cmds = Vec::new();
        while running && !self.has_pending() {
            let Some(step) = self.queue.pop_front() else {
                break;
            };
            match step {
                Step::Send(bytes) => {
                    cmds.extend(self.send(bytes, now_s, names));
                }
                Step::ComputeKey => match self.computed_key_request(names) {
                    Some(bytes) => cmds.extend(self.send(bytes, now_s, names)),
                    None => self.notice = Some("cannot compute a key".into()),
                },
                Step::Select(prefix) => {
                    if let Some(i) = self.history.iter().position(|e| e.req.starts_with(&prefix)) {
                        self.selected = Some(i);
                        self.follow = false;
                    }
                }
            }
        }
        let want = (self.tester_present && running).then(|| TpApplied {
            bus: self.resolve_bus(names).unwrap_or(BusId(0)),
            ids: self.ids.clone(),
            functional: self.functional,
            period_ms: self.tester_present_ms.max(1),
        });
        if want != self.tp_applied {
            let spec = want.as_ref().or(self.tp_applied.as_ref());
            if let Some(s) = spec {
                cmds.push(Command::TesterPresent {
                    enable: want.is_some(),
                    tester_bus: s.bus,
                    req_id: s.ids.req_id,
                    functional_id: s.ids.functional_id,
                    functional: s.functional,
                    extended: s.ids.extended,
                    fd: s.ids.fd,
                    period_ms: s.period_ms,
                });
            }
            self.tp_applied = want;
        }
        cmds
    }

    fn resolve_bus(&self, names: &NameLookup) -> Option<BusId> {
        self.ids
            .bus
            .or_else(|| self.target_of(names).and_then(|t| t.buses.first().copied()))
            .or_else(|| names.bus_names.keys().min().copied())
    }

    /// `27 <level+1> <key>` answering the newest seed with the target's
    /// configured algorithm.
    fn computed_key_request(&self, names: &NameLookup) -> Option<Vec<u8>> {
        let (level, seed) = seed_offer(&self.history)?;
        let algo = &self.target_of(names)?.cfg.security.as_ref()?.key_algo;
        let key = compute_key(algo, &seed)?;
        let mut req = vec![0x27, level + 1];
        req.extend(key);
        Some(req)
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        id: WindowId,
        names: &NameLookup,
        running: bool,
        now_s: f64,
    ) -> Vec<Command> {
        let mut cmds = Vec::new();
        ui.push_id(("diag_window", id), |ui| {
            self.header_ui(ui, id, names, running);
            ui.separator();
            match self.tab {
                Tab::Console => self.console_ui(ui, names, running, now_s, &mut cmds),
                Tab::Dtcs => self.dtc_ui(ui, names, running, now_s, &mut cmds),
            }
        });
        cmds
    }

    fn header_ui(&mut self, ui: &mut egui::Ui, id: WindowId, names: &NameLookup, running: bool) {
        if self.renaming {
            ui.horizontal(|ui| {
                ui.label("Title:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.rename_buf)
                        .desired_width(160.0)
                        .hint_text("Window title"),
                );
                if !resp.has_focus() && !resp.lost_focus() {
                    resp.request_focus();
                }
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if enter || escape || resp.lost_focus() {
                    if !escape {
                        let t = self.rename_buf.trim();
                        self.title = (!t.is_empty()).then(|| t.to_string());
                    }
                    self.renaming = false;
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Target:");
            let selected = self.target.clone().unwrap_or_else(|| "(manual)".into());
            let mut picked: Option<DiagTarget> = None;
            egui::ComboBox::from_id_salt(("diag_target", id))
                .selected_text(selected)
                .width(120.0)
                .show_ui(ui, |ui| {
                    if names.diag_targets.is_empty() {
                        ui.weak("No ECU has diagnostics enabled");
                    }
                    for t in &names.diag_targets {
                        let on = self.target.as_ref() == Some(&t.name);
                        if ui.selectable_label(on, &t.name).clicked() {
                            picked = Some(t.clone());
                        }
                    }
                    if ui
                        .selectable_label(self.target.is_none(), "(manual)")
                        .clicked()
                    {
                        self.target = None;
                    }
                });
            if let Some(t) = picked {
                self.select_target(&t);
            }
            ui.separator();
            ui.selectable_value(&mut self.functional, false, "Physical")
                .on_hover_text("Send to the ECU's request id");
            ui.selectable_value(&mut self.functional, true, "Functional")
                .on_hover_text("Send to the functional (broadcast) id; single frames only");
            ui.separator();
            ui.checkbox(&mut self.tester_present, "Tester Present")
                .on_hover_text("Send 3E 80 periodically while the measurement runs");
            ui.add_enabled(
                self.tester_present,
                egui::DragValue::new(&mut self.tester_present_ms)
                    .range(10..=60_000)
                    .speed(10.0)
                    .suffix(" ms"),
            );
            if !running {
                ui.weak("Start the measurement to send.");
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Bus:");
            let bus_text = match self.ids.bus {
                Some(b) => names.bus_name(b),
                None => "(first)".to_string(),
            };
            egui::ComboBox::from_id_salt(("diag_bus", id))
                .selected_text(bus_text)
                .width(90.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.ids.bus, None, "(first)");
                    let mut buses: Vec<_> = names.bus_names.iter().collect();
                    buses.sort_by_key(|(b, _)| **b);
                    for (b, n) in buses {
                        ui.selectable_value(&mut self.ids.bus, Some(*b), n);
                    }
                });
            let fields: [(&str, usize); 3] = [("Req", 0), ("Resp", 1), ("Func", 2)];
            for (label, i) in fields {
                ui.label(label);
                let parsed = parse_hex_u32(&self.id_bufs[i]);
                let mut te = egui::TextEdit::singleline(&mut self.id_bufs[i])
                    .desired_width(54.0)
                    .font(egui::TextStyle::Monospace);
                if parsed.is_none() {
                    te = te.text_color(RED);
                }
                ui.add(te);
                if let Some(v) = parsed {
                    match i {
                        0 => self.ids.req_id = v,
                        1 => self.ids.resp_id = v,
                        _ => self.ids.functional_id = v,
                    }
                }
            }
            ui.checkbox(&mut self.ids.extended, "29-bit");
            ui.checkbox(&mut self.ids.fd, "FD");
        });
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Console, "Console");
            ui.selectable_value(&mut self.tab, Tab::Dtcs, "DTCs");
        });
    }

    fn console_ui(
        &mut self,
        ui: &mut egui::Ui,
        names: &NameLookup,
        running: bool,
        now_s: f64,
        cmds: &mut Vec<Command>,
    ) {
        self.request_ui(ui, names, running, now_s, cmds);
        ui.separator();
        self.security_ui(ui, names);
        let avail = ui.available_height();
        let detail_h = (avail * 0.4).clamp(90.0, 260.0);
        let table_h = (avail - detail_h - 14.0).max(60.0);
        self.history_ui(ui, table_h, names);
        ui.separator();
        self.detail_ui(ui, names);
    }

    fn request_ui(
        &mut self,
        ui: &mut egui::Ui,
        names: &NameLookup,
        running: bool,
        now_s: f64,
        cmds: &mut Vec<Command>,
    ) {
        let mut send = false;
        ui.horizontal(|ui| {
            ui.label("Request:");
            ui.selectable_value(&mut self.entry, EntryMode::Raw, "Hex");
            ui.selectable_value(&mut self.entry, EntryMode::Builder, "Builder");
        });
        match self.entry {
            EntryMode::Raw => {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 70.0).max(80.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.raw)
                            .desired_width(w)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("22 F1 90"),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        send = true;
                        resp.request_focus();
                    }
                    send |= self.send_button(ui, running);
                });
            }
            EntryMode::Builder => {
                self.builder_ui(ui, names);
                ui.horizontal(|ui| {
                    send |= self.send_button(ui, running);
                });
            }
        }
        let current = self.current_request();
        match &current {
            Ok(bytes) => {
                ui.weak(format!(
                    "{}  \u{2192}  {}",
                    hex_bytes(bytes),
                    describe(bytes, true)
                ));
            }
            Err(e) => {
                ui.colored_label(RED, e);
            }
        }
        if let Some(n) = &self.notice {
            ui.colored_label(RED, n);
        }
        if send && running {
            match current {
                Ok(bytes) => cmds.extend(self.send(bytes, now_s, names)),
                Err(e) => self.notice = Some(e),
            }
        }
        let current = self.current_request();
        self.saved_ui(ui, &current);
    }

    fn send_button(&self, ui: &mut egui::Ui, running: bool) -> bool {
        ui.add_enabled(running, egui::Button::new("Send"))
            .on_hover_text("Send the request (Enter)")
            .on_disabled_hover_text("Start the measurement first")
            .clicked()
    }

    fn saved_ui(&mut self, ui: &mut egui::Ui, current: &Result<Vec<u8>, String>) {
        let mut load: Option<usize> = None;
        let mut delete: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label("Saved:");
            ui.add(
                egui::TextEdit::singleline(&mut self.save_name)
                    .desired_width(110.0)
                    .hint_text("name"),
            );
            if ui
                .add_enabled(current.is_ok(), egui::Button::new("Save"))
                .on_hover_text("Add the current request to this window's list")
                .clicked()
                && let Ok(bytes) = current
            {
                let name = self.save_name.trim();
                let name = if name.is_empty() {
                    describe(bytes, true)
                } else {
                    name.to_string()
                };
                self.saved.push(SavedRequest {
                    name,
                    hex: hex_bytes(bytes),
                });
                self.save_name.clear();
            }
            for (i, s) in self.saved.iter().enumerate() {
                if ui.small_button(&s.name).on_hover_text(&s.hex).clicked() {
                    load = Some(i);
                }
                if ui.small_button("\u{d7}").on_hover_text("Delete").clicked() {
                    delete = Some(i);
                }
            }
        });
        if let Some(i) = load {
            self.raw = self.saved[i].hex.clone();
            self.entry = EntryMode::Raw;
        }
        if let Some(i) = delete {
            self.saved.remove(i);
        }
    }

    fn builder_ui(&mut self, ui: &mut egui::Ui, names: &NameLookup) {
        let dids: Vec<DidEntry> = self
            .target_of(names)
            .map(|t| t.cfg.dids.clone())
            .unwrap_or_default();
        let b = &mut self.builder;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("diag_service")
                .selected_text(b.service.label())
                .show_ui(ui, |ui| {
                    for s in Service::ALL {
                        ui.selectable_value(&mut b.service, s, s.label());
                    }
                });
            match b.service {
                Service::Session => {
                    let label = |s: u8| match s {
                        1 => "01 default",
                        2 => "02 programming",
                        3 => "03 extended",
                        _ => "other",
                    };
                    egui::ComboBox::from_id_salt("diag_session")
                        .selected_text(label(b.session))
                        .show_ui(ui, |ui| {
                            for s in [1u8, 2, 3] {
                                ui.selectable_value(&mut b.session, s, label(s));
                            }
                        });
                }
                Service::Reset => {
                    let label = |s: u8| match s {
                        1 => "01 hard reset",
                        2 => "02 key off/on",
                        3 => "03 soft reset",
                        _ => "other",
                    };
                    egui::ComboBox::from_id_salt("diag_reset")
                        .selected_text(label(b.reset))
                        .show_ui(ui, |ui| {
                            for s in [1u8, 2, 3] {
                                ui.selectable_value(&mut b.reset, s, label(s));
                            }
                        });
                }
                Service::SeedRequest | Service::SendKey => {
                    ui.label("Level");
                    ui.add(
                        egui::TextEdit::singleline(&mut b.level)
                            .desired_width(36.0)
                            .font(egui::TextStyle::Monospace),
                    )
                    .on_hover_text("Odd for a seed request, even (seed level + 1) for the key");
                    if b.service == Service::SendKey {
                        ui.label("Key");
                        ui.add(
                            egui::TextEdit::singleline(&mut b.key)
                                .desired_width(150.0)
                                .font(egui::TextStyle::Monospace)
                                .hint_text("hex bytes"),
                        );
                    }
                }
                Service::ReadDid => {
                    ui.label("DIDs");
                    ui.add(
                        egui::TextEdit::singleline(&mut b.dids)
                            .desired_width(120.0)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("F190 F191"),
                    );
                    did_picker(ui, &dids, |d| {
                        let t = b.dids.trim_end();
                        b.dids = if t.is_empty() {
                            format!("{d:04X}")
                        } else {
                            format!("{t} {d:04X}")
                        };
                    });
                }
                Service::WriteDid => {
                    ui.label("DID");
                    ui.add(
                        egui::TextEdit::singleline(&mut b.did)
                            .desired_width(46.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    did_picker(ui, &dids, |d| b.did = format!("{d:04X}"));
                    ui.label("Data");
                    ui.add(
                        egui::TextEdit::singleline(&mut b.data)
                            .desired_width(150.0)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("hex bytes"),
                    );
                }
                Service::Routine => {
                    let label = |s: u8| match s {
                        1 => "start",
                        2 => "stop",
                        _ => "results",
                    };
                    egui::ComboBox::from_id_salt("diag_routine")
                        .selected_text(label(b.routine_sub))
                        .width(70.0)
                        .show_ui(ui, |ui| {
                            for s in [1u8, 2, 3] {
                                ui.selectable_value(&mut b.routine_sub, s, label(s));
                            }
                        });
                    ui.label("Routine");
                    ui.add(
                        egui::TextEdit::singleline(&mut b.rid)
                            .desired_width(46.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    ui.label("Data");
                    ui.add(
                        egui::TextEdit::singleline(&mut b.data)
                            .desired_width(110.0)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("optional"),
                    );
                }
                Service::ReadDtc => {
                    let label = |s: u8| match s {
                        1 => "01 count by mask",
                        2 => "02 list by mask",
                        _ => "0A supported",
                    };
                    egui::ComboBox::from_id_salt("diag_dtc_sub")
                        .selected_text(label(b.dtc_sub))
                        .show_ui(ui, |ui| {
                            for s in [1u8, 2, 0x0A] {
                                ui.selectable_value(&mut b.dtc_sub, s, label(s));
                            }
                        });
                    if matches!(b.dtc_sub, 1 | 2) {
                        ui.label("Status mask");
                        ui.add(
                            egui::TextEdit::singleline(&mut b.mask)
                                .desired_width(36.0)
                                .font(egui::TextStyle::Monospace),
                        );
                    }
                }
                Service::ClearDtc => {
                    ui.label("Group");
                    ui.add(
                        egui::TextEdit::singleline(&mut b.group)
                            .desired_width(64.0)
                            .font(egui::TextStyle::Monospace),
                    )
                    .on_hover_text("FFFFFF clears every DTC");
                }
                Service::TesterPresent => {}
            }
            if matches!(
                b.service,
                Service::Session
                    | Service::Reset
                    | Service::SeedRequest
                    | Service::SendKey
                    | Service::ReadDtc
                    | Service::TesterPresent
            ) {
                ui.checkbox(&mut b.suppress, "Suppress response")
                    .on_hover_text("Set the suppressPositiveResponse bit (0x80)");
            }
        });
    }

    /// "Seed received, compute key" bar after a successful `27 01`.
    fn security_ui(&mut self, ui: &mut egui::Ui, names: &NameLookup) {
        let Some((level, seed)) = seed_offer(&self.history) else {
            return;
        };
        let algo = self
            .target_of(names)
            .and_then(|t| t.cfg.security.as_ref())
            .map(|s| s.key_algo.clone());
        let key = algo.as_ref().and_then(|a| compute_key(a, &seed));
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Seed (level 0x{level:02X}): {}", hex_bytes(&seed)));
            let mut btn = ui.add_enabled(key.is_some(), egui::Button::new("Compute key"));
            btn = match &algo {
                Some(a) => btn
                    .on_hover_text(format!(
                        "Key from the target's algorithm: {}",
                        algo_label(a)
                    ))
                    .on_disabled_hover_text("The key comes from the node's script"),
                None => btn.on_disabled_hover_text("The target has no SecurityAccess configured"),
            };
            if btn.clicked()
                && let Some(key) = &key
            {
                self.builder.service = Service::SendKey;
                self.builder.level = format!("{:02X}", level + 1);
                self.builder.key = hex_bytes(key);
                self.entry = EntryMode::Builder;
            }
            if let Some(k) = &key {
                ui.weak(format!("\u{2192} {}", hex_bytes(k)));
            }
        });
        ui.separator();
    }

    fn history_ui(&mut self, ui: &mut egui::Ui, height: f32, names: &NameLookup) {
        let _ = names;
        if self.history.is_empty() {
            ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                ui.weak("No requests yet.");
            });
            return;
        }
        let rows: Vec<(usize, Outcome)> = self
            .history
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.outcome()))
            .collect();
        let n = rows.len();
        let (dark, weak) = (ui.visuals().dark_mode, ui.visuals().weak_text_color());
        let mut clicked: Option<usize> = None;
        ui.scope(|ui| {
            // A light selection tint keeps the green and red texts readable.
            let fill = ui.visuals().selection.bg_fill.gamma_multiply(0.45);
            ui.visuals_mut().selection.bg_fill = fill;
            TableBuilder::new(ui)
                .id_salt("diag_history")
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .max_scroll_height(height)
                .stick_to_bottom(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(64.0))
                .column(Column::initial(170.0).at_least(80.0).clip(true))
                .column(Column::initial(200.0).at_least(80.0).clip(true))
                .column(Column::exact(58.0))
                .column(Column::remainder().at_least(70.0).clip(true))
                .header(18.0, |mut h| {
                    for t in ["Time (s)", "Request", "Response", "ms", "Result"] {
                        h.col(|ui| {
                            ui.strong(t);
                        });
                    }
                })
                .body(|body| {
                    body.rows(ROW_H, n, |mut row| {
                        let i = row.index();
                        let e = &self.history[i];
                        let (_, outcome) = &rows[i];
                        let color = match outcome {
                            Outcome::Positive => green(dark),
                            Outcome::Negative(_) => RED,
                            _ => weak,
                        };
                        row.set_selected(self.selected == Some(i));
                        row.col(|ui| {
                            ui.monospace(format!("{:.3}", e.t_s));
                        });
                        row.col(|ui| {
                            two_line(ui, &hex_bytes(&e.req), &describe(&e.req, true), None);
                        });
                        row.col(|ui| match &e.resp {
                            Some(Ok(r)) => {
                                two_line(ui, &hex_bytes(r), &describe(r, false), Some(color))
                            }
                            Some(Err(m)) => two_line(ui, "\u{2014}", m, None),
                            None => two_line(ui, "\u{2026}", "waiting for the response", None),
                        });
                        row.col(|ui| {
                            if e.resp.is_some() {
                                ui.monospace(format!("{:.1}", e.elapsed_ms));
                            }
                        });
                        row.col(|ui| {
                            let (top, bottom) = match outcome {
                                Outcome::Pending => ("pending".to_string(), String::new()),
                                Outcome::Positive => ("positive".to_string(), String::new()),
                                Outcome::Negative(n) => (
                                    format!("NRC 0x{n:02X}"),
                                    Nrc::from_u8(*n).name().to_string(),
                                ),
                                Outcome::Timeout => ("timeout".to_string(), String::new()),
                                Outcome::Failed(m) => ("failed".to_string(), m.clone()),
                            };
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.add(
                                    egui::Label::new(egui::RichText::new(top).color(color))
                                        .truncate(),
                                );
                                if !bottom.is_empty() {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(bottom).small().color(color),
                                        )
                                        .truncate(),
                                    );
                                }
                            });
                        });
                        if row.response().clicked() {
                            clicked = Some(i);
                        }
                    });
                });
        });
        if let Some(i) = clicked {
            self.selected = Some(i);
            self.follow = i + 1 == self.history.len();
        }
    }

    fn detail_ui(&mut self, ui: &mut egui::Ui, names: &NameLookup) {
        let Some(e) = self.selected.and_then(|i| self.history.get(i)) else {
            ui.weak("Select a history entry to see it decoded.");
            return;
        };
        let table = self
            .target_of(names)
            .map(|t| t.cfg.dids.clone())
            .unwrap_or_default();
        let tree = detail_tree(e, &table);
        egui::ScrollArea::vertical()
            .id_salt("diag_detail")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (i, n) in tree.iter().enumerate() {
                    node_ui(ui, n, ("detail", i));
                }
            });
    }

    fn dtc_ui(
        &mut self,
        ui: &mut egui::Ui,
        names: &NameLookup,
        running: bool,
        now_s: f64,
        cmds: &mut Vec<Command>,
    ) {
        let mut read = false;
        let mut clear = false;
        ui.horizontal(|ui| {
            read = ui
                .add_enabled(running, egui::Button::new("Read (19 02 FF)"))
                .on_hover_text("ReadDTCInformation, report DTCs by status mask 0xFF")
                .on_disabled_hover_text("Start the measurement first")
                .clicked();
            if self.confirm_clear {
                ui.colored_label(RED, "Clear every DTC?");
                if ui.button("Yes, clear").clicked() {
                    clear = true;
                    self.confirm_clear = false;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_clear = false;
                }
            } else if ui
                .add_enabled(running, egui::Button::new("Clear all (14 FF FF FF)"))
                .on_hover_text("ClearDiagnosticInformation for every group")
                .on_disabled_hover_text("Start the measurement first")
                .clicked()
            {
                self.confirm_clear = true;
            }
        });
        if read {
            cmds.extend(self.send(vec![0x19, 0x02, 0xFF], now_s, names));
        }
        if clear {
            cmds.extend(self.send(vec![0x14, 0xFF, 0xFF, 0xFF], now_s, names));
        }
        ui.separator();
        let dark = ui.visuals().dark_mode;
        match last_dtc_readout(&self.history) {
            None => {
                ui.weak("No DTC read yet. Press Read to list the stored trouble codes.");
            }
            Some(DtcReadout {
                t_s,
                result: Err(e),
            }) => {
                ui.colored_label(RED, format!("Read at {t_s:.3} s failed: {e}"));
            }
            Some(DtcReadout {
                t_s,
                result: Ok(dtcs),
            }) => {
                ui.weak(format!("{} DTC(s), read at {t_s:.3} s", dtcs.len()));
                egui::ScrollArea::vertical()
                    .id_salt("diag_dtcs")
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            cell(ui, 86.0, |ui| {
                                ui.strong("DTC");
                            });
                            cell(ui, 52.0, |ui| {
                                ui.strong("Status");
                            });
                            ui.strong("Status bits");
                        });
                        for d in &dtcs {
                            ui.horizontal_wrapped(|ui| {
                                cell(ui, 86.0, |ui| {
                                    ui.label(
                                        egui::RichText::new(dtc_to_string(d.code))
                                            .monospace()
                                            .strong(),
                                    );
                                });
                                cell(ui, 52.0, |ui| {
                                    ui.monospace(format!("0x{:02X}", d.status));
                                });
                                let bits = status_bit_names(d.status);
                                if bits.is_empty() {
                                    ui.weak("no bits set");
                                }
                                for b in bits {
                                    chip(ui, b, status_chip_color(b, dark));
                                }
                            });
                            ui.add_space(2.0);
                        }
                    });
            }
        }
    }
}

/// A left-aligned cell of a fixed width.
fn cell(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 18.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(width, 18.0));
            add(ui);
        },
    );
}

/// A "pick a DID by name" menu; `apply` receives the chosen DID.
fn did_picker(ui: &mut egui::Ui, dids: &[DidEntry], apply: impl FnOnce(u16)) {
    let mut chosen: Option<u16> = None;
    ui.menu_button("DID\u{2026}", |ui| {
        if dids.is_empty() {
            ui.weak("The target has no DID list");
        }
        for d in dids {
            let name = if d.name.is_empty() {
                "(unnamed)"
            } else {
                &d.name
            };
            let w = if d.writable { " (writable)" } else { "" };
            if ui.button(format!("{:04X}  {name}{w}", d.did)).clicked() {
                chosen = Some(d.did);
                ui.close();
            }
        }
    });
    if let Some(d) = chosen {
        apply(d);
    }
}

/// A monospace line over a weak description, both truncated.
fn two_line(ui: &mut egui::Ui, top: &str, bottom: &str, color: Option<egui::Color32>) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut t = egui::RichText::new(top).monospace();
        if let Some(c) = color {
            t = t.color(c);
        }
        ui.add(egui::Label::new(t).truncate());
        ui.add(egui::Label::new(egui::RichText::new(bottom).small().weak()).truncate());
    });
}

fn node_ui(ui: &mut egui::Ui, n: &Node, path: (&str, usize)) {
    if n.children.is_empty() {
        ui.label(&n.text);
        return;
    }
    egui::CollapsingHeader::new(&n.text)
        .id_salt(path)
        .default_open(true)
        .show(ui, |ui| {
            for (i, c) in n.children.iter().enumerate() {
                node_ui(ui, c, (path.0, path.1 * 31 + i + 1));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use operow_core::{DiagConfig, SecurityConfig};

    fn built(f: impl FnOnce(&mut Builder)) -> Vec<u8> {
        let mut b = Builder::default();
        f(&mut b);
        b.build().unwrap()
    }

    #[test]
    fn builder_encodes_every_service() {
        assert_eq!(built(|_| {}), [0x10, 0x03]);
        assert_eq!(
            built(|b| {
                b.service = Service::Session;
                b.session = 1;
                b.suppress = true;
            }),
            [0x10, 0x81]
        );
        assert_eq!(
            built(|b| {
                b.service = Service::Reset;
                b.reset = 3;
            }),
            [0x11, 0x03]
        );
        assert_eq!(
            built(|b| {
                b.service = Service::SeedRequest;
                b.level = "03".into();
            }),
            [0x27, 0x03]
        );
        assert_eq!(
            built(|b| {
                b.service = Service::SendKey;
                b.level = "02".into();
                b.key = "B7 91 F3 DD".into();
            }),
            [0x27, 0x02, 0xB7, 0x91, 0xF3, 0xDD]
        );
        assert_eq!(
            built(|b| {
                b.service = Service::ReadDid;
                b.dids = "F190, F191".into();
            }),
            [0x22, 0xF1, 0x90, 0xF1, 0x91]
        );
        assert_eq!(
            built(|b| {
                b.service = Service::WriteDid;
                b.did = "0100".into();
                b.data = "DE AD".into();
            }),
            [0x2E, 0x01, 0x00, 0xDE, 0xAD]
        );
        assert_eq!(
            built(|b| {
                b.service = Service::Routine;
                b.routine_sub = 3;
                b.rid = "FF00".into();
            }),
            [0x31, 0x03, 0xFF, 0x00]
        );
        assert_eq!(
            built(|b| {
                b.service = Service::ReadDtc;
                b.dtc_sub = 2;
                b.mask = "08".into();
            }),
            [0x19, 0x02, 0x08]
        );
        assert_eq!(
            built(|b| {
                b.service = Service::ReadDtc;
                b.dtc_sub = 0x0A;
            }),
            [0x19, 0x0A],
            "the supported-DTC report has no mask"
        );
        assert_eq!(
            built(|b| {
                b.service = Service::ClearDtc;
            }),
            [0x14, 0xFF, 0xFF, 0xFF]
        );
        assert_eq!(
            built(|b| {
                b.service = Service::TesterPresent;
                b.suppress = true;
            }),
            [0x3E, 0x80]
        );
    }

    #[test]
    fn builder_reports_bad_fields() {
        let mut b = Builder {
            service: Service::ReadDid,
            dids: "F190 XYZ".into(),
            ..Default::default()
        };
        assert!(b.build().unwrap_err().contains("DID"));
        b.dids = "  ".into();
        assert!(b.build().is_err());
        b.service = Service::SendKey;
        b.key = String::new();
        assert!(b.build().is_err());
        b.service = Service::ClearDtc;
        b.group = "1000000".into();
        assert!(b.build().is_err());
    }

    #[test]
    fn raw_hex_forms() {
        assert_eq!(parse_request_hex("22 F1 90").unwrap(), [0x22, 0xF1, 0x90]);
        assert_eq!(parse_request_hex("22F190").unwrap(), [0x22, 0xF1, 0x90]);
        assert_eq!(parse_request_hex("22,f1,90").unwrap(), [0x22, 0xF1, 0x90]);
        assert_eq!(parse_request_hex("3E").unwrap(), [0x3E]);
        assert!(parse_request_hex("").is_err());
        assert!(parse_request_hex("22 F").is_ok());
        assert!(parse_request_hex("22F19").is_err());
        assert!(parse_request_hex("22 GG").is_err());
    }

    #[test]
    fn key_algorithms() {
        let seed = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(
            compute_key(&KeyAlgo::XorConst(vec![0xA5]), &seed).unwrap(),
            [0xB7, 0x91, 0xF3, 0xDD]
        );
        assert_eq!(
            compute_key(&KeyAlgo::XorConst(vec![0xFF, 0x00]), &seed).unwrap(),
            [0xED, 0x34, 0xA9, 0x78],
            "the constant repeats cyclically"
        );
        assert_eq!(
            compute_key(&KeyAlgo::XorConst(vec![]), &seed).unwrap(),
            seed
        );
        assert_eq!(
            compute_key(&KeyAlgo::AddConst(0x0000_0101), &seed).unwrap(),
            [0x12, 0x34, 0x57, 0x79]
        );
        assert_eq!(
            compute_key(&KeyAlgo::AddConst(1), &[0xFF, 0xFF]).unwrap(),
            [0x00, 0x00],
            "truncated to the seed length"
        );
        assert_eq!(compute_key(&KeyAlgo::Script, &seed), None);
    }

    fn entry(req: &[u8], resp: Option<Result<Vec<u8>, String>>) -> HistoryEntry {
        HistoryEntry {
            t_s: 1.0,
            req: req.to_vec(),
            resp,
            elapsed_ms: 12.5,
        }
    }

    #[test]
    fn history_outcomes() {
        assert_eq!(entry(&[0x3E, 0], None).outcome(), Outcome::Pending);
        assert_eq!(
            entry(&[0x3E, 0], Some(Ok(vec![0x7E, 0]))).outcome(),
            Outcome::Positive
        );
        assert_eq!(
            entry(&[0x22, 0xFF, 0xFF], Some(Ok(vec![0x7F, 0x22, 0x31]))).outcome(),
            Outcome::Negative(0x31)
        );
        assert_eq!(
            entry(&[0x22], Some(Err("timeout: no response".into()))).outcome(),
            Outcome::Timeout
        );
        assert_eq!(
            entry(&[0x22], Some(Err("simulation is not running".into()))).outcome(),
            Outcome::Failed("simulation is not running".into())
        );
        assert_eq!(nrc_text(0x31), "0x31 requestOutOfRange");
    }

    #[test]
    fn seed_offer_follows_the_newest_exchange() {
        let seed = entry(
            &[0x27, 0x01],
            Some(Ok(vec![0x67, 0x01, 0x12, 0x34, 0x56, 0x78])),
        );
        assert_eq!(
            seed_offer(std::slice::from_ref(&seed)),
            Some((1, vec![0x12, 0x34, 0x56, 0x78]))
        );
        let key = entry(&[0x27, 0x02, 1, 2, 3, 4], Some(Ok(vec![0x67, 0x02])));
        assert_eq!(seed_offer(&[seed.clone(), key]), None);
        let pending_key = entry(&[0x27, 0x02, 1], None);
        assert!(seed_offer(&[seed.clone(), pending_key]).is_some());
        let denied = entry(&[0x27, 0x01], Some(Ok(vec![0x7F, 0x27, 0x22])));
        assert_eq!(seed_offer(&[seed, denied]), None);
        assert_eq!(seed_offer(&[]), None);
    }

    #[test]
    fn dtc_readout_uses_the_newest_read() {
        assert_eq!(last_dtc_readout(&[]), None);
        let ok = entry(
            &[0x19, 0x02, 0xFF],
            Some(Ok(vec![0x59, 0x02, 0xFF, 0x01, 0x23, 0x00, 0x09])),
        );
        let r = last_dtc_readout(std::slice::from_ref(&ok)).unwrap();
        let dtcs = r.result.unwrap();
        assert_eq!(dtcs.len(), 1);
        assert_eq!(dtc_to_string(dtcs[0].code), "P0123-00");
        assert_eq!(dtcs[0].status, 0x09);
        let neg = entry(&[0x19, 0x02, 0xFF], Some(Ok(vec![0x7F, 0x19, 0x22])));
        let r = last_dtc_readout(&[ok, neg]).unwrap();
        assert!(r.result.unwrap_err().contains("conditionsNotCorrect"));
    }

    #[test]
    fn rdbi_records_are_split_by_the_did_table() {
        let table = vec![
            DidEntry {
                did: 0xF190,
                name: "VIN".into(),
                data: b"ABC".to_vec(),
                writable: false,
            },
            DidEntry {
                did: 0x0100,
                name: "Cal".into(),
                data: vec![1, 2],
                writable: true,
            },
        ];
        let recs = split_rdbi(
            &[0xF190, 0x0100],
            0xF190,
            &[b'A', b'B', b'C', 0x01, 0x00, 1, 2],
            &table,
        );
        assert_eq!(recs, vec![(0xF190, b"ABC".to_vec()), (0x0100, vec![1, 2])]);
        let one = split_rdbi(&[0xF190], 0xF190, b"ABCDEF", &table);
        assert_eq!(one, vec![(0xF190, b"ABCDEF".to_vec())]);
    }

    #[test]
    fn detail_tree_names_dids_and_dtc_bits() {
        let table = vec![DidEntry {
            did: 0xF190,
            name: "VIN".into(),
            data: b"WAU".to_vec(),
            writable: false,
        }];
        let e = entry(
            &[0x22, 0xF1, 0x90],
            Some(Ok(vec![0x62, 0xF1, 0x90, b'W', b'A', b'U'])),
        );
        let tree = detail_tree(&e, &table);
        let text = format!("{tree:?}");
        assert!(text.contains("DID F190 VIN"), "{text}");
        assert!(text.contains("57 41 55  \\\"WAU\\\""), "{text}");

        let e = entry(
            &[0x19, 0x02, 0xFF],
            Some(Ok(vec![0x59, 0x02, 0xFF, 0x01, 0x23, 0x00, 0x09])),
        );
        let text = format!("{:?}", detail_tree(&e, &[]));
        assert!(text.contains("P0123-00  status 0x09"), "{text}");
        assert!(text.contains("testFailed"), "{text}");
        assert!(text.contains("confirmedDTC"), "{text}");

        let e = entry(&[0x22, 0xFF, 0xFF], Some(Ok(vec![0x7F, 0x22, 0x31])));
        let text = format!("{:?}", detail_tree(&e, &[]));
        assert!(text.contains("NRC: 0x31 requestOutOfRange"), "{text}");
        let e = entry(&[0x3E, 0], Some(Err("timeout: no response".into())));
        assert!(format!("{:?}", detail_tree(&e, &[])).contains("No response"));
    }

    fn names_with_engine() -> NameLookup {
        let mut n = NameLookup::default();
        n.bus_names.insert(BusId(1), "CAN0".into());
        n.diag_targets.push(DiagTarget {
            node: operow_core::NodeId(1),
            name: "Engine".into(),
            buses: vec![BusId(1)],
            cfg: DiagConfig {
                security: Some(SecurityConfig {
                    level: 1,
                    seed: vec![0x12, 0x34, 0x56, 0x78],
                    key_algo: KeyAlgo::XorConst(vec![0xA5]),
                }),
                ..Default::default()
            },
        });
        n
    }

    #[test]
    fn requests_flow_through_history_and_the_queue() {
        let names = names_with_engine();
        let mut w = DiagWindow::default();
        w.select_target(&names.diag_targets[0].clone());
        assert_eq!(w.ids.bus, Some(BusId(1)));
        assert_eq!(w.ids.req_id, 0x7E0);
        w.queue_steps([
            Step::Send(vec![0x27, 0x01]),
            Step::ComputeKey,
            Step::Select(vec![0x27, 0x01]),
        ]);
        // Not running: nothing goes out.
        assert!(w.update(&names, false, 0.0).is_empty());
        let cmds = w.update(&names, true, 0.5);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(
            &cmds[0],
            Command::DiagRequest { payload, req_id: 0x7E0, resp_id: 0x7E8, tester_bus: BusId(1), .. }
                if payload == &[0x27, 0x01]
        ));
        // Waits for the response before the next step.
        assert!(w.update(&names, true, 0.6).is_empty());
        assert!(
            !w.on_response(&[0x22], &Ok(vec![0x62]), 1.0),
            "unknown request"
        );
        let seed = Ok(vec![0x67, 0x01, 0x12, 0x34, 0x56, 0x78]);
        assert!(w.on_response(&[0x27, 0x01], &seed, 20.0));
        let cmds = w.update(&names, true, 0.7);
        assert!(matches!(
            &cmds[0],
            Command::DiagRequest { payload, .. } if payload == &[0x27, 0x02, 0xB7, 0x91, 0xF3, 0xDD]
        ));
        assert_eq!(w.history.len(), 2);
        assert_eq!(w.history[0].outcome(), Outcome::Positive);
        assert_eq!(w.history[1].outcome(), Outcome::Pending);
        w.on_stopped();
        assert_eq!(
            w.history[1].outcome(),
            Outcome::Failed("simulation stopped".into())
        );
    }

    #[test]
    fn functional_mode_uses_the_functional_id_and_clear_rereads() {
        let names = names_with_engine();
        let mut w = DiagWindow::default();
        w.select_target(&names.diag_targets[0].clone());
        w.functional = true;
        let c = w.send(vec![0x3E, 0x00], 0.0, &names).unwrap();
        assert!(matches!(
            c,
            Command::DiagRequest {
                req_id: 0x7DF,
                functional: true,
                ..
            }
        ));
        w.functional = false;
        w.send(vec![0x14, 0xFF, 0xFF, 0xFF], 0.0, &names).unwrap();
        w.on_response(&[0x14, 0xFF, 0xFF, 0xFF], &Ok(vec![0x54]), 5.0);
        // Answer the first pending one so the queued re-read can go out.
        w.on_response(&[0x3E, 0x00], &Ok(vec![0x7E, 0x00]), 5.0);
        let cmds = w.update(&names, true, 1.0);
        assert!(matches!(
            &cmds[0],
            Command::DiagRequest { payload, .. } if payload == &[0x19, 0x02, 0xFF]
        ));
    }

    #[test]
    fn tester_present_commands_follow_the_settings() {
        let names = names_with_engine();
        let mut w = DiagWindow::default();
        w.select_target(&names.diag_targets[0].clone());
        w.tester_present = true;
        w.tester_present_ms = 500;
        w.functional = true;
        assert!(w.update(&names, false, 0.0).is_empty());
        let cmds = w.update(&names, true, 0.0);
        assert!(matches!(
            &cmds[0],
            Command::TesterPresent {
                enable: true,
                tester_bus: BusId(1),
                req_id: 0x7E0,
                functional_id: 0x7DF,
                functional: true,
                period_ms: 500,
                ..
            }
        ));
        assert!(w.update(&names, true, 0.1).is_empty(), "unchanged");
        w.tester_present = false;
        let cmds = w.update(&names, true, 0.2);
        assert!(matches!(
            &cmds[0],
            Command::TesterPresent {
                enable: false,
                period_ms: 500,
                ..
            }
        ));
    }

    #[test]
    fn view_round_trips() {
        let mut w = DiagWindow {
            title: Some("Bench".into()),
            target: Some("Engine".into()),
            functional: true,
            tester_present: true,
            tester_present_ms: 1500,
            ..Default::default()
        };
        w.ids.bus = Some(BusId(2));
        w.ids.req_id = 0x18DA10F1;
        w.ids.extended = true;
        w.saved.push(SavedRequest {
            name: "VIN".into(),
            hex: "22 F1 90".into(),
        });
        let json = serde_json::to_string(&w.view()).unwrap();
        let mut back = DiagWindow::default();
        back.apply_view(serde_json::from_str(&json).unwrap());
        assert_eq!(back.view(), w.view());
        assert_eq!(back.id_bufs[0], "18DA10F1");
        // Missing fields fall back to the defaults.
        let v: DiagView = serde_json::from_str("{}").unwrap();
        assert_eq!(v.tester_present_ms, 2000);
        assert!(!v.tester_present);
    }
}
