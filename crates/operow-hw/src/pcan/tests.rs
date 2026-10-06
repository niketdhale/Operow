//! Tests against a mock PCAN-Basic library with two channels looped together.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use super::*;

const T: Duration = Duration::from_millis(500);
const USB1: PcanHandle = 0x51;
const USB2: PcanHandle = 0x52;

#[derive(Debug, Clone, PartialEq)]
enum Call {
    SetParam(PcanHandle, u8, u32),
    Init(PcanHandle, u16),
    InitFd(PcanHandle, String),
    Uninit(PcanHandle),
}

#[derive(Default)]
struct Queues {
    classic: VecDeque<(PcanMsg, PcanTimestamp)>,
    fd: VecDeque<(PcanMsgFd, u64)>,
}

#[derive(Default)]
struct State {
    calls: Vec<Call>,
    initialized: Vec<PcanHandle>,
    fd: HashMap<PcanHandle, bool>,
    queues: HashMap<PcanHandle, Queues>,
    channels: Vec<PcanChannel>,
    attached_unsupported: bool,
    status: PcanStatus,
    write_status: Option<PcanStatus>,
    read_error: Option<PcanStatus>,
    echo_unsupported: bool,
    init_status: PcanStatus,
}

#[derive(Default)]
struct Mock {
    st: Mutex<State>,
}

fn entry(handle: PcanHandle, name: &str, features: u32, condition: u32) -> PcanChannel {
    PcanChannel {
        handle,
        device_name: name.into(),
        features,
        condition,
    }
}

impl Mock {
    fn new() -> Arc<Self> {
        let m = Arc::new(Mock::default());
        m.st.lock().unwrap().channels = vec![
            entry(
                USB1,
                "PCAN-USB FD",
                FEATURE_FD_CAPABLE,
                PCAN_CHANNEL_AVAILABLE,
            ),
            entry(USB2, "PCAN-USB", 0, PCAN_CHANNEL_AVAILABLE),
        ];
        m
    }

    fn calls(&self) -> Vec<Call> {
        self.st.lock().unwrap().calls.clone()
    }

    fn driver(self: &Arc<Self>) -> PcanDriver {
        PcanDriver::with_api(self.clone())
    }

    fn open(
        self: &Arc<Self>,
        name: &str,
        f: impl FnOnce(&mut ChannelConfig),
    ) -> Box<dyn CanChannel> {
        let mut cfg = ChannelConfig::new(format!("pcan:{name}"));
        f(&mut cfg);
        self.driver().open(&cfg).expect("open")
    }

    fn open_err(self: &Arc<Self>, name: &str, f: impl FnOnce(&mut ChannelConfig)) -> HwError {
        let mut cfg = ChannelConfig::new(format!("pcan:{name}"));
        f(&mut cfg);
        self.driver().open(&cfg).err().expect("open must fail")
    }

    /// Queues a raw message for `handle`.
    fn push(&self, handle: PcanHandle, m: PcanMsg, ts: PcanTimestamp) {
        self.st
            .lock()
            .unwrap()
            .queues
            .entry(handle)
            .or_default()
            .classic
            .push_back((m, ts));
    }

    fn push_fd(&self, handle: PcanHandle, m: PcanMsgFd, ts_us: u64) {
        self.st
            .lock()
            .unwrap()
            .queues
            .entry(handle)
            .or_default()
            .fd
            .push_back((m, ts_us));
    }
}

impl PcanApi for Mock {
    fn initialize(&self, h: PcanHandle, btr: u16) -> PcanStatus {
        let mut st = self.st.lock().unwrap();
        st.calls.push(Call::Init(h, btr));
        if st.init_status == PCAN_ERROR_OK {
            st.initialized.push(h);
            st.fd.insert(h, false);
        }
        st.init_status
    }

