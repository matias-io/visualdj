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
    Counter, TimeHit, confirm_counters, find_bytes, find_f32_near, find_sample_counters,
    find_time_values, heap_regions, known_tails, pointer_scan, read_time_hit, roots_for_tail,
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

/// Finds every value that encodes a deck's displayed time while it is paused, waits for the
/// operator to press play, and keeps the ones that then advance at an audio rate: those are
/// absolute positions. Static chains to the survivors are printed.
pub fn memfind(
    seconds: f64,
    tolerance_s: f64,
    wait_s: u64,
    depth: usize,
    branch: usize,
) -> anyhow::Result<()> {
    let p = Process::open(REKORDBOX_EXE)?;
    println!("scanning for {seconds:.2} s (±{tolerance_s} s) in all encodings...");
    let hits = find_time_values(&p, seconds, tolerance_s);
    let mut by_kind: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for h in &hits {
        *by_kind
            .entry(format!("{:?}@{}", h.format, h.rate_hz))
            .or_default() += 1;
    }
    println!("{} hit(s): {by_kind:?}", hits.len());
    println!("Press play on that deck now; checking again in {wait_s} s.");
    std::thread::sleep(Duration::from_secs(wait_s));
    let first: Vec<Option<f64>> = hits.iter().map(|h| read_time_hit(&p, h)).collect();
    let t0 = std::time::Instant::now();
    std::thread::sleep(Duration::from_millis(500));
    let dt = t0.elapsed().as_secs_f64();
    let mut movers: Vec<(TimeHit, f64)> = Vec::new();
    for (h, a) in hits.iter().zip(first) {
        let (Some(a), Some(b)) = (a, read_time_hit(&p, h)) else {
            continue;
        };
        let rate = (b - a) / dt / h.rate_hz.abs();
        if (0.5..=1.5).contains(&rate) {
            movers.push((*h, b / h.rate_hz.abs()));
        }
    }
    println!("{} of them advance like playback now:", movers.len());
    for (h, secs) in &movers {
        println!(
            "  {:#x} {:?}@{} now {secs:.2} s",
            h.addr, h.format, h.rate_hz
        );
    }
    for (h, _) in movers.iter().take(4) {
        let chains = pointer_scan(&p, h.addr, depth, branch);
        println!("== {:#x}: {} static chain(s)", h.addr, chains.len());
        for c in chains.iter().take(12) {
            println!("  {}", chain_line(c));
        }
    }
    Ok(())
}

/// One object a chain walks through: where it starts and the hops that lead to it, so a
/// field inside it can be written as a chain of its own.
struct Object {
    base: u64,
    hops: Vec<u64>,
    /// The chain's static root itself rather than a dereferenced pointer.
    is_static: bool,
}

/// The objects along a chain: the static root first (no hops), then each dereferenced
/// pointer with the hops before it.
fn chain_objects(p: &Process, chain: &Chain) -> Vec<Object> {
    let mut out = vec![Object {
        base: p.module_base + chain.root,
        hops: Vec::new(),
        is_static: true,
    }];
    let mut addr = p.module_base + chain.root;
    for (i, hop) in chain.hops.iter().enumerate() {
        let Ok(ptr) = p.read_u64(addr) else { break };
        out.push(Object {
            base: ptr,
            hops: chain.hops[..i].to_vec(),
            is_static: false,
        });
        addr = ptr + hop;
    }
    out
}

/// A field at `offset` inside `obj`, as a chain from the same root.
fn field_chain(root: u64, obj: &Object, offset: u64) -> Chain {
    if obj.is_static {
        return Chain {
            root: root + offset,
            hops: Vec::new(),
        };
    }
    let mut hops = obj.hops.clone();
    hops.push(offset);
    Chain { root, hops }
}

/// Reads up to `window` bytes from `base`, page by page, stopping at the first page the
/// process will not give up.
fn read_window(p: &Process, base: u64, window: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut page = [0u8; 0x1000];
    while out.len() < window {
        if p.read_exact(base + out.len() as u64, &mut page).is_err() {
            break;
        }
        out.extend_from_slice(&page);
    }
    out
}

