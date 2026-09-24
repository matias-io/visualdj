//! `calibrate`: derive the pointer chains for the running rekordbox version and write
//! `offsets/<version>.toml`. Interactive: it asks the operator to play and swap tracks on
//! deck 1 (and once to move MASTER) and keeps only the chains that follow those changes.
#![cfg(windows)]

use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use onset_transport::memory::REKORDBOX_EXE;
use onset_transport::memory::calibrate::{
    chain_for_deck, infer_stride, rank_chains, sibling_field, surviving_chains,
};
use onset_transport::memory::chain::Chain;
use onset_transport::memory::offsets::{DeckChains, Offsets, PositionFormat};
use onset_transport::memory::process::Process;
use onset_transport::memory::reader::Mem;
use onset_transport::memory::scan::{
    Counter, CounterFormat, confirm_counters, find_bytes, find_f32_near, find_sample_counters,
    pointer_scan,
};

const DEPTH: usize = 6;
const BRANCH: usize = 64;

fn prompt(text: &str) -> anyhow::Result<String> {
    print!("{text}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn wait_for_enter(text: &str) -> anyhow::Result<()> {
    prompt(&format!("{text} Press Enter when done. "))?;
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
fn chain_moves(
    p: &Process,
    chain: &Chain,
    format: CounterFormat,
    nominal: f64,
    dt: Duration,
) -> bool {
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
    let rate = (b - a) / dt.as_secs_f64() / nominal;
    (0.5..=1.5).contains(&rate)
}

fn chain_text_starts_with(p: &Process, chain: &Chain, prefix: &[u8]) -> bool {
    let Some(addr) = chain.resolve(p) else {
        return false;
    };
    let mut buf = vec![0u8; prefix.len()];
    p.read_exact(addr, &mut buf).is_ok() && buf == prefix
}

fn text_chains(p: &Process, needle: &[u8]) -> Vec<Chain> {
    let mut out = Vec::new();
    for addr in find_bytes(p, needle, 6) {
        out.extend(pointer_scan(p, addr, DEPTH, BRANCH));
    }
    rank_chains(&mut out);
    out.dedup();
    out
}

fn position_chains(p: &Process) -> anyhow::Result<(Vec<Chain>, Counter)> {
    println!("  scanning for the playing position (two passes over rekordbox's memory)...");
    let candidates = find_sample_counters(p, Duration::from_millis(400), 0.2);
    let confirmed = confirm_counters(p, &candidates, Duration::from_millis(400), 0.2);
    let confirmed = confirm_counters(p, &confirmed, Duration::from_millis(700), 0.2);
    if confirmed.is_empty() {
        bail!("no playback counter found; is deck 1 playing?");
    }
    println!("  {} counter(s) confirmed:", confirmed.len());
    let mut best: Option<(Vec<Chain>, Counter)> = None;
    for c in confirmed.iter().take(4) {
        println!(
            "    {:#x} {:?} at {:.0}/s (position {:.1} s)",
            c.addr,
            c.format,
            c.nominal_hz,
            c.seconds(c.second)
        );
        let mut chains = pointer_scan(p, c.addr, DEPTH, BRANCH);
        rank_chains(&mut chains);
        println!("      {} chain(s) from static memory", chains.len());
        if !chains.is_empty() && best.as_ref().is_none_or(|(b, _)| chains.len() > b.len()) {
            best = Some((chains, *c));
        }
    }
    best.context("no counter is reachable from static memory")
}

#[allow(clippy::too_many_lines)] // one guided session, read top to bottom
pub fn calibrate(out_dir: &Path, title_hint: Option<String>) -> anyhow::Result<()> {
    let p = Process::open(REKORDBOX_EXE)?;
    let version = p.file_version().context("rekordbox version unknown")?;
    println!(
        "rekordbox {version} (pid {}), module at {:#x}",
        p.pid, p.module_base
    );
    println!("This takes a few minutes and needs the decks to yourself.");

    // Step 1: position and its neighbours, while deck 1 plays.
    wait_for_enter("Load a track on deck 1, make it MASTER, and press play.")?;
    let title = match title_hint {
        Some(t) => t,
        None => prompt("Title of the track on deck 1, exactly as rekordbox shows it: ")?,
    };
    let (mut pos_chains, counter) = position_chains(&p)?;
    let format = counter.format;
    let nominal = counter.nominal_hz;

    let bpm_text = prompt("BPM shown on deck 1 right now (e.g. 124.00): ")?;
    let bpm: f32 = bpm_text.parse().context("BPM must be a number")?;
    let bpm_offsets: Vec<i64> = find_f32_near(&p, counter.addr, 0x400, bpm, 0.011)
        .into_iter()
        .map(|a| a.cast_signed() - counter.addr.cast_signed())
        .collect();
    println!("  tempo candidates near the position: {bpm_offsets:?} bytes");

    let needle = format!("Track Title: {title}\n");
    let mut info_chains = text_chains(&p, needle.as_bytes());
    println!("  {} chain(s) to the deck 1 text block", info_chains.len());
    let mut anlz_chains = text_chains(&p, b"PIONEER/USBANLZ/");
    println!("  {} chain(s) to analysis paths", anlz_chains.len());

    // Step 2: a different track on deck 1. Only chains that follow it are the deck's own.
    wait_for_enter("Now load a DIFFERENT track on deck 1 and press play again.")?;
    let title2 = prompt("Title of the new track on deck 1: ")?;
    std::thread::sleep(Duration::from_millis(300));
    pos_chains = surviving_chains(&pos_chains, |c| {
        chain_moves(&p, c, format, nominal, Duration::from_millis(250))
    });
    let needle2 = format!("Track Title: {title2}\n");
    info_chains = surviving_chains(&info_chains, |c| {
        chain_text_starts_with(&p, c, needle2.as_bytes())
    });
    anlz_chains = surviving_chains(&anlz_chains, |c| {
        chain_text_starts_with(&p, c, b"PIONEER/USBANLZ/")
    });
    println!(
        "  after the track change: {} position, {} text, {} path chain(s) survive",
        pos_chains.len(),
        info_chains.len(),
        anlz_chains.len()
    );
    if pos_chains.is_empty() {
        bail!("no position chain survived the track change; try again with a longer play");
    }
    let bpm_chain = bpm_offsets
        .iter()
        .filter_map(|d| sibling_field(&pos_chains[0], *d))
        .find(|c| {
            c.resolve(&p)
                .and_then(|a| p.read_f32(a).ok())
                .is_some_and(|v| (60.0..=220.0).contains(&v))
        });
    if bpm_chain.is_none() {
        println!("  warning: no tempo field confirmed next to the position; deck BPM will read 0");
    }

    // Step 3: deck 2, to learn the stride between decks.
    wait_for_enter("Stop deck 1. Load a track on deck 2 and press play on deck 2 only.")?;
    let (deck2_pos, _) = position_chains(&p)?;
    let stride = pos_chains
        .iter()
        .find_map(|d1| deck2_pos.iter().find_map(|d2| infer_stride(d1, d2)))
        .context("could not relate deck 2's position chain to deck 1's")?;
    println!("  deck stride: {stride:?}");

    // Step 4: master deck. A byte that reads 1 with deck 2 as master and 0 with deck 1.
    wait_for_enter("Press MASTER on deck 2.")?;
    let master_candidates = master_candidates(&p, &pos_chains[0], 1)?;
    wait_for_enter("Now press MASTER on deck 1.")?;
    let master_chains: Vec<Chain> = master_candidates
        .into_iter()
        .filter(|c| c.resolve(&p).and_then(|a| p.read_u8(a).ok()) == Some(0))
        .collect();
    println!("  {} master-deck chain(s)", master_chains.len());
    let master = master_chains
        .first()
        .cloned()
        .context("no master-deck byte followed the MASTER button")?;

    let deck1 = DeckChains {
        bpm: bpm_chain.clone().unwrap_or_else(|| pos_chains[0].clone()),
        position: pos_chains[0].clone(),
        track_info: info_chains.first().cloned(),
        anlz_path: anlz_chains.first().cloned(),
    };
    let all_decks: Vec<DeckChains> = (0..4u64)
        .map(|n| DeckChains {
            bpm: chain_for_deck(&deck1.bpm, stride, n).unwrap_or_else(|| deck1.bpm.clone()),
            position: chain_for_deck(&deck1.position, stride, n)
                .unwrap_or_else(|| deck1.position.clone()),
            track_info: deck1
                .track_info
                .as_ref()
                .and_then(|c| chain_for_deck(c, stride, n)),
            anlz_path: deck1
                .anlz_path
                .as_ref()
                .and_then(|c| chain_for_deck(c, stride, n)),
        })
        .collect();
    let offsets = Offsets {
        rekordbox_version: version.clone(),
        platform: "windows".into(),
        position_format: match format {
            CounterFormat::I64 => PositionFormat::I64,
            CounterFormat::I32 => PositionFormat::I32,
            CounterFormat::F64 => PositionFormat::F64,
        },
        position_rate_hz: nominal,
        master_deck: master,
        decks: all_decks,
        provenance: Some(format!(
            "onset-cli calibrate on {} with tracks {title:?} and {title2:?}",
            chrono_free_date()
        )),
    };
    let path = out_dir.join(format!("{version}.toml"));
    offsets.save(&path)?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Chains that reach a byte equal to `expected` near static memory or the deck structs.
/// The MASTER index is small state; the scan looks for it in the first 4 KB around the
/// positions' parent struct and among static-reachable bytes found by a pointer scan.
fn master_candidates(p: &Process, position: &Chain, expected: u8) -> anyhow::Result<Vec<Chain>> {
    let mut out = Vec::new();
    // Same struct as the position field: try every byte offset in a window.
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
    // Static bytes: the index may simply live in the module's data section.
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
    Ok(out)
}

/// A date string without pulling in a date crate: seconds since the epoch.
fn chrono_free_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!("unix {secs}")
}

pub fn default_out_dir() -> PathBuf {
    std::env::var_os("ONSET_OFFSETS").map_or_else(|| PathBuf::from("offsets"), PathBuf::from)
}
