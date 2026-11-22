//! Deterministic synthetic Windows 11 x64 fixtures for `--replay` and golden tests.

use crate::{
    CoreKind, CoreSample, CpuSnapshot, DataState, DiskSnapshot, GpuSnapshot, LogicalCpuId,
    MemSnapshot, NetInterfaceSnapshot, ProcessEntry, ProcessIdentity, ServiceEntry, Snapshot,
};

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

/// Representative 8-core (16-LP) Windows 11 x64 desktop with a single discrete GPU,
/// no battery, and 50 processes including a Windows PID-reuse edge case.
pub fn win11_8core_single_gpu_desktop() -> Vec<Snapshot> {
    (0..3u8)
        .map(|tick| {
            let step = u64::from(tick);
            let delta = f32::from(tick);
            let cores = (0..16u8)
                .map(|idx| {
                    let wave = f32::from((idx.wrapping_mul(7).wrapping_add(tick * 5)) % 45);
                    CoreSample {
                        id: LogicalCpuId {
                            group: 0,
                            number: idx,
                        },
                        busy_pct: 12.0 + wave,
                        kind: CoreKind::Uniform,
                        temp_celsius: None,
                    }
                })
                .collect::<Vec<_>>();
            #[allow(clippy::cast_precision_loss)]
            let total_busy = cores.iter().map(|c| c.busy_pct).sum::<f32>() / (cores.len() as f32);

            Snapshot {
                timestamp_ms: 1_735_689_600_000 + step * 2000,
                hostname: "WIN11-RYZEN8".to_owned(),
                username: "alex".to_owned(),
                total_phys_mem_bytes: None,
                cpu: DataState::Available(CpuSnapshot {
                    model_name: "AMD Ryzen 7 7800X3D 8-Core Processor".to_owned(),
                    total_busy_pct: (total_busy * 10.0).round() / 10.0,
                    utility_pct: Some(((total_busy + 4.2) * 10.0).round() / 10.0),
                    freq_mhz: Some(4_850 + u32::from(tick) * 50),
                    freq_range_mhz: None,
                    package_temp_celsius: None,
                    package_watts: None,
                    run_queue_ewma: [1.2 + delta * 0.1, 0.9, 0.7],
                    uptime_secs: 14_400 + step * 2,
                    cores,
                }),
                mem: DataState::Available(MemSnapshot {
                    total_bytes: 32 * GIB,
                    used_bytes: (14 * GIB) + step * 64 * MIB,
                    available_bytes: (18 * GIB) - step * 64 * MIB,
                    cached_bytes: 6 * GIB,
                    free_bytes: 12 * GIB - step * 64 * MIB,
                    commit_used_bytes: (17 * GIB) + step * 64 * MIB,
                    commit_limit_bytes: 37 * GIB,
                    pagefile_used_bytes: 512 * MIB,
                    pagefile_total_bytes: 5 * GIB,
                    compressed_bytes: Some(320 * MIB),
                }),
                disks: DataState::Available(synthetic_disks(step, delta)),
                net: DataState::Available(synthetic_net(step)),
                processes: DataState::Available(synthetic_processes(50, tick, "alex")),
                services: DataState::Available(synthetic_services()),
                gpus: vec![DataState::Available(GpuSnapshot {
                    index: 0,
                    name: "NVIDIA GeForce RTX 4080".to_owned(),
                    vendor: "NVIDIA".to_owned(),
                    pci_bdf: Some("0000:01:00.0".to_owned()),
                    utilization_pct: Some(24.0 + delta * 3.5),
                    vram_used_bytes: Some(4 * GIB + step * 128 * MIB),
                    vram_total_bytes: Some(16 * GIB),
                    shared_vram_used_bytes: None,
                    shared_vram_total_bytes: None,
                    luid: None,
                    core_clock_mhz: Some(2_475),
                    mem_clock_mhz: Some(11_200),
                    temp_celsius: Some(46.0 + delta),
                    temp_max_celsius: None,
                    power_watts: Some(112.5 + delta * 4.0),
                    power_limit_watts: Some(320.0),
                    pstate: None,
                    pcie_tx_kbs: Some(48_000),
                    pcie_rx_kbs: Some(62_000),
                    encoder_pct: Some(2.0),
                    decoder_pct: Some(6.0),
                })],
                battery: DataState::Unavailable("no battery detected".to_owned()),
            }
        })
        .collect()
}

