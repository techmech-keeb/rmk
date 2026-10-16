//! High-resolution scrolling: the HID Resolution Multiplier the host selects.
//!
//! The composite descriptor declares one Resolution Multiplier feature per
//! scrolling axis (see [`crate::hid`]). A host that understands hi-res
//! scrolling writes the logical maximum into that feature report, which means
//! "send me [`crate::hid::RESOLUTION_MULTIPLIER_MAX`] units per detent from
//! now on"; a host that doesn't never touches the report, and the device keeps
//! sending one unit per detent. Every producer of wheel/pan motion multiplies
//! its output by [`resolution_multipliers`], so the scrolling speed is the
//! same either way and only the step size changes.

#[cfg(feature = "hires_scroll")]
use core::sync::atomic::{AtomicU8, Ordering};

#[cfg(feature = "hires_scroll")]
use rmk_types::connection::ConnectionType;

#[cfg(feature = "hires_scroll")]
use crate::hid::RESOLUTION_MULTIPLIER_MAX;

/// The raw logical values (bit 0 = wheel, bit 1 = pan) in one atomic, so a
/// producer reading the pair can never observe half of a `SET_REPORT`.
#[cfg(feature = "hires_scroll")]
static MULTIPLIERS_RAW: AtomicU8 = AtomicU8::new(0);

/// The (wheel, pan) units per detent to emit right now: 1 until the host
/// selects hi-res over USB, then [`RESOLUTION_MULTIPLIER_MAX`].
///
/// Only USB carries a feature report, so a keyboard whose reports are
/// currently going out over BLE stays in detents no matter what a USB host
/// asked for earlier.
#[cfg(feature = "hires_scroll")]
pub fn resolution_multipliers() -> (i16, i16) {
    if crate::state::active_transport() != Some(ConnectionType::Usb) {
        return (1, 1);
    }
    let raw = MULTIPLIERS_RAW.load(Ordering::Relaxed);
    (effective(raw & 1), effective((raw >> 1) & 1))
}

/// Without the `hires_scroll` feature there is nothing to negotiate: one unit
/// is one detent, and every producer's multiplication folds away at compile
/// time.
#[cfg(not(feature = "hires_scroll"))]
pub fn resolution_multipliers() -> (i16, i16) {
    (1, 1)
}

/// The HID resolution mapping for logical 0..=1 over physical 1..=MAX:
/// `(value - Lmin) / (Lmax - Lmin) * (Pmax - Pmin) + Pmin`.
#[cfg(feature = "hires_scroll")]
fn effective(raw: u8) -> i16 {
    1 + raw.min(1) as i16 * (RESOLUTION_MULTIPLIER_MAX as i16 - 1)
}

/// The raw logical pair, as the host would read it back.
#[cfg(feature = "hires_scroll")]
pub(crate) fn raw_multipliers() -> (u8, u8) {
    let raw = MULTIPLIERS_RAW.load(Ordering::Relaxed);
    (raw & 1, (raw >> 1) & 1)
}

/// Store an accepted `SET_REPORT`.
#[cfg(feature = "hires_scroll")]
pub(crate) fn set_raw_multipliers(wheel: u8, pan: u8) {
    MULTIPLIERS_RAW.store((wheel & 1) | ((pan & 1) << 1), Ordering::Relaxed);
}

/// Back to one unit per detent. The USB connection's state ending (reset,
/// deconfiguration, disable) drops the negotiation with it: the host has to
/// ask again, and until it does the device must scroll in detents.
#[cfg(feature = "hires_scroll")]
pub(crate) fn reset_multipliers() {
    MULTIPLIERS_RAW.store(0, Ordering::Relaxed);
}

#[cfg(all(test, feature = "hires_scroll"))]
mod tests {
    use rmk_types::connection::UsbState;

    use super::{RESOLUTION_MULTIPLIER_MAX, reset_multipliers, resolution_multipliers, set_raw_multipliers};
    use crate::state::set_usb_state;

    /// Only USB can negotiate. A dual-mode keyboard that switches to BLE has
    /// to go back to detents there, or the host would scroll that many times
    /// too far for the same motion.
    #[test]
    fn reports_leaving_over_another_transport_stay_in_detents() {
        set_raw_multipliers(1, 1);

        set_usb_state(UsbState::Disabled);
        assert_eq!(resolution_multipliers(), (1, 1), "no USB connection, no multiplier");

        set_usb_state(UsbState::Configured);
        let max = RESOLUTION_MULTIPLIER_MAX as i16;
        assert_eq!(resolution_multipliers(), (max, max));

        reset_multipliers();
    }
}
