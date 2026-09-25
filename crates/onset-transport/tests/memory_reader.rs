//! The memory transport against fake memory: offsets files, chain reading, play detection
//! and snapshot mapping. Nothing here touches a real process.
use std::time::{Duration, Instant};

use onset_core::transport::TrackRef;
use onset_transport::memory::chain::Chain;
use std::path::{Path, PathBuf};

use onset_transport::memory::offsets::{
    Anchor, DeckChains, DeckSignature, FlagField, Offsets, PositionFormat,
};
use onset_transport::memory::reader::{
    ChainReader, DeckChooser, DeckFiles, FakeMem, PlayTracker, decks_from_hits,
    parse_track_info, signature_matches,
};

const BASE: u64 = 0x1000_0000;

fn chain(root: u64, hops: &[u64]) -> Chain {
    Chain {
        root,
        hops: hops.to_vec(),
    }
}

/// A tiny rekordbox: static roots in the module, a deck array on the heap, one deck struct,
/// a track-info text block and an analysis path.
fn fake_rekordbox() -> (FakeMem, Offsets) {
    let mut mem = FakeMem::new(BASE);
    // Static roots (module + 0x100..): pointers into the heap.
    mem.put_u64(BASE + 0x100, 0x5000); // deck array
    mem.put_u64(BASE + 0x108, 0x7000); // track info text
    mem.put_u64(BASE + 0x110, 0x8000); // master deck byte holder
    // Deck array: slot 0 -> deck struct A, slot 1 -> deck struct B.
    mem.put_u64(0x5000, 0x6000);
    mem.put_u64(0x5008, 0x6400);
    // Deck A: bpm f32 at +0x20, position i64 at +0x28.
    mem.put_f32(0x6020, 128.0);
    mem.put_i64(0x6028, 441_000);
    // Deck B.
    mem.put_f32(0x6420, 92.5);
    mem.put_i64(0x6428, 88_200);
    mem.put_bytes(
        0x7000,
        b"Track Title: Move\nArtist: Adam Port\nAlbum: Move\0\0\0",
    );
    mem.put_bytes(0x7200, b"/PIONEER/USBANLZ/abc/ANLZ0000.DAT\0");
    mem.put_u64(BASE + 0x118, 0x7200); // analysis path
    mem.put_u64(0x8000, 0x8100);
    mem.put_bytes(0x8100, &[1u8, 0, 0, 0]);

    let offsets = Offsets {
        rekordbox_version: "9.9.9".into(),
        platform: "windows".into(),
        position_format: PositionFormat::I64,
        position_rate_hz: 44_100.0,
        master_deck: Some(chain(0x110, &[0x0, 0x0])),
        signature: None,
        decks: vec![
            DeckChains {
                bpm: Some(chain(0x100, &[0x0, 0x20])),
                position: chain(0x100, &[0x0, 0x28]),
                track_info: Some(chain(0x108, &[0x0])),
                anlz_path: Some(chain(0x118, &[0x0])),
            },
            DeckChains {
                bpm: Some(chain(0x100, &[0x8, 0x20])),
                position: chain(0x100, &[0x8, 0x28]),
                track_info: None,
                anlz_path: None,
            },
        ],
        provenance: Some("test".into()),
    };
    (mem, offsets)
}

#[test]
fn offsets_round_trip_through_toml() {
    let (_, offsets) = fake_rekordbox();
    let text = offsets.to_toml().unwrap();
    assert!(text.contains("rekordbox_version = \"9.9.9\""), "{text}");
    let back = Offsets::from_toml(&text).unwrap();
    assert_eq!(back, offsets);
}