/// Representative 16-core hybrid P/E Windows 11 x64 desktop with dual GPUs
/// (discrete NVIDIA + integrated AMD), no battery, and 52 processes including PID reuse.
pub fn win11_16core_dual_gpu_desktop() -> Vec<Snapshot> {
    (0..3u8)
        .map(|tick| {
            let step = u64::from(tick);
            let delta = f32::from(tick);
            let cores = (0..16u8)
                .map(|idx| {
                    let kind = if idx < 8 {
                        CoreKind::Performance
                    } else {
                        CoreKind::Efficiency
                    };
                    let base = if matches!(kind, CoreKind::Performance) {
                        28.0
                    } else {
                        9.0
                    };
                    let wave = f32::from((idx.wrapping_mul(11).wrapping_add(tick * 7)) % 35);
                    CoreSample {
                        id: LogicalCpuId {
                            group: 0,
                            number: idx,
                        },
                        busy_pct: base + wave,
                        kind,
                        temp_celsius: None,
                    }
                })
                .collect::<Vec<_>>();
            #[allow(clippy::cast_precision_loss)]
            let total_busy = cores.iter().map(|c| c.busy_pct).sum::<f32>() / (cores.len() as f32);

            Snapshot {
                timestamp_ms: 1_735_693_200_000 + step * 2000,
                hostname: "WIN11-HYBRID16".to_owned(),
                username: "franz".to_owned(),
                total_phys_mem_bytes: None,
                cpu: DataState::Available(CpuSnapshot {
                    model_name: "Intel Core i9-14900K (8P+8E)".to_owned(),
                    total_busy_pct: (total_busy * 10.0).round() / 10.0,
                    utility_pct: Some(((total_busy + 5.1) * 10.0).round() / 10.0),
                    freq_mhz: Some(5_300 + u32::from(tick) * 100),
                    freq_range_mhz: None,
                    package_temp_celsius: None,
                    package_watts: None,
                    run_queue_ewma: [2.4 + delta * 0.2, 1.8, 1.3],
                    uptime_secs: 86_400 + step * 2,
                    cores,
                }),
                mem: DataState::Available(MemSnapshot {
                    total_bytes: 64 * GIB,
                    used_bytes: (26 * GIB) + step * 128 * MIB,
                    available_bytes: (38 * GIB) - step * 128 * MIB,
                    cached_bytes: 11 * GIB,
                    free_bytes: 27 * GIB - step * 128 * MIB,
                    commit_used_bytes: (31 * GIB) + step * 128 * MIB,
                    commit_limit_bytes: 72 * GIB,
                    pagefile_used_bytes: GIB,
                    pagefile_total_bytes: 8 * GIB,
                    compressed_bytes: Some(512 * MIB),
                }),
                disks: DataState::Available(synthetic_disks(step, delta)),
                net: DataState::Available(synthetic_net(step)),
                processes: DataState::Available(synthetic_processes(52, tick, "franz")),
                services: DataState::Available(synthetic_services()),
                gpus: dual_gpu_snapshots(step, delta),
                battery: DataState::Unavailable("no battery detected".to_owned()),
            }
        })
        .collect()
}

