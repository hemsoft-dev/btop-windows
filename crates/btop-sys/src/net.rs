//! Windows 11 64-bit IP Helper (`GetIfTable2`, `GetIfEntry2`, `GetAdaptersAddresses`)
//! network interface telemetry helpers keyed by `NET_LUID`.

/// Hardware network interface metadata, addresses, and 64-bit cumulative byte counters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawNetInterface {
    pub luid: u64,
    pub name: String,
    pub description: String,
    pub is_up: bool,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub in_octets: u64,
    pub out_octets: u64,
    pub link_speed_bps: Option<u64>,
}

/// Refreshed 64-bit counters returned by `GetIfEntry2` for one `NET_LUID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawNetEntry {
    pub is_up: bool,
    pub in_octets: u64,
    pub out_octets: u64,
    pub link_speed_bps: Option<u64>,
}

#[cfg(windows)]
mod win {
    use core::mem::size_of;
    use core::ptr;
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, Ipv6Addr};

    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, GetIfEntry2, GetIfTable2, IP_ADAPTER_ADDRESSES_LH, MIB_IF_ROW2,
        MIB_IF_TABLE2,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6,
    };

    use super::{RawNetEntry, RawNetInterface};

    // Bit positions in `MIB_IF_ROW2.InterfaceAndOperStatusFlags._bitfield` (`netioapi.h`):
    const FLAG_HARDWARE_INTERFACE: u8 = 1 << 0;
    const FLAG_FILTER_INTERFACE: u8 = 1 << 1;
    const FLAG_CONNECTOR_PRESENT: u8 = 1 << 2;

    struct TableGuard(*mut MIB_IF_TABLE2);

    impl Drop for TableGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` was allocated by `GetIfTable2`.
                unsafe {
                    FreeMibTable(self.0.cast());
                }
            }
        }
    }

    /// Enumerates hardware network interfaces via `GetIfTable2`
    /// (`HardwareInterface && !FilterInterface && ConnectorPresent`) and joins IPv4/IPv6
    /// unicast addresses via `GetAdaptersAddresses` keyed by `NET_LUID`.
    pub fn enumerate_hardware_interfaces() -> Result<Vec<RawNetInterface>, String> {
        let addr_map = query_adapter_addresses_by_luid();

        let mut table_ptr: *mut MIB_IF_TABLE2 = ptr::null_mut();
        // SAFETY: `GetIfTable2` allocates a `MIB_IF_TABLE2` freed via `FreeMibTable` below.
        let status = unsafe { GetIfTable2(&raw mut table_ptr) };
        if status != NO_ERROR || table_ptr.is_null() {
            return Err(format!("GetIfTable2 failed with code {status}"));
        }

        let _guard = TableGuard(table_ptr);

        // SAFETY: `table_ptr` is valid and `NumEntries` bounds the flexible `Table` array.
        let num_entries = unsafe { (*table_ptr).NumEntries as usize };
        let rows_ptr = unsafe { ptr::addr_of!((*table_ptr).Table).cast::<MIB_IF_ROW2>() };

        let mut result = Vec::new();
        for idx in 0..num_entries {
            // SAFETY: `idx < num_entries` is strictly within the table allocated by `GetIfTable2`.
            let row = unsafe { &*rows_ptr.add(idx) };
            let flags = row.InterfaceAndOperStatusFlags._bitfield;
            let is_hardware = (flags & FLAG_HARDWARE_INTERFACE) != 0;
            let is_filter = (flags & FLAG_FILTER_INTERFACE) != 0;
            let connector_present = (flags & FLAG_CONNECTOR_PRESENT) != 0;
            if !is_hardware || is_filter || !connector_present {
                continue;
            }

            // SAFETY: `InterfaceLuid.Value` is a plain `u64` union field.
            let luid = unsafe { row.InterfaceLuid.Value };
            let alias = decode_utf16_array(&row.Alias);
            let description = decode_utf16_array(&row.Description);
            let name = if alias.is_empty() {
                description.clone()
            } else {
                alias
            };
            let (ipv4, ipv6) = addr_map.get(&luid).cloned().unwrap_or((None, None));
            let link_speed = row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed);
            let link_speed_bps = (link_speed > 0 && link_speed != u64::MAX).then_some(link_speed);

            result.push(RawNetInterface {
                luid,
                name,
                description,
                is_up: row.OperStatus == IfOperStatusUp,
                ipv4,
                ipv6,
                in_octets: row.InOctets,
                out_octets: row.OutOctets,
                link_speed_bps,
            });
        }

        // Prefer active (`is_up`) interfaces first, then stable name ordering.
        result.sort_by(|a, b| b.is_up.cmp(&a.is_up).then_with(|| a.name.cmp(&b.name)));
        Ok(result)
    }

    /// Refreshes 64-bit byte counters for a single interface keyed by `NET_LUID` via `GetIfEntry2`.
    pub fn refresh_interface_entry(luid: u64) -> Result<RawNetEntry, String> {
        // SAFETY: `MIB_IF_ROW2` is plain-old-data zero-initialized except `InterfaceLuid.Value`.
        let mut row: MIB_IF_ROW2 = unsafe { core::mem::zeroed() };
        row.InterfaceLuid.Value = luid;
        // SAFETY: `row` is valid and keyed by `InterfaceLuid`.
        let status = unsafe { GetIfEntry2(&raw mut row) };
        if status != NO_ERROR {
            return Err(format!("GetIfEntry2 failed for LUID {luid}: code {status}"));
        }
        let link_speed = row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed);
        let link_speed_bps = (link_speed > 0 && link_speed != u64::MAX).then_some(link_speed);
        Ok(RawNetEntry {
            is_up: row.OperStatus == IfOperStatusUp,
            in_octets: row.InOctets,
            out_octets: row.OutOctets,
            link_speed_bps,
        })
    }

    fn query_adapter_addresses_by_luid() -> HashMap<u64, (Option<String>, Option<String>)> {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut buf_size = 16 * 1024u32;
        let mut raw: Vec<u64> = Vec::new();

        for _ in 0..4 {
            let u64_len = (buf_size as usize).div_ceil(size_of::<u64>()).max(1);
            raw.resize(u64_len, 0);
            let mut out_size = u32::try_from(raw.len() * size_of::<u64>()).unwrap_or(u32::MAX);
            // SAFETY: `raw` is 8-byte aligned with `out_size` bytes of capacity.
            let status = unsafe {
                GetAdaptersAddresses(
                    u32::from(AF_UNSPEC),
                    flags,
                    ptr::null(),
                    raw.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                    &raw mut out_size,
                )
            };
            if status == NO_ERROR {
                return parse_adapter_addresses(raw.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>());
            }
            if status == ERROR_BUFFER_OVERFLOW && out_size > buf_size {
                buf_size = out_size.saturating_add(4096);
            } else {
                break;
            }
        }
        HashMap::new()
    }

    fn parse_adapter_addresses(
        mut cur: *const IP_ADAPTER_ADDRESSES_LH,
    ) -> HashMap<u64, (Option<String>, Option<String>)> {
        let mut map = HashMap::new();
        let mut visited = 0usize;
        while !cur.is_null() && visited < 512 {
            visited += 1;
            // SAFETY: `cur` points to a valid node in the buffer populated by `GetAdaptersAddresses`.
            let adapter = unsafe { &*cur };
            // SAFETY: `Luid.Value` is a plain `u64` union field.
            let luid = unsafe { adapter.Luid.Value };

            let mut ipv4: Option<String> = None;
            let mut ipv6: Option<String> = None;
            let mut ipv6_link_local: Option<String> = None;

            let mut uni = adapter.FirstUnicastAddress.cast_const();
            let mut uni_count = 0usize;
            while !uni.is_null() && uni_count < 64 {
                uni_count += 1;
                // SAFETY: `uni` is a valid unicast address node within the same Win32 buffer.
                let node = unsafe { &*uni };
                let sa = node.Address.lpSockaddr;
                let sa_len = usize::try_from(node.Address.iSockaddrLength).unwrap_or(0);
                if !sa.is_null() {
                    // SAFETY: `sa` points to a valid `SOCKADDR` whose family identifies its subtype.
                    let family = unsafe { (*sa).sa_family };
                    if family == AF_INET && sa_len >= size_of::<SOCKADDR_IN>() {
                        // SAFETY: `sa_len >= size_of::<SOCKADDR_IN>()` and `sa_family == AF_INET`.
                        #[allow(clippy::cast_ptr_alignment)]
                        let sin = unsafe { sa.cast::<SOCKADDR_IN>().read_unaligned() };
                        // SAFETY: `S_un.S_addr` is the 4-byte IPv4 address in network byte order.
                        let bytes = unsafe { sin.sin_addr.S_un.S_addr }.to_ne_bytes();
                        if ipv4.is_none() {
                            ipv4 = Some(Ipv4Addr::from(bytes).to_string());
                        }
                    } else if family == AF_INET6 && sa_len >= size_of::<SOCKADDR_IN6>() {
                        // SAFETY: `sa_len >= size_of::<SOCKADDR_IN6>()` and `sa_family == AF_INET6`.
                        #[allow(clippy::cast_ptr_alignment)]
                        let sin6 = unsafe { sa.cast::<SOCKADDR_IN6>().read_unaligned() };
                        // SAFETY: `sin6_addr.u.Byte` is the 16-byte IPv6 address.
                        let bytes = unsafe { sin6.sin6_addr.u.Byte };
                        let addr = Ipv6Addr::from(bytes);
                        if is_ipv6_link_local(addr) {
                            if ipv6_link_local.is_none() {
                                ipv6_link_local = Some(addr.to_string());
                            }
                        } else if ipv6.is_none() {
                            ipv6 = Some(addr.to_string());
                        }
                    }
                }
                uni = node.Next.cast_const();
            }

            map.insert(luid, (ipv4, ipv6.or(ipv6_link_local)));
            cur = adapter.Next.cast_const();
        }
        map
    }

    fn is_ipv6_link_local(addr: Ipv6Addr) -> bool {
        let seg0 = addr.segments()[0];
        (seg0 & 0xffc0) == 0xfe80
    }

    fn decode_utf16_array(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end]).trim().to_owned()
    }
}

#[cfg(not(windows))]
mod stub {
    use super::{RawNetEntry, RawNetInterface};

    pub fn enumerate_hardware_interfaces() -> Result<Vec<RawNetInterface>, String> {
        Err("Windows IP Helper telemetry is only available on Windows".to_owned())
    }

    pub fn refresh_interface_entry(_luid: u64) -> Result<RawNetEntry, String> {
        Err("Windows IP Helper telemetry is only available on Windows".to_owned())
    }
}

#[cfg(not(windows))]
pub use stub::{enumerate_hardware_interfaces, refresh_interface_entry};
#[cfg(windows)]
pub use win::{enumerate_hardware_interfaces, refresh_interface_entry};
