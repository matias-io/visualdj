//! The decisions behind the calibrator: which discovered chains survive a track change,
//! how deck 2's chains relate to deck 1's, and how a tempo field is expressed relative to
//! the position field. Pure functions, tested without a process.
use super::chain::Chain;

/// Keeps the chains for which `still_valid` holds after the environment changed (a new track
/// was loaded, rekordbox was restarted). Order is preserved.
pub fn surviving_chains(candidates: &[Chain], still_valid: impl Fn(&Chain) -> bool) -> Vec<Chain> {
    candidates
        .iter()
        .filter(|c| still_valid(c))
        .cloned()
        .collect()
}

/// Where deck 2's chain differs from deck 1's: `(hop index, delta)` when exactly one hop
/// differs and the roots match, or `Root(delta)` when only the root differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stride {
    Root(u64),
    Hop { index: usize, delta: u64 },
}

pub fn infer_stride(deck1: &Chain, deck2: &Chain) -> Option<Stride> {
    if deck1.hops.len() != deck2.hops.len() {
        return None;
    }
    let differing: Vec<usize> = (0..deck1.hops.len())
        .filter(|i| deck1.hops[*i] != deck2.hops[*i])
        .collect();
    match (deck1.root == deck2.root, differing.as_slice()) {
        (true, [i]) => Some(Stride::Hop {
            index: *i,
            delta: deck2.hops[*i].checked_sub(deck1.hops[*i])?,
        }),
        (false, []) => Some(Stride::Root(deck2.root.checked_sub(deck1.root)?)),
        _ => None,
    }
}

/// Deck `n` (0-based) from deck 1's chain and the stride between decks.
pub fn chain_for_deck(deck1: &Chain, stride: Stride, n: u64) -> Option<Chain> {
    match stride {
        Stride::Root(delta) => Some(Chain {
            root: deck1.root.checked_add(delta.checked_mul(n)?)?,
            hops: deck1.hops.clone(),
        }),
        Stride::Hop { index, delta } => deck1.with_hop_offset(index, delta.checked_mul(n)?),
    }
}

/// A field that sits `delta` bytes from the position field in the same struct shares every
/// hop but the last.
pub fn sibling_field(position: &Chain, delta: i64) -> Option<Chain> {
    let mut c = position.clone();
    let last = c.hops.last_mut()?;
    let moved = i64::try_from(*last).ok()?.checked_add(delta)?;
    *last = u64::try_from(moved).ok()?;
    Some(c)
}

/// Prefers short chains with small offsets: they are the ones that survive point releases.
pub fn rank_chains(chains: &mut [Chain]) {
    chains.sort_by_key(|c| (c.hops.len(), c.hops.iter().sum::<u64>(), c.root));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(root: u64, hops: &[u64]) -> Chain {
        Chain {
            root,
            hops: hops.to_vec(),
        }
    }

    #[test]
    fn survivors_keep_order_and_drop_the_dead() {
        let all = vec![c(1, &[0]), c(2, &[0]), c(3, &[0])];
        let kept = surviving_chains(&all, |ch| ch.root != 2);
        assert_eq!(kept, vec![c(1, &[0]), c(3, &[0])]);
    }

    #[test]
    fn stride_is_read_off_the_hop_that_differs() {
        let d1 = Chain::from_rkbx_line("0564B038 0 2B0 1A0").unwrap();
        let d2 = Chain::from_rkbx_line("0564B038 8 2B0 1A0").unwrap();
        assert_eq!(
            infer_stride(&d1, &d2),
            Some(Stride::Hop { index: 0, delta: 8 })
        );
        let d3 = chain_for_deck(&d1, Stride::Hop { index: 0, delta: 8 }, 2).unwrap();
        assert_eq!(d3.hops[0], 0x10);
        assert_eq!(
            chain_for_deck(&d1, Stride::Hop { index: 0, delta: 8 }, 0).unwrap(),
            d1
        );
    }

    #[test]
    fn stride_can_live_in_the_root() {
        let d1 = c(0x100, &[0x20]);
        let d2 = c(0x108, &[0x20]);
        assert_eq!(infer_stride(&d1, &d2), Some(Stride::Root(8)));
        assert_eq!(chain_for_deck(&d1, Stride::Root(8), 3).unwrap().root, 0x118);
        assert_eq!(
            infer_stride(&d1, &c(0x108, &[0x28])),
            None,
            "two differences"
        );
        assert_eq!(
            infer_stride(&d1, &c(0x100, &[0x20, 0])),
            None,
            "different depth"
        );
    }

    #[test]
    fn sibling_fields_move_the_last_hop() {
        let pos = Chain::from_rkbx_line("0564B038 0 2B0 130").unwrap();
        let bpm = sibling_field(&pos, 0x70).unwrap();
        assert_eq!(bpm, Chain::from_rkbx_line("0564B038 0 2B0 1A0").unwrap());
        assert_eq!(sibling_field(&pos, -0x3E0).unwrap().hops[1], 0);
        assert!(
            sibling_field(&pos, -0x3E1).is_none(),
            "cannot go before the struct"
        );
    }

    #[test]
    fn ranking_prefers_short_small_chains() {
        let mut v = vec![c(9, &[0x1000, 0x10]), c(5, &[0x20]), c(1, &[0x10])];
        rank_chains(&mut v);
        assert_eq!(v[0], c(1, &[0x10]));
        assert_eq!(v[1], c(5, &[0x20]));
        assert_eq!(v[2].hops.len(), 2);
    }
}
