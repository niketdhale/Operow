use thiserror::Error;

use crate::model::{ByteOrder, Database, MessageDef, Mux, SignalDef, ValueType};

/// Error returned by [`Database::parse`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DbcError {
    #[error("line {line}: {msg}")]
    Syntax { line: usize, msg: String },
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Punct(char),
}

/// Split one statement into tokens. Quoted strings may span lines; `\"`
/// and `\\` escapes are honoured.
fn tokenize(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '"' {
            let mut text = String::new();
            while let Some(d) = chars.next() {
                match d {
                    '\\' => {
                        if let Some(e) = chars.next() {
                            if e != '"' && e != '\\' {
                                text.push('\\');
                            }
                            text.push(e);
                        }
                    }
                    '"' => break,
                    _ => text.push(d),
                }
            }
            out.push(Tok::Str(text.replace("\r\n", "\n")));
        } else if ":|[](),@;".contains(c) {
            out.push(Tok::Punct(c));
        } else {
            let mut w = String::from(c);
            while let Some(&d) = chars.peek() {
                if d.is_whitespace() || d == '"' || ":|[](),@;".contains(d) {
                    break;
                }
                w.push(d);
                chars.next();
            }
            out.push(Tok::Word(w));
        }
    }
    out
}

/// The leading keyword of a statement (`BU_:` yields `BU_`).
fn keyword_of(stmt: &str) -> &str {
    stmt.split(|c: char| c.is_whitespace() || c == ':')
        .next()
        .unwrap_or("")
}

/// Number of unescaped quotes in `s`.
fn quote_count(s: &str) -> usize {
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            n += 1;
        }
    }
    n
}

/// Cursor over the tokens of one statement.
struct Cur {
    toks: Vec<Tok>,
    pos: usize,
    line: usize,
}

impl Cur {
    fn err(&self, msg: impl Into<String>) -> DbcError {
        DbcError::Syntax {
            line: self.line,
            msg: msg.into(),
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn word(&mut self, what: &str) -> Result<String, DbcError> {
        match self.next() {
            Some(Tok::Word(w)) => Ok(w),
            _ => Err(self.err(format!("expected {what}"))),
        }
    }

    fn string(&mut self, what: &str) -> Result<String, DbcError> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(s),
            _ => Err(self.err(format!("expected {what}"))),
        }
    }

    fn punct(&mut self, c: char) -> Result<(), DbcError> {
        match self.next() {
            Some(Tok::Punct(p)) if p == c => Ok(()),
            _ => Err(self.err(format!("expected '{c}'"))),
        }
    }

    fn num<T: std::str::FromStr>(&mut self, what: &str) -> Result<T, DbcError> {
        let w = self.word(what)?;
        w.parse()
            .map_err(|_| self.err(format!("invalid {what} '{w}'")))
    }
}

fn parse_signal(c: &mut Cur) -> Result<SignalDef, DbcError> {
    let name = c.word("signal name")?;
    let mut multiplexer = None;
    if let Some(Tok::Word(w)) = c.peek() {
        let w = w.clone();
        multiplexer = if w == "M" {
            Some(Mux::Multiplexor)
        } else if let Some(n) = w.strip_prefix('m') {
            // Extended multiplexing (`m3M`) is treated as plain `m3`.
            let n = n.strip_suffix('M').unwrap_or(n);
            Some(Mux::Multiplexed(
                n.parse()
                    .map_err(|_| c.err(format!("invalid multiplexer '{w}'")))?,
            ))
        } else {
            return Err(c.err(format!("invalid multiplexer '{w}'")));
        };
        c.pos += 1;
    }
    c.punct(':')?;
    let start_bit = c.num("start bit")?;
    c.punct('|')?;
    let size = c.num("signal size")?;
    c.punct('@')?;
    let layout = c.word("byte order and sign")?;
    let mut ch = layout.chars();
    let byte_order = match ch.next() {
        Some('1') => ByteOrder::Intel,
        Some('0') => ByteOrder::Motorola,
        _ => return Err(c.err(format!("invalid byte order '{layout}'"))),
    };
    let value_type = match ch.next() {
        Some('+') => ValueType::Unsigned,
        Some('-') => ValueType::Signed,
        _ => return Err(c.err(format!("invalid value type '{layout}'"))),
    };
    c.punct('(')?;
    let factor = c.num("factor")?;
    c.punct(',')?;
    let offset = c.num("offset")?;
    c.punct(')')?;
    c.punct('[')?;
    let min = c.num("minimum")?;
    c.punct('|')?;
    let max = c.num("maximum")?;
    c.punct(']')?;
    let unit = c.string("unit string")?;
    let mut receivers = Vec::new();
    while let Some(t) = c.next() {
        match t {
            Tok::Word(w) => receivers.push(w),
            Tok::Punct(',') => {}
            _ => return Err(c.err("unexpected token in receiver list")),
        }
    }
    Ok(SignalDef {
        name,
        start_bit,
        size,
        byte_order,
        value_type,
        factor,
        offset,
        min,
        max,
        unit,
        receivers,
        multiplexer,
        initial_raw: None,
        value_descriptions: Vec::new(),
        comment: None,
    })
}

