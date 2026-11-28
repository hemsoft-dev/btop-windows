//! Tailscale interface detection, strict bind-address validation, peer IP filtering, and
//! boot-time backoff retry for the Fleet Agent.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::FleetError;

/// Default initial retry delay when waiting for Tailscale to come up at boot (`1s`).
pub const DEFAULT_BACKOFF_INITIAL_MS: u64 = 1_000;

/// Default maximum retry delay when waiting for Tailscale to come up at boot (`15s`).
pub const DEFAULT_BACKOFF_MAX_MS: u64 = 15_000;

/// Interface-attributed unicast address supplied by the host network enumerator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalInterfaceAddr {
    pub interface_name: String,
    pub description: String,
    pub is_point_to_point: bool,
    pub ip: IpAddr,
}

/// Backoff schedule used while waiting for a local Tailscale interface at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffConfig {
    pub initial: Duration,
    pub max: Duration,
    /// Optional attempt cap (`None` retries until a Tailscale IP appears or `stop` is signaled).
    pub max_attempts: Option<usize>,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(DEFAULT_BACKOFF_INITIAL_MS),
            max: Duration::from_millis(DEFAULT_BACKOFF_MAX_MS),
            max_attempts: None,
        }
    }
}

/// Unwraps IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) to their canonical IPv4 form so IP range
/// checks cannot be confused or bypassed by dual-stack representations.
pub const fn canonicalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        IpAddr::V4(v4) => IpAddr::V4(v4),
    }
}

/// Returns `true` when `ip` lies inside Tailscale's CGNAT IPv4 range (`100.64.0.0/10`, RFC 6598).
pub const fn is_tailscale_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (octets[1] & 0b1100_0000) == 64
}

/// Returns `true` when `ip` lies inside Tailscale's IPv6 ULA prefix (`fd7a:115c:a1e0::/48`).
pub const fn is_tailscale_ipv6(ip: Ipv6Addr) -> bool {
    let segs = ip.segments();
    segs[0] == 0xfd7a && segs[1] == 0x115c && segs[2] == 0xa1e0
}

/// Returns `true` when `ip` (including IPv4-mapped IPv6) is within Tailscale's IPv4 or IPv6 range.
pub const fn is_tailscale_ip(ip: IpAddr) -> bool {
    match canonicalize_ip(ip) {
        IpAddr::V4(v4) => is_tailscale_ipv4(v4),
        IpAddr::V6(v6) => is_tailscale_ipv6(v6),
    }
}

/// Returns `true` when `ip` is permitted as a Fleet Agent listener bind address:
/// strictly a Tailscale address (`100.64.0.0/10` or `fd7a:115c:a1e0::/48`) or loopback
/// (`127.0.0.0/8` / `::1` for Tailscale Serve). Never allows `0.0.0.0`, `::`, LAN, or public IPs.
pub const fn is_allowed_bind_ip(ip: IpAddr) -> bool {
    let canon = canonicalize_ip(ip);
    if canon.is_unspecified() {
        return false;
    }
    is_tailscale_ip(canon) || canon.is_loopback()
}

/// Validates that `addr` is safe for the Fleet Agent listener (never `0.0.0.0`, `::`, LAN, or
/// public interfaces).
pub fn validate_bind_addr(addr: SocketAddr) -> Result<SocketAddr, FleetError> {
    let canon_ip = canonicalize_ip(addr.ip());
    if canon_ip.is_unspecified() {
        return Err(FleetError::ForbiddenBindAddr {
            addr: addr.to_string(),
            reason: "binding to wildcard addresses (0.0.0.0 or ::) is forbidden; use auto-detected Tailscale IP or --bind 127.0.0.1:<port>".to_owned(),
        });
    }
    if !is_allowed_bind_ip(canon_ip) {
        return Err(FleetError::ForbiddenBindAddr {
            addr: addr.to_string(),
            reason: "address must be within Tailscale (100.64.0.0/10 or fd7a:115c:a1e0::/48) or loopback (127.0.0.1 / ::1)".to_owned(),
        });
    }
    Ok(SocketAddr::new(canon_ip, addr.port()))
}

