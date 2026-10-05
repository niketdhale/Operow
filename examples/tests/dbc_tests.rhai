// Signal level checks against sample.dbc (dbc_demo.operow.json).

fn test_engine_speed_is_decoded() {
    let rpm = wait_for_signal("EngineData.EngineSpeed", ">", 1000, 500);
    expect_true(rpm >= 1000.0 && rpm <= 5000.0, "rpm within the scripted sweep");
    expect_signal("EngineData.EngineSpeed", ">", 1000);
}

fn test_coolant_temperature_scaling() {
    // Raw 127, offset -40.
    wait_for_message_on("Powertrain", 0x100, 50);
    expect_eq(signal("EngineData.CoolantTemp"), 87);
}

fn test_set_signal_round_trip() {
    set_signal("DashCmd.Brightness", 200);
    wait_for_signal("DashCmd.Brightness", "==", 200, 20);
    set_signal("DashCmd.Brightness", 17);
    let v = wait_for_signal("Powertrain::DashCmd.Brightness", "==", 17, 20);
    expect_eq(v, 17);
}

fn test_signals_of_one_message_combine() {
    set_signal("EngineData.Running", 1);
    set_signal("EngineData.CoolantTemp", 50);
    wait_for_signal("EngineData.CoolantTemp", "==", 50, 5);
    // The second set_signal kept the Running bit of the first.
    expect_signal("EngineData.Running", "==", 1);
    let f = last_frame("Powertrain", 0x100);
    expect_true(f != (), "frame seen");
}

fn test_multiplexed_signal() {
    set_signal("GatewayStatus.Voltage", 12.5);
    let v = wait_for_signal("GatewayStatus.Voltage", "==", 12.5, 20);
    expect_near(v, 12.5, 0.01);
    expect_signal("GatewayStatus.Mode", "==", 0);
}

fn test_engine_data_cycle_time() {
    expect_cycle_time(0x100, 10, 5, 500);
}

fn test_unseen_signal_is_unit() {
    expect_eq(signal("DashCmd.Brightness"), ());
}
