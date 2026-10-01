//! The canvas widget.

use std::collections::HashMap;
use std::hash::Hash;

use egui::emath::TSTransform;
use egui::{
    Align, Align2, Area, Color32, FontId, Frame, Id, Key, LayerId, Layout, Order, Painter,
    PointerButton, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, Vec2,
    pos2, vec2,
};

use crate::events::{FlowEvent, FlowResponse};
use crate::geometry::{dist_to_path, edge_path, end_direction, path_midpoint};
use crate::options::{Background, FlowOptions};
use crate::state::{ConnectDrag, FlowState, NodeDrag};
use crate::types::*;
use crate::viewer::FlowViewer;

/// Nodes may grow up to this wide; use `ui.set_max_width` in `node_ui` to
/// constrain further.
const MAX_NODE_WIDTH: f32 = 400.0;
const HANDLE_RADIUS: f32 = 5.0;
const EDGE_HIT_PX: f32 = 8.0;

/// A node-graph canvas. Build it each frame and call [`Flow::show`].
pub struct Flow {
    id: Id,
    opts: FlowOptions,
}

type Handles = HashMap<NodeId, Vec<Handle>>;

impl Flow {
    pub fn new(id_salt: impl Hash) -> Self {
        Self {
            id: Id::new(id_salt),
            opts: FlowOptions::default(),
        }
    }

    pub fn options(mut self, opts: FlowOptions) -> Self {
        self.opts = opts;
        self
    }

    pub fn background(mut self, background: Background) -> Self {
        self.opts.background = background;
        self
    }

    pub fn minimap(mut self, on: bool) -> Self {
        self.opts.minimap = on;
        self
    }

    pub fn controls(mut self, on: bool) -> Self {
        self.opts.controls = on;
        self
    }

    pub fn snap_to_grid(mut self, grid: f32) -> Self {
        self.opts.snap_to_grid = Some(grid);
        self
    }

    pub fn edge_kind(mut self, kind: EdgeKind) -> Self {
        self.opts.default_edge_kind = kind;
        self
    }

    /// Fill the available space with the canvas and run one frame.
    pub fn show<N, E: Default, V: FlowViewer<N, E>>(
        self,
        ui: &mut Ui,
        state: &mut FlowState<N, E>,
        viewer: &mut V,
    ) -> FlowResponse<N, E> {
        let o = self.opts;
        let id = ui.make_persistent_id(self.id);
        let (canvas, _) = ui.allocate_exact_size(ui.available_size_before_wrap(), Sense::hover());
        let mut events = Vec::new();
        let selection_before = state.selection_snapshot();
        let viewport_before = state.viewport;

        // --- fit view ------------------------------------------------------
        if o.fit_view_on_init && !state.initialized {
            state.fit_frames = 2;
        }
        state.initialized = true;
        if state.fit_frames > 0 {
            if let Some(bounds) = state.bounds() {
                state.viewport = FlowState::<N, E>::viewport_for(
                    bounds,
                    canvas.size(),
                    o.fit_view_padding,
                    (o.min_zoom, o.max_zoom.min(1.5)),
                );
                ui.ctx().request_repaint();
            }
            state.fit_frames -= 1;
        }

        // --- wheel / pinch -------------------------------------------------
        let layer_id = LayerId::new(ui.layer_id().order, id.with("layer"));
        ui.ctx().set_sublayer(ui.layer_id(), layer_id);
        let hover = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| canvas.contains(*p));
        let over_canvas = hover.is_some_and(|p| {
            ui.ctx()
                .layer_id_at(p)
                .is_none_or(|l| l == layer_id || l == ui.layer_id())
        });
        if let (true, Some(p)) = (over_canvas, hover) {
            let (scroll, zoom_delta) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            let mut factor = zoom_delta;
            if o.zoom_on_scroll {
                factor *= (scroll.y * 0.0015).exp();
            } else {
                state.viewport.pan += scroll;
            }
            if factor != 1.0 {
                let rel = (p - canvas.min).to_pos2();
                state.viewport.zoom_at(rel, factor, o.min_zoom, o.max_zoom);
            }
        }

        // --- scene layer ---------------------------------------------------
        let make_tf = |vp: &Viewport| TSTransform::new(canvas.min.to_vec2() + vp.pan, vp.zoom);
        let mut tf = make_tf(&state.viewport);
        ui.ctx().set_transform_layer(layer_id, tf);
        let mut cui = ui.new_child(
            UiBuilder::new()
                .layer_id(layer_id)
                .max_rect(tf.inverse() * canvas)
                .layout(Layout::top_down(Align::Min)),
        );
        cui.set_clip_rect(tf.inverse() * canvas);

