//! `calibrate`: derive the pointer chains for the running rekordbox version and write
//! `offsets/<version>.toml`. The operator plays and swaps tracks on deck 1, plays deck 2 and
//! moves MASTER; the calibrator keeps only the chains that follow each change.
//!
//! Two ways to drive it: interactive (Enter after each step, titles and BPM typed in) or
//! `--auto`, where each step is announced, the tool waits, and it works the titles out from
//! the text blocks that appear and the tempo from the library.
#![cfg(windows)]

use std::collections::BTreeSet;
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use onset_rekordbox::library::Library;
use onset_transport::memory::REKORDBOX_EXE;
use onset_transport::memory::calibrate::{
    Stride, chain_for_deck, infer_stride, rank_chains, sibling_field, surviving_chains,
};
use onset_transport::memory::chain::Chain;
use onset_transport::memory::offsets::{DeckChains, Offsets, PositionFormat};
use onset_transport::memory::process::Process;
use onset_transport::memory::reader::{Mem, parse_track_info};
use onset_transport::memory::scan::{
    Counter, CounterFormat, confirm_counters, find_bytes, find_f32_near, find_sample_counters,
    pointer_scan,
};

const DEPTH: usize = 6;
const BRANCH: usize = 64;
const TITLE_PREFIX: &[u8] = b"Track Title: ";

pub enum Mode {
    Interactive,
    /// Announce each step, wait this long, then read the state.
    Auto {
        step: Duration,
    },
}

struct Session {
    p: Process,
    mode: Mode,
    library: Option<Library>,
    /// Text blocks seen so far, so the next new one is the track just loaded.
    titles_seen: BTreeSet<String>,
}