fn synthetic_net(step: u64) -> Vec<NetInterfaceSnapshot> {
    vec![
        NetInterfaceSnapshot {
            luid: 0x0006_0000_0100_0000,
            name: "Ethernet".to_owned(),
            description: "Realtek PCIe 2.5GbE Family Controller".to_owned(),
            is_up: true,
            ipv4: Some("192.168.1.42".to_owned()),
            ipv6: Some("fe80::1a2b:3c4d:5e6f:7890".to_owned()),
            rx_bytes_per_sec: (18 + step * 9) * MIB,
            tx_bytes_per_sec: (6 + step * 4) * MIB,
            rx_total_bytes: 14 * GIB + step * 36 * MIB,
            tx_total_bytes: 3 * GIB + step * 12 * MIB,
            link_speed_bps: Some(2_500_000_000),
        },
        NetInterfaceSnapshot {
            luid: 0x0047_0000_0200_0000,
            name: "Wi-Fi".to_owned(),
            description: "Intel(R) Wi-Fi 7 BE200 320MHz".to_owned(),
            is_up: true,
            ipv4: Some("192.168.1.88".to_owned()),
            ipv6: None,
            rx_bytes_per_sec: (2 + step) * MIB,
            tx_bytes_per_sec: 512 * 1024 + step * 256 * 1024,
            rx_total_bytes: 2 * GIB + step * 4 * MIB,
            tx_total_bytes: 640 * MIB + step * MIB,
            link_speed_bps: Some(2_400_000_000),
        },
    ]
}

fn synthetic_disks(step: u64, delta: f32) -> Vec<DiskSnapshot> {
    vec![
        DiskSnapshot {
            mount: "C:\\".to_owned(),
            label: "Windows".to_owned(),
            fs_type: "NTFS".to_owned(),
            physical_drive: Some(0),
            total_bytes: 1_000 * GIB,
            used_bytes: 462 * GIB + step * 16 * MIB,
            free_bytes: 538 * GIB - step * 16 * MIB,
            read_bytes_per_sec: Some((85 + step * 45) * MIB),
            write_bytes_per_sec: Some((32 + step * 28) * MIB),
            busy_pct: Some(14.5 + delta * 12.0),
        },
        DiskSnapshot {
            mount: "D:\\".to_owned(),
            label: "Data".to_owned(),
            fs_type: "NTFS".to_owned(),
            physical_drive: Some(1),
            total_bytes: 2_000 * GIB,
            used_bytes: 1_240 * GIB + step * 32 * MIB,
            free_bytes: 760 * GIB - step * 32 * MIB,
            read_bytes_per_sec: Some((210 + step * 60) * MIB),
            write_bytes_per_sec: Some((140 + step * 50) * MIB),
            busy_pct: Some(36.0 + delta * 15.0),
        },
    ]
}

fn dual_gpu_snapshots(step: u64, delta: f32) -> Vec<DataState<GpuSnapshot>> {
    vec![
        DataState::Available(GpuSnapshot {
            index: 0,
            name: "NVIDIA GeForce RTX 5090".to_owned(),
            vendor: "NVIDIA".to_owned(),
            pci_bdf: Some("0000:01:00.0".to_owned()),
            utilization_pct: Some(41.0 + delta * 4.0),
            vram_used_bytes: Some(9 * GIB + step * 256 * MIB),
            vram_total_bytes: Some(32 * GIB),
            shared_vram_used_bytes: None,
            shared_vram_total_bytes: None,
            luid: None,
            core_clock_mhz: Some(2_790),
            mem_clock_mhz: Some(14_000),
            temp_celsius: Some(54.0 + delta),
            temp_max_celsius: None,
            power_watts: Some(218.0 + delta * 9.5),
            power_limit_watts: Some(575.0),
            pstate: None,
            pcie_tx_kbs: Some(115_000),
            pcie_rx_kbs: Some(140_000),
            encoder_pct: Some(5.0),
            decoder_pct: Some(12.0),
        }),
        DataState::Available(GpuSnapshot {
            index: 1,
            name: "AMD Radeon(TM) Graphics".to_owned(),
            vendor: "AMD".to_owned(),
            pci_bdf: None,
            utilization_pct: Some(4.5 + delta * 0.5),
            vram_used_bytes: Some(256 * MIB),
            vram_total_bytes: Some(512 * MIB),
            shared_vram_used_bytes: None,
            shared_vram_total_bytes: None,
            luid: None,
            core_clock_mhz: None,
            mem_clock_mhz: None,
            temp_celsius: None,
            temp_max_celsius: None,
            power_watts: None,
            power_limit_watts: None,
            pstate: None,
            pcie_tx_kbs: None,
            pcie_rx_kbs: None,
            encoder_pct: None,
            decoder_pct: None,
        }),
    ]
}

