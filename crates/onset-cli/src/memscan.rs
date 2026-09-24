//! `memscan`: find rekordbox's live values in its process memory and the pointer chains
//! that reach them. This is the calibrator's exploratory half; it prints what it finds.
#![cfg(windows)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use onset_transport::memory::REKORDBOX_EXE;
use onset_transport::memory::chain::Chain;
use onset_transport::memory::process::Process;
use onset_transport::memory::scan::{
    Counter, confirm_counters, find_bytes, find_f32_near, find_sample_counters, heap_regions,
    known_tails, pointer_scan, roots_for_tail,
};

/// Bytes of context printed around a string hit.
const CONTEXT: usize = 160;

fn printable(bytes: &[u8]) -> String {
    let mut s = String::new();
    for b in bytes {
        match *b {
            0 => s.push('·'),
            b'\n' => s.push_str("\\n"),
            32..=126 => s.push(*b as char),
            _ => s.push('.'),
        }
    }
    s
}

fn show_string_hits(p: &Process, label: &str, needle: &[u8], report: &mut String) {
    let hits = find_bytes(p, needle, 12);
    let _ = writeln!(report, "\n== {label}: {} hit(s)", hits.len());
    for addr in hits.iter().take(6) {
        let mut buf = vec![0u8; CONTEXT];
        if p.read_exact(*addr, &mut buf).is_ok() {
            let _ = writeln!(report, "  {addr:#x}  {}", printable(&buf));
        }
    }
}

/// Plausibility checks for each known chain kind at a resolved address.
fn accept_kind(p: &Process, kind: &str, addr: u64, bpm: Option<f32>) -> bool {
    match kind {
        "bpm" => p
            .read_f32(addr)
            .is_ok_and(|v| (60.0..=220.0).contains(&v) && bpm.is_none_or(|b| (v - b).abs() < 0.06)),
        "sample_position" => p
            .read_i64(addr)
            .is_ok_and(|v| (0..44_100 * 3600 * 4).contains(&v)),
        "track_info" => {
            let mut b = [0u8; 13];
            p.read_exact(addr, &mut b).is_ok() && &b == b"Track Title: "
        }
        "anlz_path" => {
            let mut b = [0u8; 64];
            p.read_exact(addr, &mut b).is_ok() && b.windows(7).any(|w| w == b"USBANLZ")
        }
        "master_deck" => p.read_u8(addr).is_ok_and(|v| v < 4),
        _ => false,
    }
}

fn chain_line(c: &Chain) -> String {
    let hops: Vec<String> = c.hops.iter().map(|h| format!("{h:#x}")).collect();
    format!(
        "root {:#x} hops [{}]  (rkbx: {})",
        c.root,
        hops.join(", "),
        c.to_rkbx_line()
    )
}

/// Resolves rkbx-format chains against the running rekordbox and prints what they point at,
/// so a candidate can be checked again after a track change or a restart.
pub fn memresolve(lines: &[String]) -> anyhow::Result<()> {
    let p = Process::open(REKORDBOX_EXE)?;
    println!(
        "{} pid {} version {} base {:#x}",
        p.name,
        p.pid,
        p.file_version().unwrap_or_else(|| "?".into()),
        p.module_base
    );
    for line in lines {
        let Some(chain) = Chain::from_rkbx_line(line) else {
            println!("{line}: not a chain");
            continue;
        };
        match chain.resolve(&p) {
            None => println!("{line}: does not resolve"),
            Some(addr) => {
                let mut b = vec![0u8; 96];
                let _ = p.read_exact(addr, &mut b);
                let i64v = p.read_i64(addr).unwrap_or(0);
                let f32v = p.read_f32(addr).unwrap_or(f32::NAN);
                let f64v = p.read_f64(addr).unwrap_or(f64::NAN);
                println!(
                    "{line}: -> {addr:#x}  i64 {i64v}  f32 {f32v:.4}  f64 {f64v:.4}\n    text {}",
                    printable(&b)
                );
            }
        }
    }
    Ok(())
}