#[test]
fn offsets_parse_rkbx_link_text_for_one_version() {
    let text = "7.2.2\n05737C48 20 278 124\n\n0564B038 0 2B0 1A0\n0564B038 0 2B0 130\n05737C48 20 410 80 168 F0 0\n057696B8 8 3F0 0\n\n0564B038 8 2B0 1A0\n0564B038 8 2B0 130\n05737C48 20 410 88 168 F0 0\n057696B8 10 3F0 0\n\n\n7.1.4\n00000001 0 0\n";
    let o = Offsets::from_rkbx_text(text, "7.2.2").expect("7.2.2 block");
    assert_eq!(o.rekordbox_version, "7.2.2");
    assert_eq!(o.decks.len(), 2);
    assert_eq!(
        o.master_deck,
        Chain::from_rkbx_line("05737C48 20 278 124")
    );
    assert_eq!(
        o.decks[1].bpm,
        Some(Chain::from_rkbx_line("0564B038 8 2B0 1A0").unwrap())
    );
    assert!(o.decks[0].anlz_path.is_some());
    assert!(Offsets::from_rkbx_text(text, "7.2.18").is_none());
}

#[test]
fn offsets_file_is_found_by_version() {
    let dir = tempfile::tempdir().unwrap();
    let (_, offsets) = fake_rekordbox();
    offsets.save(&dir.path().join("9.9.9.toml")).unwrap();
    let found = Offsets::for_version(dir.path(), "9.9.9").expect("found");
    assert_eq!(found, offsets);
    assert!(Offsets::for_version(dir.path(), "9.9.8").is_none());
}

#[test]
fn chain_reader_reads_master_and_both_decks() {
    let (mem, offsets) = fake_rekordbox();
    let reader = ChainReader::new(mem, offsets);
    assert_eq!(reader.master_deck(), Some(1));
    let a = reader.deck(0).expect("deck A");
    assert!((a.bpm - 128.0).abs() < f32::EPSILON);
    assert!((a.position_s - 10.0).abs() < 1e-9, "{}", a.position_s);
    assert_eq!(
        a.track_info.as_deref(),
        Some("Track Title: Move\nArtist: Adam Port\nAlbum: Move")
    );
    assert_eq!(
        a.anlz_path.as_deref(),
        Some("/PIONEER/USBANLZ/abc/ANLZ0000.DAT")
    );
    let b = reader.deck(1).expect("deck B");
    assert!((b.bpm - 92.5).abs() < f32::EPSILON);
    assert!((b.position_s - 2.0).abs() < 1e-9);
    assert!(b.track_info.is_none());
    assert!(reader.deck(2).is_none(), "no third deck configured");
}

#[test]
fn chain_reader_fails_softly_when_a_hop_is_missing() {
    let (mut mem, offsets) = fake_rekordbox();
    mem.put_u64(BASE + 0x100, 0); // deck array pointer nulled: rekordbox mid-load
    let reader = ChainReader::new(mem, offsets);
    assert!(reader.deck(0).is_none());
    assert_eq!(reader.master_deck(), Some(1), "unrelated chains still work");
}

#[test]
fn track_info_text_splits_into_title_artist_album() {
    let t = parse_track_info("Track Title: Move\nArtist: Adam Port\nAlbum: Move");
    assert_eq!(
        t,
        TrackRef::TitleArtist {
            title: "Move".into(),
            artist: "Adam Port".into(),
            album: "Move".into()
        }
    );
    let partial = parse_track_info("Track Title: Solo\nArtist: X");
    assert_eq!(
        partial,
        TrackRef::TitleArtist {
            title: "Solo".into(),
            artist: "X".into(),
            album: String::new()
        }
    );
    assert_eq!(parse_track_info("garbage"), TrackRef::Unknown);
}

#[test]
fn play_tracker_needs_movement_to_call_it_playing() {
    let t0 = Instant::now();
    let mut tracker = PlayTracker::default();
    assert!(
        !tracker.update(10.0, t0),
        "first sample: unknown, treated as stopped"
    );
    assert!(
        tracker.update(10.05, t0 + Duration::from_millis(50)),
        "moved: playing"
    );
    assert!(
        tracker.update(10.05, t0 + Duration::from_millis(100)),
        "a single equal read within the grace period is still playing"
    );
    assert!(
        !tracker.update(10.05, t0 + Duration::from_millis(600)),
        "no movement for half a second: paused"
    );
    assert!(
        tracker.update(3.0, t0 + Duration::from_millis(650)),
        "a jump backwards (cue, loop) counts as movement"
    );
}