    fn initialize_fd(&self, h: PcanHandle, bitrate: &str) -> PcanStatus {
        let mut st = self.st.lock().unwrap();
        st.calls.push(Call::InitFd(h, bitrate.into()));
        if st.init_status == PCAN_ERROR_OK {
            st.initialized.push(h);
            st.fd.insert(h, true);
        }
        st.init_status
    }

    fn uninitialize(&self, h: PcanHandle) -> PcanStatus {
        let mut st = self.st.lock().unwrap();
        st.calls.push(Call::Uninit(h));
        st.initialized.retain(|&x| x != h);
        PCAN_ERROR_OK
    }

    fn get_status(&self, _h: PcanHandle) -> PcanStatus {
        self.st.lock().unwrap().status
    }

    fn read(&self, h: PcanHandle) -> Result<(PcanMsg, PcanTimestamp), PcanStatus> {
        let mut st = self.st.lock().unwrap();
        if let Some(e) = st.read_error {
            return Err(e);
        }
        st.queues
            .get_mut(&h)
            .and_then(|q| q.classic.pop_front())
            .ok_or(PCAN_ERROR_QRCVEMPTY)
    }

    fn read_fd(&self, h: PcanHandle) -> Result<(PcanMsgFd, u64), PcanStatus> {
        let mut st = self.st.lock().unwrap();
        if let Some(e) = st.read_error {
            return Err(e);
        }
        st.queues
            .get_mut(&h)
            .and_then(|q| q.fd.pop_front())
            .ok_or(PCAN_ERROR_QRCVEMPTY)
    }

    fn write(&self, h: PcanHandle, msg: &PcanMsg) -> PcanStatus {
        let mut st = self.st.lock().unwrap();
        if let Some(s) = st.write_status {
            return s;
        }
        let ts = PcanTimestamp {
            millis: 1,
            ..Default::default()
        };
        // Loop back to the other channel; the sender gets an echo.
        let others: Vec<PcanHandle> = st.initialized.iter().copied().filter(|&x| x != h).collect();
        for o in others {
            let fd = st.fd[&o];
            if fd {
                let mut f = PcanMsgFd {
                    id: msg.id,
                    msg_type: msg.msg_type,
                    dlc: msg.len,
                    ..Default::default()
                };
                f.data[..8].copy_from_slice(&msg.data);
                st.queues.entry(o).or_default().fd.push_back((f, 1000));
            } else {
                st.queues
                    .entry(o)
                    .or_default()
                    .classic
                    .push_back((*msg, ts));
            }
        }
        let mut echo = *msg;
        echo.msg_type |= PCAN_MESSAGE_ECHO;
        st.queues
            .entry(h)
            .or_default()
            .classic
            .push_back((echo, ts));
        PCAN_ERROR_OK
    }

    fn write_fd(&self, h: PcanHandle, msg: &PcanMsgFd) -> PcanStatus {
        let mut st = self.st.lock().unwrap();
        if let Some(s) = st.write_status {
            return s;
        }
        let others: Vec<PcanHandle> = st.initialized.iter().copied().filter(|&x| x != h).collect();
        for o in others {
            st.queues.entry(o).or_default().fd.push_back((*msg, 2500));
        }
        let mut echo = *msg;
        echo.msg_type |= PCAN_MESSAGE_ECHO;
        st.queues.entry(h).or_default().fd.push_back((echo, 2500));
        PCAN_ERROR_OK
    }

    fn set_param(&self, h: PcanHandle, param: u8, value: u32) -> PcanStatus {
        let mut st = self.st.lock().unwrap();
        st.calls.push(Call::SetParam(h, param, value));
        if param == PCAN_ALLOW_ECHO_FRAMES && st.echo_unsupported {
            return PCAN_ERROR_ILLPARAMTYPE;
        }
        PCAN_ERROR_OK
    }

    fn attached_channels(&self) -> Result<Vec<PcanChannel>, PcanStatus> {
        let st = self.st.lock().unwrap();
        if st.attached_unsupported {
            Err(PCAN_ERROR_ILLPARAMTYPE)
        } else {
            Ok(st.channels.clone())
        }
    }

