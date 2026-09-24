//! `calibrate`: derive the pointer chains for the running rekordbox version and write
//! `offsets/<version>.toml`. The operator plays and swaps tracks on deck 1, plays deck 2 and
//! moves MASTER; the calibrator keeps only the memory paths that follow each change. It
//! never needs to know which tracks were used: a path counts when what it points at changed
//! the way the deck did.
//!
//! Interactive mode waits for Enter after each step; `--auto` announces the step, waits a
//! fixed time and carries on.
#![cfg(windows)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use onset_transport::memory::REKORDBOX_EXE;
use onset_transport::memory::calibrate::{
    Stride, chain_for_deck, infer_stride, rank_chains, sibling_field, surviving_chains,
};
use onset_transport::memory::chain::Chain;
use onset_transport::memory::offsets::{DeckChains, Offsets, PositionFormat};
use onset_transport::memory::process::Process;
use onset_transport::memory::reader::Mem;
use onset_transport::memory::scan::{
    Counter, CounterFormat, confirm_counters, find_bytes, find_sample_counters, pointer_scan,
};

const DEPTH: usize = 6;
const BRANCH: usize = 64;
/// Text blocks pointer-scanned per kind (each scan is a pass over memory).
const SCANS_PER_KIND: usize = 5;
/// Most counters pointer-scanned before giving up, and the chain count that ends it early.
const MAX_COUNTER_SCANS: usize = 14;
const ENOUGH_CHAINS: usize = 8;
const TITLE_PREFIX: &[u8] = b"Track Title: ";
const ANLZ_PREFIX: &[u8] = b"PIONEER/USBANLZ/";
/// Bytes around the position field searched for the tempo.
const BPM_WINDOW: i64 = 0x400;

pub enum Mode {
    Interactive,
    /// Announce each step, wait this long, then read the state.
    Auto {
        step: Duration,
    },
}

