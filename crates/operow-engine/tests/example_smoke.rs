use operow_core::{Timestamp, Topology};
use operow_engine::Simulation;
use std::collections::HashMap;

#[test]
fn test_example_smoke() {
    // Load the example topology
    let json_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/basic.operow.json"
    );
    let json_content =
        std::fs::read_to_string(json_path).expect("Failed to read example topology file");

    let topology = Topology::from_json(&json_content).expect("Failed to parse topology JSON");

    // Create and run the simulation
    let mut sim = Simulation::new(&topology).expect("Failed to create simulation");

    let mut events = Vec::new();
    sim.run_until(Timestamp::from_ms(10_000), &mut events);

    // Count events by frame ID
    let mut frame_counts: HashMap<u32, usize> = HashMap::new();
    for event in &events {
        *frame_counts
            .entry(event.frame.as_can().unwrap().id)
            .or_insert(0) += 1;
    }

    // Verify frame counts
    // EngineData (0x100) at 10ms period: ~1000 frames
    let count_0x100 = frame_counts.get(&0x100).copied().unwrap_or(0);
    assert!(
        (1000..=1001).contains(&count_0x100),
        "Expected 0x100 count 1000-1001, got {}",
        count_0x100
    );

    // EngineStatus (0x101) at 100ms period: ~100 frames
    let count_0x101 = frame_counts.get(&0x101).copied().unwrap_or(0);
    assert!(
        (100..=101).contains(&count_0x101),
        "Expected 0x101 count 100-101, got {}",
        count_0x101
    );

    // BrakeData (0x200) at 20ms period: ~500 frames
    let count_0x200 = frame_counts.get(&0x200).copied().unwrap_or(0);
    assert!(
        (500..=501).contains(&count_0x200),
        "Expected 0x200 count 500-501, got {}",
        count_0x200
    );

    // GwStatus (0x300) at 50ms period: ~200 frames
    let count_0x300 = frame_counts.get(&0x300).copied().unwrap_or(0);
    assert!(
        (200..=201).contains(&count_0x300),
        "Expected 0x300 count 200-201, got {}",
        count_0x300
    );

    // Verify bus load
    let stats = sim.stats();
    let bus_id = operow_core::BusId(1);

    let bus_stats = stats.get(&bus_id).expect("Expected statistics for bus 1");

    let window_ns = 10_000_000_000u64; // 10 seconds in nanoseconds
    let load = bus_stats.load(window_ns);

    assert!(
        (0.04..=0.6).contains(&load),
        "Expected bus load 0.04-0.6, got {}",
        load
    );
}

#[test]
fn test_script_example_smoke() {
    let json_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/script.operow.json"
    );
    let json_content = std::fs::read_to_string(json_path).expect("read script example");
    let topology = Topology::from_json(&json_content).expect("parse script example");
    let mut sim = Simulation::new(&topology).expect("script example compiles");

    let mut events = Vec::new();
    sim.run_until(Timestamp::from_ms(1_000), &mut events);

    let requests = events
        .iter()
        .filter(|e| e.frame.as_can().unwrap().id == 0x100)
        .count();
    let replies = events
        .iter()
        .filter(|e| e.frame.as_can().unwrap().id == 0x101)
        .count();
    assert!(requests > 0, "no 0x100 requests sent");
    assert_eq!(requests, replies, "every 0x100 should get a 0x101 reply");
}