    fn channel_condition(&self, h: PcanHandle) -> Result<u32, PcanStatus> {
        let st = self.st.lock().unwrap();
        Ok(st
            .channels
            .iter()
            .find(|c| c.handle == h)
            .map_or(PCAN_CHANNEL_UNAVAILABLE, |c| c.condition))
    }

    fn channel_features(&self, h: PcanHandle) -> Result<u32, PcanStatus> {
        let st = self.st.lock().unwrap();
        st.channels
            .iter()
            .find(|c| c.handle == h)
            .map(|c| c.features)
            .ok_or(PCAN_ERROR_ILLHW)
    }

    fn hardware_name(&self, h: PcanHandle) -> Result<String, PcanStatus> {
        let st = self.st.lock().unwrap();
        st.channels
            .iter()
            .find(|c| c.handle == h)
            .map(|c| c.device_name.clone())
            .ok_or(PCAN_ERROR_ILLHW)
    }

    fn error_text(&self, s: PcanStatus) -> String {
        format!("mock error 0x{s:X}")
    }
}

const PCAN_ERROR_ILLHW: PcanStatus = 0x01400;

fn frame(id: u32, ext: bool, data: &[u8]) -> CanFrame {
    CanFrame::new(id, ext, data).unwrap()
}

#[test]
fn ffi_constants_match_the_header() {
    assert_eq!(PCAN_USBBUS1, 0x51);
    assert_eq!(PCAN_BAUD_500K, 0x001C);
    assert_eq!(PCAN_ERROR_BUSPASSIVE, 0x40000);
    assert_eq!(PCAN_ERROR_ANYBUSERR, 0x40000 | 0x4 | 0x8 | 0x10);
    assert_eq!(
        PCAN_MESSAGE_ECHO | PCAN_MESSAGE_ERRFRAME | PCAN_MESSAGE_STATUS,
        0xE0
    );
}

#[test]
fn handle_names_roundtrip() {
    for sel in [
        "PCAN_USBBUS1",
        "usbbus1",
        "USB1",
        "usb1",
        "0x51",
        "0X51",
        " pcan_usb1 ",
    ] {
        assert_eq!(parse_handle(sel), Some(0x51), "{sel}");
    }
    assert_eq!(parse_handle("usb9"), Some(0x509));
    assert_eq!(parse_handle("PCAN_PCIBUS2"), Some(0x42));
    assert_eq!(parse_handle("lan3"), Some(0x803));
    assert_eq!(parse_handle("usb17"), None);
    assert_eq!(parse_handle("usb0"), None);
    assert_eq!(parse_handle("pcc3"), None);
    assert_eq!(parse_handle("foo"), None);
    assert_eq!(parse_handle("0x0"), None);
    assert_eq!(handle_name(0x51), "PCAN_USBBUS1");
    assert_eq!(handle_name(0x510), "PCAN_USBBUS16");
    assert_eq!(handle_name(0x7777), "PCAN_0x7777");
    for fam in 0..FAMILIES.len() {
        for n in 1..=16 {
            if let Some(h) = family_handle(fam, n) {
                assert_eq!(parse_handle(&handle_name(h)), Some(h));
            }
        }
    }
}

#[test]
fn listing_uses_attached_channels() {
    let m = Mock::new();
    let list = m.driver().list_channels();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].name, "pcan:PCAN_USBBUS1");
    assert_eq!(list[0].description, "PCAN-USB FD (USBBUS1)");
    assert!(list[0].fd_capable && !list[0].is_virtual);
    assert!(!list[1].fd_capable);
    assert_eq!(list[1].driver, "pcan");
}