/// Enforces the peer source-IP boundary on every accepted TCP connection:
/// - When bound to a Tailscale IP, accepts only peers inside Tailscale ranges (preventing LAN
///   weak-host-model access).
/// - When bound to loopback (`--bind 127.0.0.1:<port>` for Tailscale Serve), accepts only loopback
///   peers.
/// - Fails closed for any other bind address.
pub const fn is_allowed_peer_ip(peer: IpAddr, bind_ip: IpAddr) -> bool {
    let peer = canonicalize_ip(peer);
    let bind = canonicalize_ip(bind_ip);
    if bind.is_loopback() {
        peer.is_loopback()
    } else if is_tailscale_ip(bind) {
        is_tailscale_ip(peer)
    } else {
        false
    }
}

fn has_tailscale_name(addr: &LocalInterfaceAddr) -> bool {
    addr.interface_name
        .to_ascii_lowercase()
        .contains("tailscale")
        || addr.description.to_ascii_lowercase().contains("tailscale")
}

/// Deterministically picks the best Tailscale interface address from `candidates`, preferring
/// Tailscale IPv4 (`100.64.0.0/10`) over Tailscale IPv6 (`fd7a:115c:a1e0::/48`).
///
/// To prevent accidentally binding to an unrelated ISP CGNAT or non-Tailscale VPN interface that
/// happens to use `100.64.0.0/10`, an interface is only accepted as a Tailscale interface when:
/// 1. Its name or description identifies Tailscale (`tailscale0` on Linux, `Tailscale` /
///    `Tailscale Tunnel` on Windows), OR
/// 2. It is a point-to-point tunnel (`utun*` on macOS) carrying a `fd7a:115c:a1e0::/48` Tailscale
///    ULA IPv6 address on the same interface.
pub fn select_tailscale_bind_ip(candidates: &[LocalInterfaceAddr]) -> Option<IpAddr> {
    let p2p_with_ts_v6: HashSet<&str> = candidates
        .iter()
        .filter(|entry| {
            entry.is_point_to_point
                && !entry.interface_name.is_empty()
                && matches!(canonicalize_ip(entry.ip), IpAddr::V6(v6) if is_tailscale_ipv6(v6))
        })
        .map(|entry| entry.interface_name.as_str())
        .collect();

    let mut v4s: Vec<Ipv4Addr> = Vec::new();
    let mut v6s: Vec<Ipv6Addr> = Vec::new();

    for entry in candidates {
        let verified_ts_iface = has_tailscale_name(entry)
            || (entry.is_point_to_point && p2p_with_ts_v6.contains(entry.interface_name.as_str()));
        if !verified_ts_iface {
            continue;
        }
        match canonicalize_ip(entry.ip) {
            IpAddr::V4(v4) if is_tailscale_ipv4(v4) => v4s.push(v4),
            IpAddr::V6(v6) if is_tailscale_ipv6(v6) => v6s.push(v6),
            _ => {}
        }
    }

    v4s.sort_unstable();
    if let Some(first_v4) = v4s.first().copied() {
        return Some(IpAddr::V4(first_v4));
    }
    v6s.sort_unstable();
    v6s.first().copied().map(IpAddr::V6)
}

