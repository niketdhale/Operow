//! Runtime loader for `vxlapi64.dll` / `vxlapi.dll` (Windows only).

use std::ffi::{CStr, c_char, c_uint};
use std::mem::MaybeUninit;

use libloading::Library;

use super::ffi::*;
use super::{APP_NAME, XlApi, XlChannel};

#[cfg(target_pointer_width = "64")]
const DLL_NAME: &str = "vxlapi64.dll";
#[cfg(not(target_pointer_width = "64"))]
const DLL_NAME: &str = "vxlapi.dll";

type FnVoid = unsafe extern "system" fn() -> XlStatus;
type FnOpenPort = unsafe extern "system" fn(
    *mut XlPortHandle,
    *const c_char,
    XlAccess,
    *mut XlAccess,
    c_uint,
    c_uint,
    c_uint,
) -> XlStatus;
type FnBitrate = unsafe extern "system" fn(XlPortHandle, XlAccess, u32) -> XlStatus;
type FnFdConf = unsafe extern "system" fn(XlPortHandle, XlAccess, *mut XlCanFdConf) -> XlStatus;
type FnOutput = unsafe extern "system" fn(XlPortHandle, XlAccess, u8) -> XlStatus;
type FnActivate = unsafe extern "system" fn(XlPortHandle, XlAccess, c_uint, c_uint) -> XlStatus;
type FnDeactivate = unsafe extern "system" fn(XlPortHandle, XlAccess) -> XlStatus;
type FnClosePort = unsafe extern "system" fn(XlPortHandle) -> XlStatus;
type FnTransmit =
    unsafe extern "system" fn(XlPortHandle, XlAccess, *mut c_uint, *mut XlEvent) -> XlStatus;
type FnTransmitEx = unsafe extern "system" fn(
    XlPortHandle,
    XlAccess,
    c_uint,
    *mut c_uint,
    *mut XlCanTxEvent,
) -> XlStatus;
type FnReceive = unsafe extern "system" fn(XlPortHandle, *mut c_uint, *mut XlEvent) -> XlStatus;
type FnCanReceive = unsafe extern "system" fn(XlPortHandle, *mut XlCanRxEvent) -> XlStatus;
type FnGetConfig = unsafe extern "system" fn(*mut XlDriverConfig) -> XlStatus;
type FnChipState = unsafe extern "system" fn(XlPortHandle, XlAccess) -> XlStatus;
type FnErrorString = unsafe extern "system" fn(XlStatus) -> *const c_char;

/// The loaded library and the resolved function pointers. The pointers are
/// valid as long as `_lib` lives, which is as long as this struct.
pub(super) struct Dll {
    _lib: Library,
    open_driver: FnVoid,
    close_driver: FnVoid,
    get_driver_config: FnGetConfig,
    open_port: FnOpenPort,
    set_bitrate: FnBitrate,
    fd_set_configuration: FnFdConf,
    set_output: FnOutput,
    activate: FnActivate,
    deactivate: FnDeactivate,
    close_port: FnClosePort,
    transmit: FnTransmit,
    transmit_ex: FnTransmitEx,
    receive: FnReceive,
    can_receive: FnCanReceive,
    request_chip_state: FnChipState,
    error_string: FnErrorString,
}

macro_rules! sym {
    ($lib:expr, $name:literal) => {
        // SAFETY: the symbol type is the documented signature of the function.
        *unsafe { $lib.get(concat!($name, "\0").as_bytes()) }
            .map_err(|e| format!("{DLL_NAME} lacks {}: {e}", $name))?
    };
}

pub(super) fn load() -> Result<Dll, String> {
    // SAFETY: loading the vendor DLL runs its initialisers; that is the point.
    let lib = unsafe { Library::new(DLL_NAME) }
        .map_err(|e| super::missing_library(DLL_NAME, &e.to_string()))?;
    Ok(Dll {
        open_driver: sym!(lib, "xlOpenDriver"),
        close_driver: sym!(lib, "xlCloseDriver"),
        get_driver_config: sym!(lib, "xlGetDriverConfig"),
        open_port: sym!(lib, "xlOpenPort"),
        set_bitrate: sym!(lib, "xlCanSetChannelBitrate"),
        fd_set_configuration: sym!(lib, "xlCanFdSetConfiguration"),
        set_output: sym!(lib, "xlCanSetChannelOutput"),
        activate: sym!(lib, "xlActivateChannel"),
        deactivate: sym!(lib, "xlDeactivateChannel"),
        close_port: sym!(lib, "xlClosePort"),
        transmit: sym!(lib, "xlCanTransmit"),
        transmit_ex: sym!(lib, "xlCanTransmitEx"),
        receive: sym!(lib, "xlReceive"),
        can_receive: sym!(lib, "xlCanReceive"),
        request_chip_state: sym!(lib, "xlCanRequestChipState"),
        error_string: sym!(lib, "xlGetErrorString"),
        _lib: lib,
    })
}

fn check(s: XlStatus) -> Result<(), XlStatus> {
    if s == XL_SUCCESS { Ok(()) } else { Err(s) }
}

fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

impl XlApi for Dll {
    fn open_driver(&self) -> Result<(), XlStatus> {
        // SAFETY: no arguments.
        check(unsafe { (self.open_driver)() })
    }

    fn close_driver(&self) {
        // SAFETY: no arguments.
        unsafe { (self.close_driver)() };
    }

