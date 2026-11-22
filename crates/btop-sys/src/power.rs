//! Windows power/battery status (`GetSystemPowerStatus`) and local timezone offset
//! (`GetTimeZoneInformation`) helpers.

/// Raw fields decoded from Win32 `SYSTEM_POWER_STATUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawPowerStatus {
    /// `0` = Offline (battery), `1` = Online (AC), `255` = Unknown.
    pub ac_line_status: u8,
    /// Bitmask: `1` = High, `2` = Low, `4` = Critical, `8` = Charging, `128` = No battery, `255` = Unknown.
    pub battery_flag: u8,
    /// Charge percentage (`0..=100`), or `255` when unknown.
    pub battery_life_percent: u8,
    /// Remaining battery lifetime in seconds, or `u32::MAX` (`0xFFFF_FFFF`) when unknown.
    pub battery_life_time_secs: u32,
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

    use super::RawPowerStatus;

    const TIME_ZONE_ID_UNKNOWN: u32 = 0;
    const TIME_ZONE_ID_STANDARD: u32 = 1;
    const TIME_ZONE_ID_DAYLIGHT: u32 = 2;

    /// Queries Windows power and battery state via `GetSystemPowerStatus`.
    pub fn query_system_power_status() -> Result<RawPowerStatus, String> {
        // SAFETY: `SYSTEM_POWER_STATUS` is a POD Win32 output struct initialized by the OS.
        let mut status: SYSTEM_POWER_STATUS = unsafe { core::mem::zeroed() };
        let ok = unsafe { GetSystemPowerStatus(&raw mut status) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(format!("GetSystemPowerStatus failed (error {err})"));
        }
        Ok(RawPowerStatus {
            ac_line_status: status.ACLineStatus,
            battery_flag: status.BatteryFlag,
            battery_life_percent: status.BatteryLifePercent,
            battery_life_time_secs: status.BatteryLifeTime,
        })
    }

    /// Returns the local timezone offset from UTC in seconds (e.g. `-18000` for EST / UTC-5).
    ///
    /// Win32 defines `UTC = local + bias`, so `local = UTC - bias`.
    pub fn local_utc_offset_secs() -> i32 {
        // SAFETY: `TIME_ZONE_INFORMATION` is a POD Win32 output struct.
        let mut tzi: TIME_ZONE_INFORMATION = unsafe { core::mem::zeroed() };
        let tz_id = unsafe { GetTimeZoneInformation(&raw mut tzi) };
        let total_bias_mins: i32 = match tz_id {
            TIME_ZONE_ID_STANDARD => tzi.Bias.saturating_add(tzi.StandardBias),
            TIME_ZONE_ID_DAYLIGHT => tzi.Bias.saturating_add(tzi.DaylightBias),
            TIME_ZONE_ID_UNKNOWN => tzi.Bias,
            _ => return 0,
        };
        total_bias_mins.saturating_neg().saturating_mul(60)
    }
}

#[cfg(windows)]
pub use win::{local_utc_offset_secs, query_system_power_status};

#[cfg(not(windows))]
pub fn query_system_power_status() -> Result<RawPowerStatus, String> {
    Err("GetSystemPowerStatus is only available on Windows".to_owned())
}

#[cfg(not(windows))]
pub fn local_utc_offset_secs() -> i32 {
    0
}
