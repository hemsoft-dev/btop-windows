//! Simple Windows battery collector backed by `GetSystemPowerStatus`.
//!
//! Hides cleanly (`DataState::Unavailable`) on desktops where `BatteryFlag == 128`
//! (no system battery). Multi-battery `SetupAPI` IOCTLs and wattage reporting are
//! intentionally deferred post-v1 per `AGENTS.md`.

use btop_model::{BatterySnapshot, DataState};
use btop_sys::{RawPowerStatus, query_system_power_status};

/// Win32 `BATTERY_FLAG_NO_BATTERY` bit (`128` / `0x80`).
pub const BATTERY_FLAG_NO_BATTERY: u8 = 128;
/// Win32 `BATTERY_FLAG_CHARGING` bit (`8` / `0x08`).
pub const BATTERY_FLAG_CHARGING: u8 = 8;
/// Win32 unknown sentinel for byte power fields (`255`).
pub const BATTERY_UNKNOWN_BYTE: u8 = 255;
/// Win32 unknown sentinel for `BatteryLifeTime` (`0xFFFF_FFFF`).
pub const BATTERY_LIFE_UNKNOWN: u32 = u32::MAX;

/// Decodes raw Win32 `SYSTEM_POWER_STATUS` fields into a [`BatterySnapshot`].
///
/// Returns `None` when no system battery is present (`BatteryFlag == 128` / bit 7 set)
/// or when charge percentage is unknown (`> 100`).
pub fn decode_power_status(raw: RawPowerStatus) -> Option<BatterySnapshot> {
    if (raw.battery_flag & BATTERY_FLAG_NO_BATTERY) != 0 {
        return None;
    }
    if raw.battery_life_percent > 100 {
        return None;
    }

    let charge_pct = raw.battery_life_percent;
    let status = if (raw.battery_flag & BATTERY_FLAG_CHARGING) != 0 {
        "charging"
    } else if raw.ac_line_status == 1 {
        if charge_pct >= 100 { "full" } else { "ac" }
    } else if raw.ac_line_status == 0 {
        "discharging"
    } else {
        "unknown"
    };

    let remaining_secs = (raw.battery_life_time_secs != BATTERY_LIFE_UNKNOWN)
        .then_some(u64::from(raw.battery_life_time_secs));

    Some(BatterySnapshot {
        charge_pct,
        status: status.to_owned(),
        watts: None,
        remaining_secs,
    })
}

/// Stateless Windows battery status collector using `GetSystemPowerStatus`.
#[derive(Debug, Default)]
pub struct BatteryCollector;

impl BatteryCollector {
    pub const fn new() -> Self {
        Self
    }

    /// Queries the current battery state, returning `DataState::Unavailable` on desktops
    /// without a battery (`BatteryFlag == 128`) or on OS query errors.
    pub fn sample(&self) -> DataState<BatterySnapshot> {
        match query_system_power_status() {
            Ok(raw) => match decode_power_status(raw) {
                Some(snap) => DataState::Available(snap),
                None => DataState::Unavailable("no battery".to_owned()),
            },
            Err(err) => DataState::Unavailable(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_cleanly_when_no_battery_flag_is_set() {
        let desktop = RawPowerStatus {
            ac_line_status: 1,
            battery_flag: BATTERY_FLAG_NO_BATTERY,
            battery_life_percent: BATTERY_UNKNOWN_BYTE,
            battery_life_time_secs: BATTERY_LIFE_UNKNOWN,
        };
        assert_eq!(decode_power_status(desktop), None);

        // Even if percentage happened to be <= 100, bit 128 means no system battery.
        let masked = RawPowerStatus {
            ac_line_status: 1,
            battery_flag: BATTERY_FLAG_NO_BATTERY | 1,
            battery_life_percent: 100,
            battery_life_time_secs: BATTERY_LIFE_UNKNOWN,
        };
        assert_eq!(decode_power_status(masked), None);
    }

    #[test]
    fn decodes_discharging_charging_ac_and_full_states() {
        let discharging = RawPowerStatus {
            ac_line_status: 0,
            battery_flag: 1,
            battery_life_percent: 67,
            battery_life_time_secs: 5400,
        };
        assert_eq!(
            decode_power_status(discharging),
            Some(BatterySnapshot {
                charge_pct: 67,
                status: "discharging".to_owned(),
                watts: None,
                remaining_secs: Some(5400),
            })
        );

        let charging = RawPowerStatus {
            ac_line_status: 1,
            battery_flag: BATTERY_FLAG_CHARGING | 1,
            battery_life_percent: 82,
            battery_life_time_secs: BATTERY_LIFE_UNKNOWN,
        };
        assert_eq!(
            decode_power_status(charging),
            Some(BatterySnapshot {
                charge_pct: 82,
                status: "charging".to_owned(),
                watts: None,
                remaining_secs: None,
            })
        );

        let full = RawPowerStatus {
            ac_line_status: 1,
            battery_flag: 1,
            battery_life_percent: 100,
            battery_life_time_secs: BATTERY_LIFE_UNKNOWN,
        };
        assert_eq!(
            decode_power_status(full)
                .as_ref()
                .map(|b| b.status.as_str()),
            Some("full")
        );

        let ac_not_charging = RawPowerStatus {
            ac_line_status: 1,
            battery_flag: 1,
            battery_life_percent: 80,
            battery_life_time_secs: BATTERY_LIFE_UNKNOWN,
        };
        assert_eq!(
            decode_power_status(ac_not_charging)
                .as_ref()
                .map(|b| b.status.as_str()),
            Some("ac")
        );
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_power_status_queries_cleanly() {
        let raw = query_system_power_status().expect("GetSystemPowerStatus should succeed");
        let state = BatteryCollector::new().sample();
        if (raw.battery_flag & BATTERY_FLAG_NO_BATTERY) != 0 {
            assert!(matches!(state, DataState::Unavailable(_)));
        } else if raw.battery_life_percent <= 100 {
            assert!(matches!(state, DataState::Available(_)));
        }
    }
}
