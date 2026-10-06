//! Runtime loader for `PCANBasic.dll` / `libpcanbasic.so` (Windows and Linux).

use std::ffi::{c_char, c_void};

use libloading::Library;

use super::ffi::*;
use super::{PcanApi, PcanChannel};

#[cfg(windows)]
const LIB_NAMES: &[&str] = &["PCANBasic.dll"];
#[cfg(not(windows))]
const LIB_NAMES: &[&str] = &["libpcanbasic.so", "libpcanbasic.so.0"];

/// `TPCANParameter` buffers are at most this long (version strings, names).
const TEXT_LEN: usize = 256;
/// Neutral-language ID for `CAN_GetErrorText` is 0; 0x09 asks for English.
const LANG_ENGLISH: u16 = 0x09;
/// Upper bound of attached channels requested at once.
const MAX_ATTACHED: usize = 64;

type FnInit = unsafe extern "system" fn(PcanHandle, u16, u8, u32, u16) -> PcanStatus;
type FnInitFd = unsafe extern "system" fn(PcanHandle, *const c_char) -> PcanStatus;
type FnChannel = unsafe extern "system" fn(PcanHandle) -> PcanStatus;
type FnRead = unsafe extern "system" fn(PcanHandle, *mut PcanMsg, *mut PcanTimestamp) -> PcanStatus;
type FnReadFd = unsafe extern "system" fn(PcanHandle, *mut PcanMsgFd, *mut u64) -> PcanStatus;
type FnWrite = unsafe extern "system" fn(PcanHandle, *mut PcanMsg) -> PcanStatus;
type FnWriteFd = unsafe extern "system" fn(PcanHandle, *mut PcanMsgFd) -> PcanStatus;
type FnValue = unsafe extern "system" fn(PcanHandle, u8, *mut c_void, u32) -> PcanStatus;
type FnErrorText = unsafe extern "system" fn(PcanStatus, u16, *mut c_char) -> PcanStatus;

/// The loaded library and the resolved function pointers. The pointers are
/// valid as long as `_lib` lives, which is as long as this struct.
pub(super) struct Dll {
    _lib: Library,
    initialize: FnInit,
    initialize_fd: FnInitFd,
    uninitialize: FnChannel,
    get_status: FnChannel,
    read: FnRead,
    read_fd: FnReadFd,
    write: FnWrite,
    write_fd: FnWriteFd,
    get_value: FnValue,
    set_value: FnValue,
    get_error_text: FnErrorText,
}

macro_rules! sym {
    ($lib:expr, $file:expr, $name:literal) => {
        // SAFETY: the symbol type is the documented signature of the function.
        *unsafe { $lib.get(concat!($name, "\0").as_bytes()) }
            .map_err(|e| format!("{} lacks {}: {e}", $file, $name))?
    };
}

pub(super) fn load() -> Result<Dll, String> {
    let mut last = String::new();
    let mut found = None;
    for name in LIB_NAMES {
        // SAFETY: loading the vendor library runs its initialisers; that is
        // the point.
        match unsafe { Library::new(name) } {
            Ok(lib) => {
                found = Some((lib, *name));
                break;
            }
            Err(e) => last = e.to_string(),
        }
    }
    let (lib, file) = found.ok_or_else(|| super::missing_library(LIB_NAMES[0], &last))?;
    Ok(Dll {
        initialize: sym!(lib, file, "CAN_Initialize"),
        initialize_fd: sym!(lib, file, "CAN_InitializeFD"),
        uninitialize: sym!(lib, file, "CAN_Uninitialize"),
        get_status: sym!(lib, file, "CAN_GetStatus"),
        read: sym!(lib, file, "CAN_Read"),
        read_fd: sym!(lib, file, "CAN_ReadFD"),
        write: sym!(lib, file, "CAN_Write"),
        write_fd: sym!(lib, file, "CAN_WriteFD"),
        get_value: sym!(lib, file, "CAN_GetValue"),
        set_value: sym!(lib, file, "CAN_SetValue"),
        get_error_text: sym!(lib, file, "CAN_GetErrorText"),
        _lib: lib,
    })
}

fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn check(s: PcanStatus) -> Result<(), PcanStatus> {
    if s == PCAN_ERROR_OK { Ok(()) } else { Err(s) }
}

impl Dll {
    fn get_u32(&self, channel: PcanHandle, param: u8) -> Result<u32, PcanStatus> {
        let mut v: u32 = 0;
        // SAFETY: the buffer is a live u32 and its size is passed.
        check(unsafe { (self.get_value)(channel, param, (&mut v as *mut u32).cast(), 4) })?;
        Ok(v)
    }
}

impl PcanApi for Dll {
    fn initialize(&self, channel: PcanHandle, btr0btr1: u16) -> PcanStatus {
        // SAFETY: plain value arguments; no non-PnP hardware parameters.
        unsafe { (self.initialize)(channel, btr0btr1, 0, 0, 0) }
    }