#[test]
fn play_tracker_measures_the_playback_rate() {
    let t0 = Instant::now();
    let mut tracker = PlayTracker::default();
    // Position advances at 1.05 s per second for two seconds, sampled every 100 ms.
    for i in 0..=20 {
        let t = t0 + Duration::from_millis(100 * i);
        tracker.update(10.0 + 1.05 * 0.1 * i as f64, t);
    }
    let rate = tracker
        .rate()
        .expect("rate after half a second of movement");
    assert!((rate - 1.05).abs() < 0.01, "rate {rate}");
    // A seek backwards is a jump, not a rate change.
    tracker.update(3.0, t0 + Duration::from_millis(2100));
    tracker.update(3.105, t0 + Duration::from_millis(2200));
    let after = tracker.rate().unwrap();
    assert!((after - 1.05).abs() < 0.05, "rate after a seek {after}");
}

#[test]
fn offsets_work_without_a_master_chain() {
    let (mem, mut offsets) = fake_rekordbox();
    offsets.master_deck = None;
    let text = offsets.to_toml().unwrap();
    let back = Offsets::from_toml(&text).unwrap();
    assert_eq!(back.master_deck, None);
    let reader = ChainReader::new(mem, offsets);
    assert_eq!(reader.master_deck(), None);
    assert!(reader.deck(0).is_some());
}

#[test]
fn deck_chooser_follows_the_master_while_it_plays() {
    let mut chooser = DeckChooser::default();
    assert_eq!(chooser.choose(Some(1), &[Some(true), Some(true)]), Some(1));
    // The master sits idle while the other deck plays: the playing deck is the show.
    assert_eq!(chooser.choose(Some(1), &[Some(true), Some(false)]), Some(0));
    // Nothing plays: back to the master.
    assert_eq!(chooser.choose(Some(1), &[Some(false), Some(false)]), Some(1));
}

#[test]
fn deck_chooser_sticks_to_the_playing_deck_through_a_transition() {
    let mut chooser = DeckChooser::default();
    assert_eq!(chooser.choose(None, &[Some(true), Some(false)]), Some(0));
    assert_eq!(
        chooser.choose(None, &[Some(true), Some(true)]),
        Some(0),
        "both play: no switch until the first deck stops"
    );
    assert_eq!(chooser.choose(None, &[Some(false), Some(true)]), Some(1));
    assert_eq!(
        chooser.choose(None, &[Some(false), Some(false)]),
        Some(1),
        "nothing plays: keep showing the last deck"
    );
}

#[test]
fn deck_chooser_falls_back_to_a_loaded_deck() {
    let mut chooser = DeckChooser::default();
    assert_eq!(chooser.choose(None, &[None, Some(false)]), Some(1));
    assert_eq!(chooser.choose(None, &[None, None]), None);
}

/// Two deck objects laid out the way rekordbox 7.2 does: module pointers at fixed
/// distances below the position field, the objects anywhere in memory.
fn signature_rekordbox() -> (FakeMem, DeckSignature) {
    let mut mem = FakeMem::new(BASE);
    let sig = DeckSignature {
        anchors: vec![
            Anchor { offset: -0x20, module_offset: 0x1046 },
            Anchor { offset: -0x50, module_offset: 0xeee3 },
        ],
        max_decks: 4,
        master_flag: Some(FlagField {
            offset: -0x328,
            mask: 0x80,
        }),
    };
    for (pos, samples) in [(0x9000u64, 44_100i32), (0x7000, 88_200)] {
        mem.put_u64(pos - 0x20, BASE + 0x1046);
        mem.put_u64(pos - 0x50, BASE + 0xeee3);
        mem.put_bytes(pos, &samples.to_le_bytes());
    }
    // Deck at 0x9000 (the second by address) is MASTER: bit 7 set, other bits noise.
    mem.put_bytes(0x9000 - 0x328, &[0xc1]);
    mem.put_bytes(0x7000 - 0x328, &[0x41]);
    // A stray copy of the first anchor with nothing else around it: not a deck.
    mem.put_u64(0x5000 - 0x20, BASE + 0x1046);
    (mem, sig)
}

