//! `operow-cli diag`.

use std::process::ExitCode;

use operow_engine::{DiagRequestSpec, Simulation};
use operow_test::Project;
use operow_uds::describe;

use crate::DiagArgs;

/// Virtual time advanced per step while waiting for the response.
const STEP_NS: u64 = 1_000_000;
/// The tester gives up on its own (P2 and P2*); this only bounds the loop.
const MAX_WAIT_NS: u64 = 30_000_000_000;

fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let digits: String = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|t| {
            t.strip_prefix("0x")
                .or_else(|| t.strip_prefix("0X"))
                .unwrap_or(t)
        })
        .collect();
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return Err(format!(
            "bad request {s:?}: expected hex bytes like \"22 F1 90\""
        ));
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&digits[i..i + 2], 16)
                .map_err(|_| format!("bad request {s:?}: expected hex bytes like \"22 F1 90\""))
        })
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn run(a: &DiagArgs) -> Result<ExitCode, String> {
    let payload = parse_hex(&a.req)?;
    let project = Project::load(&a.project).map_err(|e| e.to_string())?;
    let topo = &project.topology;
    let node = topo
        .nodes
        .iter()
        .find(|n| n.name == a.node)
        .ok_or_else(|| {
            let known: Vec<&str> = topo.nodes.iter().map(|n| n.name.as_str()).collect();
            format!("unknown node {:?} (known: {})", a.node, known.join(", "))
        })?;
    let diag = node
        .diag
        .as_ref()
        .ok_or_else(|| format!("node {:?} has no diagnostics configuration", a.node))?;
    let bus = diag
        .bus
        .or_else(|| topo.links.iter().find(|l| l.node == node.id).map(|l| l.bus))
        .ok_or_else(|| format!("node {:?} is not linked to a bus", a.node))?;

    let mut sim = Simulation::new(topo).map_err(|e| e.to_string())?;
    let mut events = Vec::new();
    let mut now = a.warmup_ms.saturating_mul(1_000_000);
    sim.run_until(operow_core::Timestamp(now), &mut events);
    let tester = sim.diag_request(DiagRequestSpec {
        bus,
        req_id: diag.req_id,
        resp_id: diag.resp_id,
        extended: diag.extended_ids,
        fd: diag.fd,
        payload: payload.clone(),
        functional: false,
    })?;
    let limit = now + MAX_WAIT_NS;
    let result = loop {
        now += STEP_NS;
        events.clear();
        sim.run_until(operow_core::Timestamp(now), &mut events);
        if let Some(r) = sim
            .take_diag_results()
            .into_iter()
            .find(|r| r.node == tester)
        {
            break r;
        }
        if now > limit {
            return Err("the tester never finished".into());
        }
    };

    println!(
        "Request:  {}  ({})",
        hex(&payload),
        describe(&payload, true)
    );
    match result.resp {
        Ok(resp) => {
            println!(
                "Response: {}  ({}) after {:.1} ms",
                hex(&resp),
                describe(&resp, false),
                result.elapsed_ms
            );
            if resp.first() == Some(&0x62)
                && resp.len() > 3
                && resp[3..].iter().all(|b| (0x20..0x7F).contains(b))
            {
                println!("ASCII:    {}", String::from_utf8_lossy(&resp[3..]));
            }
            Ok(ExitCode::from(u8::from(resp.first() == Some(&0x7F))))
        }
        Err(e) => {
            println!("Response: none ({e})");
            Ok(ExitCode::FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_hex;

    #[test]
    fn hex_requests() {
        assert_eq!(parse_hex("22 F1 90"), Ok(vec![0x22, 0xF1, 0x90]));
        assert_eq!(parse_hex("22F190"), Ok(vec![0x22, 0xF1, 0x90]));
        assert_eq!(parse_hex("0x22,0xF1"), Ok(vec![0x22, 0xF1]));
        assert!(parse_hex("2").is_err());
        assert!(parse_hex("zz").is_err());
        assert!(parse_hex("").is_err());
    }
}