    fn initialize_fd(&self, channel: PcanHandle, bitrate: &str) -> PcanStatus {
        let Ok(text) = std::ffi::CString::new(bitrate) else {
            return PCAN_ERROR_ILLPARAMVAL;
        };
        // SAFETY: text is NUL-terminated and outlives the call.
        unsafe { (self.initialize_fd)(channel, text.as_ptr()) }
    }

    fn uninitialize(&self, channel: PcanHandle) -> PcanStatus {
        // SAFETY: plain value argument.
        unsafe { (self.uninitialize)(channel) }
    }

    fn get_status(&self, channel: PcanHandle) -> PcanStatus {
        // SAFETY: plain value argument.
        unsafe { (self.get_status)(channel) }
    }

    fn read(&self, channel: PcanHandle) -> Result<(PcanMsg, PcanTimestamp), PcanStatus> {
        let mut m = PcanMsg::default();
        let mut ts = PcanTimestamp::default();
        // SAFETY: both out-parameters are live and correctly sized.
        check(unsafe { (self.read)(channel, &mut m, &mut ts) })?;
        Ok((m, ts))
    }

    fn read_fd(&self, channel: PcanHandle) -> Result<(PcanMsgFd, u64), PcanStatus> {
        let mut m = PcanMsgFd::default();
        let mut ts: u64 = 0;
        // SAFETY: both out-parameters are live and correctly sized.
        check(unsafe { (self.read_fd)(channel, &mut m, &mut ts) })?;
        Ok((m, ts))
    }

    fn write(&self, channel: PcanHandle, msg: &PcanMsg) -> PcanStatus {
        let mut m = *msg;
        // SAFETY: one live TPCANMsg.
        unsafe { (self.write)(channel, &mut m) }
    }

    fn write_fd(&self, channel: PcanHandle, msg: &PcanMsgFd) -> PcanStatus {
        let mut m = *msg;
        // SAFETY: one live TPCANMsgFD.
        unsafe { (self.write_fd)(channel, &mut m) }
    }

    fn set_param(&self, channel: PcanHandle, param: u8, value: u32) -> PcanStatus {
        let mut v = value;
        // SAFETY: the buffer is a live u32 and its size is passed.
        unsafe { (self.set_value)(channel, param, (&mut v as *mut u32).cast(), 4) }
    }

    fn attached_channels(&self) -> Result<Vec<PcanChannel>, PcanStatus> {
        let count = self
            .get_u32(PCAN_NONEBUS, PCAN_ATTACHED_CHANNELS_COUNT)?
            .min(MAX_ATTACHED as u32) as usize;
        let mut buf = vec![PcanChannelInformation::default(); count];
        if count > 0 {
            let bytes = (count * std::mem::size_of::<PcanChannelInformation>()) as u32;
            // SAFETY: the buffer holds `count` entries and `bytes` is its size.
            check(unsafe {
                (self.get_value)(
                    PCAN_NONEBUS,
                    PCAN_ATTACHED_CHANNELS,
                    buf.as_mut_ptr().cast(),
                    bytes,
                )
            })?;
        }
        Ok(buf
            .iter()
            .map(|c| PcanChannel {
                handle: c.channel_handle,
                device_name: c_str(&c.device_name),
                features: c.device_features,
                condition: c.channel_condition,
            })
            .collect())
    }

    fn channel_condition(&self, channel: PcanHandle) -> Result<u32, PcanStatus> {
        self.get_u32(channel, PCAN_CHANNEL_CONDITION)
    }

    fn channel_features(&self, channel: PcanHandle) -> Result<u32, PcanStatus> {
        self.get_u32(channel, PCAN_CHANNEL_FEATURES)
    }

    fn hardware_name(&self, channel: PcanHandle) -> Result<String, PcanStatus> {
        let mut buf = [0u8; TEXT_LEN];
        // SAFETY: the buffer is live and its size is passed.
        check(unsafe {
            (self.get_value)(
                channel,
                PCAN_HARDWARE_NAME,
                buf.as_mut_ptr().cast(),
                TEXT_LEN as u32,
            )
        })?;
        Ok(c_str(&buf))
    }

    fn error_text(&self, status: PcanStatus) -> String {
        let mut buf = [0u8; TEXT_LEN];
        // SAFETY: the library writes at most 255 characters and a NUL.
        let r = unsafe { (self.get_error_text)(status, LANG_ENGLISH, buf.as_mut_ptr().cast()) };
        let text = c_str(&buf);
        if r != PCAN_ERROR_OK || text.is_empty() {
            format!("PCAN status 0x{status:X}")
        } else {
            format!("{text} (PCAN status 0x{status:X})")
        }
    }
}
