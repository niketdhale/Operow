# Operow: handoff notes

## Project
- **Operow** is a CANoe-style vehicle network simulator written in Rust. Repo: `niketdhale/Operow` (public).
- Workspace crates: operow-core, -engine, -dbc, -log, -isotp, -uds, -project, -test, -cli, -hw, -app (egui GUI).
- The GUI graph is built on **egui-flow** (`niketdhale/egui-flow`), pinned at v0.2.0, rev `ce28d5c`.

## Working rules (from the user)
- **Only work on `develop`.** Never change `main` and never open PRs against `main`.
- Branch: `claude/vehicle-network-simulator-plan-qxee9u`. Recreate it from `develop` after each merge, and open PRs into `develop`.
- Before any PR, run: `cargo fmt --check`, `cargo clippy --workspace -- -D warnings` and `cargo test --workspace`, and take screenshots of UI changes.
- Agents write the code; the lead session verifies it.
- The user tests on Windows now and plans to test on Linux. They have no PCAN or Vector hardware yet.

## Done and merged into develop
| PR | Content |
|---|---|
| #5–#10 | Phases 1–5: core and engine, DBC, logging, ISO-TP and UDS, projects and tests, CLI, hardware (Vector XL and PCAN FFI, checked against python-can), CI optimisation |
| #11 | README and demo GIF updated for the latest features |
| #12 | egui-flow 0.2.0: domains (map to egui-flow groups), per-link wire styles (`Topology.wires: Vec<WireOverride{node,bus,style}>`, `Topology.wire_default`), themes, hover-only handles, animated auto-layout, signal pulses |

Key app files:
- `crates/operow-app/src/`: `graph.rs`, `network_view.rs`, `wire_ui.rs`, `windows.rs`, `theme.rs`, `inspector.rs`, `project_tree.rs`
- `crates/operow-core/src/topology.rs`

## Open items for the user
- Delete the old branches `claude/rust-gui-react-flow-jg49yt` and `claude/vibrant-pasteur-if57uq` on GitHub (a session cannot do this).
- Push the egui-flow `v0.2.0` tag (the push was blocked from the session).
- Test by hand on Windows with the UDP bus and Vector virtual channels.

## Next step (proposed, not started)
Plan: release egui-flow v0.3.0 with items 1–4 below, then open a small Operow PR into develop that adopts them. A session needs `niketdhale/egui-flow` added (via add_repo with push access) before it can change it.

1. **Connect anywhere along a node edge:** add `Handle::along(Side)` and store the attach offset on the edge. This lets wires attach anywhere on a bus bar instead of only at the spare points at its left end. Highest value.
2. **`FlowOptions::group_delete: DeleteMembers | KeepMembers`:** today the Delete key removes a domain's members, while the context menu keeps them.
3. **`can_join_group(node, group)` hook:** refuse bus bars dropped onto a domain, so Operow no longer has to revert those drops by hand and filter the events out of undo.
4. **Label collision avoidance:** a pulse's hover label currently overlaps the edge (route) label. Also add `PulseStyle::label_mode: OnHover | Always`.
5. (Later) Obstacle-aware routing for step edges, so gateway wires don't cross bus bars.
6. (Operow side) Make wire style edits undoable: keep the style in the edge data and call `Editor::commit()`.

Other notes:
- Pulses on wires inside a collapsed group are currently dropped. A badge or glow on the group could show them instead.
- Decide whether the `--light` flag should persist the theme.
