//! Pointer chains: how a value is reached from a static address in `rekordbox.exe`.
//!
//! A chain is a root (an offset from the module base) and hops. Resolving it: start at
//! `base + root`; for each hop read the pointer stored there and add the hop's offset; the
//! value lives at the address that remains. `rkbx_link`'s format splits the last hop into an
//! offset plus a "final offset"; here they are folded into the last hop.
use serde::{Deserialize, Serialize};

use super::process::Process;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Chain {
    /// Offset of the static root from the module base.
    pub root: u64,
    /// Each hop: dereference, then add this offset.
    pub hops: Vec<u64>,
}

impl Chain {
    /// Address of the value, or `None` when a hop reads an unreadable address.
    pub fn resolve(&self, p: &Process) -> Option<u64> {
        let mut addr = p.module_base.checked_add(self.root)?;
        for hop in &self.hops {
            let next = p.read_u64(addr).ok()?;
            if next == 0 {
                return None;
            }
            addr = next.checked_add(*hop)?;
        }
        Some(addr)
    }

    /// A chain from `rkbx_link`'s line format: `ROOT o1 o2 ... FINAL`, all hex.
    pub fn from_rkbx_line(line: &str) -> Option<Self> {
        let nums: Vec<u64> = line
            .split_whitespace()
            .map(|t| u64::from_str_radix(t.trim_start_matches("0x"), 16).ok())
            .collect::<Option<_>>()?;
        let (&root, rest) = nums.split_first()?;
        if rest.is_empty() {
            return None;
        }
        let (&final_offset, hops) = rest.split_last()?;
        let mut hops = hops.to_vec();
        *hops.last_mut()? += final_offset;
        Some(Self { root, hops })
    }

    /// `rkbx_link`'s line format with a zero final offset.
    pub fn to_rkbx_line(&self) -> String {
        use std::fmt::Write as _;
        let mut s = format!("{:08X}", self.root);
        for h in &self.hops {
            let _ = write!(s, " {h:X}");
        }
        s.push_str(" 0");
        s
    }

    /// The same chain for another deck when decks are `stride` apart at hop `hop_index`.
    pub fn with_hop_offset(&self, hop_index: usize, delta: u64) -> Option<Self> {
        let mut c = self.clone();
        let hop = c.hops.get_mut(hop_index)?;
        *hop = hop.checked_add(delta)?;
        Some(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rkbx_lines_fold_the_final_offset_into_the_last_hop() {
        let c = Chain::from_rkbx_line("0564B038 0 2B0 1A0").unwrap();
        assert_eq!(c.root, 0x0564_B038);
        assert_eq!(c.hops, vec![0x0, 0x2B0 + 0x1A0]);
        let m = Chain::from_rkbx_line("05737C48 20 278 124").unwrap();
        assert_eq!(m.hops, vec![0x20, 0x278 + 0x124]);
        assert!(Chain::from_rkbx_line("05737C48").is_none());
        assert!(Chain::from_rkbx_line("zz 1 2").is_none());
    }

    #[test]
    fn round_trip_keeps_the_addresses() {
        let c = Chain {
            root: 0x1234,
            hops: vec![0x8, 0x3F0],
        };
        assert_eq!(c.to_rkbx_line(), "00001234 8 3F0 0");
        assert_eq!(Chain::from_rkbx_line(&c.to_rkbx_line()).unwrap(), c);
    }

    #[test]
    fn deck_stride_shifts_one_hop() {
        let c = Chain::from_rkbx_line("0564B038 0 2B0 1A0").unwrap();
        let d2 = c.with_hop_offset(0, 8).unwrap();
        assert_eq!(d2.hops, vec![0x8, 0x2B0 + 0x1A0]);
        assert!(c.with_hop_offset(5, 8).is_none());
    }
}