/// Pointer-scans the given absolute addresses and prints every static chain to each.
pub fn memchains(addrs: &[String], depth: usize, branch: usize) -> anyhow::Result<()> {
    let p = Process::open(REKORDBOX_EXE)?;
    for text in addrs {
        let addr = u64::from_str_radix(text.trim_start_matches("0x"), 16)
            .map_err(|e| anyhow::anyhow!("{text}: {e}"))?;
        let value = p.read_i64(addr).unwrap_or(-1);
        println!("== {addr:#x} (i64 {value}): chains");
        let chains = pointer_scan(&p, addr, depth, branch);
        for c in &chains {
            println!("  {}", chain_line(c));
        }
        if chains.is_empty() {
            println!("  none");
        }
    }
    Ok(())
}

pub struct MemscanArgs {
    pub bpm: Option<f32>,
    /// Title of the track loaded on the deck of interest: its text block and analysis path
    /// are located and pointer-scanned.
    pub title: Option<String>,
    pub app_dir: Option<PathBuf>,
    pub dt_ms: u64,
    pub depth: usize,
    pub branch: usize,
    pub skip_pointers: bool,
    pub skip_counters: bool,
    pub out: Option<PathBuf>,
}

fn scan_and_print(
    p: &Process,
    label: &str,
    addr: u64,
    depth: usize,
    branch: usize,
    report: &mut String,
) {
    println!("\n== pointer scan: {label} at {addr:#x} (depth {depth}, branch {branch})");
    let chains = pointer_scan(p, addr, depth, branch);
    let _ = writeln!(
        report,
        "\n== pointer chains to {label} {addr:#x}: {}",
        chains.len()
    );
    for ch in chains.iter().take(20) {
        let line = chain_line(ch);
        println!("  {line}");
        let _ = writeln!(report, "  {line}");
    }
    if chains.is_empty() {
        println!("  none within depth {depth}");
    }
}

/// The deck's own strings: `Track Title: <title>` and, when the library knows the track,
/// its ANLZ path. Both are pointer-scanned; they do not need the deck to be playing.
fn title_targets(p: &Process, args: &MemscanArgs, report: &mut String) -> Vec<(String, u64)> {
    let Some(title) = &args.title else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    let needle = format!("Track Title: {title}\n");
    let hits = find_bytes(p, needle.as_bytes(), 8);
    let _ = writeln!(
        report,
        "\n== deck text block for {title:?}: {} hit(s)",
        hits.len()
    );
    for (i, a) in hits.iter().enumerate() {
        let mut buf = vec![0u8; 120];
        let _ = p.read_exact(*a, &mut buf);
        let _ = writeln!(report, "  {a:#x}  {}", printable(&buf));
        targets.push((format!("track_info#{i}"), *a));
    }
    if let Ok((_, lib)) = crate::common::open_library(args.app_dir.clone())
        && let Some(meta) = lib.find_by_title_artist(title, "")
        && let Some(rel) = &meta.analysis_path
    {
        let needle = rel.trim_start_matches('/').as_bytes().to_vec();
        let hits = find_bytes(p, &needle, 8);
        let _ = writeln!(report, "== analysis path {rel}: {} hit(s)", hits.len());
        for (i, a) in hits.iter().enumerate() {
            targets.push((format!("anlz_path#{i}"), *a));
        }
    }
    targets.truncate(4);
    targets
}

