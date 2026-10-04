# Operow

A ECU network simulator for automotive CAN bus design and testing, written in Rust with a native egui UI and an in-process deterministic simulation engine.

## Features

- **CAN Bus Arbitration**: Bit-accurate simulation of CAN 2.0 standard and extended frames
- **Deterministic Timing**: Nanosecond-precision event-driven simulation
- **Periodic ECUs**: Configure nodes with periodic message transmission patterns
- **Node-graph Editor**: ECU and CAN Bus nodes wired together on a canvas with left properties inspector (TX messages, bitrate), bottom live Trace table, and top control bar
- **Live Monitoring**: Real-time simulation control with Start/Stop/Pause, speed adjustment, bus load display, and Save/Load
- **Flexible Topology**: JSON-based network configuration with nodes, buses, and links

## Architecture

The workspace consists of three crates:

- **operow-core**: Data types and CAN frame definitions
- **operow-engine**: Discrete-event simulation engine
- **operow-app**: Native egui frontend

## Build & Run

### Linux

```bash
sudo apt-get install libxkbcommon-dev libgtk-3-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgl1-mesa-dev
cargo run -p operow-app --release
```

### macOS & Windows

```bash
cargo run -p operow-app --release
```

## Example

Use the built-in example: File > Open `examples/basic.operow.json`. This network simulates 3 ECUs (Engine, Brake, Gateway) on a single CAN bus, generating approximately 180 frames per second at 4% bus load (500 kbit/s).

## Testing

```bash
cargo test --workspace
cargo test -p operow-engine --test example_smoke
```

## Roadmap

- Ethernet/SOME-IP protocol layers
- DBC parser for automotive database files
- Rhai scripting (CAPL-like) for complex ECU behavior
- CAN FD (flexible data rate) support
- SocketCAN and Vector hardware interface