/// Split a raw DBC id into (id, extended).
fn split_id(raw: u64) -> (u32, bool) {
    ((raw & 0x1FFF_FFFF) as u32, raw & 0x8000_0000 != 0)
}

impl Database {
    /// Parse DBC text. Unknown sections are ignored.
    pub fn parse(text: &str) -> Result<Database, DbcError> {
        let mut db = Database::default();
        // Index of the message that following `SG_` lines belong to.
        let mut current: Option<usize> = None;
        // Enum values of the `GenMsgSendType` attribute definition.
        let mut send_types: Vec<String> = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line_no = i + 1;
            let mut stmt = lines[i].trim().to_string();
            i += 1;
            let keyword = keyword_of(&stmt);
            if keyword == "NS_" {
                // Skip the indented symbol list.
                while i < lines.len() && lines[i].starts_with([' ', '\t']) {
                    i += 1;
                }
                continue;
            }
            // Strings (comments) may span several lines.
            while quote_count(&stmt) % 2 == 1 && i < lines.len() {
                stmt.push('\n');
                stmt.push_str(lines[i].trim_end_matches('\r'));
                i += 1;
            }
            let keyword = keyword_of(&stmt);
            if keyword != "SG_" {
                current = None;
            }
            let mut c = Cur {
                toks: tokenize(&stmt),
                pos: 1,
                line: line_no,
            };
            match keyword {
                "VERSION" => db.version = c.string("version string")?,
                "BU_" => {
                    c.punct(':')?;
                    while let Some(Tok::Word(w)) = c.next() {
                        db.nodes.push(w);
                    }
                }
                "BO_" => {
                    let raw: u64 = c.num("message id")?;
                    let name = c.word("message name")?;
                    c.punct(':')?;
                    let dlc = c.num("message length")?;
                    let transmitter = c.word("transmitter")?;
                    if name == "VECTOR__INDEPENDENT_SIG_MSG" {
                        continue;
                    }
                    let (id, extended) = split_id(raw);
                    db.messages.push(MessageDef {
                        id,
                        extended,
                        name,
                        dlc,
                        transmitter,
                        signals: Vec::new(),
                        cycle_time_ms: None,
                        send_type: None,
                        comment: None,
                    });
                    current = Some(db.messages.len() - 1);
                }
                "SG_" => {
                    let sig = parse_signal(&mut c)?;
                    match current {
                        Some(m) => db.messages[m].signals.push(sig),
                        None if db.messages.is_empty() => {
                            return Err(c.err("SG_ outside of a BO_ message"));
                        }
                        None => {}
                    }
                }
                "CM_" => parse_comment(&mut db, &mut c)?,
                "VAL_" => parse_val(&mut db, &mut c)?,
                "BA_DEF_" => {
                    // BA_DEF_ BO_ "GenMsgSendType" ENUM "A","B";
                    let scope = c.word("attribute scope").unwrap_or_default();
                    if scope == "BO_"
                        && c.string("attribute name").ok().as_deref() == Some("GenMsgSendType")
                        && c.word("type").ok().as_deref() == Some("ENUM")
                    {
                        send_types.clear();
                        while let Some(t) = c.next() {
                            if let Tok::Str(s) = t {
                                send_types.push(s);
                            }
                        }
                    }
                }
                "BA_" => parse_attr(&mut db, &mut c, &send_types)?,
                _ => {}
            }
        }
        Ok(db)
    }
}

