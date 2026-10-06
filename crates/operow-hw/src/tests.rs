use std::time::Duration;

use operow_core::{CanErrorKind, CanFrame};

use super::*;

const T: Duration = Duration::from_millis(500);

fn cfg(name: &str) -> ChannelConfig {
    ChannelConfig::new(format!("virtual:{name}"))
}

#[test]
fn virtual_loopback_between_channels() {
    let mut a = open_channel(&cfg("t_loop")).unwrap();
    let mut b = open_channel(&cfg("t_loop")).unwrap();
    let f = CanFrame::new(0x100, false, &[1, 2, 3]).unwrap();
    a.send(&f).unwrap();
    let rx = b.recv(T).unwrap().expect("frame");
    assert_eq!(rx.frame, f);
    assert!(!rx.is_echo && rx.error.is_none());
    // The sender does not see its own frame.
    assert!(a.recv(Duration::from_millis(20)).unwrap().is_none());
}

#[test]
fn different_names_are_isolated() {
    let mut a = open_channel(&cfg("t_iso_a")).unwrap();
    let mut b = open_channel(&cfg("t_iso_b")).unwrap();
    a.send(&CanFrame::new(1, false, &[]).unwrap()).unwrap();
    assert!(b.recv(Duration::from_millis(20)).unwrap().is_none());
}

#[test]
fn receive_own_flags_echo() {
    let mut c = cfg("t_own");
    c.receive_own = true;
    let mut a = open_channel(&c).unwrap();
    let f = CanFrame::new(0x7, true, &[0xAA]).unwrap();
    a.send(&f).unwrap();
    let rx = a.recv(T).unwrap().expect("echo");
    assert!(rx.is_echo);
    assert_eq!(rx.frame, f);
}

#[test]
fn fd_frames_over_virtual() {
    let mut c = cfg("t_fd");
    c.fd = true;
    let mut a = open_channel(&c).unwrap();
    let mut b = open_channel(&c).unwrap();
    let f = CanFrame::new_fd(0x1ABC, true, true, &[0x55; 48]).unwrap();
    a.send(&f).unwrap();
    assert_eq!(b.recv(T).unwrap().unwrap().frame, f);

    // A classic channel refuses to send FD and does not receive it.
    let mut classic = open_channel(&cfg("t_fd")).unwrap();
    assert!(matches!(classic.send(&f), Err(HwError::Unsupported(_))));
    a.send(&f).unwrap();
    assert!(classic.recv(Duration::from_millis(20)).unwrap().is_none());
    assert_eq!(b.recv(T).unwrap().unwrap().frame, f);
}

#[test]
fn listen_only_refuses_to_send() {
    let mut c = cfg("t_lo");
    c.listen_only = true;
    let mut a = open_channel(&c).unwrap();
    let f = CanFrame::new(1, false, &[]).unwrap();
    assert_eq!(a.send(&f), Err(HwError::ListenOnly));
}

#[test]
fn close_is_idempotent_and_removes_channel() {
    let mut a = open_channel(&cfg("t_close")).unwrap();
    assert!(
        VirtualDriver
            .list_channels()
            .iter()
            .any(|c| c.name == "virtual:t_close")
    );
    a.close();
    a.close();
    assert_eq!(a.recv(Duration::ZERO), Err(HwError::Closed));
    assert_eq!(
        a.send(&CanFrame::new(1, false, &[]).unwrap()),
        Err(HwError::Closed)
    );
    assert!(
        !VirtualDriver
            .list_channels()
            .iter()
            .any(|c| c.name == "virtual:t_close")
    );
}

#[test]
fn injected_error_frames_arrive() {
    let mut a = open_channel(&cfg("t_err")).unwrap();
    assert_eq!(virtual_inject_error("t_err", CanErrorKind::Crc), 1);
    let rx = a.recv(T).unwrap().unwrap();
    assert_eq!(rx.error, Some(CanErrorKind::Crc));
    assert_eq!(virtual_inject_error("t_nobody", CanErrorKind::Crc), 0);
}

#[test]
fn registry() {
    let names: Vec<String> = drivers().iter().map(|d| d.name().to_string()).collect();
    assert!(names.contains(&"virtual".to_string()));
    assert!(names.contains(&"udp".to_string()));
    #[cfg(feature = "vector")]
    assert!(names.contains(&"vector".to_string()));
    #[cfg(all(target_os = "linux", feature = "socketcan"))]
    assert!(names.contains(&"socketcan".to_string()));
    assert!(driver("virtual").is_some());
    assert!(driver("nope").is_none());
    assert!(matches!(
        open_channel(&ChannelConfig::new("nope:x")),
        Err(HwError::UnknownDriver(_))
    ));
    assert!(matches!(
        open_channel(&ChannelConfig::new("can0")),
        Err(HwError::BadInterface(_))
    ));
    assert_eq!(
        split_interface("socketcan:can0").unwrap(),
        ("socketcan", "can0")
    );
}