fn prompt(text: &str) -> anyhow::Result<String> {
    print!("{text}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
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

/// Every `Track Title: …` block currently in memory, as parsed text.
fn title_blocks(p: &Process) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for addr in find_bytes(p, TITLE_PREFIX, 64) {
        let mut buf = vec![0u8; 200];
        if p.read_exact(addr, &mut buf).is_ok() {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            out.insert(String::from_utf8_lossy(&buf[..end]).into_owned());
        }
    }
    out
}

fn position_chains(p: &Process) -> anyhow::Result<(Vec<Chain>, Counter)> {
    println!("  scanning for the playing position (two passes over rekordbox's memory)...");
    let candidates = find_sample_counters(p, Duration::from_millis(400), 0.2);
    let confirmed = confirm_counters(p, &candidates, Duration::from_millis(400), 0.2);
    let confirmed = confirm_counters(p, &confirmed, Duration::from_millis(700), 0.2);
    if confirmed.is_empty() {
        bail!("no playback counter found; is a deck playing?");
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

impl Session {
    /// Announces a step and waits for the operator (Enter, or the auto delay).
    fn step(&self, instruction: &str) -> anyhow::Result<()> {
        match self.mode {
            Mode::Interactive => {
                prompt(&format!("{instruction} Press Enter when done. "))?;
            }
            Mode::Auto { step } => {
                println!("{instruction} (continuing in {} s)", step.as_secs());
                let deadline = Instant::now() + step;
                while Instant::now() < deadline {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
        Ok(())
    }

    /// The title on deck 1: typed, or the text block that is new since the last look.
    fn deck_title(&mut self, question: &str) -> anyhow::Result<String> {
        match self.mode {
            Mode::Interactive => {
                let t = prompt(question)?;
                self.titles_seen.insert(format!("Track Title: {t}"));
                Ok(t)
            }
            Mode::Auto { .. } => {
                let now = title_blocks(&self.p);
                let fresh: Vec<&String> = now.difference(&self.titles_seen).collect();
                let block = fresh
                    .last()
                    .map(|s| (*s).clone())
                    .or_else(|| now.iter().next_back().cloned())
                    .context("no track text block in memory")?;
                self.titles_seen.extend(now.iter().cloned());
                let onset_core::transport::TrackRef::TitleArtist { title, .. } =
                    parse_track_info(&block)
                else {
                    bail!("unreadable track block {block:?}")
                };
                println!("  deck track appears to be {title:?}");
                Ok(title)
            }
        }
    }

    /// The tempo the deck shows: typed, or the library's BPM scaled by the observed rate.
    fn deck_bpm(&self, title: &str, rate: f64) -> anyhow::Result<f32> {
        match self.mode {
            Mode::Interactive => prompt("BPM shown on deck 1 right now (e.g. 124.00): ")?
                .parse()
                .context("BPM must be a number"),
            Mode::Auto { .. } => {
                let bpm = self
                    .library
                    .as_ref()
                    .and_then(|l| l.find_by_title_artist(title, ""))
                    .and_then(|t| t.bpm)
                    .with_context(|| format!("library has no BPM for {title:?}"))?;
                let shown = bpm * rate as f32;
                println!(
                    "  library says {bpm:.2} BPM; at rate x{rate:.4} the deck shows about {shown:.2}"
                );
                Ok(shown)
            }
        }
    }
}

#[allow(clippy::too_many_lines)] // one guided session, read top to bottom
pub fn calibrate(out_dir: &Path, mode: Mode, app_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let p = Process::open(REKORDBOX_EXE)?;
    let version = p.file_version().context("rekordbox version unknown")?;
    println!(
        "rekordbox {version} (pid {}), module at {:#x}",
        p.pid, p.module_base
    );
    println!("This takes a few minutes and needs the decks to yourself.");
    let library = match crate::common::open_library(app_dir) {
        Ok((_, lib)) => Some(lib),
        Err(e) => {
            println!("  (library unavailable, tempo will be asked for: {e:#})");
            None
        }
    };
    let mut s = Session {
        p,
        mode,
        library,
        titles_seen: BTreeSet::new(),
    };
    if matches!(s.mode, Mode::Auto { .. }) {
        // Everything already in memory is old news; only new blocks identify loads.
        s.titles_seen = title_blocks(&s.p);
    }

    // Step 1: position and its neighbours, while deck 1 plays.
    s.step("Step 1: load a track on deck 1, make it MASTER, and press play.")?;
    let title = s.deck_title("Title of the track on deck 1, exactly as rekordbox shows it: ")?;
    let (mut pos_chains, counter) = position_chains(&s.p)?;
    let format = counter.format;
    let nominal = counter.nominal_hz;

    let bpm = s.deck_bpm(&title, counter.playback_rate)?;
    let tolerance = match s.mode {
        Mode::Interactive => 0.011,
        Mode::Auto { .. } => bpm * 0.012,
    };
    let bpm_offsets: Vec<i64> = find_f32_near(&s.p, counter.addr, 0x400, bpm, tolerance)
        .into_iter()
        .map(|a| a.cast_signed() - counter.addr.cast_signed())
        .collect();
    println!("  tempo candidates near the position: {bpm_offsets:?} bytes");

    let needle = format!("Track Title: {title}\n");
    let mut info_chains = text_chains(&s.p, needle.as_bytes());
    println!("  {} chain(s) to the deck 1 text block", info_chains.len());
    let mut anlz_chains = text_chains(&s.p, b"PIONEER/USBANLZ/");
    println!("  {} chain(s) to analysis paths", anlz_chains.len());

    // Step 2: a different track on deck 1. Only chains that follow it are the deck's own.
    s.step("Step 2: load a DIFFERENT track on deck 1 and press play again.")?;
    let title2 = s.deck_title("Title of the new track on deck 1: ")?;
    std::thread::sleep(Duration::from_millis(300));
    pos_chains = surviving_chains(&pos_chains, |c| {
        chain_moves(&s.p, c, format, nominal, Duration::from_millis(250))
    });
    let needle2 = format!("Track Title: {title2}\n");
    info_chains = surviving_chains(&info_chains, |c| {
        chain_text_starts_with(&s.p, c, needle2.as_bytes())
    });
    anlz_chains = surviving_chains(&anlz_chains, |c| {
        chain_text_starts_with(&s.p, c, b"PIONEER/USBANLZ/")
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
            c.resolve(&s.p)
                .and_then(|a| s.p.read_f32(a).ok())
                .is_some_and(|v| (60.0..=220.0).contains(&v))
        });
    if bpm_chain.is_none() {
        println!("  warning: no tempo field confirmed next to the position; deck BPM will read 0");
    }

    // Step 3: deck 2, to learn the stride between decks.
    s.step("Step 3: stop deck 1. Load a track on deck 2 and press play on deck 2 only.")?;
    let (deck2_pos, _) = position_chains(&s.p)?;
    let stride = pos_chains
        .iter()
        .find_map(|d1| deck2_pos.iter().find_map(|d2| infer_stride(d1, d2)))
        .context("could not relate deck 2's position chain to deck 1's")?;
    println!("  deck stride: {stride:?}");

    // Step 4: master deck. A byte that reads 1 with deck 2 as master and 0 with deck 1.
    s.step("Step 4: press MASTER on deck 2.")?;
    let master_candidates = master_candidates(&s.p, &pos_chains[0], 1);
    println!("  {} byte(s) read 1", master_candidates.len());
    s.step("Step 5: press MASTER on deck 1.")?;
    let master_chains: Vec<Chain> = master_candidates
        .into_iter()
        .filter(|c| c.resolve(&s.p).and_then(|a| s.p.read_u8(a).ok()) == Some(0))
        .collect();
    println!("  {} master-deck chain(s)", master_chains.len());
    let master = master_chains
        .first()
        .cloned()
        .context("no master-deck byte followed the MASTER button")?;

    let offsets = assemble(&Assembly {
        version: &version,
        format,
        nominal,
        master,
        position: &pos_chains[0],
        bpm: bpm_chain.as_ref(),
        info: info_chains.first(),
        anlz: anlz_chains.first(),
        stride,
        titles: (&title, &title2),
    });
    let path = out_dir.join(format!("{version}.toml"));
    offsets.save(&path)?;
    println!("wrote {}", path.display());
    Ok(())
}

struct Assembly<'a> {
    version: &'a str,
    format: CounterFormat,
    nominal: f64,
    master: Chain,
    position: &'a Chain,
    bpm: Option<&'a Chain>,
    info: Option<&'a Chain>,
    anlz: Option<&'a Chain>,
    stride: Stride,
    titles: (&'a str, &'a str),
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
        position_rate_hz: a.nominal,
        master_deck: a.master.clone(),
        decks: all_decks,
        provenance: Some(format!(
            "onset-cli calibrate at unix {} with tracks {:?} and {:?}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            a.titles.0,
            a.titles.1
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