        let pane = cui.interact(
            tf.inverse() * canvas,
            id.with("pane"),
            Sense::click_and_drag(),
        );
        let shift = ui.input(|i| i.modifiers.shift);
        let pointer_scene = |tf: &TSTransform| {
            ui.input(|i| i.pointer.latest_pos())
                .map(|p| tf.inverse() * p)
        };

        if pane.drag_started_by(PointerButton::Primary) && shift && o.elements_selectable {
            state.interaction.box_select = pointer_scene(&tf);
        }
        if pane.dragged()
            && state.interaction.box_select.is_none()
            && (pane.dragged_by(PointerButton::Primary) || pane.dragged_by(PointerButton::Middle))
        {
            state.viewport.pan += pane.drag_delta() * state.viewport.zoom;
            tf = make_tf(&state.viewport);
            ui.ctx().set_transform_layer(layer_id, tf);
            cui.set_clip_rect(tf.inverse() * canvas);
        }

        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 0.0, visuals.extreme_bg_color);
        paint_background(
            &painter,
            canvas,
            &state.viewport,
            &o,
            visuals.widgets.noninteractive.bg_stroke.color,
        );
        let sel_color = visuals.selection.stroke.color;

        // --- pass 1: node interaction (before drawing, so drags don't lag) --
        let handles: Handles = state
            .nodes
            .iter()
            .map(|n| (n.id, viewer.handles(n)))
            .collect();

        let node_resps: Vec<(NodeId, Response)> = state
            .nodes
            .iter()
            .map(|n| {
                (
                    n.id,
                    cui.interact(n.rect(), id.with(("node", n.id)), Sense::click_and_drag()),
                )
            })
            .collect();

        let mut bring_to_front = None;
        let mut dragged = Vec::new();
        let mut stopped = Vec::new();
        for (nid, r) in &node_resps {
            let Some(idx) = state.nodes.iter().position(|n| n.id == *nid) else {
                continue;
            };
            if r.clicked() {
                if o.elements_selectable {
                    select_node(state, *nid, shift);
                }
                events.push(FlowEvent::NodeClicked(*nid));
            }
            if r.drag_started_by(PointerButton::Primary) {
                if o.elements_selectable && !state.nodes[idx].selected {
                    select_node(state, *nid, shift);
                }
                if o.nodes_draggable && state.nodes[idx].draggable {
                    let origins = state
                        .nodes
                        .iter()
                        .filter(|n| n.selected && n.draggable)
                        .map(|n| (n.id, n.position))
                        .collect();
                    state.interaction.node_drag = Some(NodeDrag {
                        origins,
                        accum: Vec2::ZERO,
                    });
                }
                bring_to_front = Some(*nid);
            }
            if r.dragged_by(PointerButton::Primary)
                && let Some(d) = &mut state.interaction.node_drag
            {
                d.accum += r.drag_delta();
                for (oid, origin) in &d.origins {
                    if let Some(n) = state.nodes.iter_mut().find(|n| n.id == *oid) {
                        n.position = snap(*origin + d.accum, o.snap_to_grid);
                        dragged.push(*oid);
                    }
                }
            }
            if r.drag_stopped()
                && let Some(d) = state.interaction.node_drag.take()
            {
                stopped.extend(d.origins.iter().map(|(id, _)| *id));
            }
        }
        if !dragged.is_empty() {
            events.push(FlowEvent::NodesDragged(dragged));
        }
        if !stopped.is_empty() {
            events.push(FlowEvent::NodesDragStopped(stopped));
        }
        if let Some(front) = bring_to_front
            && let Some(i) = state.nodes.iter().position(|n| n.id == front)
        {
            let n = state.nodes.remove(i);
            state.nodes.push(n);
        }

        // --- edges: routing, hover, click ---------------------------------
        let rects: HashMap<NodeId, Rect> = state.nodes.iter().map(|n| (n.id, n.rect())).collect();
        let geoms: Vec<(usize, Vec<Pos2>, Side)> = state
            .edges
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let sh = find_handle(&handles, e.source, e.source_handle)?;
                let th = find_handle(&handles, e.target, e.target_handle)?;
                let path = edge_path(
                    e.kind.unwrap_or(o.default_edge_kind),
                    sh.position(*rects.get(&e.source)?),
                    sh.side,
                    th.position(*rects.get(&e.target)?),
                    th.side,
                );
                Some((i, path, th.side))
            })
            .collect();

        let pointer_flow = pointer_scene(&tf);
        let hovered_edge = if pane.hovered() && o.elements_selectable {
            pointer_flow.and_then(|p| {
                geoms
                    .iter()
                    .map(|(i, path, _)| (*i, dist_to_path(p, path)))
                    .filter(|(_, d)| *d * state.viewport.zoom <= EDGE_HIT_PX)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| i)
            })
        } else {
            None
        };
        if pane.clicked() {
            if let Some(i) = hovered_edge {
                let eid = state.edges[i].id;
                if !shift {
                    state.clear_selection();
                }
                state.edges[i].selected = !shift || !state.edges[i].selected;
                events.push(FlowEvent::EdgeClicked(eid));
            } else {
                if !shift && o.elements_selectable {
                    state.clear_selection();
                }
                if let Some(p) = pointer_flow {
                    events.push(FlowEvent::PaneClicked(p));
                }
            }
        }

        // --- pass 2a: draw edges -------------------------------------------
        let cp = cui.painter().clone();
        let time = ui.input(|i| i.time) as f32;
        let mut order: Vec<_> = geoms.iter().collect();
        order.sort_by_key(|(i, _, _)| state.edges[*i].selected);
        for (i, path, _) in order {
            let e = &state.edges[*i];
            let hovered = hovered_edge == Some(*i);
            let color = if e.selected {
                sel_color
            } else {
                visuals
                    .widgets
                    .noninteractive
                    .fg_stroke
                    .color
                    .gamma_multiply(0.7)
            };
            let stroke = Stroke::new(if e.selected || hovered { 2.5_f32 } else { 1.5 }, color);
            if e.animated {
                cp.extend(Shape::dashed_line_with_offset(
                    path,
                    stroke,
                    &[6.0],
                    &[4.0],
                    -time * 20.0,
                ));
                ui.ctx().request_repaint();
            } else {
                cp.add(Shape::line(path.clone(), stroke));
            }
            if e.arrow {
                let tip = *path.last().unwrap();
                let dir = end_direction(path);
                let normal = vec2(-dir.y, dir.x);
                let base = tip - dir * 10.0;
                cp.add(Shape::convex_polygon(
                    vec![tip, base + normal * 5.0, base - normal * 5.0],
                    color,
                    Stroke::NONE,
                ));
            }
            if let Some(label) = &e.label {
                let galley = cp.layout_no_wrap(
                    label.clone(),
                    FontId::proportional(12.0),
                    visuals.text_color(),
                );
                let mid = path_midpoint(path);
                cp.rect_filled(
                    Rect::from_center_size(mid, galley.size() + vec2(8.0, 4.0)),
                    3.0,
                    visuals.window_fill,
                );
                cp.galley(mid - galley.size() / 2.0, galley, visuals.text_color());
            }
        }

        // --- pass 2b: node contents ----------------------------------------
        for i in 0..state.nodes.len() {
            let node = &mut state.nodes[i];
            let frame = viewer.node_frame(&cui, node);
            let content = Rect::from_min_size(node.position, vec2(MAX_NODE_WIDTH, 10_000.0));
            let inner = cui.scope_builder(
                UiBuilder::new()
                    .id_salt(("flow_node", node.id))
                    .max_rect(content),
                |ui| {
                    // Selectable labels would swallow the drag that should move the node.
                    ui.style_mut().interaction.selectable_labels = false;
                    frame.show(ui, |ui| viewer.node_ui(ui, node))
                },
            );
            let rect = inner.inner.response.rect;
            node.size = rect.size();
            if node.selected {
                cp.rect_stroke(
                    rect,
                    6.0,
                    Stroke::new(2.0_f32, sel_color),
                    StrokeKind::Outside,
                );
            }
        }

        // --- pass 2c: handles + connection drag -----------------------------
        let rects: HashMap<NodeId, Rect> = state.nodes.iter().map(|n| (n.id, n.rect())).collect();
        let connecting = state
            .interaction
            .connecting
            .as_ref()
            .map(|c| (c.node, c.handle));
        let mut all_handles: Vec<(NodeId, Handle, Pos2)> = Vec::new();
        for n in &state.nodes {
            let connectable = o.nodes_connectable && n.connectable;
            for h in handles.get(&n.id).into_iter().flatten() {
                let pos = h.position(rects[&n.id]);
                all_handles.push((n.id, *h, pos));
                let hit = Rect::from_center_size(pos, Vec2::splat(2.0 * (HANDLE_RADIUS + 5.0)));
                let hr = cui.interact(
                    hit,
                    id.with(("handle", n.id, h.id)),
                    if connectable {
                        Sense::drag()
                    } else {
                        Sense::hover()
                    },
                );
                if connectable && hr.drag_started() {
                    state.interaction.connecting = Some(ConnectDrag {
                        node: n.id,
                        handle: h.id,
                    });
                }
                let valid_target = connecting.is_some_and(|from| {
                    connection_between(state, &handles, &*viewer, &o, from, (n.id, h.id)).is_some()
                });
                let active = hr.hovered() || connecting == Some((n.id, h.id));
                let r = if active || valid_target {
                    HANDLE_RADIUS + 1.5
                } else {
                    HANDLE_RADIUS
                };
                let fill = if active || valid_target {
                    sel_color
                } else {
                    visuals.widgets.inactive.fg_stroke.color
                };
                cp.circle_filled(pos, r, fill);
                cp.circle_stroke(pos, r, Stroke::new(1.5_f32, visuals.window_fill));
                if let Some(text) = viewer.handle_label(n, h.id) {
                    let galley = cp.layout_no_wrap(
                        text,
                        FontId::proportional(10.0),
                        visuals.weak_text_color(),
                    );
                    let size = galley.size();
                    let anchor = pos - h.side.dir() * 8.0;
                    let top_left = match h.side {
                        Side::Left => pos2(anchor.x - size.x, anchor.y - size.y / 2.0),
                        Side::Right => pos2(anchor.x, anchor.y - size.y / 2.0),
                        Side::Top => pos2(anchor.x - size.x / 2.0, anchor.y - size.y),
                        Side::Bottom => pos2(anchor.x - size.x / 2.0, anchor.y),
                    };
                    cp.galley(top_left, galley, visuals.weak_text_color());
                }
            }
        }

        if let Some(from) = state
            .interaction
            .connecting
            .as_ref()
            .map(|c| (c.node, c.handle))
        {
            ui.ctx().request_repaint();
            let from_handle = find_handle(&handles, from.0, from.1);
            let from_rect = rects.get(&from.0);
            if let (Some(fh), Some(fr), Some(pointer)) =
                (from_handle, from_rect, pointer_scene(&tf))
            {
                let from_pos = fh.position(*fr);
                let candidate = all_handles
                    .iter()
                    .filter(|(_, _, p)| (*p - pointer).length() <= o.connection_radius)
                    .filter_map(|(n, h, p)| {
                        connection_between(state, &handles, &*viewer, &o, from, (*n, h.id))
                            .map(|c| (c, *h, *p))
                    })
                    .min_by(|a, b| {
                        (a.2 - pointer)
                            .length()
                            .total_cmp(&(b.2 - pointer).length())
                    });

                let (end, end_side) = match &candidate {
                    Some((_, h, p)) => (*p, h.side),
                    None => (pointer, opposite(fh.side)),
                };
                let (s, ss, t, ts) = if fh.kind == HandleKind::Source {
                    (from_pos, fh.side, end, end_side)
                } else {
                    (end, end_side, from_pos, fh.side)
                };
                cp.add(Shape::line(
                    edge_path(o.default_edge_kind, s, ss, t, ts),
                    Stroke::new(
                        2.0_f32,
                        if candidate.is_some() {
                            sel_color
                        } else {
                            sel_color.gamma_multiply(0.5)
                        },
                    ),
                ));

                let released = ui.input(|i| i.pointer.any_released());
                let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
                if released || cancelled {
                    state.interaction.connecting = None;
                    if released && !cancelled {
                        match candidate {
                            Some((conn, _, _)) => {
                                if let Some(eid) = state.add_edge(conn, E::default()) {
                                    events.push(FlowEvent::Connected(eid));
                                }
                            }
                            None => events.push(FlowEvent::ConnectionDropped {
                                node: from.0,
                                handle: from.1,
                                pos: pointer,
                            }),
                        }
                    }
                }
            } else {
                state.interaction.connecting = None;
            }
        }

        // --- box selection --------------------------------------------------
        if let (Some(start), Some(cur)) = (state.interaction.box_select, pointer_scene(&tf)) {
            let r = Rect::from_two_pos(start, cur);
            cp.rect_filled(r, 0.0, sel_color.gamma_multiply(0.12));
            cp.rect_stroke(r, 0.0, Stroke::new(1.0_f32, sel_color), StrokeKind::Inside);
            if !ui.input(|i| i.pointer.primary_down()) {
                state.interaction.box_select = None;
                state.clear_selection();
                for n in &mut state.nodes {
                    n.selected = r.intersects(n.rect());
                }
            }
        }

        // --- keyboard -------------------------------------------------------
        if o.delete_key
            && hover.is_some()
            && !ui.ctx().wants_keyboard_input()
            && ui.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace))
        {
            let (nodes, edges) = state.delete_selected();
            if !nodes.is_empty() || !edges.is_empty() {
                events.push(FlowEvent::Deleted { nodes, edges });
            }
        }

        // --- overlays -------------------------------------------------------
        if o.controls {
            controls(ui, id, canvas, state, &o);
        }
        if o.minimap {
            minimap(ui, id, canvas, state, &*viewer, sel_color);
        }

        let selection_after = state.selection_snapshot();
        if selection_after != selection_before {
            events.push(FlowEvent::SelectionChanged {
                nodes: selection_after.0,
                edges: selection_after.1,
            });
        }
        if state.viewport != viewport_before {
            events.push(FlowEvent::ViewportChanged(state.viewport));
        }

        FlowResponse {
            pane,
            nodes: node_resps,
            events,
        }
    }
}