#[test]
fn listing_falls_back_to_probing_conditions() {
    let m = Mock::new();
    {
        let mut st = m.st.lock().unwrap();
        st.attached_unsupported = true;
        st.channels
            .push(entry(0x53, "", 0, PCAN_CHANNEL_UNAVAILABLE));
        st.channels
            .push(entry(0x54, "PCAN-USB Pro", 0, PCAN_CHANNEL_OCCUPIED));
    }
    let names: Vec<_> = m
        .driver()
        .list_channels()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(
        names,
        [
            "pcan:PCAN_USBBUS1",
            "pcan:PCAN_USBBUS2",
            "pcan:PCAN_USBBUS4"
        ]
    );
}

#[test]
fn missing_library_is_unavailable_with_message() {
    let d = PcanDriver {
        api: Err(missing_library("PCANBasic.dll", "not found")),
    };
    let msg = d.available().unwrap_err();
    assert!(msg.contains("PCANBasic.dll") && msg.contains("Install PEAK PCAN-Basic"));
    assert!(msg.contains("peak-linux-driver"));
    assert!(d.list_channels().is_empty());
    let r = d.open(&ChannelConfig::new("pcan:usb1"));
    assert!(matches!(r, Err(HwError::NotAvailable(_))));
    assert!(Mock::new().driver().available().is_ok());
}

#[cfg(not(any(windows, target_os = "linux")))]
#[test]
fn unsupported_os_reports_unavailable() {
    assert!(PcanDriver::new().available().is_err());
}

#[test]
fn open_classic_initializes_with_baud_register() {
    let m = Mock::new();
    let mut c = ChannelConfig::new("pcan:usb1");
    c.bitrate = 250_000;
    let _ch = m.driver().open(&c).unwrap();
    assert!(m.calls().contains(&Call::Init(USB1, PCAN_BAUD_250K)));
    assert!(
        m.calls()
            .contains(&Call::SetParam(USB1, PCAN_ALLOW_ERROR_FRAMES, 1))
    );
}

#[test]
fn baud_registers() {
    for (rate, reg) in [
        (1_000_000, 0x0014),
        (800_000, 0x0016),
        (500_000, 0x001C),
        (250_000, 0x011C),
        (125_000, 0x031C),
        (100_000, 0x432F),
        (83_333, 0x852B),
        (50_000, 0x472F),
        (33_333, 0x8B2F),
        (20_000, 0x532F),
        (10_000, 0x672F),
        (5_000, 0x7F7F),
    ] {
        assert_eq!(baud_register(rate), Some(reg), "{rate}");
    }
    assert_eq!(baud_register(300_000), None);
}

#[test]
fn unsupported_bitrate_fails_before_init() {
    let m = Mock::new();
    let e = m.open_err("usb1", |c| c.bitrate = 300_000);
    let HwError::Open { msg, .. } = &e else {
        panic!("{e:?}")
    };
    assert!(msg.contains("unsupported bitrate 300000"), "{msg}");
    assert!(m.calls().is_empty());
}

#[test]
fn open_fd_passes_bit_timing_string() {
    let m = Mock::new();
    let _ch = m.open("PCAN_USBBUS1", |c| {
        c.fd = true;
        c.bitrate = 500_000;
        c.data_bitrate = 2_000_000;
    });
    assert!(
        m.calls().contains(&Call::InitFd(
            USB1,
            "f_clock_mhz=80, nom_brp=8, nom_tseg1=15, nom_tseg2=4, nom_sjw=4, data_brp=2, \
         data_tseg1=15, data_tseg2=4, data_sjw=4"
                .into()
        ))
    );
}

