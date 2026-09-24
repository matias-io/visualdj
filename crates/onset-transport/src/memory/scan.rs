//! Finding rekordbox's live values without knowing where they are: a sample counter that
//! advances at the audio rate while a deck plays, the tempo next to it, the track-info text,
//! and then the pointer paths from static memory back to those addresses.
use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::chain::Chain;
use super::process::{Process, Region, RegionKind};

/// Sample rates a deck counter may run at.
pub const SAMPLE_RATES: [f64; 2] = [44_100.0, 48_000.0];

/// How a position counter is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterFormat {
    /// Signed 64-bit integer of samples.
    I64,
    /// Signed 32-bit integer of samples or milliseconds.
    I32,
    /// Double: seconds, milliseconds or samples.
    F64,
}

/// Units a counter may advance in, per second of playback at nominal pitch.
const RATES_PER_SECOND: [f64; 4] = [1.0, 1000.0, 44_100.0, 48_000.0];

fn read_counter(bytes: &[u8], format: CounterFormat) -> Option<f64> {
    match format {
        CounterFormat::I64 => {
            let v = i64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
            (v >= 0).then_some(v as f64)
        }
        CounterFormat::I32 => {
            let v = i32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
            (v >= 0).then_some(f64::from(v))
        }
        CounterFormat::F64 => {
            let v = f64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
            (v.is_finite() && v >= 0.0).then_some(v)
        }
    }
}

/// The nominal rate (units per second) that `advance / elapsed` matches within
/// `tolerance`, if any.
fn matching_rate(advance: f64, elapsed: f64, tolerance: f64) -> Option<(f64, f64)> {
    if advance <= 0.0 || elapsed <= 0.0 {
        return None;
    }
    let rate = advance / elapsed;
    RATES_PER_SECOND
        .iter()
        .find(|nominal| (rate / *nominal - 1.0).abs() <= tolerance)
        .map(|nominal| (*nominal, rate / *nominal))
}

/// Longest struct-relative offset the pointer scan accepts per hop.
pub const MAX_HOP_OFFSET: u64 = 0x2000;

/// Regions worth scanning for live values: the heap, not images or file mappings.
pub fn heap_regions(p: &Process) -> Vec<Region> {
    p.regions()
        .into_iter()
        .filter(|r| r.kind == RegionKind::Private && r.writable)
        .collect()
}

/// A counter that advanced like a playback position between two reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Counter {
    pub addr: u64,
    pub format: CounterFormat,
    pub first: f64,
    pub second: f64,
    /// Observed advance rate in units per second.
    pub rate_hz: f64,
    /// The nominal rate it matched (1, 1000, 44100 or 48000 per second), and the playback
    /// rate implied (1.0 = nominal pitch).
    pub nominal_hz: f64,
    pub playback_rate: f64,
}

impl Counter {
    /// Playback position in seconds implied by `value`.
    pub fn seconds(&self, value: f64) -> f64 {
        value / self.nominal_hz
    }
}

/// Longest playback position considered (four hours), in seconds.
const MAX_POSITION_S: f64 = 4.0 * 3600.0;

/// Reads every heap region twice, `dt` apart, and keeps 4-byte aligned values (i64, i32 or
/// f64) that advanced like a playback position: at 1, 1000, 44100 or 48000 units per second
/// within `pitch_tolerance` of nominal (0.16 covers a ±16 % tempo range).
pub fn find_sample_counters(p: &Process, dt: Duration, pitch_tolerance: f64) -> Vec<Counter> {
    let mut out = Vec::new();
    // Batches of about half a gigabyte: the first copy stays small and each batch gets its
    // own quiet window, so a 4 GB heap costs eight windows, not eight gigabytes.
    let mut batch: Vec<Region> = Vec::new();
    let mut batch_bytes = 0u64;
    let regions = heap_regions(p);
    for (i, r) in regions.iter().enumerate() {
        batch.push(*r);
        batch_bytes += r.size;
        if batch_bytes >= 512 * 1024 * 1024 || i + 1 == regions.len() {
            counters_in_batch(p, &batch, dt, pitch_tolerance, &mut out);
            batch.clear();
            batch_bytes = 0;
        }
    }
    out
}

