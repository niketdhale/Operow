# Operow

A CANoe-inspired ECU network simulator for automotive CAN / CAN FD design and testing, written in Rust with a native egui UI and an in-process deterministic simulation engine.

[![CI](https://github.com/niketdhale/Operow/actions/workflows/ci.yml/badge.svg)](https://github.com/niketdhale/Operow/actions/workflows/ci.yml)
[![Release (Windows)](https://github.com/niketdhale/Operow/actions/workflows/release-windows.yml/badge.svg)](https://github.com/niketdhale/Operow/actions/workflows/release-windows.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust edition 2024](https://img.shields.io/badge/rust-edition%202024-orange.svg)](Cargo.toml)
[![egui 0.33](https://img.shields.io/badge/egui-0.33-5b8def.svg)](https://github.com/emilk/egui)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey.svg)](#build--run)

![Operow overview](docs/media/overview.png)

## Feature tour

![Operow feature tour](docs/media/tour.gif)

## Features

### Network & simulation

- Multiple buses per project, with CAN 2.0 and CAN FD frames
- Gateways with routing, ID remapping and delay between buses
- Send types: Cyclic, Event, OnChange, CyclicIfActive and CyclicAndEvent
- Rhai scripting (CAPL-like) for ECU behavior, with a live log
- Interactive generators for raw and DBC-decoded frames
- Deterministic, event-driven timing

### Analysis

- Multiple trace windows, in chronological or fixed-position mode
- Per-column filters with a global on/off toggle
- Tx/Rx direction shown from the bus point of view
- DBC decoding of frames and signals
- Live graphs of DBC, user and raw signals
- Bus statistics and CSV export

### Workspace

- Docked windows and a project tree
- Window layouts saved inside the project
- Bus-line and free-form network views
- Undo / redo
- Settings, including the frame buffer size (1M frames by default)

### Databases

- DBC import, referenced by path
- User-defined signals

## Download

Windows x64 builds come from the "Release (Windows)" workflow. Pushes to `develop` publish a zip as an Actions artifact (`operow-windows-x64`, kept 30 days; open the workflow run under the Actions tab). Tags matching `v*` also create a GitHub Release with the zip attached, found under Releases. The zip contains `operow.exe`, `operow-cli.exe`, `examples/`, `README.md` and `LICENSE`.

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

## Examples

Open them with File > Open.

- `examples/basic.operow.json`: 3 ECUs (Engine, Brake, Gateway) on one 500 kbit/s CAN bus, about 180 frames/s at 4% bus load
- `examples/gateway.operow.json`: Powertrain and Body buses joined by a routing gateway
- `examples/dbc_demo.operow.json`: DBC-decoded traffic using `examples/sample.dbc`
- `examples/script.operow.json`: Requester and Responder ECUs driven by Rhai scripts

## Testing your network

Projects list Rhai test modules (`"tests": ["tests/gateway_tests.rhai"]`). Every `test_*` function runs against a fresh simulation in virtual time, so tests are fast and repeatable. Optional `module_setup`, `module_teardown`, `setup` and `teardown` functions wrap the cases.

```rhai
fn test_forwards_engine_data() {
    // Engine sends 0x100 on Powertrain; the gateway forwards it to Body.
    let f = wait_for_message_on("Body", 0x100, 50);
    expect_eq(f.dlc, 8);
}

fn test_vin() {
    let r = uds("Engine", [0x22, 0xF1, 0x90]);
    expect_eq(r[0], 0x62);
}
```

The full script API is documented in `crates/operow-test/src/lib.rs`; see `examples/tests/` for more.

The headless `operow-cli` runs them without the GUI:

```bash
cargo run -p operow-cli -- test examples/gateway.operow.json --report out/ --junit out/junit.xml
operow-cli test project.operow.json --filter 'test_cycle*' --jobs 4 --fail-fast
operow-cli run examples/gateway.operow.json --duration 10s --log trace.blf
operow-cli replay trace.blf --export csv --out trace.csv --dbc examples/sample.dbc
operow-cli convert trace.blf trace.asc
operow-cli diag examples/diag_demo.operow.json --node Engine --req "22 F1 90"
```

`test` exit codes: `0` all passed (skipped cases are fine), `1` a test failed, `2` an error: unreadable project, script compile or runtime error, bad usage. Reports: `--report DIR` writes a self-contained `report.html` (collapsible modules, steps, failure trace extracts, light and dark theme), `--junit FILE` JUnit XML for CI systems, `--json FILE` the full result model. `--seed` makes runs with fault injection reproducible.

## Testing Operow itself

```bash
cargo test --workspace
cargo test -p operow-engine --test example_smoke
```

CI uses [cargo-nextest](https://nexte.st) (optional locally: `cargo install cargo-nextest --locked`):

```bash
cargo nextest run --workspace
cargo test --workspace --doc   # nextest skips doctests
```

## Hardware

A bus can be bound to a real CAN adapter (`"hardware": {"interface": "socketcan:can0", "listen_only": true, "receive_own": false}` on the bus). Frames from the adapter appear as received frames on that bus (sender `HW <bus>`), reach the simulated nodes and are forwarded by gateways to other buses, including other hardware buses. Frames simulated nodes send onto the bus go to the adapter.

- **Real-time only.** With any hardware bus the engine runs at speed 1.0 (other speeds are refused) and virtual time follows the wall clock. Simulated buses keep their simulated timing, paced to real time. `operow-cli run` does the same.
- **Listen-only by default.** Nothing is transmitted unless `listen_only` is `false`; dropped frames are counted per bus. Opening failures are reported and the measurement does not start.
- **SocketCAN (Linux).** `socketcan:<interface>`; classic and FD. Bitrate and the controller's listen-only mode are configured outside Operow:
  ```bash
  sudo ip link set can0 type can bitrate 500000 dbitrate 2000000 fd on listen-only on
  sudo ip link set up can0
  ```
  For testing without hardware: `sudo modprobe vcan; sudo ip link add dev vcan0 type vcan; sudo ip link set up vcan0`, then `cargo test -p operow-hw -- --ignored`.
- **`virtual:<name>`**: in-process loopback, all platforms (tests, demos).
- **`udp:<name>`**: connects Operow processes on one machine over loopback multicast (no admin rights; works on Windows). The datagram format is documented in `crates/operow-hw/src/udp.rs`.
- **Vector XL (Windows).** `vector:<channel name>` (e.g. `vector:VN1630 Channel 1`, `vector:Virtual Channel 1`) or `vector:<channelIndex>`; the names are listed in the interface chooser. The XL Driver Library (`vxlapi64.dll`) is loaded at run time, so Operow starts without it and the driver just reports itself unavailable. Classic and CAN FD (FD needs a channel with FD support); Operow sets the bitrate, and listen-only selects the controller's silent mode (the default). If another application holds init access to the channel, Operow still opens it but cannot set the bitrate (the channel description says so). Vector virtual channels count as virtual in the app (no LIVE warning).
  **Testing without hardware:** install *Vector Driver Setup* (it includes the XL Driver Library and the virtual CAN channels, "Virtual Channel 1/2" and so on), then bind one bus to `vector:Virtual Channel 1` and run a second tool, or a second Operow bus bound to `vector:Virtual Channel 2`, against the other end. **Status: tested only against a mock, never with the real library or hardware; please report issues.** The mock tests run everywhere: `cargo test -p operow-hw vector`.
- PCAN driver is planned.

## Architecture

The workspace consists of the following crates:

- **operow-core**: Data types and CAN / CAN FD frame definitions
- **operow-engine**: Discrete-event simulation engine
- **operow-dbc**: DBC parser and signal decoding
- **operow-isotp**: Sans-IO ISO 15765-2 (ISO-TP) transport state machine, independent of the engine
- **operow-hw**: CAN hardware abstraction: SocketCAN, virtual and UDP drivers
- **operow-log**: ASC and BLF log readers and writers
- **operow-uds**: UDS (ISO 14229) message encoding and decoding
- **operow-test**: Rhai test runner, result model and HTML / JUnit / JSON reports
- **operow-cli**: Headless command line: `test`, `run`, `replay`, `convert`, `diag`
- **operow-app**: Native egui frontend. The node-graph canvas comes from [`egui-flow`](https://github.com/niketdhale/egui-flow), a separate React Flow-style widget crate pulled in as a git dependency

## Roadmap

- Logging & replay
- Error simulation & node controls
- ISO-TP / UDS
- Test sequences & headless CLI
- Hardware: SocketCAN / PCAN / Vector
- Ethernet / SOME-IP

## License

MIT, see [LICENSE](LICENSE).
