//! Edge routing. Every edge is reduced to a polyline, which is used both for
//! drawing and for hit-testing.

use egui::{Pos2, Vec2, pos2};

use crate::types::{EdgeKind, Side};

const BEZIER_SEGMENTS: usize = 40;
const STEP_STUB: f32 = 20.0;
const CORNER_RADIUS: f32 = 8.0;

/// Polyline from `s` (leaving through `ss`) to `t` (entering through `ts`).
pub fn edge_path(kind: EdgeKind, s: Pos2, ss: Side, t: Pos2, ts: Side) -> Vec<Pos2> {
    match kind {
        EdgeKind::Straight => vec![s, t],
        EdgeKind::Bezier => bezier(s, ss, t, ts),
        EdgeKind::Step => orthogonal(s, ss, t, ts, 0.0),
        EdgeKind::SmoothStep => orthogonal(s, ss, t, ts, CORNER_RADIUS),
    }
}

/// React Flow's control-point rule: proportional when the target is ahead,
/// a square-root fall-off when the edge has to loop backwards.
fn control_offset(distance: f32) -> f32 {
    const CURVATURE: f32 = 0.25;
    if distance >= 0.0 {
        0.5 * distance
    } else {
        CURVATURE * 25.0 * (-distance).sqrt()
    }
}

fn control_point(p: Pos2, side: Side, other: Pos2) -> Pos2 {
    match side {
        Side::Right => pos2(p.x + control_offset(other.x - p.x), p.y),
        Side::Left => pos2(p.x - control_offset(p.x - other.x), p.y),
        Side::Bottom => pos2(p.x, p.y + control_offset(other.y - p.y)),
        Side::Top => pos2(p.x, p.y - control_offset(p.y - other.y)),
    }
}

fn bezier(s: Pos2, ss: Side, t: Pos2, ts: Side) -> Vec<Pos2> {
    let c1 = control_point(s, ss, t);
    let c2 = control_point(t, ts, s);
    (0..=BEZIER_SEGMENTS)
        .map(|i| {
            let u = i as f32 / BEZIER_SEGMENTS as f32;
            let v = 1.0 - u;
            let w = [v * v * v, 3.0 * v * v * u, 3.0 * v * u * u, u * u * u];
            pos2(
                w[0] * s.x + w[1] * c1.x + w[2] * c2.x + w[3] * t.x,
                w[0] * s.y + w[1] * c1.y + w[2] * c2.y + w[3] * t.y,
            )
        })
        .collect()
}

fn orthogonal(s: Pos2, ss: Side, t: Pos2, ts: Side, radius: f32) -> Vec<Pos2> {
    let p1 = s + ss.dir() * STEP_STUB;
    let p2 = t + ts.dir() * STEP_STUB;
    let mut pts = vec![s, p1];
    match (ss.is_horizontal(), ts.is_horizontal()) {
        (true, true) => {
            let mx = (p1.x + p2.x) / 2.0;
            pts.extend([pos2(mx, p1.y), pos2(mx, p2.y)]);
        }
        (false, false) => {
            let my = (p1.y + p2.y) / 2.0;
            pts.extend([pos2(p1.x, my), pos2(p2.x, my)]);
        }
        (true, false) => pts.push(pos2(p2.x, p1.y)),
        (false, true) => pts.push(pos2(p1.x, p2.y)),
    }
    pts.extend([p2, t]);
    pts.dedup_by(|a, b| (*a - *b).length_sq() < 1e-6);
    if radius > 0.0 {
        round_corners(&pts, radius)
    } else {
        pts
    }
}

/// Replace each interior vertex by a quadratic arc.
fn round_corners(pts: &[Pos2], radius: f32) -> Vec<Pos2> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    for w in pts.windows(3) {
        let (a, b, c) = (w[0], w[1], w[2]);
        let r = radius
            .min((b - a).length() / 2.0)
            .min((c - b).length() / 2.0);
        let dir_in = (b - a).normalized();
        let dir_out = (c - b).normalized();
        let start = b - dir_in * r;
        let end = b + dir_out * r;
        for i in 0..=6 {
            let u = i as f32 / 6.0;
            let v = 1.0 - u;
            out.push(pos2(
                v * v * start.x + 2.0 * v * u * b.x + u * u * end.x,
                v * v * start.y + 2.0 * v * u * b.y + u * u * end.y,
            ));
        }
    }
    out.push(*pts.last().unwrap());
    out
}

/// Distance from `p` to the segment `a`–`b`.
pub fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq == 0.0 {
        return (p - a).length();
    }
    let u = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    (p - (a + ab * u)).length()
}

/// Distance from `p` to a polyline.
pub fn dist_to_path(p: Pos2, path: &[Pos2]) -> f32 {
    path.windows(2)
        .map(|w| dist_to_segment(p, w[0], w[1]))
        .fold(f32::INFINITY, f32::min)
}

/// Point halfway along the polyline (by length), for label placement.
pub fn path_midpoint(path: &[Pos2]) -> Pos2 {
    let total: f32 = path.windows(2).map(|w| (w[1] - w[0]).length()).sum();
    let mut left = total / 2.0;
    for w in path.windows(2) {
        let len = (w[1] - w[0]).length();
        if left <= len && len > 0.0 {
            return w[0] + (w[1] - w[0]) * (left / len);
        }
        left -= len;
    }
    path.last().copied().unwrap_or(Pos2::ZERO)
}

/// Direction of the final segment, for orienting arrowheads.
pub fn end_direction(path: &[Pos2]) -> Vec2 {
    path.windows(2)
        .rev()
        .map(|w| w[1] - w[0])
        .find(|d| d.length_sq() > 1e-6)
        .map(|d| d.normalized())
        .unwrap_or(Vec2::X)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_hits_endpoints() {
        let (s, t) = (pos2(0.0, 0.0), pos2(200.0, 100.0));
        let p = edge_path(EdgeKind::Bezier, s, Side::Right, t, Side::Left);
        assert_eq!(p.first().copied(), Some(s));
        assert!((*p.last().unwrap() - t).length() < 1e-3);
    }

    #[test]
    fn step_is_axis_aligned() {
        let p = edge_path(
            EdgeKind::Step,
            pos2(0.0, 0.0),
            Side::Right,
            pos2(200.0, 100.0),
            Side::Left,
        );
        for w in p.windows(2) {
            assert!(w[0].x == w[1].x || w[0].y == w[1].y, "{w:?}");
        }
    }

    #[test]
    fn smoothstep_keeps_endpoints() {
        let (s, t) = (pos2(0.0, 0.0), pos2(-50.0, 100.0));
        let p = edge_path(EdgeKind::SmoothStep, s, Side::Right, t, Side::Left);
        assert_eq!(p[0], s);
        assert_eq!(*p.last().unwrap(), t);
    }

    #[test]
    fn segment_distance() {
        assert_eq!(
            dist_to_segment(pos2(5.0, 3.0), pos2(0.0, 0.0), pos2(10.0, 0.0)),
            3.0
        );
        assert_eq!(
            dist_to_segment(pos2(-4.0, 3.0), pos2(0.0, 0.0), pos2(10.0, 0.0)),
            5.0
        );
    }

    #[test]
    fn midpoint_of_line() {
        let m = path_midpoint(&[pos2(0.0, 0.0), pos2(10.0, 0.0), pos2(10.0, 10.0)]);
        assert_eq!(m, pos2(10.0, 0.0));
    }
}
