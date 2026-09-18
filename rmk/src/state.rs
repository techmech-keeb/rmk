use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use rmk_types::ble::BleState;
#[cfg(feature = "_ble")]
use rmk_types::ble::BleStatus;
use rmk_types::connection::{ConnectionStatus, ConnectionType, UsbState};

use crate::RawMutex;
use crate::event::{ConnectionStatusChangeEvent, publish_event};

/// Single source of truth for transport state and routing. All writes go
/// through the mutator helpers below so the active-output cascade runs and
/// change events fire on every transition.
pub(crate) static CONNECTION_STATUS: Mutex<RawMutex, Cell<ConnectionStatus>> =
    Mutex::new(Cell::new(ConnectionStatus::new()));

pub(crate) fn active_transport() -> Option<ConnectionType> {
    CONNECTION_STATUS.lock(|c| c.get().decide_active())
}

pub(crate) fn current_connection_status() -> ConnectionStatus {
    CONNECTION_STATUS.lock(|c| c.get())
}

pub(crate) fn current_usb_state() -> UsbState {
    CONNECTION_STATUS.lock(|c| c.get().usb)
}

/// Current central sleep state for host polling. Sourced from the BLE sleep
/// manager's `SLEEPING_STATE`; always `false` in builds without BLE.
pub(crate) fn current_sleep_state() -> bool {
    #[cfg(feature = "_ble")]
    {
        crate::ble::sleep::SLEEPING_STATE.load(core::sync::atomic::Ordering::Acquire)
    }
    #[cfg(not(feature = "_ble"))]
    {
        false
    }
}

#[cfg(feature = "_ble")]
pub(crate) fn current_ble_status() -> BleStatus {
    CONNECTION_STATUS.lock(|c| c.get().ble)
}

/// The HID Resolution Multiplier the USB host selected, as the raw logical
/// values of the feature report (bit 0 = wheel, bit 1 = pan) in one atomic, so
/// a producer reading the pair never observes half of a `SET_REPORT`. It sits
/// beside the connection state because it has that lifetime: a host that
/// understands hi-res scrolling writes the feature report after enumeration,
/// and the value means nothing once that USB connection ends.
#[cfg(feature = "hires_scroll")]
static SCROLL_MULTIPLIERS_RAW: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// The (wheel, pan) units per detent a scroll producer must emit right now:
/// 1 until the host selects hi-res over USB, then
/// [`crate::hid::RESOLUTION_MULTIPLIER_MAX`] per axis.
///
/// Only USB carries a feature report, so a keyboard whose reports are going out
/// over BLE stays in detents no matter what a USB host asked for earlier.
#[cfg(feature = "hires_scroll")]
pub fn resolution_multipliers() -> (i16, i16) {
    if active_transport() != Some(ConnectionType::Usb) {
        return (1, 1);
    }
    let raw = SCROLL_MULTIPLIERS_RAW.load(core::sync::atomic::Ordering::Relaxed);
    // HID's resolution mapping for logical 0..=1 over physical 1..=MAX:
    // (value - Lmin) / (Lmax - Lmin) * (Pmax - Pmin) + Pmin.
    let per_axis = |bit: u8| 1 + (bit & 1) as i16 * (crate::hid::RESOLUTION_MULTIPLIER_MAX as i16 - 1);
    (per_axis(raw), per_axis(raw >> 1))
}

/// Without the `hires_scroll` feature there is nothing to negotiate: one unit
/// is one detent, and every producer's multiplication folds away at compile
/// time.
#[cfg(not(feature = "hires_scroll"))]
pub fn resolution_multipliers() -> (i16, i16) {
    (1, 1)
}

/// The raw logical pair, as the host reads it back with `GET_REPORT`.
#[cfg(feature = "hires_scroll")]
pub(crate) fn raw_multipliers() -> (u8, u8) {
    let raw = SCROLL_MULTIPLIERS_RAW.load(core::sync::atomic::Ordering::Relaxed);
    (raw & 1, (raw >> 1) & 1)
}