#[allow(clippy::too_many_lines)] // a report printer; splitting it would only scatter the sections
pub fn memscan(args: &MemscanArgs) -> anyhow::Result<()> {
    let (bpm, dt_ms, depth, branch, skip_pointers) = (
        args.bpm,
        args.dt_ms,
        args.depth,
        args.branch,
        args.skip_pointers,
    );
    let p = Process::open(REKORDBOX_EXE)?;
    let mut report = String::new();
    let heap = heap_regions(&p);
    let heap_mb: u64 = heap.iter().map(|r| r.size).sum::<u64>() / (1024 * 1024);
    let _ = writeln!(
        report,
        "{} pid {} version {} base {:#x} size {:#x}; {} heap regions, {heap_mb} MB; {} static regions",
        p.name,
        p.pid,
        p.file_version().unwrap_or_else(|| "?".into()),
        p.module_base,
        p.module_size,
        heap.len(),
        p.static_regions().len()
    );
    println!("{report}");

    show_string_hits(&p, "text 'Track Title: '", b"Track Title: ", &mut report);
    show_string_hits(
        &p,
        "text 'PIONEER/USBANLZ'",
        b"PIONEER/USBANLZ",
        &mut report,
    );
    println!("{}", report.lines().skip(1).collect::<Vec<_>>().join("\n"));

    // Fast path: rkbx_link's 7.2.2 chain shapes with re-derived static roots.
    let _ = writeln!(
        report,
        "\n== known chain tails (7.2.2 shapes) with roots re-derived"
    );
    let mut fast = String::new();
    for (kind, tail) in known_tails() {
        let roots = roots_for_tail(&p, &tail, |addr| accept_kind(&p, kind, addr, bpm));
        let _ = writeln!(fast, "  {kind}: {} plausible root(s)", roots.len());
        for c in roots.iter().take(5) {
            let addr = c.resolve(&p).unwrap_or(0);
            let value = match kind {
                "bpm" => format!("{:.3}", p.read_f32(addr).unwrap_or(f32::NAN)),
                "sample_position" => format!("{}", p.read_i64(addr).unwrap_or(-1)),
                "master_deck" => format!("{}", p.read_u8(addr).unwrap_or(255)),
                _ => {
                    let mut b = vec![0u8; 80];
                    let _ = p.read_exact(addr, &mut b);
                    printable(&b)
                }
            };
            let _ = writeln!(fast, "    {}  -> {addr:#x} = {value}", chain_line(c));
        }
    }
    print!("{fast}");
    report.push_str(&fast);

    let strings = title_targets(&p, args, &mut report);
    println!(
        "{}",
        report
            .lines()
            .rev()
            .take(strings.len() + 3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    );
    if !skip_pointers {
        for (label, addr) in &strings {
            scan_and_print(&p, label, *addr, depth, branch, &mut report);
        }
    }
    if args.skip_counters {
        if let Some(path) = &args.out {
            std::fs::write(path, &report)?;
        }
        return Ok(());
    }

    // Slow path: audio-rate counters, then pointer paths back to static memory.
    let dt = Duration::from_millis(dt_ms);
    let _ = writeln!(report, "\n== sample counters ({dt_ms} ms window)");
    let counters = find_sample_counters(&p, dt, 0.2);
    let _ = writeln!(
        report,
        "  {} candidate(s) advanced like a playback position",
        counters.len()
    );
    println!(
        "\n== {} counter candidate(s); confirming...",
        counters.len()
    );
    // Two more windows: survivors of both are almost certainly real positions.
    let confirmed = confirm_counters(&p, &counters, Duration::from_millis(400), 0.2);
    let confirmed: Vec<Counter> = confirm_counters(&p, &confirmed, Duration::from_millis(700), 0.2);
    let _ = writeln!(
        report,
        "  {} confirmed over two more windows",
        confirmed.len()
    );
    for c in &confirmed {
        let _ = writeln!(
            report,
            "  {:#x}  {:?}  {:.1} -> {:.1}  ({:.2} s -> {:.2} s)  {:.1}/s  nominal {:.0}  rate x{:.4}",
            c.addr,
            c.format,
            c.first,
            c.second,
            c.seconds(c.first),
            c.seconds(c.second),
            c.rate_hz,
            c.nominal_hz,
            c.playback_rate
        );
        if let Some(b) = bpm {
            let near = find_f32_near(&p, c.addr, 0x800, b, 0.02);
            if !near.is_empty() {
                let offs: Vec<String> = near
                    .iter()
                    .map(|a| format!("{:+#x}", a.cast_signed() - c.addr.cast_signed()))
                    .collect();
                let _ = writeln!(report, "      bpm {b} found nearby at {}", offs.join(" "));
            }
        }
    }
    println!(
        "{}",
        report
            .lines()
            .rev()
            .take(confirmed.len() * 2 + 2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    );

    if !skip_pointers {
        for c in confirmed.iter().take(3) {
            scan_and_print(&p, "counter", c.addr, depth, branch, &mut report);
        }
    }

    if let Some(path) = &args.out {
        std::fs::write(path, &report)?;
        println!("\nreport written to {}", path.display());
    }
    Ok(())
}
