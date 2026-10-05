// Gateway forwarding and cycle times (gateway.operow.json).

fn test_forwards_engine_data() {
    // Engine sends 0x100 on Powertrain; the gateway forwards 0x100-0x1FF to Body.
    let f = wait_for_message_on("Body", 0x100, 50);
    expect_eq(f.bus, "Body");
    expect_eq(f.dlc, 8);
}

fn test_forwards_injected_frame() {
    send("Powertrain", 0x1F0, [1, 2, 3, 4]);
    let f = wait_for_message_on("Body", 0x1F0, 20);
    expect_eq(f.data, [1, 2, 3, 4]);
    expect_eq(f.sender, "Gateway");
}

fn test_range_filter_blocks_other_ids() {
    send("Powertrain", 0x600, [0xAA]);
    expect_no_message_on("Body", 0x600, 50);
}

fn test_remaps_door_status() {
    // Body's DoorStatus 0x200 comes back on Powertrain as 0x300.
    let f = wait_for_message_on("Powertrain", 0x300, 200);
    expect_eq(f.dlc, 2);
    expect_no_message_on("Powertrain", 0x200, 120);
}

fn test_engine_data_cycle_time() {
    expect_cycle_time_on("Powertrain", 0x100, 10, 5, 500);
}

fn test_door_status_cycle_time() {
    expect_cycle_time_on("Body", 0x200, 100, 5, 1000);
}

fn test_forwarding_latency() {
    let t0 = now_ms();
    send("Powertrain", 0x1F1, [7]);
    let f = wait_for_message_on("Body", 0x1F1, 20);
    expect_lt(f.time_ms - t0, 5);
}

fn test_future_feature() {
    skip("multi-hop routing is not part of this network yet");
}
