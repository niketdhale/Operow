# egui-flow

A [React Flow](https://reactflow.dev/)-style node-graph canvas for [egui](https://github.com/emilk/egui), in pure Rust.

```sh
cargo run -p egui-flow --example basic
```

## Features

| React Flow | egui-flow |
|---|---|
| Pan / zoom viewport | drag background or middle mouse to pan, wheel / pinch to zoom (zoom-to-cursor), `fit_view()` |
| Custom nodes | implement `FlowViewer::node_ui` with any egui widgets |
| Handles | `FlowViewer::handles` — any number per node, on any side, source or target |
| Connecting | drag handle → handle, snapping, live validation (`can_connect`), `Esc` cancels |
| Edge types | `Bezier`, `Straight`, `Step`, `SmoothStep`; labels, arrowheads, animated dashes |
| Selection | click, shift-click, shift-drag box select, `Delete`/`Backspace` removes |
| `snapToGrid` | `FlowOptions::snap_to_grid` |
| `<Background>` | `Dots`, `Lines`, `Cross`, `None` |
| `<MiniMap>` / `<Controls>` | `FlowOptions::{minimap, controls}` |
| `onConnect`, `onNodesDelete`, … | `FlowEvent` values returned from `show` |

## Usage

```rust
use egui_flow::{Flow, FlowEvent, FlowState, FlowViewer, Node};

struct Viewer;
impl FlowViewer<String, ()> for Viewer {
    fn node_ui(&mut self, ui: &mut egui::Ui, node: &mut Node<String>) {
        ui.text_edit_singleline(&mut node.data);
    }
}

// once
let mut state: FlowState<String, ()> = FlowState::new();
let a = state.add_node(egui::pos2(0.0, 0.0), "a".into());
let b = state.add_node(egui::pos2(250.0, 60.0), "b".into());
state.connect(a, b, ());

// every frame
let out = Flow::new("graph").minimap(true).show(ui, &mut state, &mut Viewer);
for event in out.events {
    if let FlowEvent::Connected(edge) = event { /* mirror into your model */ }
}
```

`FlowState` is plain data (`nodes`, `edges`, `viewport`) that you own and may mutate between frames.
Node sizes are measured from the rendered content each frame. Enable the `serde` feature to serialize nodes, edges and the viewport.

## Notes

* Content scales with zoom by rasterising at 1× and transforming, so text is soft when zoomed in far (same as `egui::Scene`).
* Selectable labels are disabled inside nodes so dragging on text moves the node; re-enable in `node_ui` if needed.
* Not yet implemented: re-connecting existing edges by dragging their ends, node resizing, nested/grouped nodes, auto-layout.