#[test]
fn signature_confirms_every_anchor() {
    let (mem, sig) = signature_rekordbox();
    assert!(signature_matches(&mem, &sig, 0x9000));
    assert!(!signature_matches(&mem, &sig, 0x5000), "one anchor is not enough");
    assert!(!signature_matches(&mem, &sig, 0x9008));
}

#[test]
fn decks_come_from_hits_in_address_order() {
    let (mem, sig) = signature_rekordbox();
    // Hits are where the first anchor's bytes were seen, in scan order.
    let hits = [0x9000 - 0x20, 0x5000 - 0x20, 0x7000 - 0x20, 0x9000 - 0x20];
    assert_eq!(decks_from_hits(&mem, &sig, &hits), vec![0x7000, 0x9000]);
}

#[test]
fn reader_reads_signature_decks_without_chains() {
    let (mem, sig) = signature_rekordbox();
    let offsets = Offsets {
        rekordbox_version: "7.2.18".into(),
        platform: "windows".into(),
        position_format: PositionFormat::I32,
        position_rate_hz: 44_100.0,
        master_deck: None,
        decks: Vec::new(),
        signature: Some(sig),
        provenance: None,
    };
    let text = offsets.to_toml().unwrap();
    assert!(text.contains("[signature]"), "{text}");
    assert_eq!(Offsets::from_toml(&text).unwrap(), offsets);
    let mut reader = ChainReader::new(mem, offsets);
    assert_eq!(reader.deck_count(), 0);
    assert_eq!(reader.adopt_hits(&[0x9000 - 0x20, 0x7000 - 0x20]), 2);
    assert_eq!(reader.deck_count(), 2);
    let a = reader.deck(0).expect("deck 1");
    assert!((a.position_s - 2.0).abs() < 1e-9, "{}", a.position_s);
    let b = reader.deck(1).expect("deck 2");
    assert!((b.position_s - 1.0).abs() < 1e-9);
    assert!(!reader.found_decks_stale());
    assert_eq!(reader.master_deck(), Some(1), "the flag byte names the master");
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn deck_files_pairs_a_new_file_with_the_free_deck() {
    let mut files = DeckFiles::default();
    let none = |_: &Path| None;
    let a = files.update(&[p("a.flac")], &[Some(10.0), Some(0.0)], &none).to_vec();
    assert_eq!(a, vec![Some(p("a.flac")), None]);
    // A second file while deck 1 keeps its track: only deck 2 is free.
    let b = files
        .update(&[p("a.flac"), p("b.flac")], &[Some(12.0), Some(0.0)], &none)
        .to_vec();
    assert_eq!(b, vec![Some(p("a.flac")), Some(p("b.flac"))]);
    // Deck 1's file closes (a new track is being loaded there) and c.flac appears.
    let c = files
        .update(&[p("b.flac"), p("c.flac")], &[Some(0.0), Some(40.0)], &none)
        .to_vec();
    assert_eq!(c, vec![Some(p("c.flac")), Some(p("b.flac"))]);
}

#[test]
fn deck_files_uses_lengths_when_both_decks_are_free() {
    let mut files = DeckFiles::default();
    let duration = |path: &Path| match path.to_str() {
        Some("short.flac") => Some(60.0),
        Some("long.flac") => Some(300.0),
        _ => None,
    };
    // Startup with two tracks loaded: deck 1 sits at 150 s, which only the long file allows.
    let got = files
        .update(
            &[p("short.flac"), p("long.flac")],
            &[Some(150.0), Some(20.0)],
            &duration,
        )
        .to_vec();
    assert_eq!(got, vec![Some(p("long.flac")), Some(p("short.flac"))]);
}

#[test]
fn deck_files_waits_when_it_cannot_tell() {
    let mut files = DeckFiles::default();
    let none = |_: &Path| None;
    let got = files
        .update(&[p("a.flac"), p("b.flac")], &[Some(5.0), Some(5.0)], &none)
        .to_vec();
    assert_eq!(got, vec![None, None], "no guessing");
}