fn select_node<N, E>(state: &mut FlowState<N, E>, id: NodeId, additive: bool) {
    if additive {
        if let Some(n) = state.node_mut(id) {
            n.selected = !n.selected;
        }
    } else {
        state.clear_selection();
        if let Some(n) = state.node_mut(id) {
            n.selected = true;
        }
    }
}

fn snap(p: Pos2, grid: Option<f32>) -> Pos2 {
    match grid {
        Some(g) if g > 0.0 => pos2((p.x / g).round() * g, (p.y / g).round() * g),
        _ => p,
    }
}

fn opposite(s: Side) -> Side {
    match s {
        Side::Left => Side::Right,
        Side::Right => Side::Left,
        Side::Top => Side::Bottom,
        Side::Bottom => Side::Top,
    }
}

fn find_handle(handles: &Handles, node: NodeId, id: HandleId) -> Option<Handle> {
    handles.get(&node)?.iter().find(|h| h.id == id).copied()
}

/// The oriented connection for a drag from `from` to `to`, if it is allowed.
fn connection_between<N, E, V: FlowViewer<N, E>>(
    state: &FlowState<N, E>,
    handles: &Handles,
    viewer: &V,
    o: &FlowOptions,
    from: (NodeId, HandleId),
    to: (NodeId, HandleId),
) -> Option<Connection> {
    let fh = find_handle(handles, from.0, from.1)?;
    let th = find_handle(handles, to.0, to.1)?;
    if fh.kind == th.kind {
        return None;
    }
    let conn = if fh.kind == HandleKind::Source {
        Connection {
            source: from.0,
            source_handle: from.1,
            target: to.0,
            target_handle: to.1,
        }
    } else {
        Connection {
            source: to.0,
            source_handle: to.1,
            target: from.0,
            target_handle: from.1,
        }
    };
    if !o.allow_self_loops && conn.source == conn.target {
        return None;
    }
    if !state.node(to.0)?.connectable || !o.nodes_connectable {
        return None;
    }
    if state.edges.iter().any(|e| e.connection() == conn) {
        return None;
    }
    viewer.can_connect(&conn).then_some(conn)
}

