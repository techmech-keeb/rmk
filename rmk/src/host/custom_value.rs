//! VENDOR PATCH (olsk60): hand VIA custom values to the keyboard.
//!
//! VIA's custom-value commands (`CustomSetValue` 0x07, `CustomGetValue` 0x08,
//! `CustomSave` 0x09) carry a channel id, a value id and a payload. RMK has no
//! lighting or audio channels to serve them with, so a keyboard registers one
//! handler and uses its own channel ids for runtime settings a host tool reads
//! and writes. Without a handler the commands answer `Unhandled` (0xFF), which
//! is how a host tool tells an old firmware apart.

use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use rmk_types::protocol::vial::ViaCommand;

/// Which custom-value command arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustomValueCommand {
    Set,
    Get,
    Save,
}

/// The keyboard's handler. `data` is the 32-byte reply, pre-filled with the
/// request as `[command, channel_id, value_id, payload..]`; overwrite the bytes
/// to answer. Return `false` to answer `Unhandled` instead.
pub type CustomValueHandler = fn(CustomValueCommand, &mut [u8; 32]) -> bool;

static HANDLER: Mutex<CriticalSectionRawMutex, Cell<Option<CustomValueHandler>>> = Mutex::new(Cell::new(None));

/// Register the keyboard's handler. Call once before the host service runs.
pub fn set_custom_value_handler(handler: CustomValueHandler) {
    HANDLER.lock(|slot| slot.set(Some(handler)));
}

pub(crate) fn dispatch(command: CustomValueCommand, data: &mut [u8; 32]) {
    let handler = HANDLER.lock(|slot| slot.get());
    if !handler.is_some_and(|handler| handler(command, data)) {
        data[0] = ViaCommand::Unhandled as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_channel_7(command: CustomValueCommand, data: &mut [u8; 32]) -> bool {
        if data[1] != 7 {
            return false;
        }
        data[3] = match command {
            CustomValueCommand::Set => 1,
            CustomValueCommand::Get => 2,
            CustomValueCommand::Save => 3,
        };
        true
    }

    /// One test, because the handler slot is global: no handler answers
    /// `Unhandled`, a handler answers for its channel and declines others.
    #[test]
    fn dispatch_routes_to_the_registered_handler() {
        let mut data = [0u8; 32];
        data[0] = ViaCommand::CustomGetValue as u8;
        data[1] = 7;
        dispatch(CustomValueCommand::Get, &mut data);
        assert_eq!(data[0], ViaCommand::Unhandled as u8);

        set_custom_value_handler(echo_channel_7);
        for (command, mark) in [
            (CustomValueCommand::Set, 1),
            (CustomValueCommand::Get, 2),
            (CustomValueCommand::Save, 3),
        ] {
            let mut data = [0u8; 32];
            data[0] = ViaCommand::CustomGetValue as u8;
            data[1] = 7;
            dispatch(command, &mut data);
            assert_eq!(data[0], ViaCommand::CustomGetValue as u8);
            assert_eq!(data[3], mark);
        }

        let mut data = [0u8; 32];
        data[0] = ViaCommand::CustomSetValue as u8;
        data[1] = 8;
        dispatch(CustomValueCommand::Set, &mut data);
        assert_eq!(data[0], ViaCommand::Unhandled as u8);
    }
}
