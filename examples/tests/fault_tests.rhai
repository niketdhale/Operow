// Error injection, bus-off handling and offline nodes (gateway.operow.json).

fn test_crc_injection_raises_error_counters() {
    inject_errors(#{ bus: "Powertrain", node: "Engine", kind: "crc", count: 3 });
    wait(5);
    expect_gt(node_tec("Engine", "Powertrain"), 0);
    expect_eq(node_state("Engine", "Powertrain"), "ErrorActive");
    // The frames still get through after the retransmissions.
    wait_for_message_on("Powertrain", 0x100, 50);
}

fn test_injected_errors_cause_bus_off_and_recovery() {
    inject_errors(#{ bus: "Powertrain", node: "Engine", id: 0x100, kind: "crc", count: 40 });
    let waited = 0;
    while node_state("Engine", "Powertrain") != "BusOff" && waited < 50 {
        wait(1);
        waited += 1;
    }
    expect_eq(node_state("Engine", "Powertrain"), "BusOff");
    // Automatic recovery after 128 * 11 bit times, then traffic resumes.
    wait_for_message_on("Powertrain", 0x100, 100);
    expect_eq(node_state("Engine", "Powertrain"), "ErrorActive");
}

fn test_forced_bus_off_silences_node() {
    force_bus_off("Engine", "Powertrain");
    expect_eq(node_state("Engine", "Powertrain"), "BusOff");
    expect_no_message_on("Powertrain", 0x100, 100);
    recover_bus_off("Engine", "Powertrain");
    wait_for_message_on("Powertrain", 0x100, 50);
}

fn test_offline_gateway_forwards_nothing() {
    wait_for_message_on("Body", 0x100, 50);
    node_offline("Gateway");
    wait(5);
    expect_no_message_on("Body", 0x100, 100);
    node_online("Gateway");
    wait_for_message_on("Body", 0x100, 50);
}

fn test_message_control_pauses_cyclic_frames() {
    msg_control("Engine", 0x100, #{ paused: true });
    wait(5);
    expect_no_message_on("Powertrain", 0x100, 100);
    msg_control("Engine", 0x100, #{});
    wait_for_message_on("Powertrain", 0x100, 50);
}