#[test]
fn fd_timing_has_integer_prescaler_and_80_percent_sample_point() {
    for rate in [
        125_000, 250_000, 500_000, 800_000, 1_000_000, 2_000_000, 4_000_000, 5_000_000, 8_000_000,
    ] {
        let t = fd_timing(rate).unwrap_or_else(|| panic!("{rate}"));
        let total = 1 + t.tseg1 + t.tseg2;
        assert_eq!(
            u64::from(t.brp) * u64::from(total) * u64::from(rate),
            CAN_CLOCK_HZ
        );
        let sample = f64::from(1 + t.tseg1) / f64::from(total);
        assert!((0.75..=0.875).contains(&sample), "{rate}: {sample}");
        assert!(t.sjw >= 1 && t.sjw <= t.tseg2);
    }
    assert_eq!(fd_timing(0), None);
    assert_eq!(fd_timing(79_999_999), None);
    assert!(fd_bitrate_string(500_000, 7).is_err());
    assert!(fd_bitrate_string(7, 2_000_000).is_err());
}

#[test]
fn fd_on_classic_only_channel_is_refused() {
    let m = Mock::new();
    let e = m.open_err("usb2", |c| c.fd = true);
    assert!(matches!(&e, HwError::Open { msg, .. } if msg.contains("does not support CAN FD")));
    assert!(m.calls().is_empty());
}

#[test]
fn listen_only_is_set_before_initialize() {
    let m = Mock::new();
    let mut ch = m.open("usb1", |c| c.listen_only = true);
    let calls = m.calls();
    let set = calls
        .iter()
        .position(|c| *c == Call::SetParam(USB1, PCAN_LISTEN_ONLY, 1))
        .expect("listen only set");
    let init = calls
        .iter()
        .position(|c| matches!(c, Call::Init(..)))
        .unwrap();
    assert!(set < init);
    assert_eq!(ch.send(&frame(1, false, &[])), Err(HwError::ListenOnly));
}

#[test]
fn bad_channel_and_init_failure() {
    let m = Mock::new();
    let e = m.open_err("nope", |_| {});
    assert!(matches!(&e, HwError::Open { msg, .. } if msg.contains("unknown PCAN channel")));
    m.st.lock().unwrap().init_status = PCAN_ERROR_HWINUSE;
    let e = m.open_err("usb1", |_| {});
    assert!(
        matches!(&e, HwError::Open { msg, .. } if msg.contains("CAN_Initialize failed: mock error 0x400"))
    );
    assert!(matches!(
        m.driver().open(&ChannelConfig::new("vector:1")),
        Err(HwError::UnknownDriver(_))
    ));
}

#[test]
fn classic_loopback_with_extended_id_and_dlc() {
    let m = Mock::new();
    let mut a = m.open("usb1", |_| {});
    let mut b = m.open("usb2", |_| {});
    for f in [
        frame(0x123, false, &[1, 2, 3]),
        frame(0x1ABC_DEF0, true, &[9; 8]),
        frame(0x7FF, false, &[]),
    ] {
        a.send(&f).unwrap();
        let rx = b.recv(T).unwrap().expect("frame");
        assert_eq!(rx.frame, f);
        assert!(!rx.is_echo && rx.error.is_none());
        assert_eq!(rx.timestamp_ns, 1_000_000);
    }
    // The echo is not delivered without receive_own.
    assert!(a.recv(Duration::from_millis(5)).unwrap().is_none());
}

#[test]
fn fd_loopback_keeps_flags_and_length() {
    let m = Mock::new();
    let mut a = m.open("usb1", |c| c.fd = true);
    let mut b = m.open("usb2", |_| {});
    // The mock hands the FD frame to channel 2 as a classic channel's queue,
    // so use two FD-capable channels instead.
    drop(b);
    m.st.lock().unwrap().channels[1].features = FEATURE_FD_CAPABLE;
    b = m.open("usb2", |c| c.fd = true);
    let data: Vec<u8> = (0..24).collect();
    let f = CanFrame::new_fd(0x1FFF_0000, true, true, &data).unwrap();
    a.send(&f).unwrap();
    let rx = b.recv(T).unwrap().expect("frame");
    assert_eq!(rx.frame, f);
    assert!(rx.frame.fd && rx.frame.brs && rx.frame.extended);
    assert_eq!(rx.timestamp_ns, 2_500_000);
    // A classic frame on an FD channel stays classic.
    let c = frame(0x10, false, &[1, 2]);
    a.send(&c).unwrap();
    let rx = b.recv(T).unwrap().unwrap();
    assert_eq!(rx.frame, c);
    assert!(!rx.frame.fd);
}