/// Store an accepted `SET_REPORT`.
#[cfg(feature = "hires_scroll")]
pub(crate) fn set_raw_multipliers(wheel: u8, pan: u8) {
    SCROLL_MULTIPLIERS_RAW.store((wheel & 1) | ((pan & 1) << 1), core::sync::atomic::Ordering::Relaxed);
}

/// Back to one unit per detent. The USB connection's state ending (reset,
/// deconfiguration, disable) drops the negotiation with it: the host has to
/// ask again, and until it does the device must scroll in detents.
#[cfg(feature = "hires_scroll")]
pub(crate) fn reset_multipliers() {
    SCROLL_MULTIPLIERS_RAW.store(0, core::sync::atomic::Ordering::Relaxed);
}

/// Read-modify-write the connection status atomically.
pub(crate) fn update_status(f: impl FnOnce(&mut ConnectionStatus)) {
    let Some((prev, new)) = CONNECTION_STATUS.lock(|c| {
        let prev = c.get();
        let mut new = prev;
        f(&mut new);
        if prev == new {
            return None;
        }
        c.set(new);
        Some((prev, new))
    }) else {
        return;
    };

    let prev_active = prev.decide_active();
    let new_active = new.decide_active();

    if prev_active != new_active
        && let Some(prev_active) = prev_active
    {
        // Drain after the commit so any producer racing past the mutex reads
        // the new state and routes to the new channel rather than the one
        // about to be cleared.
        crate::channel::clear_and_release_report_channel(prev_active);
    }

    publish_event(ConnectionStatusChangeEvent(new));
}

pub fn set_usb_state(s: UsbState) {
    update_status(|c| c.usb = s);
}

pub(crate) fn set_ble_state(s: BleState) {
    update_status(|c| c.ble.state = s);
}

/// Switching profiles always drops the BLE state back to `Inactive`; the
/// connection loop re-advertises and updates state from there.
pub(crate) fn set_ble_profile(profile: u8) {
    update_status(|c| {
        c.ble.profile = profile;
        c.ble.state = BleState::Inactive;
    });
}

/// Persistence is the caller's responsibility: `storage::store` the new
/// `ConnectionType`.
pub(crate) fn set_preferred_connection(t: ConnectionType) {
    update_status(|c| c.preferred = t);
}

/// Load the preferred connection type at startup.
///
/// With the `storage` feature, reads the persisted `ConnectionType` from flash;
/// otherwise falls back to a build-time default — `Ble` when USB is disabled, `Usb` otherwise.
#[cfg(feature = "_ble")]
pub(crate) async fn load_preferred_connection() -> ConnectionType {
    #[cfg(feature = "storage")]
    if let Ok(Some(crate::storage::StorageValue::ConnectionType(c))) =
        crate::storage::read(crate::storage::StorageKey::ConnectionType).await
    {
        return c;
    }
    if cfg!(feature = "_no_usb") {
        ConnectionType::Ble
    } else {
        ConnectionType::Usb
    }
}

#[cfg(all(feature = "_ble", not(feature = "_no_usb")))]
pub(crate) async fn toggle_preferred() {
    let mut new = ConnectionType::Usb;
    update_status(|c| {
        c.preferred = match c.preferred {
            ConnectionType::Usb => ConnectionType::Ble,
            ConnectionType::Ble => ConnectionType::Usb,
        };
        new = c.preferred;
    });
    info!("Switching preferred transport to: {:?}", new);
    #[cfg(feature = "storage")]
    crate::storage::store_unchecked(crate::storage::StorageItem::ConnectionType(new)).await;
}

