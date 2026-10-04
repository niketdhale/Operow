use super::*;

fn rt_req(r: Request, bytes: &[u8]) {
    assert_eq!(r.encode(), bytes);
    assert_eq!(Request::decode(bytes).unwrap(), r);
}

fn rt_resp(r: Response, bytes: &[u8]) {
    assert_eq!(r.encode(), bytes);
    assert_eq!(Response::decode(bytes).unwrap(), r);
}

#[test]
fn request_round_trips() {
    rt_req(Request::DiagnosticSessionControl(3), &[0x10, 0x03]);
    rt_req(Request::EcuReset(1), &[0x11, 0x01]);
    rt_req(Request::SecurityAccessRequestSeed(1), &[0x27, 0x01]);
    rt_req(
        Request::SecurityAccessSendKey {
            level: 2,
            key: vec![0xAA, 0xBB],
        },
        &[0x27, 0x02, 0xAA, 0xBB],
    );
    rt_req(
        Request::ReadDataByIdentifier(vec![0xF190, 0xF187]),
        &[0x22, 0xF1, 0x90, 0xF1, 0x87],
    );
    rt_req(
        Request::WriteDataByIdentifier {
            did: 0x0100,
            data: vec![1, 2],
        },
        &[0x2E, 0x01, 0x00, 1, 2],
    );
    rt_req(
        Request::RoutineControl {
            sub: RoutineSub::Start,
            rid: 0xFF00,
            data: vec![],
        },
        &[0x31, 0x01, 0xFF, 0x00],
    );
    rt_req(
        Request::RoutineControl {
            sub: RoutineSub::Results,
            rid: 0x0203,
            data: vec![9],
        },
        &[0x31, 0x03, 0x02, 0x03, 9],
    );
    rt_req(
        Request::ReadDtcInformation {
            sub: 0x02,
            mask: Some(0xFF),
        },
        &[0x19, 0x02, 0xFF],
    );
    rt_req(
        Request::ReadDtcInformation {
            sub: 0x0A,
            mask: None,
        },
        &[0x19, 0x0A],
    );
    rt_req(
        Request::ClearDiagnosticInformation(0xFFFFFF),
        &[0x14, 0xFF, 0xFF, 0xFF],
    );
    rt_req(Request::TesterPresent { suppress: false }, &[0x3E, 0x00]);
    rt_req(Request::TesterPresent { suppress: true }, &[0x3E, 0x80]);
    rt_req(Request::Raw(vec![0x85, 0x01]), &[0x85, 0x01]);
}

#[test]
fn malformed_requests() {
    assert_eq!(Request::decode(&[]), Err(UdsError::Empty));
    assert!(Request::decode(&[0x22, 0xF1]).is_err());
    assert!(Request::decode(&[0x22]).is_err());
    assert!(Request::decode(&[0x10]).is_err());
    assert!(Request::decode(&[0x14, 0xFF]).is_err());
}

#[test]
fn suppress_bit() {
    let r = Request::decode(&[0x10, 0x83]).unwrap();
    assert!(r.suppress_positive_response());
    assert_eq!(r.sub_function(), Some(3));
    assert_eq!(r.encode(), vec![0x10, 0x83]);
    let r = Request::decode(&[0x3E, 0x80]).unwrap();
    assert!(r.suppress_positive_response());
    assert!(!Request::DiagnosticSessionControl(1).suppress_positive_response());
    assert_eq!(Request::ReadDataByIdentifier(vec![1]).sub_function(), None);
}