fn prompt(text: &str) -> anyhow::Result<String> {
    print!("{text}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn step(mode: &Mode, instruction: &str) -> anyhow::Result<()> {
    match mode {
        Mode::Interactive => {
            prompt(&format!("{instruction} Press Enter when done. "))?;
        }
        Mode::Auto { step } => {
            println!("{instruction} (continuing in {} s)", step.as_secs());
            let deadline = Instant::now() + *step;
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    Ok(())
}

fn read_counter(p: &Process, addr: u64, format: CounterFormat) -> Option<f64> {
    match format {
        CounterFormat::I64 => p.read_i64(addr).ok().map(|v| v as f64),
        CounterFormat::I32 => p.read_i32(addr).ok().map(f64::from),
        CounterFormat::F64 => p.read_f64(addr).ok(),
    }
}

/// True when the chain resolves to a value that advances like a position over `dt`.
fn chain_moves(p: &Process, chain: &Chain, format: CounterFormat, dt: Duration) -> bool {
    let Some(addr) = chain.resolve(p) else {
        return false;
    };
    let Some(a) = read_counter(p, addr, format) else {
        return false;
    };
    std::thread::sleep(dt);
    let Some(b) = read_counter(p, addr, format) else {
        return false;
    };
    // Any file sample rate between 22 kHz and 100 kHz, at up to ±50 % pitch.
    let rate = (b - a) / dt.as_secs_f64();
    (11_000.0..=150_000.0).contains(&rate)
}

/// The text a chain points at, when it starts with `prefix`.
fn text_at(p: &Process, chain: &Chain, prefix: &[u8]) -> Option<String> {
    let addr = chain.resolve(p)?;
    let mut buf = vec![0u8; 160];
    p.read_exact(addr, &mut buf).ok()?;
    if !buf.starts_with(prefix) {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// Chains to the first blocks starting with `prefix`, each paired with its current text.
fn text_chains(p: &Process, prefix: &[u8]) -> Vec<(Chain, String)> {
    let mut out: Vec<(Chain, String)> = Vec::new();
    let mut seen: BTreeSet<Chain> = BTreeSet::new();
    for addr in find_bytes(p, prefix, SCANS_PER_KIND) {
        for c in pointer_scan(p, addr, DEPTH, BRANCH) {
            if seen.insert(c.clone())
                && let Some(t) = text_at(p, &c, prefix)
            {
                out.push((c, t));
            }
        }
    }
    out
}

/// Confirmed playback counters, best first: sample-rate counters grouped by the second they
/// show, largest group first (the deck position exists in several copies).
fn ranked_counters(p: &Process) -> anyhow::Result<Vec<Counter>> {
    println!("  scanning for the playing position (two passes over rekordbox's memory)...");
    let candidates = find_sample_counters(p, Duration::from_millis(400), 0.2);
    let confirmed = confirm_counters(p, &candidates, Duration::from_millis(400), 0.2);
    let mut confirmed = confirm_counters(p, &confirmed, Duration::from_millis(700), 0.2);
    confirmed.retain(|c| c.nominal_hz >= 22_000.0 && c.seconds(c.second) < 30.0 * 60.0);
    if confirmed.is_empty() {
        bail!("no playback counter found; is a deck playing?");
    }
    let mut groups: BTreeMap<i64, Vec<Counter>> = BTreeMap::new();
    for c in &confirmed {
        groups
            .entry((c.seconds(c.second) * 2.0).round() as i64)
            .or_default()
            .push(*c);
    }
    let mut ordered: Vec<Vec<Counter>> = groups.into_values().collect();
    ordered.sort_by_key(|g| std::cmp::Reverse(g.len()));
    let mut best: Vec<Counter> = ordered.into_iter().flatten().collect();
    // Within the ranking, prefer lone integer fields: runs of adjacent counters are audio or
    // waveform buffers, and doubles are mostly derived copies. Struct fields have chains.
    let addrs: BTreeSet<u64> = best.iter().map(|c| c.addr).collect();
    let in_array = |c: &Counter| {
        [4u64, 8, 16]
            .iter()
            .any(|d| addrs.contains(&(c.addr + d)) || addrs.contains(&c.addr.wrapping_sub(*d)))
    };
    best.sort_by_key(|c| (in_array(c), matches!(c.format, CounterFormat::F64)));
    println!(
        "  {} counter(s) confirmed; the largest group reads {:.1} s",
        best.len(),
        best[0].seconds(best[0].second)
    );
    Ok(best)
}

/// Static chains to the best counters, with the counter format they read.
fn position_chains(p: &Process) -> anyhow::Result<(Vec<Chain>, Counter)> {
    let counters = ranked_counters(p)?;
    let mut chains: Vec<Chain> = Vec::new();
    let mut picked = counters[0];
    for c in counters.iter().take(MAX_COUNTER_SCANS) {
        let found = pointer_scan(p, c.addr, DEPTH, BRANCH);
        println!(
            "    {:#x} {:?} {:.0}/s: {} chain(s)",
            c.addr,
            c.format,
            c.nominal_hz,
            found.len()
        );
        if !found.is_empty() && chains.is_empty() {
            picked = *c;
        }
        chains.extend(found);
        if chains.len() >= ENOUGH_CHAINS {
            break;
        }
    }
    rank_chains(&mut chains);
    chains.dedup();
    if chains.is_empty() {
        bail!("no counter is reachable from static memory");
    }
    Ok((chains, picked))
}

/// Plausible tempo floats around the position field: (byte delta, value).
fn bpm_candidates(p: &Process, position: &Chain) -> Vec<(i64, f32)> {
    let Some(addr) = position.resolve(p) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut delta = -BPM_WINDOW;
    while delta < BPM_WINDOW {
        if let Ok(v) = p.read_f32((addr.cast_signed() + delta).cast_unsigned())
            && (60.0..=220.0).contains(&v)
            && (v * 100.0).fract().abs() < 0.01
        {
            out.push((delta, v));
        }
        delta += 4;
    }
    out
}

#[allow(clippy::too_many_lines)] // one guided session, read top to bottom
pub fn calibrate(out_dir: &Path, mode: &Mode) -> anyhow::Result<()> {
    let p = Process::open(REKORDBOX_EXE)?;
    let version = p.file_version().context("rekordbox version unknown")?;
    println!(
        "rekordbox {version} (pid {}), module at {:#x}",
        p.pid, p.module_base
    );
    println!("This takes a few minutes and needs the decks to yourself.");

    // Step 1: position and its neighbours, while deck 1 plays.
    step(
        mode,
        "Step 1: load a track on deck 1, make it MASTER, and press play.",
    )?;
    let (mut pos_chains, counter) = position_chains(&p)?;
    let format = counter.format;
    println!("  {} position chain(s)", pos_chains.len());
    let mut bpm = bpm_candidates(&p, &pos_chains[0]);
    println!("  {} tempo-like float(s) near the position", bpm.len());
    let mut info = text_chains(&p, TITLE_PREFIX);
    println!("  {} chain(s) to track text blocks", info.len());
    let mut anlz = text_chains(&p, ANLZ_PREFIX);
    println!("  {} chain(s) to analysis paths", anlz.len());

    // Step 2: a different track on deck 1. Only paths whose content changed are the deck's.
    step(
        mode,
        "Step 2: stop deck 1, load a DIFFERENT track on deck 1 and press play again.",
    )?;
    std::thread::sleep(Duration::from_millis(300));
    pos_chains = surviving_chains(&pos_chains, |c| {
        chain_moves(&p, c, format, Duration::from_millis(250))
    });
    info.retain(|(c, before)| text_at(&p, c, TITLE_PREFIX).is_some_and(|now| now != *before));
    anlz.retain(|(c, before)| text_at(&p, c, ANLZ_PREFIX).is_some_and(|now| now != *before));
    if let Some(pos) = pos_chains.first() {
        let addr = pos.resolve(&p);
        bpm.retain(|(delta, before)| {
            addr.and_then(|a| p.read_f32((a.cast_signed() + delta).cast_unsigned()).ok())
                .is_some_and(|now| (60.0..=220.0).contains(&now) && (now - before).abs() > 0.05)
        });
    }
    println!(
        "  after the track change: {} position, {} text, {} path, {} tempo candidate(s) follow the deck",
        pos_chains.len(),
        info.len(),
        anlz.len(),
        bpm.len()
    );
    if pos_chains.is_empty() {
        bail!("no position chain survived the track change; try again with a longer play");
    }
    if bpm.is_empty() {
        println!(
            "  note: no tempo field changed (same BPM twice?); deck BPM will come from the library"
        );
    }

    // Step 3: deck 2, to learn the stride between decks.
    step(
        mode,
        "Step 3: stop deck 1. Load a track on deck 2 and press play on deck 2 only.",
    )?;
    let (deck2, _) = position_chains(&p)?;
    let mut strides: BTreeMap<String, (Stride, usize)> = BTreeMap::new();
    for d1 in &pos_chains {
        for d2 in &deck2 {
            if let Some(s) = infer_stride(d1, d2) {
                let e = strides.entry(format!("{s:?}")).or_insert((s, 0));
                e.1 += 1;
            }
        }
    }
    let (stride, votes) = strides
        .values()
        .max_by_key(|(_, n)| *n)
        .copied()
        .context("could not relate deck 2's position chains to deck 1's")?;
    println!("  deck stride {stride:?} ({votes} matching pair(s))");
    // Keep only deck 1 chains that have a deck 2 twin under that stride.
    pos_chains.retain(|d1| chain_for_deck(d1, stride, 1).is_some_and(|twin| deck2.contains(&twin)));
    if pos_chains.is_empty() {
        bail!("no position chain has a deck 2 twin");
    }

    // Steps 4 and 5: master deck, a byte that reads 1 with deck 2 as master and 0 after.
    step(mode, "Step 4: press MASTER on deck 2.")?;
    let candidates = master_candidates(&p, &pos_chains[0], 1);
    println!("  {} byte(s) read 1", candidates.len());
    step(mode, "Step 5: press MASTER on deck 1.")?;
    let masters: Vec<Chain> = candidates
        .into_iter()
        .filter(|c| c.resolve(&p).and_then(|a| p.read_u8(a).ok()) == Some(0))
        .collect();
    println!("  {} master-deck chain(s)", masters.len());
    let master = masters
        .first()
        .cloned()
        .context("no master-deck byte followed the MASTER button")?;

    let position = pos_chains[0].clone();
    let bpm_chain = bpm.first().and_then(|(d, _)| sibling_field(&position, *d));
    let offsets = assemble(&Assembly {
        version: &version,
        format,
        master,
        position: &position,
        bpm: bpm_chain.as_ref(),
        info: info.first().map(|(c, _)| c),
        anlz: anlz.first().map(|(c, _)| c),
        stride,
    });
    let path = out_dir.join(format!("{version}.toml"));
    offsets.save(&path)?;
    println!("wrote {}", path.display());
    Ok(())
}

struct Assembly<'a> {
    version: &'a str,
    format: CounterFormat,
    master: Chain,
    position: &'a Chain,
    bpm: Option<&'a Chain>,
    info: Option<&'a Chain>,
    anlz: Option<&'a Chain>,
    stride: Stride,
}

fn assemble(a: &Assembly<'_>) -> Offsets {
    let deck1 = DeckChains {
        bpm: a.bpm.cloned().unwrap_or_else(|| a.position.clone()),
        position: a.position.clone(),
        track_info: a.info.cloned(),
        anlz_path: a.anlz.cloned(),
    };
    let all_decks: Vec<DeckChains> = (0..4u64)
        .map(|n| DeckChains {
            bpm: chain_for_deck(&deck1.bpm, a.stride, n).unwrap_or_else(|| deck1.bpm.clone()),
            position: chain_for_deck(&deck1.position, a.stride, n)
                .unwrap_or_else(|| deck1.position.clone()),
            track_info: deck1
                .track_info
                .as_ref()
                .and_then(|c| chain_for_deck(c, a.stride, n)),
            anlz_path: deck1
                .anlz_path
                .as_ref()
                .and_then(|c| chain_for_deck(c, a.stride, n)),
        })
        .collect();
    Offsets {
        rekordbox_version: a.version.to_string(),
        platform: "windows".into(),
        position_format: match a.format {
            CounterFormat::I64 => PositionFormat::I64,
            CounterFormat::I32 => PositionFormat::I32,
            CounterFormat::F64 => PositionFormat::F64,
        },
        // Positions count samples of the loaded file (44.1 or 48 kHz files differ).
        position_rate_hz: 0.0,
        master_deck: a.master.clone(),
        decks: all_decks,
        provenance: Some(format!(
            "onset-cli calibrate at unix {}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        )),
    }
}

/// Chains that reach a byte equal to `expected`: every byte in a window around the position
/// field's struct, and every 4-aligned static byte in the module.
fn master_candidates(p: &Process, position: &Chain, expected: u8) -> Vec<Chain> {
    let mut out = Vec::new();
    if let Some(pos_addr) = position.resolve(p) {
        for delta in -0x800i64..0x800 {
            let Some(c) = sibling_field(position, delta) else {
                continue;
            };
            let addr = pos_addr.cast_signed() + delta;
            if p.read_u8(addr.cast_unsigned()).ok() == Some(expected) {
                out.push(c);
            }
        }
    }
    for r in p.static_regions() {
        let Some(bytes) = p.read_region(&r) else {
            continue;
        };
        for (i, b) in bytes.iter().enumerate() {
            if *b == expected && i % 4 == 0 {
                out.push(Chain {
                    root: r.base + i as u64 - p.module_base,
                    hops: Vec::new(),
                });
            }
        }
    }
    out
}

pub fn default_out_dir() -> PathBuf {
    std::env::var_os("ONSET_OFFSETS").map_or_else(|| PathBuf::from("offsets"), PathBuf::from)
}