#[cfg(feature = "_ble")]
pub(crate) fn current_profile() -> u8 {
    CONNECTION_STATUS.lock(|c| c.get().ble.profile)
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use embassy_futures::select::{Either, select};
    use embassy_time::{Duration, Timer};

    use super::{
        CONNECTION_STATUS, ConnectionStatus, ConnectionType, UsbState, set_preferred_connection, set_usb_state,
    };
    use crate::event::{ConnectionStatusChangeEvent, EventSubscriber, SubscribableEvent};
    use crate::hid::{KeyboardReport, Report};
    use crate::test_support::test_block_on as block_on;

    fn state_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn reset_state() {
        CONNECTION_STATUS.lock(|c| c.set(ConnectionStatus::default()));
        #[cfg(not(feature = "_no_usb"))]
        crate::channel::USB_REPORT_CHANNEL.clear();
        #[cfg(feature = "_ble")]
        crate::channel::BLE_REPORT_CHANNEL.clear();
    }

    fn pressed_keyboard_report() -> Report {
        Report::KeyboardReport(KeyboardReport {
            modifier: 0x02,
            reserved: 0,
            leds: 0,
            keycodes: [4, 0, 0, 0, 0, 0],
        })
    }

    fn assert_all_up_keyboard_report(report: Report) {
        match report {
            Report::KeyboardReport(r) => {
                assert_eq!(r.modifier, 0);
                assert_eq!(r.reserved, 0);
                assert_eq!(r.leds, 0);
                assert_eq!(r.keycodes, [0; 6]);
            }
            _ => panic!("expected keyboard all-up report"),
        }
    }

    #[test]
    fn preferred_transport_change_publishes_status_event() {
        let _guard = state_test_lock().lock().unwrap();
        reset_state();
        set_usb_state(UsbState::Configured);
        let mut sub = ConnectionStatusChangeEvent::subscriber();

        set_preferred_connection(ConnectionType::Ble);

        let event = block_on(sub.next_event());
        assert_eq!(event.0.preferred, ConnectionType::Ble);
    }

    #[test]
    fn usb_state_change_publishes_status_event() {
        let _guard = state_test_lock().lock().unwrap();
        reset_state();
        let mut sub = ConnectionStatusChangeEvent::subscriber();

        set_usb_state(UsbState::Configured);

        let event = block_on(sub.next_event());
        assert_eq!(event.0.usb, UsbState::Configured);
    }

    #[test]
    fn unchanged_status_does_not_publish_event() {
        let _guard = state_test_lock().lock().unwrap();
        reset_state();
        set_usb_state(UsbState::Configured);
        let mut sub = ConnectionStatusChangeEvent::subscriber();

        // Re-setting the same value should not publish.
        set_usb_state(UsbState::Configured);

        match block_on(select(Timer::after(Duration::from_millis(1)), sub.next_event())) {
            Either::First(_) => {}
            Either::Second(event) => panic!("unexpected status change event: {:?}", event),
        }
    }

    #[cfg(not(feature = "_no_usb"))]
    #[test]
    fn flipping_away_from_active_clears_stale_reports_and_queues_all_up() {
        use crate::channel::USB_REPORT_CHANNEL;

        let _guard = state_test_lock().lock().unwrap();
        reset_state();
        set_usb_state(UsbState::Configured);
        assert_eq!(super::active_transport(), Some(ConnectionType::Usb));

        // Drain anything left over from earlier tests, then queue a sentinel
        // that would otherwise persist across a flip.
        USB_REPORT_CHANNEL.clear();
        USB_REPORT_CHANNEL
            .try_send(pressed_keyboard_report())
            .expect("channel should have capacity for sentinel");
        assert!(USB_REPORT_CHANNEL.try_receive().is_ok());
        USB_REPORT_CHANNEL
            .try_send(pressed_keyboard_report())
            .expect("channel should have capacity for sentinel");

        set_usb_state(UsbState::Disabled);
        assert!(super::active_transport().is_none());
        assert_all_up_keyboard_report(
            USB_REPORT_CHANNEL
                .try_receive()
                .expect("USB_REPORT_CHANNEL should contain keyboard all-up report"),
        );
        assert!(
            USB_REPORT_CHANNEL.try_receive().is_err(),
            "USB_REPORT_CHANNEL should contain only the all-up report"
        );
    }

    #[cfg(not(feature = "_no_usb"))]
    #[test]
    fn blocked_send_drops_report_after_transport_change() {
        use embassy_futures::join::join;

        use crate::channel::{USB_REPORT_CHANNEL, send_hid_report};

        let _guard = state_test_lock().lock().unwrap();
        reset_state();
        set_usb_state(UsbState::Configured);

        for _ in 0..crate::REPORT_CHANNEL_SIZE {
            USB_REPORT_CHANNEL
                .try_send(pressed_keyboard_report())
                .expect("channel should have capacity while filling");
        }

        block_on(join(
            send_hid_report(Report::KeyboardReport(KeyboardReport::default())),
            async {
                Timer::after(Duration::from_millis(1)).await;
                set_usb_state(UsbState::Disabled);
            },
        ));

        assert_all_up_keyboard_report(
            USB_REPORT_CHANNEL
                .try_receive()
                .expect("USB_REPORT_CHANNEL should contain keyboard all-up report"),
        );
        assert!(
            USB_REPORT_CHANNEL.try_receive().is_err(),
            "USB_REPORT_CHANNEL should contain only the all-up report"
        );
    }

    #[cfg(all(not(feature = "_no_usb"), feature = "_ble"))]
    #[test]
    fn usb_preference_flip_releases_previous_ble_transport() {
        use crate::channel::BLE_REPORT_CHANNEL;
        use crate::state::{BleState, set_ble_state};

        let _guard = state_test_lock().lock().unwrap();
        reset_state();
        set_preferred_connection(ConnectionType::Usb);
        set_ble_state(BleState::Connected);
        assert_eq!(super::active_transport(), Some(ConnectionType::Ble));

        BLE_REPORT_CHANNEL
            .try_send(pressed_keyboard_report())
            .expect("BLE report channel should have capacity for sentinel");

        set_usb_state(UsbState::Configured);
        assert_eq!(super::active_transport(), Some(ConnectionType::Usb));
        assert_all_up_keyboard_report(
            BLE_REPORT_CHANNEL
                .try_receive()
                .expect("BLE_REPORT_CHANNEL should contain keyboard all-up report"),
        );
        assert!(
            BLE_REPORT_CHANNEL.try_receive().is_err(),
            "BLE_REPORT_CHANNEL should contain only the all-up report"
        );
    }

    #[cfg(not(feature = "_no_usb"))]
    #[test]
    fn blocked_send_enqueues_when_transport_stays_active() {
        use embassy_futures::join::join;

        use crate::channel::{USB_REPORT_CHANNEL, send_hid_report};

        let _guard = state_test_lock().lock().unwrap();
        reset_state();
        set_usb_state(UsbState::Configured);

        for _ in 0..crate::REPORT_CHANNEL_SIZE {
            USB_REPORT_CHANNEL
                .try_send(Report::KeyboardReport(KeyboardReport::default()))
                .expect("channel should have capacity while filling");
        }

        block_on(join(
            send_hid_report(Report::KeyboardReport(KeyboardReport::default())),
            async {
                Timer::after(Duration::from_millis(1)).await;
                let _ = USB_REPORT_CHANNEL.try_receive();
            },
        ));

        assert_eq!(USB_REPORT_CHANNEL.len(), crate::REPORT_CHANNEL_SIZE);
    }

    /// Only USB can negotiate a resolution multiplier. A dual-mode keyboard
    /// that switches to BLE has to go back to detents there, or the host would
    /// scroll that many times too far for the same motion.
    #[cfg(feature = "hires_scroll")]
    #[test]
    fn scroll_reports_leaving_over_another_transport_stay_in_detents() {
        let _guard = state_test_lock().lock().unwrap();
        reset_state();
        super::set_raw_multipliers(1, 1);

        set_usb_state(UsbState::Disabled);
        assert_eq!(
            super::resolution_multipliers(),
            (1, 1),
            "no USB connection, no multiplier"
        );

        set_usb_state(UsbState::Configured);
        let max = crate::hid::RESOLUTION_MULTIPLIER_MAX as i16;
        assert_eq!(super::resolution_multipliers(), (max, max));

        super::reset_multipliers();
    }
}