fn find_msg(db: &mut Database, raw: u64) -> Option<&mut MessageDef> {
    let (id, extended) = split_id(raw);
    db.messages
        .iter_mut()
        .find(|m| m.id == id && m.extended == extended)
}

fn parse_comment(db: &mut Database, c: &mut Cur) -> Result<(), DbcError> {
    match c.peek() {
        Some(Tok::Word(w)) if w == "BO_" => {
            c.pos += 1;
            let id = c.num("message id")?;
            let text = c.string("comment")?;
            if let Some(m) = find_msg(db, id) {
                m.comment = Some(text);
            }
        }
        Some(Tok::Word(w)) if w == "SG_" => {
            c.pos += 1;
            let id = c.num("message id")?;
            let sig = c.word("signal name")?;
            let text = c.string("comment")?;
            if let Some(s) =
                find_msg(db, id).and_then(|m| m.signals.iter_mut().find(|s| s.name == sig))
            {
                s.comment = Some(text);
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_val(db: &mut Database, c: &mut Cur) -> Result<(), DbcError> {
    // Environment-variable tables (`VAL_ name ...`) have no numeric id.
    let Some(Tok::Word(w)) = c.peek() else {
        return Ok(());
    };
    let Ok(id) = w.parse::<u64>() else {
        return Ok(());
    };
    c.pos += 1;
    let sig = c.word("signal name")?;
    let mut descs = Vec::new();
    while let Some(Tok::Word(v)) = c.peek() {
        let v = v.clone();
        c.pos += 1;
        let value = v
            .parse::<i64>()
            .or_else(|_| v.parse::<f64>().map(|f| f as i64))
            .map_err(|_| c.err(format!("invalid value '{v}'")))?;
        descs.push((value, c.string("value description")?));
    }
    if let Some(s) = find_msg(db, id).and_then(|m| m.signals.iter_mut().find(|s| s.name == sig)) {
        s.value_descriptions = descs;
    }
    Ok(())
}

fn parse_attr(db: &mut Database, c: &mut Cur, send_types: &[String]) -> Result<(), DbcError> {
    let attr = c.string("attribute name")?;
    let scope = match c.peek() {
        Some(Tok::Word(w)) => w.clone(),
        _ => return Ok(()),
    };
    match (attr.as_str(), scope.as_str()) {
        ("GenMsgCycleTime", "BO_") => {
            c.pos += 1;
            let id = c.num("message id")?;
            let v: f64 = c.num("cycle time")?;
            if let Some(m) = find_msg(db, id) {
                m.cycle_time_ms = Some(v as u32);
            }
        }
        ("GenMsgSendType", "BO_") => {
            c.pos += 1;
            let id = c.num("message id")?;
            let v = match c.next() {
                Some(Tok::Str(s)) => Some(s),
                Some(Tok::Word(w)) => w
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| send_types.get(n).cloned()),
                _ => None,
            };
            if let Some(m) = find_msg(db, id) {
                m.send_type = v;
            }
        }
        ("GenSigStartValue", "SG_") => {
            c.pos += 1;
            let id = c.num("message id")?;
            let sig = c.word("signal name")?;
            let v: f64 = c.num("start value")?;
            if let Some(s) =
                find_msg(db, id).and_then(|m| m.signals.iter_mut().find(|s| s.name == sig))
            {
                s.initial_raw = Some(v as u64);
            }
        }
        _ => {}
    }
    Ok(())
}
