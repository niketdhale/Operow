//! Tests against a mock XL library with two virtual channels looped together.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::*;

#[derive(Debug, Clone, PartialEq)]
enum Call {
    OpenDriver,
    CloseDriver,
    OpenPort {
        access: XlAccess,
        permission: XlAccess,
        version: u32,
    },
    Bitrate {
        access: XlAccess,
        bitrate: u32,
    },
    FdConf(XlCanFdConf),
    Output {
        access: XlAccess,
        mode: u8,
    },
    Activate {
        access: XlAccess,
        flags: u32,
    },
    Deactivate,
    ClosePort,
    Transmit(XlEvent),
    TransmitFd(Box<XlCanTxEvent>),
}

struct Port {
    access: XlAccess,
    fd: bool,
    classic: VecDeque<XlEvent>,
    fdq: VecDeque<XlCanRxEvent>,
}

#[derive(Default)]
struct State {
    calls: Vec<Call>,
    ports: Vec<Port>,
    deny_init: bool,
    chip: (u8, u8, u8),
    tx_status: Option<XlStatus>,
    open_drivers: i32,
}

#[derive(Default)]
struct Mock {
    st: Mutex<State>,
}

const CAPS_FD: u32 = XL_CHANNEL_FLAG_CANFD_ISO_SUPPORT | XL_CHANNEL_FLAG_CANFD_BOSCH_SUPPORT;

fn chan(name: &str, hw_type: u8, index: u8, caps: u32, bus: u32) -> XlChannel {
    XlChannel {
        name: name.into(),
        hw_type,
        hw_index: index,
        hw_channel: 0,
        channel_index: index,
        mask: 1 << index,
        capabilities: caps,
        bus_capabilities: bus,
        is_on_bus: false,
    }
}

const CAN_BUS: u32 = XL_BUS_COMPATIBLE_CAN | XL_BUS_ACTIVE_CAP_CAN;

impl Mock {
    fn new() -> Arc<Self> {
        Arc::new(Mock::default())
    }

    fn calls(&self) -> Vec<Call> {
        self.st.lock().unwrap().calls.clone()
    }

    fn port_index(port: XlPortHandle) -> usize {
        port as usize
    }

    /// Delivers a frame to every port that is not on channel `from`.
    #[allow(clippy::too_many_arguments)]
    fn deliver(
        st: &mut State,
        from: XlAccess,
        id: u32,
        edl: bool,
        brs: bool,
        dlc: u8,
        data: &[u8],
    ) {
        let len = dlc_to_len(dlc);
        for p in &mut st.ports {
            if p.access == from {
                continue;
            }
            if p.fd {
                let mut ev = XlCanRxEvent::zeroed();
                ev.tag = XL_CAN_EV_TAG_RX_OK;
                ev.time_stamp_sync = 1000;
                let flags = if edl { XL_CAN_RXMSG_FLAG_EDL } else { 0 }
                    | if brs { XL_CAN_RXMSG_FLAG_BRS } else { 0 };
                ev.set_rx_msg(id, flags, dlc, &data[..len]);
                p.fdq.push_back(ev);
            } else if !edl {
                let mut ev = XlEvent {
                    tag: XL_RECEIVE_MSG,
                    time_stamp: 1000,
                    ..Default::default()
                };
                ev.set_can_msg(id, 0, len as u16, &data[..len]);
                p.classic.push_back(ev);
            }
        }
    }

    /// Echo of a transmitted frame to the sending port(s).
    #[allow(clippy::too_many_arguments)]
    fn echo(st: &mut State, from: XlAccess, id: u32, edl: bool, brs: bool, dlc: u8, data: &[u8]) {
        let len = dlc_to_len(dlc);
        for p in &mut st.ports {
            if p.access != from {
                continue;
            }
            if p.fd {
                let mut ev = XlCanRxEvent::zeroed();
                ev.tag = XL_CAN_EV_TAG_TX_OK;
                let flags = if edl { XL_CAN_RXMSG_FLAG_EDL } else { 0 }
                    | if brs { XL_CAN_RXMSG_FLAG_BRS } else { 0 };
                ev.set_rx_msg(id, flags, dlc, &data[..len]);
                p.fdq.push_back(ev);
            } else {
                let mut ev = XlEvent {
                    tag: XL_RECEIVE_MSG,
                    ..Default::default()
                };
                ev.set_can_msg(id, XL_CAN_MSG_FLAG_TX_COMPLETED, len as u16, &data[..len]);
                p.classic.push_back(ev);
            }
        }
    }
}