#[test]
fn fd_frame_on_classic_channel_is_unsupported() {
    let m = Mock::new();
    let mut a = m.open("usb1", |_| {});
    let f = CanFrame::new_fd(1, false, false, &[0; 12]).unwrap();
    assert!(matches!(a.send(&f), Err(HwError::Unsupported(_))));
}

#[test]
fn tx_message_layout() {
    let f = CanFrame::new_fd(0x1234, true, true, &[1; 12]).unwrap();
    assert_eq!(
        tx_type(&f),
        PCAN_MESSAGE_EXTENDED | PCAN_MESSAGE_FD | PCAN_MESSAGE_BRS
    );
    assert_eq!(tx_id(&f), 0x1234);
    assert_eq!(f.dlc_code(), 9);
    assert_eq!(tx_type(&frame(1, false, &[])), PCAN_MESSAGE_STANDARD);
}

#[test]
fn fd_dlc_conversion_on_receive() {
    let m = Mock::new();
    let mut a = m.open("usb1", |c| c.fd = true);
    for (dlc, len) in [(8u8, 8usize), (9, 12), (13, 32), (15, 64)] {
        let mut msg = PcanMsgFd {
            id: 5,
            msg_type: PCAN_MESSAGE_FD,
            dlc,
            ..Default::default()
        };
        msg.data[..len].fill(0xAB);
        m.push_fd(USB1, msg, 10);
        let rx = a.recv(T).unwrap().unwrap();
        assert_eq!(rx.frame.dlc as usize, len);
        assert!(rx.frame.payload().iter().all(|&b| b == 0xAB));
        assert_eq!(rx.timestamp_ns, 10_000);
    }
}

#[test]
fn timestamp_conversion() {
    let ts = PcanTimestamp {
        millis: 2,
        millis_overflow: 0,
        micros: 500,
    };
    assert_eq!(timestamp_ns(&ts), 2_500_000);
    let ts = PcanTimestamp {
        millis: 0,
        millis_overflow: 1,
        micros: 1,
    };
    assert_eq!(timestamp_ns(&ts), (4_294_967_296_000 + 1) * 1000);
}

#[test]
fn error_frames_status_messages_and_remote_frames() {
    let m = Mock::new();
    let mut a = m.open("usb1", |_| {});
    let ts = PcanTimestamp {
        millis: 3,
        ..Default::default()
    };
    let raw = |msg_type| PcanMsg {
        id: 0x20,
        msg_type,
        len: 1,
        data: [1; 8],
    };
    m.push(USB1, raw(PCAN_MESSAGE_STATUS), ts);
    m.push(USB1, raw(PCAN_MESSAGE_RTR), ts);
    m.push(USB1, raw(PCAN_MESSAGE_ERRFRAME), ts);
    let rx = a.recv(T).unwrap().expect("error frame");
    assert_eq!(rx.error, Some(CanErrorKind::Bit));
    assert_eq!(rx.timestamp_ns, 3_000_000);
    assert!(a.recv(Duration::from_millis(5)).unwrap().is_none());
}