/// Contiguous runs of indexes where `a` and `b` differ but `a` and `c` agree: the bytes
/// that changed with the first action and changed back with the second.
fn toggled_runs(before: &[u8], during: &[u8], after: &[u8]) -> Vec<(usize, usize)> {
    let len = before.len().min(during.len()).min(after.len());
    let toggled = |i: usize| before[i] != during[i] && before[i] == after[i];
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < len {
        if toggled(i) {
            let start = i;
            while i < len && toggled(i) {
                i += 1;
            }
            runs.push((start, i - start));
        } else {
            i += 1;
        }
    }
    runs
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

/// Finds the master-deck state by difference: snapshots the objects along both decks'
/// position chains and the module's static data, has the operator move MASTER to deck 2
/// and back, and prints every byte run that changed and then changed back.
pub fn memmaster(deck1: &str, deck2: &str, wait_s: u64, window: usize) -> anyhow::Result<()> {
    let p = Process::open(REKORDBOX_EXE)?;
    let chains: Vec<(String, Chain)> = [("deck 1", deck1), ("deck 2", deck2)]
        .into_iter()
        .map(|(name, line)| {
            Chain::from_rkbx_line(line)
                .map(|c| (name.to_string(), c))
                .ok_or_else(|| anyhow::anyhow!("{line}: not a chain"))
        })
        .collect::<anyhow::Result<_>>()?;
    let mut objects: Vec<(String, u64, Object)> = Vec::new();
    for (name, chain) in &chains {
        for obj in chain_objects(&p, chain) {
            objects.push((name.clone(), chain.root, obj));
        }
    }
    println!("{} object(s) along the chains; {window:#x} bytes watched in each", objects.len());
    let statics = p.static_regions();
    let snap = |p: &Process| -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let objs = objects
            .iter()
            .map(|(_, _, o)| read_window(p, o.base, window))
            .collect();
        let st = statics
            .iter()
            .map(|r| p.read_region(r).unwrap_or_default())
            .collect();
        (objs, st)
    };
    println!("Snapshot with MASTER on deck 1...");
    let (a_obj, a_st) = snap(&p);
    println!("Press MASTER on deck 2 now; snapshot in {wait_s} s.");
    std::thread::sleep(Duration::from_secs(wait_s));
    let (b_obj, b_st) = snap(&p);
    println!("Press MASTER on deck 1 now; snapshot in {wait_s} s.");
    std::thread::sleep(Duration::from_secs(wait_s));
    let (c_obj, c_st) = snap(&p);

    let mut shown = 0;
    for (k, (name, root, obj)) in objects.iter().enumerate() {
        for (start, len) in toggled_runs(&a_obj[k], &b_obj[k], &c_obj[k]) {
            if len > 16 {
                continue; // buffers and strings, not a flag
            }
            let addr = obj.base + start as u64;
            let chain = field_chain(*root, obj, start as u64);
            println!(
                "  {name} object {k} {addr:#x} +{start:#x} ({len} B): {} -> {}   chain {}",
                hex_bytes(&a_obj[k][start..start + len]),
                hex_bytes(&b_obj[k][start..start + len]),
                chain_line(&chain)
            );
            shown += 1;
            if shown > 80 {
                println!("  ... more");
                return Ok(());
            }
        }
    }
    for (k, r) in statics.iter().enumerate() {
        for (start, len) in toggled_runs(&a_st[k], &b_st[k], &c_st[k]) {
            if len > 16 {
                continue;
            }
            let addr = r.base + start as u64;
            println!(
                "  static {addr:#x} (module+{:#x}, {len} B): {} -> {}",
                addr - p.module_base,
                hex_bytes(&a_st[k][start..start + len]),
                hex_bytes(&b_st[k][start..start + len])
            );
            shown += 1;
            if shown > 120 {
                println!("  ... more");
                return Ok(());
            }
        }
    }
    if shown == 0 {
        println!("nothing toggled; the master state lives elsewhere");
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