fn synthetic_processes(count: usize, tick: u8, user: &str) -> Vec<ProcessEntry> {
    // Base FILETIME around 2025-01-01 UTC in 100ns units.
    let boot_time: u64 = 133_801_632_000_000_000;
    let step = u64::from(tick);
    let delta = f32::from(tick);
    let mut list = Vec::with_capacity(count);

    let base_entries: &[(u32, u32, &str, bool, u32, f32, u64)] = &[
        (4, 0, "System", true, 168, 0.8, 12 * MIB),
        (104, 4, "Registry", true, 4, 0.0, 64 * MIB),
        (512, 4, "smss.exe", true, 3, 0.0, 2 * MIB),
        (720, 512, "csrss.exe", true, 14, 0.2, 6 * MIB),
        (844, 720, "wininit.exe", true, 3, 0.0, 4 * MIB),
        (912, 844, "services.exe", true, 10, 0.3, 14 * MIB),
        (940, 844, "lsass.exe", true, 12, 0.1, 22 * MIB),
        (1204, 912, "svchost.exe", false, 24, 0.4, 38 * MIB),
        (1416, 912, "dwm.exe", true, 18, 2.6, 142 * MIB),
        (2048, 720, "explorer.exe", false, 64, 1.4, 185 * MIB),
        (3120, 2048, "WindowsTerminal.exe", false, 22, 1.8, 116 * MIB),
        (3412, 3120, "pwsh.exe", false, 14, 0.6, 84 * MIB),
        (4100, 2048, "Code.exe", false, 48, 4.9, 640 * MIB),
        (
            4216,
            4100,
            "rust-analyzer.exe",
            false,
            18,
            12.4,
            1_450 * MIB,
        ),
        (4500, 2048, "msedge.exe", false, 52, 3.2, 520 * MIB),
    ];

    for (idx, &(proc_id, parent_pid, name, protected, threads, cpu_pct, mem)) in
        base_entries.iter().enumerate()
    {
        list.push(ProcessEntry {
            identity: ProcessIdentity {
                pid: proc_id,
                create_time_100ns: boot_time + (idx as u64) * 10_000_000,
            },
            parent_pid,
            name: name.to_owned(),
            image_path: None,
            command_line: if protected {
                None
            } else {
                Some(format!("C:\\Windows\\System32\\{name}"))
            },
            command_line_denied: false,
            user: Some(if protected {
                "NT AUTHORITY\\SYSTEM".to_owned()
            } else {
                format!("WIN11\\{user}")
            }),
            user_denied: false,
            threads,
            cpu_pct: cpu_pct + delta * 0.2,
            working_set_private_bytes: mem + step * MIB,
            io_read_bytes: (idx as u64 + 1) * 4 * MIB + step * 256 * 1024,
            io_write_bytes: (idx as u64 + 1) * MIB + step * 64 * 1024,
            is_protected: protected,
        });
    }

    push_pid_reuse_case(&mut list, boot_time, tick, user);
    push_background_workers(&mut list, count, boot_time, tick, user);
    list
}