#[test]
fn bus_state_mapping() {
    let m = Mock::new();
    let mut a = m.open("usb1", |_| {});
    for (status, state) in [
        (PCAN_ERROR_OK, NodeErrorState::ErrorActive),
        (PCAN_ERROR_BUSLIGHT, NodeErrorState::ErrorActive),
        (PCAN_ERROR_BUSHEAVY, NodeErrorState::ErrorActive),
        (PCAN_ERROR_QRCVEMPTY, NodeErrorState::ErrorActive),
        (PCAN_ERROR_BUSPASSIVE, NodeErrorState::ErrorPassive),
        (PCAN_ERROR_BUSOFF, NodeErrorState::BusOff),
        (
            PCAN_ERROR_BUSOFF | PCAN_ERROR_BUSPASSIVE,
            NodeErrorState::BusOff,
        ),
    ] {
        m.st.lock().unwrap().status = status;
        assert_eq!(a.bus_state().unwrap().state, state, "{status:#x}");
    }
    m.st.lock().unwrap().status = PCAN_ERROR_INITIALIZE;
    assert!(matches!(a.bus_state(), Err(HwError::Io(_))));
}

#[test]
fn read_status_handling() {
    let m = Mock::new();
    let mut a = m.open("usb1", |_| {});
    // Bus status or an invalid frame reported by a read is not an error.
    for s in [PCAN_ERROR_BUSLIGHT, PCAN_ERROR_ILLDATA] {
        m.st.lock().unwrap().read_error = Some(s);
        assert!(
            a.recv(Duration::from_millis(3)).unwrap().is_none(),
            "{s:#x}"
        );
    }
    m.st.lock().unwrap().read_error = Some(PCAN_ERROR_ILLOPERATION);
    assert!(matches!(
        a.recv(Duration::from_millis(3)),
        Err(HwError::Io(_))
    ));
}

#[test]
fn tx_queue_full_and_errors() {
    let m = Mock::new();
    let mut a = m.open("usb1", |_| {});
    let f = frame(1, false, &[1]);
    m.st.lock().unwrap().write_status = Some(PCAN_ERROR_QXMTFULL);
    assert_eq!(a.send(&f), Err(HwError::TxQueueFull));
    m.st.lock().unwrap().write_status = Some(PCAN_ERROR_XMTFULL);
    assert_eq!(a.send(&f), Err(HwError::TxQueueFull));
    m.st.lock().unwrap().write_status = Some(PCAN_ERROR_ILLOPERATION);
    assert!(matches!(a.send(&f), Err(HwError::Io(_))));
}

#[test]
fn receive_own_enables_echo_frames() {
    let m = Mock::new();
    let mut a = m.open("usb1", |c| c.receive_own = true);
    assert!(
        m.calls()
            .contains(&Call::SetParam(USB1, PCAN_ALLOW_ECHO_FRAMES, 1))
    );
    let f = frame(0x42, false, &[7]);
    a.send(&f).unwrap();
    let rx = a.recv(T).unwrap().expect("echo");
    assert!(rx.is_echo);
    assert_eq!(rx.frame, f);
}

#[test]
fn receive_own_unsupported_is_reported_and_channel_released() {
    let m = Mock::new();
    m.st.lock().unwrap().echo_unsupported = true;
    let e = m.open_err("usb1", |c| c.receive_own = true);
    assert!(matches!(e, HwError::Unsupported(_)));
    assert!(m.st.lock().unwrap().initialized.is_empty());
}

#[test]
fn close_is_idempotent_and_releases_the_channel() {
    let m = Mock::new();
    let mut a = m.open("usb1", |_| {});
    a.close();
    a.close();
    drop(a);
    let uninits = m
        .calls()
        .iter()
        .filter(|c| matches!(c, Call::Uninit(_)))
        .count();
    assert_eq!(uninits, 1);
    let mut b = m.open("usb1", |_| {});
    b.close();
    assert_eq!(b.send(&frame(1, false, &[])), Err(HwError::Closed));
    assert_eq!(b.recv(Duration::ZERO), Err(HwError::Closed));
    assert_eq!(b.bus_state(), Err(HwError::Closed));
}

#[test]
fn drop_uninitializes() {
    let m = Mock::new();
    drop(m.open("usb1", |_| {}));
    assert!(m.st.lock().unwrap().initialized.is_empty());
}

#[test]
fn registered_in_driver_list() {
    assert!(crate::driver("pcan").is_some());
}