fn counters_in_batch(
    p: &Process,
    regions: &[Region],
    dt: Duration,
    pitch_tolerance: f64,
    out: &mut Vec<Counter>,
) {
    let mut first: Vec<(Region, Vec<u8>)> = Vec::with_capacity(regions.len());
    for r in regions {
        if let Some(bytes) = p.read_region(r) {
            first.push((*r, bytes));
        }
    }
    let t0 = Instant::now();
    std::thread::sleep(dt);
    for (r, a) in &first {
        let Some(b) = p.read_region(r) else {
            continue;
        };
        let elapsed = t0.elapsed().as_secs_f64();
        let n = a.len().min(b.len());
        let mut i = 0;
        while i + 4 <= n {
            for format in [CounterFormat::I64, CounterFormat::F64, CounterFormat::I32] {
                let (Some(va), Some(vb)) =
                    (read_counter(&a[i..], format), read_counter(&b[i..], format))
                else {
                    continue;
                };
                if vb <= va {
                    continue;
                }
                let Some((nominal, ratio)) = matching_rate(vb - va, elapsed, pitch_tolerance)
                else {
                    continue;
                };
                if va / nominal > MAX_POSITION_S {
                    continue;
                }
                out.push(Counter {
                    addr: r.base + i as u64,
                    format,
                    first: va,
                    second: vb,
                    rate_hz: (vb - va) / elapsed,
                    nominal_hz: nominal,
                    playback_rate: ratio,
                });
                break;
            }
            i += 4;
        }
    }
}

fn read_counter_at(p: &Process, addr: u64, format: CounterFormat) -> Option<f64> {
    let mut buf = [0u8; 8];
    let len = if format == CounterFormat::I32 { 4 } else { 8 };
    p.read_exact(addr, &mut buf[..len]).ok()?;
    read_counter(&buf, format)
}

/// Re-reads every candidate twice, `dt` apart, and keeps those still advancing at their
/// nominal rate. One sleep for the whole batch, so thousands of candidates cost nothing.
pub fn confirm_counters(
    p: &Process,
    candidates: &[Counter],
    dt: Duration,
    tolerance: f64,
) -> Vec<Counter> {
    let first: Vec<Option<f64>> = candidates
        .iter()
        .map(|c| read_counter_at(p, c.addr, c.format))
        .collect();
    let t0 = Instant::now();
    std::thread::sleep(dt);
    let mut out = Vec::new();
    for (c, a) in candidates.iter().zip(first) {
        let Some(a) = a else { continue };
        let Some(b) = read_counter_at(p, c.addr, c.format) else {
            continue;
        };
        let elapsed = t0.elapsed().as_secs_f64();
        if b > a && (((b - a) / elapsed) / c.nominal_hz - 1.0).abs() <= tolerance {
            out.push(Counter {
                first: a,
                second: b,
                rate_hz: (b - a) / elapsed,
                playback_rate: ((b - a) / elapsed) / c.nominal_hz,
                ..*c
            });
        }
    }
    out
}

/// Confirms one counter keeps advancing at its rate over another interval.
pub fn counter_still_advances(p: &Process, c: &Counter, dt: Duration, tolerance: f64) -> bool {
    confirm_counters(p, std::slice::from_ref(c), dt, tolerance).len() == 1
}

/// 4-byte aligned f32 values within `tolerance` of `value`, in a window around `centre`.
pub fn find_f32_near(
    p: &Process,
    centre: u64,
    window: u64,
    value: f32,
    tolerance: f32,
) -> Vec<u64> {
    let start = centre.saturating_sub(window);
    let len = (window * 2) as usize;
    let mut buf = vec![0u8; len];
    let mut out = Vec::new();
    // A short read means the window crossed a page boundary; fall back to two halves.
    if p.read_exact(start, &mut buf).is_err() {
        let half = window as usize;
        if p.read_exact(centre, &mut buf[half..]).is_err() {
            return out;
        }
        buf[..half].fill(0);
    }
    for i in (0..len - 3).step_by(4) {
        let v = f32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
        if (v - value).abs() <= tolerance {
            out.push(start + i as u64);
        }
    }
    out
}

