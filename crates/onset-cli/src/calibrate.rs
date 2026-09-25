//! `calibrate`: derive the pointer chains for the running rekordbox version and write
//! `offsets/<version>.toml`. The operator plays and swaps tracks on one deck, plays the
//! other deck and moves MASTER; the calibrator keeps only the memory paths that follow each
//! change. It never needs to know which tracks were used: a path counts when what it points
//! at changed the way the deck did.
//!
//! Each step is announced, then the calibrator watches memory until it sees the change the
//! step describes (a deck stopping, a track swapping, MASTER moving) and carries on by
//! itself. A step that is not seen within the time limit is skipped, and the later checks
//! decide whether the run still holds together.
#![cfg(windows)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use onset_transport::memory::REKORDBOX_EXE;
use onset_transport::memory::calibrate::{
    Stride, chain_for_deck, infer_stride, rank_chains, sibling_field,
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
const TEXT_SCANS: usize = 8;
/// Most counters pointer-scanned before giving up, and the chain count that ends it early.
const MAX_COUNTER_SCANS: usize = 14;
const ENOUGH_CHAINS: usize = 8;
/// A scan counts as "a deck is playing" once this many fresh counters advance.
const MIN_NEW_COUNTERS: usize = 8;
const TITLE_PREFIX: &[u8] = b"Track Title: ";
const ANLZ_PREFIX: &[u8] = b"PIONEER/USBANLZ/";

/// How long each step may take the operator before the calibrator moves on without it.
pub struct Mode {
    pub step_limit: Duration,
}

const POLL: Duration = Duration::from_millis(500);
/// Settling time after a change is seen, so rekordbox finishes its own bookkeeping first.
const SETTLE: Duration = Duration::from_millis(800);
/// Wait when a step has nothing observable to watch for.
const BLIND_WAIT: Duration = Duration::from_secs(45);

/// Announces a step and returns once `seen` reports the change it describes, or once the
/// step limit passes. Returns whether the change was seen.
fn wait_for(mode: &Mode, instruction: &str, mut seen: impl FnMut() -> bool) -> bool {
    println!(
        "{instruction} (waiting up to {} s)",
        mode.step_limit.as_secs()
    );
    let deadline = Instant::now() + mode.step_limit;
    loop {
        if seen() {
            println!("  seen");
            std::thread::sleep(SETTLE);
            return true;
        }
        if Instant::now() >= deadline {
            println!("  not seen in time; carrying on");
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// Announces a step that memory cannot confirm and waits a fixed time for it.
fn wait_blind(mode: &Mode, instruction: &str) {
    let wait = mode.step_limit.min(BLIND_WAIT);
    println!("{instruction} (nothing to watch for; waiting {} s)", wait.as_secs());
    std::thread::sleep(wait);
}

/// True when at least one counter advances at an audio rate over `dt`.
fn any_counter_moving(p: &Process, counters: &[Counter], dt: Duration) -> bool {
    let before = snapshot_counters(p, counters);
    std::thread::sleep(dt);
    let secs = dt.as_secs_f64();
    counters.iter().zip(&before).any(|(c, a)| {
        matches!((a, read_counter(p, c.addr, c.format)), (Some(a), Some(b)) if audio_rate((b - a) / secs))
    })
}

/// How many counters read the same value twice `dt` apart.
fn count_still(p: &Process, counters: &[Counter], dt: Duration) -> usize {
    let before = snapshot_counters(p, counters);
    std::thread::sleep(dt);
    counters
        .iter()
        .zip(&before)
        .filter(|(c, a)| {
            matches!((a, read_counter(p, c.addr, c.format)), (Some(a), Some(b)) if (a - b).abs() < 1e-9)
        })
        .count()
}

/// True when any chain's text differs from what was recorded for it.
fn any_text_changed(p: &Process, chains: &[(Chain, String)], prefix: &[u8]) -> bool {
    chains
        .iter()
        .any(|(c, before)| text_at(p, c, prefix).is_some_and(|now| now != *before))
}

fn read_counter(p: &Process, addr: u64, format: CounterFormat) -> Option<f64> {
    match format {
        CounterFormat::I64 => p.read_i64(addr).ok().map(|v| v as f64),
        CounterFormat::I32 => p.read_i32(addr).ok().map(f64::from),
        CounterFormat::F64 => p.read_f64(addr).ok(),
    }
}

/// A rate in units per second that looks like audio playback at up to ±50 % pitch.
fn audio_rate(rate: f64) -> bool {
    (11_000.0..=150_000.0).contains(&rate)
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
    audio_rate((b - a) / dt.as_secs_f64())
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
    for addr in find_bytes(p, prefix, TEXT_SCANS) {
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

/// Keeps the text chains whose content changed since it was recorded, and records the new
/// content for the next comparison.
fn keep_changed(p: &Process, chains: &mut Vec<(Chain, String)>, prefix: &[u8]) {
    chains.retain_mut(|(c, before)| match text_at(p, c, prefix) {
        Some(now) if now != *before => {
            *before = now;
            true
        }
        _ => false,
    });
}

/// Confirmed playback counters, best first: sample-rate counters grouped by the second they
/// show, largest group first (the deck position exists in several copies); within that,
/// lone integer fields before runs of adjacent counters (buffers) and doubles.
/// One pass over rekordbox's memory for values advancing at an audio rate, confirmed twice.
fn scan_counters(p: &Process) -> Vec<Counter> {
    println!("  scanning for playback positions (two passes over rekordbox's memory)...");
    let candidates = find_sample_counters(p, Duration::from_millis(400), 0.2);
    let confirmed = confirm_counters(p, &candidates, Duration::from_millis(400), 0.2);
    let mut confirmed = confirm_counters(p, &confirmed, Duration::from_millis(700), 0.2);
    confirmed.retain(|c| c.nominal_hz >= 22_000.0 && c.seconds(c.second) < 30.0 * 60.0);
    confirmed
}

fn ranked_counters(
    p: &Process,
    mode: &Mode,
    ignore: &BTreeSet<u64>,
) -> anyhow::Result<Vec<Counter>> {
    let deadline = Instant::now() + mode.step_limit;
    let confirmed = loop {
        // Known always-running counters only tell whether a deck has started; they stay
        // in the list, since the pause/resume check is what separates them from positions.
        let confirmed = scan_counters(p);
        let fresh = confirmed
            .iter()
            .filter(|c| !ignore.contains(&c.addr))
            .count();
        if fresh >= MIN_NEW_COUNTERS {
            break confirmed;
        }
        if Instant::now() >= deadline {
            bail!("no playback counter found; is a deck playing?");
        }
        println!("  no playing deck found yet; scanning again");
        std::thread::sleep(Duration::from_secs(2));
    };
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

/// Reads every counter now.
fn snapshot_counters(p: &Process, counters: &[Counter]) -> Vec<Option<f64>> {
    counters
        .iter()
        .map(|c| read_counter(p, c.addr, c.format))
        .collect()
}

/// The deck's absolute position: a counter that moves while the deck plays, holds still
/// while it is paused, and continues from the same value when play resumes. Counters that
/// measure time since play started (or keep running while paused) fail one of the three.
/// Counters that keep running whatever the deck does (time since load, audio clocks),
/// so a later scan can tell a newly playing deck from them.
type AlwaysRunning = BTreeSet<u64>;

fn absolute_position_counters(
    p: &Process,
    mode: &Mode,
    step: &str,
    deck: &str,
    ignore: &AlwaysRunning,
) -> anyhow::Result<(Vec<Counter>, AlwaysRunning)> {
    let mut counters = ranked_counters(p, mode, ignore)?;
    let scanned: Vec<Counter> = counters.clone();
    let total = counters.len();
    wait_for(mode, &format!("Step {step}b: pause {deck} (press play/pause once)."), || {
        // Time-since-play counters keep running while paused, so only a share stops.
        count_still(p, &counters, Duration::from_millis(300)) >= (total / 10).max(4)
    });
    let held = snapshot_counters(p, &counters);
    std::thread::sleep(Duration::from_millis(500));
    let again = snapshot_counters(p, &counters);
    let mut keep = Vec::new();
    for ((c, a), b) in counters.iter().zip(&held).zip(&again) {
        if let (Some(a), Some(b)) = (a, b)
            && (a - b).abs() < 1e-9
        {
            keep.push((*c, *a));
        }
    }
    println!("  {} counter(s) hold still while paused", keep.len());
    let held_counters: Vec<Counter> = keep.iter().map(|(c, _)| *c).collect();
    wait_for(mode, &format!("Step {step}c: press play on {deck} again (same track)."), || {
        any_counter_moving(p, &held_counters, Duration::from_millis(300))
    });
    let t0 = Instant::now();
    let now: Vec<Option<f64>> = keep
        .iter()
        .map(|(c, _)| read_counter(p, c.addr, c.format))
        .collect();
    std::thread::sleep(Duration::from_millis(500));
    let dt = t0.elapsed().as_secs_f64();
    counters = keep
        .iter()
        .zip(&now)
        .filter_map(|((c, at_pause), v0)| {
            let v0 = (*v0)?;
            let v1 = read_counter(p, c.addr, c.format)?;
            let moving = audio_rate((v1 - v0) / dt);
            // Continued from where it paused: never below it, and not restarted from zero.
            let continued = v1 >= *at_pause && *at_pause > 0.0;
            (moving && continued).then_some(*c)
        })
        .collect();
    println!(
        "  {} counter(s) resumed from the paused value: absolute positions",
        counters.len()
    );
    if counters.is_empty() {
        bail!("no counter behaved like an absolute position across pause and play");
    }
    let absolute: BTreeSet<u64> = counters.iter().map(|c| c.addr).collect();
    let always: AlwaysRunning = scanned
        .iter()
        .map(|c| c.addr)
        .filter(|a| !absolute.contains(a))
        .collect();
    Ok((counters, always))
}

/// Which chains resolve to a value advancing at an audio rate, all measured in one window.
fn chains_moving(p: &Process, chains: &[Chain], format: CounterFormat, dt: Duration) -> Vec<bool> {
    let addrs: Vec<Option<u64>> = chains.iter().map(|c| c.resolve(p)).collect();
    let before: Vec<Option<f64>> = addrs
        .iter()
        .map(|a| a.and_then(|a| read_counter(p, a, format)))
        .collect();
    std::thread::sleep(dt);
    let secs = dt.as_secs_f64();
    addrs
        .iter()
        .zip(&before)
        .map(|(a, v0)| {
            matches!(
                (a.and_then(|a| read_counter(p, a, format)), v0),
                (Some(v1), Some(v0)) if audio_rate((v1 - v0) / secs)
            )
        })
        .collect()
}

/// Static chains to the best counters, with the counter they read.
fn position_chains(p: &Process, counters: &[Counter]) -> anyhow::Result<(Vec<Chain>, Counter)> {
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

/// One chain in rkbx notation, for the log.
fn chain_line(c: &Chain) -> String {
    let hops: Vec<String> = c.hops.iter().map(|h| format!("{h:X}")).collect();
    format!("{:X} {}", c.root, hops.join(" "))
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

    // Step 1: position while deck 1 plays; text and path blocks as they are now.
    println!("Step 1: load a track on deck 1, make it MASTER, and press play.");
    let (counters, always) =
        absolute_position_counters(&p, mode, "1", "deck 1", &AlwaysRunning::new())?;
    let (mut pos_chains, counter) = position_chains(&p, &counters)?;
    let format = counter.format;
    println!("  {} position chain(s)", pos_chains.len());
    let mut info = text_chains(&p, TITLE_PREFIX);
    println!("  {} chain(s) to track text blocks", info.len());
    let mut anlz = text_chains(&p, ANLZ_PREFIX);
    println!("  {} chain(s) to analysis paths", anlz.len());
    let mut master_text = info.clone();
    let mut master_path = anlz.clone();

    // Step 2: a different track on deck 1. Only paths whose content changed are the deck's.
    wait_for(
        mode,
        "Step 2: stop deck 1, load a DIFFERENT track on deck 1 and press play again.",
        || {
            (any_text_changed(&p, &info, TITLE_PREFIX) || any_text_changed(&p, &anlz, ANLZ_PREFIX))
                && pos_chains
                    .iter()
                    .take(4)
                    .any(|c| chain_moves(&p, c, format, Duration::from_millis(250)))
        },
    );
    let moving = chains_moving(&p, &pos_chains, format, Duration::from_millis(400));
    pos_chains = pos_chains
        .into_iter()
        .zip(moving)
        .filter_map(|(c, m)| m.then_some(c))
        .collect();
    keep_changed(&p, &mut info, TITLE_PREFIX);
    keep_changed(&p, &mut anlz, ANLZ_PREFIX);
    println!(
        "  after the track change: {} position, {} text, {} path chain(s) follow deck 1",
        pos_chains.len(),
        info.len(),
        anlz.len()
    );
    if pos_chains.is_empty() {
        bail!("no position chain survived the track change; try again with a longer play");
    }

    // Step 3: deck 2 plays alone and gets the same treatment as deck 1: its own counter
    // scan, its own pause/resume check, its own static chains. The two decks' fields are
    // the same kind, so deck 2's chain is expected to end the way deck 1's does.
    wait_for(
        mode,
        "Step 3: stop deck 1 and leave both decks stopped for a moment.",
        || !chain_moves(&p, &pos_chains[0], format, Duration::from_millis(250)),
    );
    // Whatever still advances with both decks stopped is not a deck position.
    let mut ignore = always;
    ignore.extend(scan_counters(&p).iter().map(|c| c.addr));
    println!("  {} counter(s) run with both decks stopped; ignored", ignore.len());
    println!("Step 4: load a track on deck 2 and press play on deck 2 only.");
    let (counters2, _) = absolute_position_counters(&p, mode, "4", "deck 2", &ignore)?;
    let (chains2, _) = position_chains(&p, &counters2)?;
    let deck1 = pos_chains[0].clone();
    let same_kind: Vec<Chain> = chains2
        .iter()
        .filter(|c| c.hops.last() == deck1.hops.last())
        .cloned()
        .collect();
    let deck2_pool = if same_kind.is_empty() { chains2 } else { same_kind };
    let deck2 = deck2_pool
        .iter()
        .find(|c| infer_stride(&deck1, c).is_some())
        .or_else(|| deck2_pool.first())
        .cloned()
        .context("no static chain reaches deck 2's position")?;
    let stride = infer_stride(&deck1, &deck2);
    println!(
        "  deck 2 position: {} ({} chain(s) of deck 1's kind; stride {stride:?})",
        chain_line(&deck2),
        deck2_pool.len()
    );

    // Steps 4 and 5: MASTER moves to deck 2 and back. A byte that reads 1 then 0 is the
    // master deck index; a text or path chain that flips with it names the master track.
    let master_step = |instruction: &str, text: &[(Chain, String)], path: &[(Chain, String)]| {
        if text.is_empty() && path.is_empty() {
            wait_blind(mode, instruction);
        } else {
            wait_for(mode, instruction, || {
                any_text_changed(&p, text, TITLE_PREFIX) || any_text_changed(&p, path, ANLZ_PREFIX)
            });
        }
    };
    master_step("Step 5: press MASTER on deck 2.", &master_text, &master_path);
    let mut candidates = master_candidates(&p, &deck1, 1);
    candidates.extend(master_candidates(&p, &deck2, 1));
    candidates.sort();
    candidates.dedup();
    println!("  {} byte(s) read 1", candidates.len());
    keep_changed(&p, &mut master_text, TITLE_PREFIX);
    keep_changed(&p, &mut master_path, ANLZ_PREFIX);
    master_step("Step 6: press MASTER on deck 1.", &master_text, &master_path);
    let masters: Vec<Chain> = candidates
        .into_iter()
        .filter(|c| c.resolve(&p).and_then(|a| p.read_u8(a).ok()) == Some(0))
        .collect();
    keep_changed(&p, &mut master_text, TITLE_PREFIX);
    keep_changed(&p, &mut master_path, ANLZ_PREFIX);
    println!(
        "  {} master-deck chain(s); {} text and {} path chain(s) follow the master deck",
        masters.len(),
        master_text.len(),
        master_path.len()
    );
    for m in masters.iter().take(6) {
        println!("    {}", chain_line(m));
    }
    let master = masters.first().cloned();
    if master.is_none() {
        println!(
            "  no master-deck byte followed the MASTER button; the show will follow whichever deck plays"
        );
    }

    let offsets = assemble(&Assembly {
        version: &version,
        format,
        master,
        decks: [&deck1, &deck2],
        info: info.first().map(|(c, _)| c),
        anlz: anlz.first().map(|(c, _)| c),
        master_info: master_text.first().map(|(c, _)| c),
        master_anlz: master_path.first().map(|(c, _)| c),
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
    master: Option<Chain>,
    /// Deck 1 and deck 2, each found on its own.
    decks: [&'a Chain; 2],
    /// Deck 1's own text and path chains (shifted per deck), if any followed the deck.
    info: Option<&'a Chain>,
    anlz: Option<&'a Chain>,
    /// Chains that follow the master deck's track, used for every deck when the per-deck
    /// ones are missing: the reader only ever asks for the master deck.
    master_info: Option<&'a Chain>,
    master_anlz: Option<&'a Chain>,
    /// How deck 2's chain differs from deck 1's, when it is a single shift: decks 3 and 4
    /// are then extrapolated. Without one, only the two measured decks are written.
    stride: Option<Stride>,
}

fn assemble(a: &Assembly<'_>) -> Offsets {
    let deck_count = if a.stride.is_some() { 4 } else { 2 };
    let all_decks: Vec<DeckChains> = (0..deck_count)
        .map(|n| {
            let position = match (n, a.stride) {
                (0, _) => a.decks[0].clone(),
                (1, _) => a.decks[1].clone(),
                (_, Some(stride)) => {
                    chain_for_deck(a.decks[0], stride, n).unwrap_or_else(|| a.decks[0].clone())
                }
                (_, None) => a.decks[0].clone(),
            };
            let per_deck = |c: Option<&Chain>| match (n, a.stride) {
                (0, _) => c.cloned(),
                (_, Some(stride)) => c.and_then(|c| chain_for_deck(c, stride, n)),
                (_, None) => None,
            };
            DeckChains {
                bpm: None,
                position,
                track_info: per_deck(a.info).or_else(|| a.master_info.cloned()),
                anlz_path: per_deck(a.anlz).or_else(|| a.master_anlz.cloned()),
            }
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
