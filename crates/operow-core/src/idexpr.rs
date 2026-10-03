//! The "100-1FF, 3A0, !7DF" CAN ID expression used by trace filters and
//! Replay nodes.

/// A set of CAN IDs: includes (ranges or singles) minus excludes. With no
/// includes every ID is selected before excludes apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdExpr {
    include: Vec<(u32, u32)>,
    exclude: Vec<(u32, u32)>,
}

fn parse_hex(s: &str) -> Result<u32, String> {
    let h = s.trim_start_matches("0x").trim_start_matches("0X");
    let v = u32::from_str_radix(h, 16).map_err(|_| format!("bad hex ID \"{s}\""))?;
    if v > 0x1FFF_FFFF {
        return Err(format!("ID \"{s}\" exceeds 29 bits"));
    }
    Ok(v)
}

impl IdExpr {
    /// Parse `"100-1FF, 3A0, !7DF"`: comma or space separated hex IDs and
    /// `lo-hi` ranges; a leading `!` excludes. Blank input is an empty
    /// expression.
    pub fn parse(s: &str) -> Result<IdExpr, String> {
        let mut e = IdExpr::default();
        for tok in s.split(|c: char| c == ',' || c.is_whitespace()) {
            if tok.is_empty() {
                continue;
            }
            let (neg, body) = match tok.strip_prefix('!') {
                Some(b) => (true, b),
                None => (false, tok),
            };
            let (lo, hi) = match body.split_once('-') {
                Some((a, b)) => {
                    let (a, b) = (parse_hex(a)?, parse_hex(b)?);
                    (a.min(b), a.max(b))
                }
                None => {
                    let v = parse_hex(body)?;
                    (v, v)
                }
            };
            if neg {
                e.exclude.push((lo, hi));
            } else {
                e.include.push((lo, hi));
            }
        }
        Ok(e)
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    pub fn matches(&self, id: u32) -> bool {
        let inside = |r: &(u32, u32)| (r.0..=r.1).contains(&id);
        (self.include.is_empty() || self.include.iter().any(inside))
            && !self.exclude.iter().any(inside)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_singles_excludes() {
        let e = IdExpr::parse("100-1FF, 3A0, !1A0").unwrap();
        assert!(e.matches(0x100) && e.matches(0x1FF) && e.matches(0x3A0));
        assert!(!e.matches(0x1A0) && !e.matches(0x200));
        let e = IdExpr::parse("!7DF").unwrap();
        assert!(e.matches(0x100) && !e.matches(0x7DF));
        assert!(IdExpr::parse("  ").unwrap().is_empty());
        assert!(IdExpr::parse("zz").is_err());
    }
}