impl XlApi for Mock {
    fn open_driver(&self) -> Result<(), XlStatus> {
        let mut st = self.st.lock().unwrap();
        st.open_drivers += 1;
        st.calls.push(Call::OpenDriver);
        Ok(())
    }

    fn close_driver(&self) {
        let mut st = self.st.lock().unwrap();
        st.open_drivers -= 1;
        st.calls.push(Call::CloseDriver);
    }

    fn channels(&self) -> Result<Vec<XlChannel>, XlStatus> {
        Ok(vec![
            chan("Virtual Channel 1", XL_HWTYPE_VIRTUAL, 0, CAPS_FD, CAN_BUS),
            chan("Virtual Channel 2", XL_HWTYPE_VIRTUAL, 1, CAPS_FD, CAN_BUS),
            chan("VN1610 Channel 1", 55, 2, 0, CAN_BUS),
            chan("VN1610 LIN 1", 55, 3, 0, 0x2),
        ])
    }

    fn open_port(
        &self,
        access: XlAccess,
        permission: XlAccess,
        _rx_queue: u32,
        version: u32,
    ) -> Result<(XlPortHandle, XlAccess), XlStatus> {
        let mut st = self.st.lock().unwrap();
        st.calls.push(Call::OpenPort {
            access,
            permission,
            version,
        });
        st.ports.push(Port {
            access,
            fd: version == XL_INTERFACE_VERSION_V4,
            classic: VecDeque::new(),
            fdq: VecDeque::new(),
        });
        let granted = if st.deny_init { 0 } else { permission };
        Ok((st.ports.len() as i32 - 1, granted))
    }

    fn set_bitrate(
        &self,
        _p: XlPortHandle,
        access: XlAccess,
        bitrate: u32,
    ) -> Result<(), XlStatus> {
        self.st
            .lock()
            .unwrap()
            .calls
            .push(Call::Bitrate { access, bitrate });
        Ok(())
    }

    fn fd_set_configuration(
        &self,
        _p: XlPortHandle,
        _a: XlAccess,
        conf: &XlCanFdConf,
    ) -> Result<(), XlStatus> {
        self.st.lock().unwrap().calls.push(Call::FdConf(*conf));
        Ok(())
    }

    fn set_output(&self, _p: XlPortHandle, access: XlAccess, mode: u8) -> Result<(), XlStatus> {
        self.st
            .lock()
            .unwrap()
            .calls
            .push(Call::Output { access, mode });
        Ok(())
    }

    fn activate(&self, _p: XlPortHandle, access: XlAccess, flags: u32) -> Result<(), XlStatus> {
        self.st
            .lock()
            .unwrap()
            .calls
            .push(Call::Activate { access, flags });
        Ok(())
    }

    fn deactivate(&self, _p: XlPortHandle, _a: XlAccess) -> Result<(), XlStatus> {
        self.st.lock().unwrap().calls.push(Call::Deactivate);
        Ok(())
    }

    fn close_port(&self, _p: XlPortHandle) {
        self.st.lock().unwrap().calls.push(Call::ClosePort);
    }

    fn transmit(&self, _p: XlPortHandle, access: XlAccess, ev: &XlEvent) -> Result<u32, XlStatus> {
        let mut st = self.st.lock().unwrap();
        st.calls.push(Call::Transmit(*ev));
        if let Some(s) = st.tx_status {
            return Err(s);
        }
        assert_eq!(ev.tag, XL_TRANSMIT_MSG);
        let (id, _flags, len, data) = ev.can_msg();
        Self::deliver(&mut st, access, id, false, false, len as u8, &data);
        Self::echo(&mut st, access, id, false, false, len as u8, &data);
        Ok(1)
    }

