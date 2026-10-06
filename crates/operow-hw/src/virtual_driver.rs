//! In-process loopback hub: `virtual:<name>`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use operow_core::{CanErrorKind, CanFrame};

use crate::{
    CanChannel, ChannelConfig, ChannelInfo, Driver, HwBusState, HwError, RxFrame, monotonic_ns,
    split_interface,
};

const DRIVER: &str = "virtual";

struct Endpoint {
    id: u64,
    tx: Sender<RxFrame>,
    fd: bool,
    receive_own: bool,
}

type Hubs = Mutex<HashMap<String, Vec<Endpoint>>>;

fn hubs() -> &'static Hubs {
    static HUBS: OnceLock<Hubs> = OnceLock::new();
    HUBS.get_or_init(Default::default)
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Deliver an error frame of `kind` to every channel on `virtual:<name>`,
/// like a controller reporting a bus error. Returns the number of channels
/// reached.
pub fn virtual_inject_error(name: &str, kind: CanErrorKind) -> usize {
    let mut hubs = hubs().lock().unwrap();
    let Some(eps) = hubs.get_mut(name) else {
        return 0;
    };
    let rx = RxFrame {
        frame: CanFrame::new(0, false, &[]).expect("empty frame"),
        timestamp_ns: monotonic_ns(),
        is_echo: false,
        error: Some(kind),
    };
    eps.retain(|ep| ep.tx.send(rx).is_ok());
    eps.len()
}

/// Loopback driver for tests and demos. Channels opened with the same name
/// see each other's frames; a channel does not see its own unless
/// [`ChannelConfig::receive_own`] is set (then they are flagged as echoes).
pub struct VirtualDriver;

impl Driver for VirtualDriver {
    fn name(&self) -> &str {
        DRIVER
    }

    fn available(&self) -> Result<(), String> {
        Ok(())
    }

    fn list_channels(&self) -> Vec<ChannelInfo> {
        let hubs = hubs().lock().unwrap();
        let mut names: Vec<&String> = hubs
            .iter()
            .filter(|(_, eps)| !eps.is_empty())
            .map(|(n, _)| n)
            .collect();
        names.sort();
        names
            .into_iter()
            .map(|n| ChannelInfo {
                driver: DRIVER.into(),
                name: format!("{DRIVER}:{n}"),
                description: "In-process virtual bus".into(),
                fd_capable: true,
            })
            .collect()
    }

    fn open(&self, cfg: &ChannelConfig) -> Result<Box<dyn CanChannel>, HwError> {
        let (drv, name) = split_interface(&cfg.interface)?;
        if drv != DRIVER {
            return Err(HwError::UnknownDriver(drv.to_string()));
        }
        let (tx, rx) = unbounded();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        hubs()
            .lock()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .push(Endpoint {
                id,
                tx,
                fd: cfg.fd,
                receive_own: cfg.receive_own,
            });
        Ok(Box::new(VirtualChannel {
            name: name.to_string(),
            id,
            rx,
            fd: cfg.fd,
            listen_only: cfg.listen_only,
            closed: false,
        }))
    }
}

struct VirtualChannel {
    name: String,
    id: u64,
    rx: Receiver<RxFrame>,
    fd: bool,
    listen_only: bool,
    closed: bool,
}

impl CanChannel for VirtualChannel {
    fn send(&mut self, frame: &CanFrame) -> Result<(), HwError> {
        if self.closed {
            return Err(HwError::Closed);
        }
        if self.listen_only {
            return Err(HwError::ListenOnly);
        }
        if frame.fd && !self.fd {
            return Err(HwError::Unsupported(
                "CAN FD frame on a classic channel".into(),
            ));
        }
        let mut hubs = hubs().lock().unwrap();
        let Some(eps) = hubs.get_mut(&self.name) else {
            return Ok(());
        };
        let ts = monotonic_ns();
        let me = self.id;
        // Receivers that went away are dropped here.
        eps.retain(|ep| {
            let own = ep.id == me;
            if (own && !ep.receive_own) || (frame.fd && !ep.fd) {
                return true;
            }
            ep.tx
                .send(RxFrame {
                    frame: *frame,
                    timestamp_ns: ts,
                    is_echo: own,
                    error: None,
                })
                .is_ok()
        });
        Ok(())
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<RxFrame>, HwError> {
        if self.closed {
            return Err(HwError::Closed);
        }
        match self.rx.recv_timeout(timeout) {
            Ok(f) => Ok(Some(f)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(HwError::Closed),
        }
    }

    fn bus_state(&mut self) -> Result<HwBusState, HwError> {
        if self.closed {
            return Err(HwError::Closed);
        }
        Ok(HwBusState::default())
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let mut hubs = hubs().lock().unwrap();
        if let Some(eps) = hubs.get_mut(&self.name) {
            eps.retain(|ep| ep.id != self.id);
            if eps.is_empty() {
                hubs.remove(&self.name);
            }
        }
    }

    fn info(&self) -> ChannelInfo {
        ChannelInfo {
            driver: DRIVER.into(),
            name: format!("{DRIVER}:{}", self.name),
            description: "In-process virtual bus".into(),
            fd_capable: true,
        }
    }
}

impl Drop for VirtualChannel {
    fn drop(&mut self) {
        self.close();
    }
}