fn push_pid_reuse_case(list: &mut Vec<ProcessEntry>, boot_time: u64, tick: u8, user: &str) {
    // Explicit Windows PID-reuse case:
    // PID 4820 was reused for an unrelated newer process whose create_time is newer
    // than orphaned child PID 4824, and its identity changes again on tick >= 1.
    let reused_parent_create = if tick == 0 {
        boot_time + 900_000_000_000
    } else {
        boot_time + 1_200_000_000_000 + u64::from(tick) * 20_000_000
    };
    list.push(ProcessEntry {
        identity: ProcessIdentity {
            pid: 4820,
            create_time_100ns: reused_parent_create,
        },
        parent_pid: 2048,
        name: if tick == 0 {
            "old_launcher.exe".to_owned()
        } else {
            "reused_pid_worker.exe".to_owned()
        },
        image_path: None,
        command_line: Some("C:\\Tools\\worker.exe --reused-pid".to_owned()),
        command_line_denied: false,
        user: Some(format!("WIN11\\{user}")),
        user_denied: false,
        threads: 6,
        cpu_pct: 1.1 + f32::from(tick) * 0.3,
        working_set_private_bytes: 28 * MIB,
        io_read_bytes: 2 * MIB,
        io_write_bytes: MIB,
        is_protected: false,
    });
    list.push(ProcessEntry {
        identity: ProcessIdentity {
            pid: 4824,
            create_time_100ns: boot_time + 500_000_000_000,
        },
        parent_pid: 4820,
        name: "orphaned_child.exe".to_owned(),
        image_path: None,
        command_line: Some("C:\\Tools\\orphaned_child.exe".to_owned()),
        command_line_denied: false,
        user: Some(format!("WIN11\\{user}")),
        user_denied: false,
        threads: 4,
        cpu_pct: 0.4,
        working_set_private_bytes: 16 * MIB,
        io_read_bytes: MIB,
        io_write_bytes: 512 * 1024,
        is_protected: false,
    });
}

fn push_background_workers(
    list: &mut Vec<ProcessEntry>,
    count: usize,
    boot_time: u64,
    tick: u8,
    user: &str,
) {
    let templates = [
        ("svchost.exe", 912u32, 8u32, 24 * MIB),
        ("msedge.exe", 4500u32, 12u32, 96 * MIB),
        ("Code.exe", 4100u32, 10u32, 110 * MIB),
        ("cargo.exe", 3412u32, 6u32, 42 * MIB),
        ("conhost.exe", 3120u32, 4u32, 14 * MIB),
    ];
    let mut seq = 0usize;
    while list.len() < count {
        let (name, parent_pid, threads, mem) = templates[seq % templates.len()];
        let proc_id = 5000 + u32::try_from(seq * 4).unwrap_or(0);
        let cpu = f32::from(u8::try_from((seq + usize::from(tick)) % 15).unwrap_or(0)) * 0.3;
        list.push(ProcessEntry {
            identity: ProcessIdentity {
                pid: proc_id,
                create_time_100ns: boot_time + 600_000_000_000 + (seq as u64) * 5_000_000,
            },
            parent_pid,
            name: name.to_owned(),
            image_path: None,
            command_line: Some(format!("C:\\Apps\\{name} --worker={seq}")),
            command_line_denied: false,
            user: Some(format!("WIN11\\{user}")),
            user_denied: false,
            threads,
            cpu_pct: (cpu * 10.0).round() / 10.0,
            working_set_private_bytes: mem + (seq as u64 % 5) * 4 * MIB,
            io_read_bytes: (seq as u64 + 1) * 512 * 1024,
            io_write_bytes: (seq as u64 + 1) * 128 * 1024,
            is_protected: false,
        });
        seq += 1;
    }
}