    fn transmit_fd(
        &self,
        _p: XlPortHandle,
        access: XlAccess,
        ev: &XlCanTxEvent,
    ) -> Result<u32, XlStatus> {
        let mut st = self.st.lock().unwrap();
        st.calls.push(Call::TransmitFd(Box::new(*ev)));
        if let Some(s) = st.tx_status {
            return Err(s);
        }
        assert_eq!(ev.tag, XL_CAN_EV_TAG_TX_MSG);
        let edl = ev.msg_flags & XL_CAN_TXMSG_FLAG_EDL != 0;
        let brs = ev.msg_flags & XL_CAN_TXMSG_FLAG_BRS != 0;
        Self::deliver(&mut st, access, ev.can_id, edl, brs, ev.dlc, &ev.data);
        Self::echo(&mut st, access, ev.can_id, edl, brs, ev.dlc, &ev.data);
        Ok(1)
    }

    fn receive(&self, port: XlPortHandle) -> Result<Option<XlEvent>, XlStatus> {
        let mut st = self.st.lock().unwrap();
        let i = Self::port_index(port);
        Ok(st.ports[i].classic.pop_front())
    }

    fn receive_fd(&self, port: XlPortHandle) -> Result<Option<XlCanRxEvent>, XlStatus> {
        let mut st = self.st.lock().unwrap();
        let i = Self::port_index(port);
        Ok(st.ports[i].fdq.pop_front())
    }

    fn request_chip_state(&self, port: XlPortHandle, _a: XlAccess) -> Result<(), XlStatus> {
        let mut st = self.st.lock().unwrap();
        let i = Self::port_index(port);
        let (bus, tx, rx) = st.chip;
        if st.ports[i].fd {
            let mut ev = XlCanRxEvent::zeroed();
            ev.tag = XL_CAN_EV_TAG_CHIP_STATE;
            ev.tag_data[..3].copy_from_slice(&[bus, tx, rx]);
            st.ports[i].fdq.push_back(ev);
        } else {
            let mut ev = XlEvent {
                tag: XL_CHIP_STATE,
                ..Default::default()
            };
            ev.tag_data[..3].copy_from_slice(&[bus, tx, rx]);
            st.ports[i].classic.push_back(ev);
        }
        Ok(())
    }

    fn error_string(&self, status: XlStatus) -> String {
        format!("mock status {status}")
    }
}

const T: Duration = Duration::from_millis(200);

fn driver(mock: &Arc<Mock>) -> VectorDriver {
    VectorDriver::with_api(mock.clone())
}

fn cfg(name: &str) -> ChannelConfig {
    ChannelConfig::new(format!("vector:{name}"))
}

fn open(d: &VectorDriver, c: &ChannelConfig) -> Box<dyn CanChannel> {
    d.open(c).expect("open")
}

#[test]
fn missing_dll_is_unavailable_with_help() {
    let msg = missing_library("vxlapi64.dll", "not found");
    assert!(msg.contains("vxlapi64.dll") && msg.contains("Vector Driver Setup"));
    let d = VectorDriver {
        api: Err(msg.clone()),
    };
    assert_eq!(d.available(), Err(msg.clone()));
    assert!(d.list_channels().is_empty());
    assert_eq!(
        d.open(&cfg("Virtual Channel 1")).err(),
        Some(HwError::NotAvailable(msg))
    );
    #[cfg(not(windows))]
    assert!(VectorDriver::new().available().is_err());
}

#[test]
fn lists_can_channels_and_marks_virtual() {
    let mock = Mock::new();
    let d = driver(&mock);
    assert_eq!(d.available(), Ok(()));
    let list = d.list_channels();
    let names: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "vector:Virtual Channel 1",
            "vector:Virtual Channel 2",
            "vector:VN1610 Channel 1"
        ]
    );
    assert!(list[0].is_virtual && list[0].fd_capable);
    assert!(!list[2].is_virtual && !list[2].fd_capable);
    assert!(list.iter().all(|c| c.driver == "vector"));
    assert_eq!(
        mock.st.lock().unwrap().open_drivers,
        0,
        "driver closed again"
    );
}

