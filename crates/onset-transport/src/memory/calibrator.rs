//! The guided calibration for a new rekordbox version: find a deck's playback position in
//! memory and describe the object that holds it, so `offsets/<version>.toml` can find every
//! deck at runtime by scanning for that description (see `DeckSignature`).
//!
//! The operator plays a track, pauses it and plays it again. A counter that advances while
//! the deck plays, holds still while it is paused and resumes from the same value is the
//! deck's position; everything else that ticks at an audio rate (time-since-play clocks,
//! output counters) fails one of the three. Around the position sits the player object,
//! which carries pointers into the rekordbox module (its vtables and statics). Those
//! pointers at their distances from the position are the signature. The track's identity
//! does not come from memory at all: rekordbox keeps the loaded files open.
//!
//! The CLI and the launcher both drive this; only the reporter differs.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use super::REKORDBOX_EXE;
use super::calibrate::{rarest_first, shared_signature};
use super::offsets::{Anchor, DeckSignature, Offsets, PositionFormat};
use super::process::Process;
use super::reader::Mem;
use super::scan::{
    Counter, CounterFormat, confirm_counters, find_bytes, find_sample_counters, find_u64_values,
};

/// A scan counts as "a deck is playing" once this many counters advance.
const MIN_COUNTERS: usize = 8;
/// Counters that must stop for the pause to count (the position has a few copies).
const MIN_STOPPED: usize = 3;
/// Bytes below and above the position field searched for module pointers.
const ANCHOR_BELOW: u64 = 0x400;
const ANCHOR_ABOVE: u64 = 0x40;
/// A usable signature has at least this many anchors.
const MIN_ANCHORS: usize = 3;
/// Hits counted when ranking anchors by rarity; the rarest is scanned for at runtime.
const RARITY_CAP: usize = 24;
/// Position candidates whose surroundings are examined, best first.
const CANDIDATES: usize = 24;

const POLL: Duration = Duration::from_millis(500);
/// Settling time after a change is seen, so rekordbox finishes its own bookkeeping first.
const SETTLE: Duration = Duration::from_millis(800);

/// The steps the operator performs, in order, as the launcher lists them up front.
pub const STEPS: &[&str] = &[
    "Load a track on any deck and press play",
    "Pause that deck",
    "Press play on it again",
];

/// One calibration run's settings and its way of talking to the operator.
pub struct Session<'a> {
    /// How long each step may take the operator before the calibrator gives up on it.
    pub step_limit: Duration,
    /// Receives every progress line (the CLI prints them, the launcher shows them).
    pub report: &'a mut dyn FnMut(&str),
    /// Set by the caller to abandon the run at the next opportunity.
    pub cancel: &'a AtomicBool,
}

impl Session<'_> {
    fn say(&mut self, text: impl AsRef<str>) {
        (self.report)(text.as_ref());
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Sleeps in short slices so a cancel request is honoured within a tenth of a second.
    fn sleep(&self, total: Duration) -> anyhow::Result<()> {
        let deadline = Instant::now() + total;
        while Instant::now() < deadline {
            if self.cancelled() {
                bail!("calibration cancelled");
            }
            std::thread::sleep(Duration::from_millis(100).min(deadline - Instant::now()));
        }
        Ok(())
    }
}