    fn channels(&self) -> Result<Vec<XlChannel>, XlStatus> {
        let mut cfg = MaybeUninit::<XlDriverConfig>::zeroed();
        // SAFETY: the driver fills the zeroed XLdriverConfig; all-zero is a
        // valid value for every field (plain integers and arrays).
        check(unsafe { (self.get_driver_config)(cfg.as_mut_ptr()) })?;
        let cfg = unsafe { cfg.assume_init() };
        let n = (cfg.channel_count as usize).min(XL_CONFIG_MAX_CHANNELS);
        Ok(cfg.channel[..n]
            .iter()
            .map(|c| XlChannel {
                name: c_str(&{ c.name }),
                hw_type: { c.hw_type },
                hw_index: { c.hw_index },
                hw_channel: { c.hw_channel },
                channel_index: { c.channel_index },
                mask: { c.channel_mask },
                capabilities: { c.channel_capabilities },
                bus_capabilities: { c.channel_bus_capabilities },
                is_on_bus: { c.is_on_bus } != 0,
            })
            .collect())
    }

    fn open_port(
        &self,
        access: XlAccess,
        permission: XlAccess,
        rx_queue: u32,
        interface_version: u32,
    ) -> Result<(XlPortHandle, XlAccess), XlStatus> {
        let name = std::ffi::CString::new(APP_NAME).expect("no NUL");
        let mut port: XlPortHandle = -1;
        let mut perm = permission;
        // SAFETY: all pointers are live locals; name is NUL-terminated.
        check(unsafe {
            (self.open_port)(
                &mut port,
                name.as_ptr(),
                access,
                &mut perm,
                rx_queue,
                interface_version,
                XL_BUS_TYPE_CAN,
            )
        })?;
        Ok((port, perm))
    }

    fn set_bitrate(
        &self,
        port: XlPortHandle,
        access: XlAccess,
        bitrate: u32,
    ) -> Result<(), XlStatus> {
        // SAFETY: plain value arguments.
        check(unsafe { (self.set_bitrate)(port, access, bitrate) })
    }

    fn fd_set_configuration(
        &self,
        port: XlPortHandle,
        access: XlAccess,
        conf: &XlCanFdConf,
    ) -> Result<(), XlStatus> {
        let mut conf = *conf;
        // SAFETY: conf is a live XLcanFdConf.
        check(unsafe { (self.fd_set_configuration)(port, access, &mut conf) })
    }

    fn set_output(&self, port: XlPortHandle, access: XlAccess, mode: u8) -> Result<(), XlStatus> {
        // SAFETY: plain value arguments.
        check(unsafe { (self.set_output)(port, access, mode) })
    }

    fn activate(&self, port: XlPortHandle, access: XlAccess, flags: u32) -> Result<(), XlStatus> {
        // SAFETY: plain value arguments.
        check(unsafe { (self.activate)(port, access, XL_BUS_TYPE_CAN, flags) })
    }

    fn deactivate(&self, port: XlPortHandle, access: XlAccess) -> Result<(), XlStatus> {
        // SAFETY: plain value arguments.
        check(unsafe { (self.deactivate)(port, access) })
    }

    fn close_port(&self, port: XlPortHandle) {
        // SAFETY: plain value argument.
        unsafe { (self.close_port)(port) };
    }

    fn transmit(
        &self,
        port: XlPortHandle,
        access: XlAccess,
        ev: &XlEvent,
    ) -> Result<u32, XlStatus> {
        let mut ev = *ev;
        let mut count: c_uint = 1;
        // SAFETY: one live XLevent and the in/out count.
        check(unsafe { (self.transmit)(port, access, &mut count, &mut ev) })?;
        Ok(count)
    }

    fn transmit_fd(
        &self,
        port: XlPortHandle,
        access: XlAccess,
        ev: &XlCanTxEvent,
    ) -> Result<u32, XlStatus> {
        let mut ev = *ev;
        let mut sent: c_uint = 0;
        // SAFETY: one live XLcanTxEvent; sent receives the accepted count.
        check(unsafe { (self.transmit_ex)(port, access, 1, &mut sent, &mut ev) })?;
        Ok(sent)
    }

    fn receive(&self, port: XlPortHandle) -> Result<Option<XlEvent>, XlStatus> {
        let mut ev = XlEvent::default();
        let mut count: c_uint = 1;
        // SAFETY: room for one event, count says so.
        match unsafe { (self.receive)(port, &mut count, &mut ev) } {
            XL_SUCCESS if count > 0 => Ok(Some(ev)),
            XL_SUCCESS | XL_ERR_QUEUE_IS_EMPTY => Ok(None),
            s => Err(s),
        }
    }

    fn receive_fd(&self, port: XlPortHandle) -> Result<Option<XlCanRxEvent>, XlStatus> {
        let mut ev = XlCanRxEvent::zeroed();
        // SAFETY: room for one XLcanRxEvent.
        match unsafe { (self.can_receive)(port, &mut ev) } {
            XL_SUCCESS => Ok(Some(ev)),
            XL_ERR_QUEUE_IS_EMPTY => Ok(None),
            s => Err(s),
        }
    }

    fn request_chip_state(&self, port: XlPortHandle, access: XlAccess) -> Result<(), XlStatus> {
        // SAFETY: plain value arguments.
        check(unsafe { (self.request_chip_state)(port, access) })
    }

    fn error_string(&self, status: XlStatus) -> String {
        // SAFETY: returns a static NUL-terminated string (or null).
        let p = unsafe { (self.error_string)(status) };
        if p.is_null() {
            return format!("XL status {status}");
        }
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        format!("{s} (XL status {status})")
    }
}