/// Every occurrence of `needle` in heap memory (capped).
pub fn find_bytes(p: &Process, needle: &[u8], cap: usize) -> Vec<u64> {
    let mut out = Vec::new();
    for r in heap_regions(p) {
        let Some(bytes) = p.read_region(&r) else {
            continue;
        };
        let mut from = 0;
        while let Some(pos) = find_sub(&bytes[from..], needle) {
            out.push(r.base + (from + pos) as u64);
            if out.len() >= cap {
                return out;
            }
            from += pos + 1;
        }
    }
    out
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// One level of the pointer scan: where in memory does an 8-byte value point into
/// `[target - MAX_HOP_OFFSET, target]` for any of `targets`? Returns (pointer address,
/// pointed target, offset) sorted by offset.
fn pointers_into(p: &Process, regions: &[Region], targets: &[u64]) -> Vec<(u64, u64, u64)> {
    let mut sorted = targets.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let lo_bound = sorted
        .first()
        .map_or(0, |t| t.saturating_sub(MAX_HOP_OFFSET));
    let hi_bound = sorted.last().copied().unwrap_or(0);
    let mut out = Vec::new();
    for r in regions {
        let Some(bytes) = p.read_region(r) else {
            continue;
        };
        let n = bytes.len() / 8 * 8;
        let mut i = 0;
        while i + 8 <= n {
            let v = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
            if v >= lo_bound && v <= hi_bound {
                // The smallest target at or above v, if within range.
                let idx = sorted.partition_point(|t| *t < v);
                if let Some(t) = sorted.get(idx)
                    && t - v <= MAX_HOP_OFFSET
                {
                    out.push((r.base + i as u64, *t, t - v));
                }
            }
            i += 8;
        }
    }
    out.sort_by_key(|(_, _, off)| *off);
    out
}

/// Breadth-first pointer scan from `target` back to static roots inside the module.
/// `max_branch` limits how many intermediate pointers per level are followed (smallest
/// offsets first), which keeps a level at one pass over memory.
pub fn pointer_scan(p: &Process, target: u64, max_depth: usize, max_branch: usize) -> Vec<Chain> {
    let mut regions = heap_regions(p);
    regions.extend(p.static_regions());
    // Each frontier entry: the address we need a pointer to, and the hops collected so far
    // (from that address down to the value).
    let mut frontier: Vec<(u64, Vec<u64>)> = vec![(target, Vec::new())];
    let mut seen: HashSet<u64> = HashSet::new();
    let mut found = Vec::new();
    for _depth in 0..max_depth {
        if frontier.is_empty() {
            break;
        }
        let targets: Vec<u64> = frontier.iter().map(|(t, _)| *t).collect();
        let hits = pointers_into(p, &regions, &targets);
        let mut next: Vec<(u64, Vec<u64>)> = Vec::new();
        for (ptr_addr, t, off) in hits {
            let Some((_, hops_below)) = frontier.iter().find(|(ft, _)| *ft == t) else {
                continue;
            };
            let mut hops = vec![off];
            hops.extend_from_slice(hops_below);
            if p.is_in_module(ptr_addr) {
                found.push(Chain {
                    root: ptr_addr - p.module_base,
                    hops,
                });
            } else if seen.insert(ptr_addr) && next.len() < max_branch {
                next.push((ptr_addr, hops));
            }
        }
        frontier = next;
    }
    found.sort_by_key(|c| (c.hops.len(), c.hops.iter().sum::<u64>()));
    found.dedup();
    found
}

/// Known chain shapes from `rkbx_link`'s 7.2.2 offsets (hops only; roots are re-derived).
/// If a newer rekordbox kept the structures, only the static roots moved.
pub fn known_tails() -> Vec<(&'static str, Chain)> {
    let mk = |line: &str| Chain::from_rkbx_line(line).expect("literal");
    vec![
        ("bpm", mk("0 0 2B0 1A0")),
        ("sample_position", mk("0 0 2B0 130")),
        ("track_info", mk("0 20 410 80 168 F0 0")),
        ("anlz_path", mk("0 8 3F0 0")),
        ("master_deck", mk("0 20 278 124")),
    ]
}

/// Tries every static pointer slot in the module as the root of `tail`, keeping the roots
/// whose resolved address satisfies `accept`.
pub fn roots_for_tail(p: &Process, tail: &Chain, accept: impl Fn(u64) -> bool) -> Vec<Chain> {
    let mut out = Vec::new();
    for r in p.static_regions() {
        let Some(bytes) = p.read_region(&r) else {
            continue;
        };
        let n = bytes.len() / 8 * 8;
        let mut i = 0;
        while i + 8 <= n {
            let v = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
            // Static roots hold heap pointers: skip nulls and small integers cheaply.
            if v > 0x10000 && v < 0x7FFF_FFFF_FFFF {
                let chain = Chain {
                    root: r.base + i as u64 - p.module_base,
                    hops: tail.hops.clone(),
                };
                if let Some(addr) = chain.resolve(p)
                    && accept(addr)
                {
                    out.push(chain);
                }
            }
            i += 8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_search_finds_first_match() {
        assert_eq!(find_sub(b"xxTrack Title: A", b"Track Title"), Some(2));
        assert_eq!(find_sub(b"short", b"longer needle"), None);
        assert_eq!(find_sub(b"abc", b""), None);
    }

    #[test]
    fn known_tails_have_rkbx_shapes() {
        let tails = known_tails();
        let bpm = &tails.iter().find(|(n, _)| *n == "bpm").unwrap().1;
        assert_eq!(bpm.hops, vec![0x0, 0x2B0 + 0x1A0]);
        let info = &tails.iter().find(|(n, _)| *n == "track_info").unwrap().1;
        assert_eq!(info.hops.len(), 5, "five dereferences, final offset folded");
    }
}
