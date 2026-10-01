//! # egui-flow
//!
//! A [React Flow](https://reactflow.dev/)-style node-graph canvas for
//! [egui](https://github.com/emilk/egui).
//!
//! * pan (drag the background / middle mouse) and zoom (wheel, pinch)
//! * draggable nodes with arbitrary egui content
//! * connectable handles with live validation and snapping
//! * bezier / straight / step / smooth-step edges, labels, arrows, animation
//! * click, shift-click and box (shift-drag) selection, Delete to remove
//! * dots / lines / cross background, minimap, zoom controls
//! * events for everything the application needs to mirror
//!
//! ```no_run
//! use egui_flow::{Flow, FlowState, FlowViewer, Node};
//!
//! struct Viewer;
//! impl FlowViewer<String, ()> for Viewer {
//!     fn node_ui(&mut self, ui: &mut egui::Ui, node: &mut Node<String>) {
//!         ui.text_edit_singleline(&mut node.data);
//!     }
//! }
//!
//! fn ui(ui: &mut egui::Ui, state: &mut FlowState<String, ()>) {
//!     let out = Flow::new("graph").show(ui, state, &mut Viewer);
//!     for event in out.events { /* react */ }
//! }
//! ```

mod events;
mod geometry;
mod options;
mod state;
mod types;
mod view;
mod viewer;

pub use events::{FlowEvent, FlowResponse};
pub use geometry::edge_path;
pub use options::{Background, FlowOptions};
pub use state::FlowState;
pub use types::*;
pub use view::Flow;
pub use viewer::FlowViewer;