fn synthetic_services() -> Vec<ServiceEntry> {
    vec![
        ServiceEntry {
            name: "W32Time".to_owned(),
            display_name: "Windows Time".to_owned(),
            state: "Running".to_owned(),
            start_type: "Automatic".to_owned(),
            pid: Some(1204),
        },
        ServiceEntry {
            name: "EventLog".to_owned(),
            display_name: "Windows Event Log".to_owned(),
            state: "Running".to_owned(),
            start_type: "Automatic".to_owned(),
            pid: Some(1204),
        },
        ServiceEntry {
            name: "Spooler".to_owned(),
            display_name: "Print Spooler".to_owned(),
            state: "Stopped".to_owned(),
            start_type: "Manual".to_owned(),
            pid: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_core_fixture_meets_structural_invariants() {
        let ticks = win11_8core_single_gpu_desktop();
        assert_eq!(ticks.len(), 3);
        let first = &ticks[0];
        let DataState::Available(cpu) = &first.cpu else {
            panic!("expected available CPU");
        };
        assert_eq!(cpu.cores.len(), 16);
        assert_eq!(first.gpus.len(), 1);
        assert!(matches!(first.battery, DataState::Unavailable(_)));

        let DataState::Available(procs0) = &ticks[0].processes else {
            panic!("expected processes");
        };
        let DataState::Available(procs1) = &ticks[1].processes else {
            panic!("expected processes");
        };
        assert_eq!(procs0.len(), 50);

        let p0 = procs0
            .iter()
            .find(|p| p.identity.pid == 4820)
            .expect("pid 4820");
        let child = procs0
            .iter()
            .find(|p| p.identity.pid == 4824)
            .expect("pid 4824");
        let p1 = procs1
            .iter()
            .find(|p| p.identity.pid == 4820)
            .expect("pid 4820 tick 1");
        assert_eq!(child.parent_pid, 4820);
        assert!(p0.identity.create_time_100ns > child.identity.create_time_100ns);
        assert!(p1.identity.create_time_100ns > p0.identity.create_time_100ns);
        assert_ne!(p0.name, p1.name);
    }

    #[test]
    fn sixteen_core_dual_gpu_fixture_has_hybrid_cores_and_dual_gpus() {
        let ticks = win11_16core_dual_gpu_desktop();
        assert_eq!(ticks.len(), 3);
        let first = &ticks[0];
        let DataState::Available(cpu) = &first.cpu else {
            panic!("expected available CPU");
        };
        assert_eq!(cpu.cores.len(), 16);
        assert_eq!(
            cpu.cores
                .iter()
                .filter(|c| c.kind == CoreKind::Performance)
                .count(),
            8
        );
        assert_eq!(
            cpu.cores
                .iter()
                .filter(|c| c.kind == CoreKind::Efficiency)
                .count(),
            8
        );
        assert_eq!(first.gpus.len(), 2);
        assert!(matches!(first.battery, DataState::Unavailable(_)));
        let DataState::Available(procs) = &first.processes else {
            panic!("expected processes");
        };
        assert_eq!(procs.len(), 52);
    }

    #[test]
    fn committed_jsonl_fixtures_match_builders() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let loaded_8 = crate::parse_jsonl_file(&dir.join("win11-8core-single-gpu-desktop.jsonl"))
            .expect("parse 8-core fixture file");
        assert_eq!(loaded_8, win11_8core_single_gpu_desktop());

        let loaded_16 = crate::parse_jsonl_file(&dir.join("win11-16core-dual-gpu-desktop.jsonl"))
            .expect("parse 16-core fixture file");
        assert_eq!(loaded_16, win11_16core_dual_gpu_desktop());
    }

    #[test]
    #[ignore = "run manually with --ignored to regenerate fixtures/*.jsonl"]
    fn regenerate_committed_fixtures() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        std::fs::create_dir_all(&dir).expect("create fixtures dir");
        for (name, ticks) in [
            (
                "win11-8core-single-gpu-desktop.jsonl",
                win11_8core_single_gpu_desktop(),
            ),
            (
                "win11-16core-dual-gpu-desktop.jsonl",
                win11_16core_dual_gpu_desktop(),
            ),
        ] {
            let mut file = std::fs::File::create(dir.join(name)).expect("create fixture file");
            for snap in &ticks {
                crate::write_snapshot_line(&mut file, snap).expect("write fixture tick");
            }
        }
    }
}