/// Announces a step and returns once `seen` reports the change it describes, or fails
/// once the step limit passes.
fn wait_for(
    s: &mut Session<'_>,
    instruction: &str,
    mut seen: impl FnMut() -> bool,
) -> anyhow::Result<()> {
    s.say(format!(
        "{instruction} (waiting up to {} s)",
        s.step_limit.as_secs()
    ));
    let deadline = Instant::now() + s.step_limit;
    loop {
        if s.cancelled() {
            bail!("calibration cancelled");
        }
        if seen() {
            s.say("  seen");
            s.sleep(SETTLE)?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("{instruction}: not seen in time");
        }
        s.sleep(POLL)?;
    }
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

/// Reads every counter now.
fn snapshot_counters(p: &Process, counters: &[Counter]) -> Vec<Option<f64>> {
    counters
        .iter()
        .map(|c| read_counter(p, c.addr, c.format))
        .collect()
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

/// One pass over rekordbox's memory for values advancing at an audio rate, confirmed twice.
fn scan_counters(s: &mut Session<'_>, p: &Process) -> Vec<Counter> {
    s.say("  scanning for playback positions (two passes over rekordbox's memory)...");
    let candidates = find_sample_counters(p, Duration::from_millis(400), 0.2);
    let confirmed = confirm_counters(p, &candidates, Duration::from_millis(400), 0.2);
    let mut confirmed = confirm_counters(p, &confirmed, Duration::from_millis(700), 0.2);
    confirmed.retain(|c| c.nominal_hz >= 22_000.0 && c.seconds(c.second) < 30.0 * 60.0);
    confirmed
}

/// Confirmed playback counters, best first: grouped by the second they show, largest group
/// first (the position exists in several copies); within that, 32-bit integers first, since
/// a 32-bit field read as 64 bits drags its neighbour along. Scans again until a deck plays.
fn ranked_counters(s: &mut Session<'_>, p: &Process) -> anyhow::Result<Vec<Counter>> {
    let deadline = Instant::now() + s.step_limit;
    let confirmed = loop {
        let confirmed = scan_counters(s, p);
        if confirmed.len() >= MIN_COUNTERS {
            break confirmed;
        }
        if Instant::now() >= deadline {
            bail!("no playback counter found; is a deck playing?");
        }
        s.say("  no playing deck found yet; scanning again");
        s.sleep(Duration::from_secs(2))?;
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
    best.sort_by_key(|c| match c.format {
        CounterFormat::I32 => 0,
        CounterFormat::I64 => 1,
        CounterFormat::F64 => 2,
    });
    s.say(format!(
        "  {} counter(s) confirmed; the largest group reads {:.1} s",
        best.len(),
        best[0].seconds(best[0].second)
    ));
    Ok(best)
}

/// The deck's absolute position: a counter that moves while the deck plays, holds still
/// while it is paused, and continues from the same value when play resumes.
fn absolute_position_counters(s: &mut Session<'_>, p: &Process) -> anyhow::Result<Vec<Counter>> {
    let counters = ranked_counters(s, p)?;
    wait_for(
        s,
        "Step 2: pause that deck (press play/pause once).",
        || {
            // Every counter here was advancing; a few of them stopping is the pause (time-
            // since-play clocks and any other deck keep running).
            count_still(p, &counters, Duration::from_millis(300)) >= MIN_STOPPED
        },
    )?;
    let held = snapshot_counters(p, &counters);
    s.sleep(Duration::from_millis(500))?;
    let again = snapshot_counters(p, &counters);
    let mut keep = Vec::new();
    for ((c, a), b) in counters.iter().zip(&held).zip(&again) {
        if let (Some(a), Some(b)) = (a, b)
            && (a - b).abs() < 1e-9
        {
            keep.push((*c, *a));
        }
    }
    s.say(format!(
        "  {} counter(s) hold still while paused",
        keep.len()
    ));
    let held_counters: Vec<Counter> = keep.iter().map(|(c, _)| *c).collect();
    wait_for(s, "Step 3: press play on that deck again.", || {
        any_counter_moving(p, &held_counters, Duration::from_millis(300))
    })?;
    let t0 = Instant::now();
    let now: Vec<Option<f64>> = keep
        .iter()
        .map(|(c, _)| read_counter(p, c.addr, c.format))
        .collect();
    s.sleep(Duration::from_millis(500))?;
    let dt = t0.elapsed().as_secs_f64();
    let absolute: Vec<Counter> = keep
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
    s.say(format!(
        "  {} counter(s) resumed from the paused value: absolute positions",
        absolute.len()
    ));
    if absolute.is_empty() {
        bail!("no counter behaved like an absolute position across pause and play");
    }
    Ok(absolute)
}

/// Module pointers around a position field: (offset from the field, module-relative
/// value), 8-byte aligned, within the window.
fn anchors_around(p: &Process, pos: u64) -> Vec<Anchor> {
    let start = pos.saturating_sub(ANCHOR_BELOW);
    let len = usize::try_from(ANCHOR_BELOW + ANCHOR_ABOVE).unwrap_or(0x440);
    let mut buf = vec![0u8; len];
    // The window may straddle an unreadable page; shrink from the bottom until it reads.
    let mut from = start;
    while from < pos {
        let want = usize::try_from(pos + ANCHOR_ABOVE - from).unwrap_or(0);
        if p.read_exact(from, &mut buf[..want]).is_ok() {
            buf.truncate(want);
            break;
        }
        from += 0x100;
    }
    if from >= pos {
        return Vec::new();
    }
    let mut out = Vec::new();
    let first_aligned = (8 - (from % 8)) % 8;
    let mut i = usize::try_from(first_aligned).unwrap_or(0);
    while i + 8 <= buf.len() {
        let v = u64::from_le_bytes(buf[i..i + 8].try_into().unwrap_or([0; 8]));
        if p.is_in_module(v) {
            let addr = from + i as u64;
            out.push(Anchor {
                offset: addr.cast_signed() - pos.cast_signed(),
                module_offset: v - p.module_base,
            });
        }
        i += 8;
    }
    out
}

/// Among the position candidates, the anchor set shared by the most of them (the deck
/// objects all have the same layout, so the real position's anchors repeat per loaded deck
/// while stray copies of the position have none or different ones).
fn best_signature(
    s: &mut Session<'_>,
    p: &Process,
    counters: &[Counter],
) -> anyhow::Result<(DeckSignature, Counter)> {
    let mut by_set: BTreeMap<Vec<(i64, u64)>, Vec<Counter>> = BTreeMap::new();
    for c in counters.iter().take(CANDIDATES) {
        let anchors = anchors_around(p, c.addr);
        if anchors.len() < MIN_ANCHORS {
            continue;
        }
        let key: Vec<(i64, u64)> = anchors
            .iter()
            .map(|a| (a.offset, a.module_offset))
            .collect();
        by_set.entry(key).or_default().push(*c);
    }
    let (key, members) = by_set
        .into_iter()
        .max_by_key(|(k, v)| (v.len(), k.len()))
        .context("no position candidate sits in an object with module pointers")?;
    let mut anchors: Vec<Anchor> = key
        .iter()
        .map(|(offset, module_offset)| Anchor {
            offset: *offset,
            module_offset: *module_offset,
        })
        .collect();
    s.say(format!(
        "  {} anchor(s) around {} position field(s) of the same layout",
        anchors.len(),
        members.len()
    ));
    // The runtime scans for the first anchor's bytes: make that the rarest one.
    let mut rarity: Vec<(usize, Anchor)> = anchors
        .iter()
        .take(6)
        .map(|a| {
            let needle = (p.module_base + a.module_offset).to_le_bytes();
            let hits = find_bytes(p, &needle, RARITY_CAP).len();
            let sign = if a.offset < 0 { "-" } else { "+" };
            s.say(format!(
                "    module+{:#x} at {sign}{:#x}: {hits} occurrence(s)",
                a.module_offset,
                a.offset.unsigned_abs()
            ));
            (hits, a.clone())
        })
        .collect();
    rarity.sort_by_key(|(hits, a)| (*hits, a.offset));
    let first = rarity[0].1.clone();
    anchors.retain(|a| *a != first);
    anchors.insert(0, first);
    let counter = members
        .iter()
        .copied()
        .min_by_key(|c| match c.format {
            CounterFormat::I32 => 0,
            CounterFormat::I64 => 1,
            CounterFormat::F64 => 2,
        })
        .unwrap_or(counters[0]);
    Ok((
        DeckSignature {
            anchors,
            max_decks: 4,
            master_flag: None,
        },
        counter,
    ))
}

/// Hits kept per anchor value when looking for sibling decks.
const SIBLING_HITS: usize = 256;

/// Cuts `anchors` (seen around the deck at `primary`) down to those every deck object of
/// the same class holds, so the runtime scan finds all decks, not only the calibrated one.
fn refine(
    s: &mut Session<'_>,
    p: &Process,
    primary: u64,
    anchors: &[Anchor],
) -> anyhow::Result<(Vec<Anchor>, Vec<u64>)> {
    let values: Vec<u64> = anchors
        .iter()
        .map(|a| p.module_base + a.module_offset)
        .collect();
    let hits = find_u64_values(p, &values, SIBLING_HITS);
    let shared = shared_signature(p, primary, anchors, &hits);
    let counts: Vec<usize> = shared
        .anchors
        .iter()
        .map(|a| {
            anchors
                .iter()
                .position(|b| b == a)
                .map_or(0, |i| hits[i].len())
        })
        .collect();
    let mut kept = shared.anchors;
    rarest_first(&mut kept, &counts, shared.decks.len());
    s.say(format!(
        "  {} deck object(s) share {} of {} anchor(s)",
        shared.decks.len(),
        kept.len(),
        anchors.len()
    ));
    if kept.len() < MIN_ANCHORS {
        bail!("the deck objects share too few pointers to recognise them");
    }
    if shared.decks.len() < 2 {
        s.say(
            "  only one deck object found; decks that have never loaded a track may not exist yet",
        );
    }
    Ok((kept, shared.decks))
}

/// Re-derives the shared anchors of an existing offsets file against the running rekordbox,
/// without any deck interaction. Fixes files that only find the deck they were made from.
pub fn refine_existing(dir: &Path, s: &mut Session<'_>) -> anyhow::Result<PathBuf> {
    let p = Process::open(REKORDBOX_EXE)?;
    let version = p.file_version().context("rekordbox version unknown")?;
    let path = dir.join(format!("{version}.toml"));
    let mut offsets = Offsets::load(&path)
        .with_context(|| format!("no offsets for rekordbox {version} in {}", dir.display()))?;
    let sig = offsets
        .signature
        .clone()
        .context("this offsets file has no signature to refine")?;
    let needle = (p.module_base + sig.anchors[0].module_offset).to_le_bytes();
    let found: Vec<u64> = find_bytes(&p, &needle, SIBLING_HITS)
        .iter()
        .filter_map(|h| h.checked_add_signed(-sig.anchors[0].offset))
        .filter(|pos| super::reader::signature_matches(&p, &sig, *pos))
        .collect();
    let primary = *found
        .first()
        .context("the current signature finds no deck; recalibrate")?;
    s.say(format!(
        "rekordbox {version}: the current signature finds {} deck(s)",
        found.len()
    ));
    let (anchors, _) = refine(s, &p, primary, &sig.anchors)?;
    offsets.signature = Some(DeckSignature { anchors, ..sig });
    offsets.save(&path)?;
    s.say(format!("wrote {}", path.display()));
    Ok(path)
}

/// Runs the guided session against the running rekordbox and writes
/// `<out_dir>/<version>.toml`, returning its path.
pub fn calibrate(out_dir: &Path, s: &mut Session<'_>) -> anyhow::Result<PathBuf> {
    let p = Process::open(REKORDBOX_EXE)?;
    let version = p.file_version().context("rekordbox version unknown")?;
    s.say(format!(
        "rekordbox {version} (pid {}), module at {:#x}",
        p.pid, p.module_base
    ));
    s.say("This takes a minute or two: play a track, pause it, play it again.");
    s.say("Step 1: load a track on any deck and press play.");
    let counters = absolute_position_counters(s, &p)?;
    let (mut signature, counter) = best_signature(s, &p, &counters)?;
    let (anchors, _) = refine(s, &p, counter.addr, &signature.anchors)?;
    signature.anchors = anchors;

    // The signature must find the deck it came from when scanned for at runtime.
    let needle = (p.module_base + signature.anchors[0].module_offset).to_le_bytes();
    let hits = find_bytes(&p, &needle, RARITY_CAP);
    let found: BTreeSet<u64> = hits
        .iter()
        .filter_map(|h| h.checked_add_signed(-signature.anchors[0].offset))
        .filter(|pos| super::reader::signature_matches(&p, &signature, *pos))
        .collect();
    s.say(format!(
        "  a runtime scan finds {} deck object(s): {}",
        found.len(),
        found
            .iter()
            .map(|a| format!("{a:#x}"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    if !found.contains(&counter.addr) {
        bail!("the signature does not find the position it was derived from");
    }

    let offsets = Offsets {
        rekordbox_version: version.clone(),
        platform: "windows".into(),
        position_format: match counter.format {
            CounterFormat::I64 => PositionFormat::I64,
            CounterFormat::I32 => PositionFormat::I32,
            CounterFormat::F64 => PositionFormat::F64,
        },
        // Positions count samples of the loaded file (44.1 or 48 kHz files differ).
        position_rate_hz: 0.0,
        master_deck: None,
        decks: Vec::new(),
        signature: Some(signature),
        provenance: Some(format!(
            "onset calibrate at unix {}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        )),
    };
    let path = out_dir.join(format!("{version}.toml"));
    offsets.save(&path)?;
    s.say(format!("wrote {}", path.display()));
    Ok(path)
}