fn paint_background(p: &Painter, canvas: Rect, vp: &Viewport, o: &FlowOptions, color: Color32) {
    let gap = o.background_gap * vp.zoom;
    if o.background == Background::None || gap < 5.0 {
        return;
    }
    let off = vec2(vp.pan.x.rem_euclid(gap), vp.pan.y.rem_euclid(gap));
    let start = canvas.min + off;
    let cols = ((canvas.max.x - start.x) / gap).ceil().max(0.0) as usize + 1;
    let rows = ((canvas.max.y - start.y) / gap).ceil().max(0.0) as usize + 1;
    let stroke = Stroke::new(1.0_f32, color.gamma_multiply(0.6));
    match o.background {
        Background::Dots => {
            let r = (vp.zoom * 1.0).clamp(0.8, 2.0);
            for c in 0..cols {
                for r_ in 0..rows {
                    p.circle_filled(start + vec2(c as f32 * gap, r_ as f32 * gap), r, color);
                }
            }
        }
        Background::Lines => {
            for c in 0..cols {
                let x = start.x + c as f32 * gap;
                p.line_segment([pos2(x, canvas.min.y), pos2(x, canvas.max.y)], stroke);
            }
            for r_ in 0..rows {
                let y = start.y + r_ as f32 * gap;
                p.line_segment([pos2(canvas.min.x, y), pos2(canvas.max.x, y)], stroke);
            }
        }
        Background::Cross => {
            let a = (gap * 0.12).clamp(2.0, 5.0);
            for c in 0..cols {
                for r_ in 0..rows {
                    let m = start + vec2(c as f32 * gap, r_ as f32 * gap);
                    p.line_segment([m - vec2(a, 0.0), m + vec2(a, 0.0)], stroke);
                    p.line_segment([m - vec2(0.0, a), m + vec2(0.0, a)], stroke);
                }
            }
        }
        Background::None => {}
    }
}