#[test]
fn response_round_trips() {
    rt_resp(
        Response::DiagnosticSessionControl {
            session: 3,
            p2_ms: 50,
            p2_star_ms: 5000,
        },
        &[0x50, 0x03, 0x00, 0x32, 0x01, 0xF4],
    );
    rt_resp(Response::EcuReset(1), &[0x51, 0x01]);
    rt_resp(
        Response::SecuritySeed {
            level: 1,
            seed: vec![1, 2, 3, 4],
        },
        &[0x67, 0x01, 1, 2, 3, 4],
    );
    rt_resp(Response::SecurityKeyAccepted { level: 2 }, &[0x67, 0x02]);
    rt_resp(
        Response::ReadDataByIdentifier {
            did: 0xF190,
            data: b"VIN".to_vec(),
        },
        &[0x62, 0xF1, 0x90, b'V', b'I', b'N'],
    );
    rt_resp(
        Response::WriteDataByIdentifier { did: 0x0100 },
        &[0x6E, 0x01, 0x00],
    );
    rt_resp(
        Response::RoutineControl {
            sub: RoutineSub::Start,
            rid: 0xFF00,
            status: vec![0],
        },
        &[0x71, 0x01, 0xFF, 0x00, 0x00],
    );
    rt_resp(
        Response::DtcCount {
            availability_mask: 0xFF,
            format: 1,
            count: 2,
        },
        &[0x59, 0x01, 0xFF, 0x01, 0x00, 0x02],
    );
    rt_resp(
        Response::DtcList {
            sub: 0x02,
            availability_mask: 0xFF,
            dtcs: vec![
                Dtc {
                    code: 0x012300,
                    status: 0x09,
                },
                Dtc {
                    code: 0xC10000,
                    status: 0x2F,
                },
            ],
        },
        &[
            0x59, 0x02, 0xFF, 0x01, 0x23, 0x00, 0x09, 0xC1, 0x00, 0x00, 0x2F,
        ],
    );
    rt_resp(Response::ClearDiagnosticInformation, &[0x54]);
    rt_resp(Response::TesterPresent, &[0x7E, 0x00]);
    rt_resp(Response::Raw(vec![0x42, 1]), &[0x42, 1]);
}

#[test]
fn nrc_parsing() {
    for v in [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x22, 0x24, 0x25, 0x31, 0x33, 0x35, 0x36, 0x37, 0x70, 0x71,
        0x72, 0x73, 0x78, 0x7E, 0x7F,
    ] {
        let n = Nrc::from_u8(v);
        assert!(!matches!(n, Nrc::Other(_)), "{v:#x}");
        assert_eq!(n.to_u8(), v);
    }
    assert_eq!(Nrc::from_u8(0x55), Nrc::Other(0x55));
    assert_eq!(Nrc::Other(0x55).to_u8(), 0x55);
    assert_eq!(
        Response::decode(&[0x7F, 0x22, 0x31]).unwrap(),
        Response::Negative {
            sid: 0x22,
            nrc: Nrc::RequestOutOfRange
        }
    );
    assert_eq!(
        Response::Negative {
            sid: 0x27,
            nrc: Nrc::InvalidKey
        }
        .encode(),
        vec![0x7F, 0x27, 0x35]
    );
    assert!(Response::decode(&[0x7F, 0x22]).is_err());
}

#[test]
fn dtc_formatting() {
    assert_eq!(dtc_to_string(0x012300), "P0123-00");
    assert_eq!(dtc_to_string(0x0123FF), "P0123-FF");
    assert_eq!(dtc_to_string(0x410000), "C0100-00");
    assert_eq!(dtc_to_string(0x813400), "B0134-00");
    assert_eq!(dtc_to_string(0xC10001), "U0100-01");
    assert_eq!(dtc_to_string(0x112300), "P1123-00");
    assert_eq!(parse_dtc("P0123"), Some(0x012300));
    assert_eq!(parse_dtc("u0100-01"), Some(0xC10001));
    assert_eq!(parse_dtc("P1123-00"), Some(0x112300));
    assert_eq!(parse_dtc("P4123"), None);
    assert_eq!(parse_dtc("P012"), None);
    assert_eq!(parse_dtc("0x012300"), Some(0x012300));
    assert_eq!(parse_dtc(""), None);
    for c in [0x012300, 0xC10001, 0x9A1B2C] {
        assert_eq!(parse_dtc(&dtc_to_string(c)), Some(c));
    }
}

#[test]
fn dtc_status_names() {
    assert_eq!(status_bit_names(0x09), vec!["testFailed", "confirmedDTC"]);
    assert!(status_bit_names(0).is_empty());
    assert_eq!(status_bit_names(0xFF).len(), 8);
}

#[test]
fn describe_messages() {
    assert_eq!(
        describe(&[0x22, 0xF1, 0x90], true),
        "ReadDataByIdentifier F190"
    );
    assert_eq!(
        describe(&[0x7F, 0x22, 0x31], false),
        "Negative ReadDataByIdentifier NRC 0x31 requestOutOfRange"
    );
    assert!(describe(&[0x3E, 0x80], true).contains("suppress"));
    assert!(describe(&[0x59, 0x02, 0xFF, 0x01, 0x23, 0x00, 0x09], false).contains("P0123-00"));
    assert!(describe(&[0x22], true).contains("malformed"));
    assert_eq!(describe(&[], false), "empty");
    assert!(describe(&[0x85, 0x01], true).contains("0x85"));
}
