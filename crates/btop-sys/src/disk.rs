//! Windows 11 local fixed-drive (`DRIVE_FIXED`) enumeration and non-elevated
//! `IOCTL_DISK_PERFORMANCE` / `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` helpers.

/// Capacity and identity of a single local fixed volume (`C:\`, `D:\`, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawFixedDrive {
    pub mount: String,
    pub label: String,
    pub fs_type: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub physical_drive: Option<u32>,
}

/// Raw cumulative counters from `IOCTL_DISK_PERFORMANCE` on `\\.\PhysicalDriveN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawDiskPerformance {
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub idle_time_100ns: i64,
    pub query_time_100ns: i64,
}

#[cfg(windows)]
mod win {
    use core::ffi::c_void;
    use core::mem::size_of;
    use core::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetDriveTypeW,
        GetLogicalDriveStringsW, GetVolumeInformationW, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        DISK_PERFORMANCE, IOCTL_DISK_PERFORMANCE, VOLUME_DISK_EXTENTS,
    };

    use super::{RawDiskPerformance, RawFixedDrive};

    const DRIVE_FIXED_U32: u32 = 3;
    /// From Windows SDK `winioctl.h`:
    /// `CTL_CODE(IOCTL_VOLUME_BASE /* 'V' = 0x56 */, 0, METHOD_BUFFERED, FILE_ANY_ACCESS)`.
    const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;

    struct SafeHandle(HANDLE);

    impl Drop for SafeHandle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                // SAFETY: `self.0` is an exclusively owned handle opened by `CreateFileW`.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    /// Enumerates local fixed drive letters (`DRIVE_FIXED`), collecting capacity via
    /// `GetDiskFreeSpaceExW`, volume label/filesystem via `GetVolumeInformationW`, and
    /// primary physical disk index via `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`.
    pub fn enumerate_fixed_drives() -> Result<Vec<RawFixedDrive>, String> {
        // SAFETY: Passing 0 length queries required UTF-16 buffer length.
        let needed = unsafe { GetLogicalDriveStringsW(0, ptr::null_mut()) };
        if needed == 0 {
            return Err("GetLogicalDriveStringsW returned 0".to_owned());
        }
        let mut buf = vec![0u16; (needed as usize).saturating_add(2)];
        // SAFETY: `buf` is large enough for all drive strings plus double NUL terminator.
        let required = unsafe {
            GetLogicalDriveStringsW(
                u32::try_from(buf.len()).unwrap_or(u32::MAX),
                buf.as_mut_ptr(),
            )
        };
        if required == 0 {
            return Err("GetLogicalDriveStringsW failed".to_owned());
        }

        let used_len = (required as usize).min(buf.len());
        let mut drives = Vec::new();
        let mut start = 0usize;
        while start < used_len {
            let Some(rel_end) = buf[start..used_len].iter().position(|&c| c == 0) else {
                break;
            };
            if rel_end == 0 {
                break;
            }
            let slice = &buf[start..start + rel_end];
            let mut wide_nul = Vec::with_capacity(slice.len() + 1);
            wide_nul.extend_from_slice(slice);
            wide_nul.push(0);

            // SAFETY: `wide_nul` is NUL-terminated UTF-16 root path (e.g., `C:\`).
            let drive_type = unsafe { GetDriveTypeW(wide_nul.as_ptr()) };
            if drive_type == DRIVE_FIXED_U32
                && let Some((total_bytes, free_bytes)) = query_disk_free_space(&wide_nul)
            {
                let mount = String::from_utf16_lossy(slice);
                let (label, fs_type) = query_volume_info(&wide_nul);
                let physical_drive = query_volume_primary_disk(&mount);
                drives.push(RawFixedDrive {
                    mount,
                    label,
                    fs_type,
                    total_bytes,
                    free_bytes: free_bytes.min(total_bytes),
                    physical_drive,
                });
            }

            start += rel_end + 1;
        }
        Ok(drives)
    }

    fn query_disk_free_space(root_wide: &[u16]) -> Option<(u64, u64)> {
        let mut free_avail = 0u64;
        let mut total_bytes = 0u64;
        let mut total_free = 0u64;
        // SAFETY: `root_wide` is NUL-terminated UTF-16; output pointers are valid stack locals.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                root_wide.as_ptr(),
                &raw mut free_avail,
                &raw mut total_bytes,
                &raw mut total_free,
            )
        };
        if ok == 0 {
            None
        } else {
            Some((total_bytes, total_free))
        }
    }

    fn query_volume_info(root_wide: &[u16]) -> (String, String) {
        let mut vol_name = [0u16; 261];
        let mut fs_name = [0u16; 65];
        // SAFETY: `root_wide` is NUL-terminated; buffers have explicit lengths matching array sizes.
        let ok = unsafe {
            GetVolumeInformationW(
                root_wide.as_ptr(),
                vol_name.as_mut_ptr(),
                u32::try_from(vol_name.len()).unwrap_or(261),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                fs_name.as_mut_ptr(),
                u32::try_from(fs_name.len()).unwrap_or(65),
            )
        };
        if ok == 0 {
            return (String::new(), String::new());
        }
        let vol_len = vol_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(vol_name.len());
        let fs_len = fs_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(fs_name.len());
        (
            String::from_utf16_lossy(&vol_name[..vol_len]),
            String::from_utf16_lossy(&fs_name[..fs_len]),
        )
    }

    fn query_volume_primary_disk(mount: &str) -> Option<u32> {
        let letter = mount.chars().next()?;
        if !letter.is_ascii_alphabetic() {
            return None;
        }
        let device_path = format!("\\\\.\\{}:", letter.to_ascii_uppercase());
        let handle = open_device_metadata(&device_path)?;

        // SAFETY: `VOLUME_DISK_EXTENTS` is plain-old-data with 1 inline `DISK_EXTENT`.
        // Per v1 scope (`AGENTS.md`), single-extent local fixed volumes use the first extent.
        let mut extents: VOLUME_DISK_EXTENTS = unsafe { core::mem::zeroed() };
        let mut returned = 0u32;
        // SAFETY: `handle.0` is a valid device handle; `extents` is properly sized and aligned.
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                ptr::null(),
                0,
                (&raw mut extents).cast::<c_void>(),
                u32::try_from(size_of::<VOLUME_DISK_EXTENTS>()).unwrap_or(0),
                &raw mut returned,
                ptr::null_mut(),
            )
        };
        if ok == 0
            || (returned as usize) < size_of::<VOLUME_DISK_EXTENTS>()
            || extents.NumberOfDiskExtents == 0
        {
            return None;
        }
        Some(extents.Extents[0].DiskNumber)
    }

    /// Queries `IOCTL_DISK_PERFORMANCE` non-elevated on `\\.\PhysicalDrive{disk_number}`
    /// opened with access `0` (`FILE_SHARE_READ | FILE_SHARE_WRITE`).
    pub fn query_physical_drive_performance(
        disk_number: u32,
    ) -> Result<RawDiskPerformance, String> {
        let device_path = format!("\\\\.\\PhysicalDrive{disk_number}");
        let handle = open_device_metadata(&device_path)
            .ok_or_else(|| format!("CreateFileW failed for {device_path}"))?;

        // SAFETY: `DISK_PERFORMANCE` is plain-old-data filled by `DeviceIoControl`.
        let mut perf: DISK_PERFORMANCE = unsafe { core::mem::zeroed() };
        let mut returned = 0u32;
        // SAFETY: `handle.0` is valid and `perf` has exact `size_of::<DISK_PERFORMANCE>()` capacity.
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                IOCTL_DISK_PERFORMANCE,
                ptr::null(),
                0,
                (&raw mut perf).cast::<c_void>(),
                u32::try_from(size_of::<DISK_PERFORMANCE>()).unwrap_or(0),
                &raw mut returned,
                ptr::null_mut(),
            )
        };
        if ok == 0 || (returned as usize) < size_of::<DISK_PERFORMANCE>() {
            return Err(format!("IOCTL_DISK_PERFORMANCE failed for {device_path}"));
        }

        let bytes_read = u64::try_from(perf.BytesRead).unwrap_or(0);
        let bytes_written = u64::try_from(perf.BytesWritten).unwrap_or(0);
        Ok(RawDiskPerformance {
            bytes_read,
            bytes_written,
            idle_time_100ns: perf.IdleTime,
            query_time_100ns: perf.QueryTime,
        })
    }

    fn open_device_metadata(path: &str) -> Option<SafeHandle> {
        let wide: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
        // SAFETY: `wide` is NUL-terminated UTF-16; access `0` opens device metadata/IOCTL
        // handle without requiring administrator elevation.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            None
        } else {
            Some(SafeHandle(handle))
        }
    }
}

#[cfg(not(windows))]
mod stub {
    use super::{RawDiskPerformance, RawFixedDrive};

    pub fn enumerate_fixed_drives() -> Result<Vec<RawFixedDrive>, String> {
        Err("Windows disk telemetry is only available on Windows".to_owned())
    }

    pub fn query_physical_drive_performance(
        _disk_number: u32,
    ) -> Result<RawDiskPerformance, String> {
        Err("Windows disk I/O telemetry is only available on Windows".to_owned())
    }
}

#[cfg(not(windows))]
pub use stub::{enumerate_fixed_drives, query_physical_drive_performance};
#[cfg(windows)]
pub use win::{enumerate_fixed_drives, query_physical_drive_performance};