#[test]
fn classic_open_configures_and_loops_back() {
    let mock = Mock::new();
    let d = driver(&mock);
    let mut a = open(&d, &cfg("Virtual Channel 1"));
    let mut b = open(&d, &cfg("Virtual Channel 2"));
    let calls = mock.calls();
    assert!(calls.contains(&Call::OpenPort {
        access: 1,
        permission: 1,
        version: XL_INTERFACE_VERSION_V3
    }));
    assert!(calls.contains(&Call::Bitrate {
        access: 1,
        bitrate: 500_000
    }));
    assert!(calls.contains(&Call::Output {
        access: 1,
        mode: XL_OUTPUT_MODE_NORMAL
    }));
    assert!(calls.contains(&Call::Activate {
        access: 2,
        flags: XL_ACTIVATE_RESET_CLOCK
    }));
    assert!(a.info().is_virtual);

    let f = CanFrame::new(0x1FFF_FFF0, true, &[1, 2, 3, 4, 5]).unwrap();
    a.send(&f).unwrap();
    let rx = b.recv(T).unwrap().expect("frame");
    assert_eq!(rx.frame, f);
    assert!(!rx.is_echo && rx.error.is_none());
    assert_eq!(rx.timestamp_ns, 1000);
    // The id went out with the extended flag.
    let tx = mock
        .calls()
        .into_iter()
        .find_map(|c| match c {
            Call::Transmit(e) => Some(e),
            _ => None,
        })
        .unwrap();
    assert_eq!(tx.can_msg().0, 0x1FFF_FFF0 | XL_CAN_EXT_MSG_ID);
    // No echo unless asked for.
    assert!(a.recv(Duration::from_millis(5)).unwrap().is_none());
    // A standard frame the other way.
    let g = CanFrame::new(0x123, false, &[]).unwrap();
    b.send(&g).unwrap();
    assert_eq!(a.recv(T).unwrap().unwrap().frame, g);
}