/// Resolves the Tailscale bind address at startup, retrying with exponential backoff until a local
/// verified Tailscale interface is detected or `stop` is signaled.
pub fn resolve_tailscale_bind_addr_with_backoff<F, W, N>(
    port: u16,
    backoff: BackoffConfig,
    stop: &AtomicBool,
    mut discover_local_addrs: F,
    mut wait: W,
    mut on_waiting: N,
) -> Result<SocketAddr, FleetError>
where
    F: FnMut() -> Result<Vec<LocalInterfaceAddr>, String>,
    W: FnMut(Duration) -> bool,
    N: FnMut(usize, Duration, Option<&str>),
{
    let mut delay = backoff.initial.max(Duration::from_millis(1));
    let max_delay = backoff.max.max(delay);
    let mut attempt = 0usize;

    loop {
        if stop.load(Ordering::SeqCst) {
            return Err(FleetError::BindCancelled);
        }
        attempt += 1;

        let err_msg = match discover_local_addrs() {
            Ok(addrs) => {
                if let Some(ts_ip) = select_tailscale_bind_ip(&addrs) {
                    return validate_bind_addr(SocketAddr::new(ts_ip, port));
                }
                None
            }
            Err(err) => Some(err),
        };

        if let Some(max_attempts) = backoff.max_attempts
            && attempt >= max_attempts
        {
            return Err(FleetError::BindCancelled);
        }

        on_waiting(attempt, delay, err_msg.as_deref());

        if !wait(delay) || stop.load(Ordering::SeqCst) {
            return Err(FleetError::BindCancelled);
        }

        delay = delay.saturating_mul(2).min(max_delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_addr(name: &str, desc: &str, p2p: bool, ip: &str) -> LocalInterfaceAddr {
        LocalInterfaceAddr {
            interface_name: name.to_owned(),
            description: desc.to_owned(),
            is_point_to_point: p2p,
            ip: ip.parse().expect("ip"),
        }
    }

    #[test]
    fn tailscale_ipv4_and_ipv6_ranges_match_exact_cidrs() {
        assert!(!is_tailscale_ipv4(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(is_tailscale_ipv4(Ipv4Addr::new(100, 64, 0, 0)));
        assert!(is_tailscale_ipv4(Ipv4Addr::new(100, 100, 100, 100)));
        assert!(is_tailscale_ipv4(Ipv4Addr::new(100, 127, 255, 255)));
        assert!(!is_tailscale_ipv4(Ipv4Addr::new(100, 128, 0, 0)));
        assert!(!is_tailscale_ipv4(Ipv4Addr::new(192, 168, 1, 10)));
        assert!(!is_tailscale_ipv4(Ipv4Addr::LOCALHOST));

        assert!(is_tailscale_ipv6(
            "fd7a:115c:a1e0::1".parse::<Ipv6Addr>().expect("v6")
        ));
        assert!(is_tailscale_ipv6(
            "fd7a:115c:a1e0:ab12::cafe".parse::<Ipv6Addr>().expect("v6")
        ));
        assert!(!is_tailscale_ipv6(
            "fd7a:115c:a1e1::1".parse::<Ipv6Addr>().expect("v6")
        ));
        assert!(!is_tailscale_ipv6(Ipv6Addr::LOCALHOST));

        let mapped_ts: IpAddr = "::ffff:100.64.10.20".parse().expect("mapped");
        assert!(is_tailscale_ip(mapped_ts));
        let mapped_lan: IpAddr = "::ffff:192.168.1.5".parse().expect("mapped lan");
        assert!(!is_tailscale_ip(mapped_lan));
    }

    #[test]
    fn validate_bind_addr_forbids_wildcard_lan_and_public_ips() {
        assert!(validate_bind_addr("0.0.0.0:9127".parse().expect("addr")).is_err());
        assert!(validate_bind_addr("[::]:9127".parse().expect("addr")).is_err());
        assert!(validate_bind_addr("192.168.1.20:9127".parse().expect("addr")).is_err());
        assert!(validate_bind_addr("10.0.0.5:9127".parse().expect("addr")).is_err());
        assert!(validate_bind_addr("8.8.8.8:9127".parse().expect("addr")).is_err());

        assert!(validate_bind_addr("100.64.1.2:9127".parse().expect("addr")).is_ok());
        assert!(validate_bind_addr("[fd7a:115c:a1e0::5]:9127".parse().expect("addr")).is_ok());
        assert!(validate_bind_addr("127.0.0.1:9127".parse().expect("addr")).is_ok());
        assert!(validate_bind_addr("[::1]:9127".parse().expect("addr")).is_ok());
    }

    #[test]
    fn peer_ip_filter_enforces_matching_scope() {
        let ts_bind: IpAddr = "100.64.1.10".parse().expect("ts bind");
        assert!(is_allowed_peer_ip(
            "100.120.5.9".parse().expect("ts peer"),
            ts_bind
        ));
        assert!(is_allowed_peer_ip(
            "fd7a:115c:a1e0::99".parse().expect("ts v6 peer"),
            ts_bind
        ));
        assert!(!is_allowed_peer_ip(
            "192.168.1.50".parse().expect("lan peer"),
            ts_bind
        ));
        assert!(!is_allowed_peer_ip(
            "127.0.0.1".parse().expect("loopback peer"),
            ts_bind
        ));

        let lo_bind: IpAddr = "127.0.0.1".parse().expect("lo bind");
        assert!(is_allowed_peer_ip(
            "127.0.0.1".parse().expect("lo peer"),
            lo_bind
        ));
        assert!(is_allowed_peer_ip(
            "::1".parse().expect("v6 lo peer"),
            lo_bind
        ));
        assert!(!is_allowed_peer_ip(
            "100.64.1.10".parse().expect("ts peer"),
            lo_bind
        ));
    }

    #[test]
    fn select_tailscale_ip_rejects_non_tailscale_cgnat_and_matches_all_three_os_layouts() {
        // Non-Tailscale ISP CGNAT on eth0 and corporate VPN on wg0 must NEVER be selected
        let non_ts = vec![
            mk_addr("eth0", "Realtek PCIe", false, "100.64.0.1"),
            mk_addr("wg0", "WireGuard Corp", true, "100.64.0.2"),
            mk_addr("utun2", "", true, "100.64.0.3"),
        ];
        assert_eq!(select_tailscale_bind_ip(&non_ts), None);

        // Linux `tailscale0` beats lower non-Tailscale CGNAT on `eth0`
        let linux_addrs = vec![
            mk_addr("eth0", "", false, "100.64.0.1"),
            mk_addr("tailscale0", "", true, "100.97.164.73"),
            mk_addr("tailscale0", "", true, "fd7a:115c:a1e0::9e32:a44c"),
        ];
        assert_eq!(
            select_tailscale_bind_ip(&linux_addrs),
            Some("100.97.164.73".parse().expect("linux ts"))
        );

        // Windows `Tailscale` / `Tailscale Tunnel` adapter
        let win_addrs = vec![
            mk_addr("Ethernet", "Intel I225-V", false, "100.64.1.1"),
            mk_addr("Tailscale", "Tailscale Tunnel", true, "100.101.122.39"),
        ];
        assert_eq!(
            select_tailscale_bind_ip(&win_addrs),
            Some("100.101.122.39".parse().expect("win ts"))
        );

        // macOS `utun4` verified via paired `fd7a:115c:a1e0::/48` Tailscale ULA on same interface
        let mac_addrs = vec![
            mk_addr("utun2", "", true, "100.64.0.5"), // other VPN without Tailscale ULA
            mk_addr("utun4", "", true, "100.69.182.27"),
            mk_addr("utun4", "", true, "fd7a:115c:a1e0::3932:b61c"),
        ];
        assert_eq!(
            select_tailscale_bind_ip(&mac_addrs),
            Some("100.69.182.27".parse().expect("mac ts"))
        );

        // IPv6-only Tailscale fallback
        let v6_only = vec![mk_addr("tailscale0", "", true, "fd7a:115c:a1e0::42")];
        assert_eq!(
            select_tailscale_bind_ip(&v6_only),
            Some("fd7a:115c:a1e0::42".parse().expect("v6 ts"))
        );
    }

    #[test]
    fn backoff_retries_until_tailscale_appears_and_caps_delay() {
        let stop = AtomicBool::new(false);
        let mut calls = 0usize;
        let mut observed_delays = Vec::new();
        let cfg = BackoffConfig {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(4),
            max_attempts: Some(10),
        };

        let resolved = resolve_tailscale_bind_addr_with_backoff(
            9127,
            cfg,
            &stop,
            || {
                calls += 1;
                if calls < 5 {
                    Ok(vec![mk_addr("lo", "", false, "127.0.0.1")])
                } else {
                    Ok(vec![
                        mk_addr("lo", "", false, "127.0.0.1"),
                        mk_addr("tailscale0", "", true, "100.88.1.7"),
                    ])
                }
            },
            |dur| {
                observed_delays.push(dur);
                true
            },
            |_, _, _| {},
        )
        .expect("resolved after backoff");

        assert_eq!(resolved, "100.88.1.7:9127".parse().expect("expected"));
        assert_eq!(
            observed_delays,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(4),
            ]
        );
    }

    #[test]
    fn backoff_stops_promptly_when_cancelled() {
        let stop = AtomicBool::new(false);
        let cfg = BackoffConfig::default();
        let err = resolve_tailscale_bind_addr_with_backoff(
            9127,
            cfg,
            &stop,
            || Ok(Vec::new()),
            |_| {
                stop.store(true, Ordering::SeqCst);
                false
            },
            |_, _, _| {},
        )
        .expect_err("expected cancellation");
        assert!(matches!(err, FleetError::BindCancelled));
    }
}
