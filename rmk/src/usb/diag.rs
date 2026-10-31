//! Opt-in USB event log (feature `usb_diag`).
//!
//! Records the bus events and the delegated control requests the device
//! handler sees, together with the resolution multiplier at that moment, so a
//! keyboard can report what a host did around suspend and resume without a
//! capture tool on the host. Standard device requests (descriptors, address,
//! configuration) are answered inside embassy-usb and show up only as the
//! `Addressed` / `Configured` events.

use core::cell::RefCell;
use core::fmt::Write;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Instant;
use embassy_usb::control::Request;
use embassy_usb::driver::Direction;

/// Entries kept; the oldest are overwritten first.
pub const CAPACITY: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbEvent {
    Enabled(bool),
    /// Bus reset; `kept` = the multiplier survived it.
    Reset {
        kept: bool,
    },
    Addressed(u8),
    Configured(bool),
    Suspended(bool),
    RemoteWakeup(bool),
    /// A request offered to the handlers (class, vendor and interface requests).
    Control {
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        length: u16,
    },
    MultiplierSet {
        data: [u8; 3],
        len: u8,
        accepted: bool,
    },
    MultiplierGet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbLogEntry {
    pub at_ms: u32,
    /// Raw multiplier bits after the event (bit 0 = wheel, bit 1 = pan).
    pub multiplier_raw: u8,
    pub event: UsbEvent,
}

struct Ring {
    entries: [Option<UsbLogEntry>; CAPACITY],
    total: u32,
}

static LOG: Mutex<CriticalSectionRawMutex, RefCell<Ring>> = Mutex::new(RefCell::new(Ring {
    entries: [None; CAPACITY],
    total: 0,
}));

pub(crate) fn record(event: UsbEvent, multiplier_raw: u8) {
    let entry = UsbLogEntry {
        at_ms: Instant::now().as_millis() as u32,
        multiplier_raw,
        event,
    };
    LOG.lock(|log| {
        let mut log = log.borrow_mut();
        let slot = log.total as usize % CAPACITY;
        log.entries[slot] = Some(entry);
        log.total = log.total.wrapping_add(1);
    });
}

pub(crate) fn control(req: &Request) -> UsbEvent {
    let direction = match req.direction {
        Direction::In => 0x80,
        Direction::Out => 0x00,
    };
    UsbEvent::Control {
        request_type: direction | (req.request_type as u8) << 5 | req.recipient as u8,
        request: req.request,
        value: req.value,
        index: req.index,
        length: req.length,
    }
}

/// The kept entries, oldest first, and how many were ever recorded.
pub fn snapshot() -> (heapless::Vec<UsbLogEntry, CAPACITY>, u32) {
    LOG.lock(|log| {
        let log = log.borrow();
        let kept = (log.total as usize).min(CAPACITY);
        let first = log.total as usize - kept;
        let entries = (first..log.total as usize)
            .filter_map(|i| log.entries[i % CAPACITY])
            .collect();
        (entries, log.total)
    })
}

impl UsbLogEntry {
    /// One line of lowercase letters, digits and spaces, so it can be typed
    /// into a text editor on any keyboard layout.
    pub fn write_line(&self, w: &mut impl Write) -> core::fmt::Result {
        write!(w, "{} m{} ", self.at_ms, self.multiplier_raw)?;
        match self.event {
            UsbEvent::Enabled(on) => write!(w, "enable {}", on as u8),
            UsbEvent::Reset { kept } => write!(w, "reset {}", if kept { "keep" } else { "clear" }),
            UsbEvent::Addressed(addr) => write!(w, "addr {}", addr),
            UsbEvent::Configured(on) => write!(w, "config {}", on as u8),
            UsbEvent::Suspended(on) => write!(w, "suspend {}", on as u8),
            UsbEvent::RemoteWakeup(on) => write!(w, "wakeup {}", on as u8),
            UsbEvent::Control {
                request_type,
                request,
                value,
                index,
                length,
            } => write!(
                w,
                "ctl {:02x} {:02x} {:04x} {:04x} {}",
                request_type, request, value, index, length
            ),
            UsbEvent::MultiplierSet { data, len, accepted } => {
                write!(w, "mset")?;
                for byte in &data[..(len as usize).min(3)] {
                    write!(w, " {:02x}", byte)?;
                }
                write!(w, " {}", if accepted { "ok" } else { "ng" })
            }
            UsbEvent::MultiplierGet => write!(w, "mget"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(entry: &UsbLogEntry) -> heapless::String<64> {
        let mut s = heapless::String::new();
        entry.write_line(&mut s).unwrap();
        s
    }

    #[test]
    fn the_log_keeps_the_newest_entries_in_order() {
        for i in 0..(CAPACITY as u8 + 5) {
            record(UsbEvent::Addressed(i), 0);
        }
        let (entries, total) = snapshot();
        assert_eq!(total, CAPACITY as u32 + 5);
        assert_eq!(entries.len(), CAPACITY);
        assert_eq!(entries[0].event, UsbEvent::Addressed(5));
        assert_eq!(entries[CAPACITY - 1].event, UsbEvent::Addressed(CAPACITY as u8 + 4));
    }

    #[test]
    fn lines_use_only_characters_a_keyboard_can_type() {
        let entry = UsbLogEntry {
            at_ms: 1234,
            multiplier_raw: 3,
            event: UsbEvent::Control {
                request_type: 0x21,
                request: 0x09,
                value: 0x0305,
                index: 1,
                length: 3,
            },
        };
        assert_eq!(line(&entry).as_str(), "1234 m3 ctl 21 09 0305 0001 3");
        let set = UsbLogEntry {
            at_ms: 0,
            multiplier_raw: 0,
            event: UsbEvent::MultiplierSet {
                data: [5, 1, 1],
                len: 3,
                accepted: false,
            },
        };
        assert_eq!(line(&set).as_str(), "0 m0 mset 05 01 01 ng");
        assert!(
            line(&set)
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b' ')
        );
    }
}