#[test]
fn fd_open_and_loopback() {
    let mock = Mock::new();
    let d = driver(&mock);
    let mut c = cfg("Virtual Channel 1");
    c.fd = true;
    c.bitrate = 500_000;
    c.data_bitrate = 2_000_000;
    let mut a = open(&d, &c);
    let mut c2 = c.clone();
    c2.interface = "vector:Virtual Channel 2".into();
    let mut b = open(&d, &c2);
    assert!(a.info().fd_capable);
    let conf = mock
        .calls()
        .into_iter()
        .find_map(|c| match c {
            Call::FdConf(c) => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!(conf.arbitration_bit_rate, 500_000);
    assert_eq!(conf.data_bit_rate, 2_000_000);
    assert_eq!((conf.sjw_abr, conf.tseg1_abr, conf.tseg2_abr), (4, 15, 4));
    assert_eq!(conf.options, 0);
    assert!(mock.calls().iter().any(|c| matches!(
        c,
        Call::OpenPort { version, .. } if *version == XL_INTERFACE_VERSION_V4
    )));

    let fd = CanFrame::new_fd(0x001A_BCDE, true, true, &[0x5A; 48]).unwrap();
    a.send(&fd).unwrap();
    let rx = b.recv(T).unwrap().unwrap();
    assert_eq!(rx.frame, fd);
    let tx = mock
        .calls()
        .into_iter()
        .find_map(|c| match c {
            Call::TransmitFd(e) => Some(e),
            _ => None,
        })
        .unwrap();
    assert_eq!(tx.dlc, 14, "DLC code for 48 bytes");
    assert_eq!(tx.msg_flags, XL_CAN_TXMSG_FLAG_EDL | XL_CAN_TXMSG_FLAG_BRS);
    // Classic frames work on an FD port too.
    let classic = CanFrame::new(0x55, false, &[9; 8]).unwrap();
    b.send(&classic).unwrap();
    assert_eq!(a.recv(T).unwrap().unwrap().frame, classic);
}

#[test]
fn fd_frame_on_classic_channel_is_unsupported() {
    let mock = Mock::new();
    let mut a = open(&driver(&mock), &cfg("0"));
    let fd = CanFrame::new_fd(1, false, false, &[0; 12]).unwrap();
    assert!(matches!(a.send(&fd), Err(HwError::Unsupported(_))));
}

#[test]
fn echo_is_flagged_only_with_receive_own() {
    for fd in [false, true] {
        let mock = Mock::new();
        let d = driver(&mock);
        let mut c = cfg("Virtual Channel 1");
        c.fd = fd;
        c.receive_own = true;
        let mut own = open(&d, &c);
        c.receive_own = false;
        c.interface = "vector:Virtual Channel 2".into();
        let mut quiet = open(&d, &c);
        let f = CanFrame::new(0x10, false, &[1]).unwrap();
        own.send(&f).unwrap();
        let echo = own.recv(T).unwrap().expect("echo");
        assert!(echo.is_echo, "fd={fd}");
        assert_eq!(echo.frame, f);
        assert_eq!(quiet.recv(T).unwrap().unwrap().frame, f);
        quiet.send(&f).unwrap();
        // `quiet` swallowed its echo and sees nothing; `own` sees the real one.
        assert!(quiet.recv(Duration::from_millis(5)).unwrap().is_none());
        assert!(!own.recv(T).unwrap().unwrap().is_echo);
    }
}

#[test]
fn error_events_map_to_error_frames() {
    let mock = Mock::new();
    let d = driver(&mock);
    let mut classic = open(&d, &cfg("Virtual Channel 1"));
    let mut c = cfg("Virtual Channel 2");
    c.fd = true;
    let mut fd = open(&d, &c);
    {
        let mut st = mock.st.lock().unwrap();
        let mut ev = XlEvent {
            tag: XL_RECEIVE_MSG,
            time_stamp: 77,
            ..Default::default()
        };
        ev.set_can_msg(0, XL_CAN_MSG_FLAG_ERROR_FRAME, 0, &[]);
        st.ports[0].classic.push_back(ev);
        for (tag, code) in [
            (XL_CAN_EV_TAG_RX_ERROR, XL_CAN_ERRC_STUFF_ERROR),
            (XL_CAN_EV_TAG_RX_ERROR, XL_CAN_ERRC_FORM_ERROR),
            (XL_CAN_EV_TAG_RX_ERROR, XL_CAN_ERRC_CRC_ERROR),
            (XL_CAN_EV_TAG_TX_ERROR, XL_CAN_ERRC_NACK_ERROR),
            (XL_CAN_EV_TAG_RX_ERROR, XL_CAN_ERRC_OVLD_ERROR),
            (XL_CAN_EV_TAG_RX_ERROR, XL_CAN_ERRC_BIT_ERROR),
        ] {
            let mut ev = XlCanRxEvent::zeroed();
            ev.tag = tag;
            ev.tag_data[0] = code;
            st.ports[1].fdq.push_back(ev);
        }
    }
    let rx = classic.recv(T).unwrap().unwrap();
    assert_eq!(rx.error, Some(CanErrorKind::Bit));
    assert_eq!(rx.timestamp_ns, 77);
    let kinds: Vec<_> = (0..5).map(|_| fd.recv(T).unwrap().unwrap().error).collect();
    assert_eq!(
        kinds,
        [
            Some(CanErrorKind::Stuff),
            Some(CanErrorKind::Form),
            Some(CanErrorKind::Crc),
            Some(CanErrorKind::Ack),
            Some(CanErrorKind::Bit)
        ],
        "the overload frame is dropped"
    );
}

#[test]
fn chip_state_maps_to_bus_state() {
    for fd in [false, true] {
        let mock = Mock::new();
        let mut c = cfg("Virtual Channel 1");
        c.fd = fd;
        let mut a = open(&driver(&mock), &c);
        assert_eq!(a.bus_state().unwrap().state, NodeErrorState::ErrorActive);
        mock.st.lock().unwrap().chip = (XL_CHIPSTAT_ERROR_PASSIVE, 130, 5);
        let s = a.bus_state().unwrap();
        assert_eq!(s.state, NodeErrorState::ErrorPassive);
        assert_eq!((s.tec, s.rec), (130, 5));
        mock.st.lock().unwrap().chip = (XL_CHIPSTAT_BUSOFF, 255, 0);
        assert_eq!(a.bus_state().unwrap().state, NodeErrorState::BusOff);
        mock.st.lock().unwrap().chip = (XL_CHIPSTAT_ERROR_WARNING | XL_CHIPSTAT_ERROR_ACTIVE, 0, 0);
        assert_eq!(a.bus_state().unwrap().state, NodeErrorState::ErrorActive);
    }
}

#[test]
fn bus_state_keeps_frames_for_recv() {
    let mock = Mock::new();
    let d = driver(&mock);
    let mut a = open(&d, &cfg("0"));
    let mut b = open(&d, &cfg("1"));
    let f = CanFrame::new(5, false, &[5]).unwrap();
    b.send(&f).unwrap();
    a.bus_state().unwrap();
    assert_eq!(a.recv(T).unwrap().unwrap().frame, f);
}

#[test]
fn listen_only_sets_silent_mode_and_refuses_to_send() {
    let mock = Mock::new();
    let mut c = cfg("Virtual Channel 1");
    c.listen_only = true;
    let mut a = open(&driver(&mock), &c);
    assert!(mock.calls().contains(&Call::Output {
        access: 1,
        mode: XL_OUTPUT_MODE_SILENT
    }));
    let f = CanFrame::new(1, false, &[]).unwrap();
    assert_eq!(a.send(&f), Err(HwError::ListenOnly));
    assert!(
        !mock
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Transmit(_) | Call::TransmitFd(_)))
    );
}