/// Zoom in / out / fit buttons, bottom-left.
fn controls<N, E>(ui: &Ui, id: Id, canvas: Rect, state: &mut FlowState<N, E>, o: &FlowOptions) {
    Area::new(id.with("controls"))
        .order(Order::Foreground)
        .movable(false)
        .fixed_pos(canvas.left_bottom() + vec2(12.0, -12.0))
        .pivot(Align2::LEFT_BOTTOM)
        .show(ui.ctx(), |ui| {
            Frame::popup(ui.style()).inner_margin(4.0).show(ui, |ui| {
                let center = (canvas.size() / 2.0).to_pos2();
                if ui.button("+").on_hover_text("Zoom in").clicked() {
                    state.viewport.zoom_at(center, 1.25, o.min_zoom, o.max_zoom);
                }
                if ui.button("\u{2212}").on_hover_text("Zoom out").clicked() {
                    state.viewport.zoom_at(center, 0.8, o.min_zoom, o.max_zoom);
                }
                if ui.button("Fit").on_hover_text("Fit view").clicked() {
                    state.fit_view();
                }
            });
        });
}

/// Overview of the whole graph, bottom-right. Click or drag to recentre.
fn minimap<N, E, V: FlowViewer<N, E>>(
    ui: &Ui,
    id: Id,
    canvas: Rect,
    state: &mut FlowState<N, E>,
    viewer: &V,
    accent: Color32,
) {
    const SIZE: Vec2 = vec2(160.0, 110.0);
    Area::new(id.with("minimap"))
        .order(Order::Foreground)
        .movable(false)
        .fixed_pos(canvas.right_bottom() - vec2(12.0, 12.0))
        .pivot(Align2::RIGHT_BOTTOM)
        .show(ui.ctx(), |ui| {
            let (rect, resp) = ui.allocate_exact_size(SIZE, Sense::click_and_drag());
            let v = ui.visuals();
            let painter = ui.painter_at(rect);
            painter.rect(
                rect,
                4.0,
                v.window_fill.gamma_multiply(0.92),
                v.widgets.noninteractive.bg_stroke,
                StrokeKind::Inside,
            );

            let vp = state.viewport;
            let view =
                Rect::from_min_max(vp.to_flow(Pos2::ZERO), vp.to_flow(canvas.size().to_pos2()));
            let mut bounds = view;
            if let Some(b) = state.bounds() {
                bounds = bounds.union(b);
            }
            bounds = bounds.expand(bounds.size().max_elem() * 0.05 + 1.0);
            let scale = (rect.width() / bounds.width()).min(rect.height() / bounds.height());
            let to_mini = |p: Pos2| rect.center() + (p - bounds.center()) * scale;
            let to_rect = |r: Rect| Rect::from_min_max(to_mini(r.min), to_mini(r.max));

            for n in &state.nodes {
                let color = viewer
                    .minimap_color(n)
                    .unwrap_or_else(|| v.widgets.inactive.fg_stroke.color.gamma_multiply(0.6));
                painter.rect_filled(to_rect(n.rect()), 1.0, color);
            }
            painter.rect_stroke(
                to_rect(view),
                1.0,
                Stroke::new(1.5_f32, accent),
                StrokeKind::Inside,
            );

            if (resp.clicked() || resp.dragged())
                && let Some(p) = resp.interact_pointer_pos()
            {
                let target = bounds.center() + (p - rect.center()) / scale;
                state.viewport.pan = canvas.size() / 2.0 - target.to_vec2() * vp.zoom;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, Event, RawInput};

    struct V;
    impl FlowViewer<&'static str, ()> for V {
        fn node_ui(&mut self, ui: &mut Ui, node: &mut Node<&'static str>) {
            ui.label(node.data);
        }
    }

    fn run_frame(
        ctx: &Context,
        state: &mut FlowState<&'static str, ()>,
        events: Vec<Event>,
        fit: bool,
    ) -> Vec<FlowEvent<&'static str, ()>> {
        let mut out = Vec::new();
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(Frame::NONE)
                .show(ctx, |ui| {
                    let opts = FlowOptions {
                        minimap: true,
                        fit_view_on_init: fit,
                        ..Default::default()
                    };
                    out = Flow::new("t").options(opts).show(ui, state, &mut V).events;
                });
        });
        out
    }

    #[test]
    fn renders_measures_and_fits() {
        let ctx = Context::default();
        let mut s = FlowState::new();
        let a = s.add_node(pos2(0.0, 0.0), "a");
        let b = s.add_node(pos2(400.0, 300.0), "b");
        s.connect(a, b, ());
        for _ in 0..4 {
            run_frame(&ctx, &mut s, vec![], true);
        }
        // Size was measured from content rather than the default estimate.
        assert_ne!(s.nodes[0].size, vec2(150.0, 40.0));
        // Fit view framed both nodes inside the 800x600 canvas.
        for n in &s.nodes {
            let p = s.viewport.to_screen(n.position);
            assert!(
                p.x >= 0.0 && p.y >= 0.0 && p.x <= 800.0 && p.y <= 600.0,
                "{p:?}"
            );
        }
    }

    #[test]
    fn dragging_a_node_moves_it() {
        let ctx = Context::default();
        let mut s = FlowState::new();
        s.viewport = Viewport::default();
        let a = s.add_node(pos2(100.0, 100.0), "node");
        for _ in 0..2 {
            run_frame(&ctx, &mut s, vec![], false);
        }
        let start = s.node(a).unwrap().rect().center();
        let mut moved = start;
        let btn = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        run_frame(&ctx, &mut s, vec![Event::PointerMoved(start)], false);
        run_frame(&ctx, &mut s, vec![btn(start, true)], false);
        for step in 1..=5 {
            moved = start + vec2(20.0 * step as f32, 10.0 * step as f32);
            run_frame(&ctx, &mut s, vec![Event::PointerMoved(moved)], false);
        }
        run_frame(&ctx, &mut s, vec![btn(moved, false)], false);
        let n = s.node(a).unwrap();
        assert!(
            n.position.x > 150.0 && n.position.y > 130.0,
            "{:?}",
            n.position
        );
        assert!(n.selected);
    }

    #[test]
    fn dragging_between_handles_creates_an_edge() {
        let ctx = Context::default();
        let mut s = FlowState::new();
        let a = s.add_node(pos2(50.0, 100.0), "a");
        let b = s.add_node(pos2(400.0, 250.0), "b");
        for _ in 0..3 {
            run_frame(&ctx, &mut s, vec![], false);
        }
        let from =
            Handle::source(Handle::DEFAULT_SOURCE, Side::Right).position(s.node(a).unwrap().rect());
        let to =
            Handle::target(Handle::DEFAULT_TARGET, Side::Left).position(s.node(b).unwrap().rect());
        let btn = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        run_frame(&ctx, &mut s, vec![Event::PointerMoved(from)], false);
        run_frame(&ctx, &mut s, vec![btn(from, true)], false);
        let mut events = Vec::new();
        for i in 1..=6 {
            let p = from + (to - from) * (i as f32 / 6.0);
            events.extend(run_frame(&ctx, &mut s, vec![Event::PointerMoved(p)], false));
        }
        events.extend(run_frame(&ctx, &mut s, vec![btn(to, false)], false));
        assert_eq!(s.edges.len(), 1, "{events:?}");
        assert_eq!((s.edges[0].source, s.edges[0].target), (a, b));
        assert!(events.iter().any(|e| matches!(e, FlowEvent::Connected(_))));
    }
}
