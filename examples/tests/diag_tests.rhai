// UDS services of the Engine ECU (diag_demo.operow.json).

fn test_read_vin() {
    let r = uds("Engine", [0x22, 0xF1, 0x90]);
    expect_eq(r[0], 0x62);
    expect_eq(r[1], 0xF1);
    expect_eq(r[2], 0x90);
    // "WAUZZZ8K9BA123456"
    expect_eq(r.len(), 3 + 17);
    expect_eq(r[3], 0x57);
}

fn test_default_session_is_active() {
    let r = uds("Engine", [0x10, 0x01]);
    expect_eq(r[0], 0x50);
    expect_eq(r[1], 0x01);
}

fn test_security_access_unlock() {
    let r = uds("Engine", [0x10, 0x03]);
    expect_eq(r[0], 0x50);
    let seed = uds("Engine", [0x27, 0x01]);
    expect_eq(seed, [0x67, 0x01, 0x12, 0x34, 0x56, 0x78]);
    // key = seed XOR 0xA5
    let key = uds("Engine", [0x27, 0x02, 0xB7, 0x91, 0xF3, 0xDD]);
    expect_eq(key, [0x67, 0x02]);
}

fn test_security_wrong_key_is_rejected() {
    uds("Engine", [0x10, 0x03]);
    uds("Engine", [0x27, 0x01]);
    uds_expect_nrc("Engine", [0x27, 0x02, 0, 0, 0, 0], 0x35);
}

fn test_unknown_did_is_out_of_range() {
    uds_expect_nrc("Engine", [0x22, 0xFF, 0xFF], 0x31);
}

fn test_unknown_service_not_supported() {
    uds_expect_nrc("Engine", [0xAA], 0x11);
}

fn test_multi_frame_response() {
    // 40 byte data identifier: needs ISO-TP segmentation.
    let r = uds("Engine", [0x22, 0x02, 0x00]);
    expect_eq(r.len(), 3 + 40);
}