#[test]
fn bitrate_is_passed_through() {
    let mock = Mock::new();
    let mut c = cfg("VN1610 Channel 1");
    c.bitrate = 125_000;
    open(&driver(&mock), &c);
    assert!(mock.calls().contains(&Call::Bitrate {
        access: 4,
        bitrate: 125_000
    }));
}

#[test]
fn missing_init_access_warns_but_opens() {
    let mock = Mock::new();
    mock.st.lock().unwrap().deny_init = true;
    let a = open(&driver(&mock), &cfg("Virtual Channel 1"));
    assert!(a.info().description.contains("bitrate not set"));
    let calls = mock.calls();
    assert!(!calls.iter().any(|c| matches!(c, Call::Bitrate { .. })));
    assert!(calls.iter().any(|c| matches!(c, Call::Activate { .. })));
}

#[test]
fn channel_selection() {
    let mock = Mock::new();
    let d = driver(&mock);
    // By index and by name, case-insensitively.
    assert_eq!(open(&d, &cfg("1")).info().name, "vector:1");
    open(&d, &cfg("virtual channel 2"));
    assert!(matches!(d.open(&cfg("Nope")), Err(HwError::Open { .. })));
    // The LIN channel is not a CAN channel.
    assert!(matches!(d.open(&cfg("3")), Err(HwError::Open { .. })));
    // FD on a channel without FD support.
    let mut c = cfg("VN1610 Channel 1");
    c.fd = true;
    assert!(matches!(d.open(&c), Err(HwError::Open { msg, .. }) if msg.contains("FD")));
    // Failed opens leave no driver open.
    let open_drivers = mock.st.lock().unwrap().open_drivers;
    assert_eq!(open_drivers, 0, "failed and dropped opens leave none open");
    assert!(matches!(
        d.open(&ChannelConfig::new("socketcan:can0")),
        Err(HwError::UnknownDriver(_))
    ));
}

#[test]
fn transmit_queue_full_and_close() {
    let mock = Mock::new();
    let mut a = open(&driver(&mock), &cfg("0"));
    let f = CanFrame::new(1, false, &[]).unwrap();
    mock.st.lock().unwrap().tx_status = Some(XL_ERR_QUEUE_IS_FULL);
    assert_eq!(a.send(&f), Err(HwError::TxQueueFull));
    mock.st.lock().unwrap().tx_status = Some(112);
    assert!(matches!(a.send(&f), Err(HwError::Io(m)) if m.contains("mock status 112")));
    a.close();
    a.close();
    assert_eq!(a.send(&f), Err(HwError::Closed));
    assert_eq!(a.recv(Duration::ZERO), Err(HwError::Closed));
    assert_eq!(a.bus_state(), Err(HwError::Closed));
    let calls = mock.calls();
    assert_eq!(calls.iter().filter(|c| **c == Call::ClosePort).count(), 1);
    assert_eq!(mock.st.lock().unwrap().open_drivers, 0);
}

#[test]
fn fd_timing_uses_integer_prescalers() {
    assert_eq!(fd_timing(500_000), (4, 15, 4));
    assert_eq!(fd_timing(2_000_000), (4, 15, 4));
    assert_eq!(fd_timing(5_000_000), (3, 12, 3));
    // Not representable exactly: falls back to 10 quanta.
    assert_eq!(fd_timing(83_333), (2, 7, 2));
}
