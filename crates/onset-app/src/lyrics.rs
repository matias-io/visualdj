//! Looks up lyrics on a worker thread when the MASTER track changes, so the show never waits
//! on the network. Answers come back tagged with the track they belong to.
use std::path::Path;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use onset_core::lyrics::Lyrics;
use onset_core::track::{TrackId, TrackMeta};
use onset_lyrics::{LyricsStore, Outcome, Query};

pub struct Answer {
    pub track: TrackId,
    pub lyrics: Option<Arc<Lyrics>>,
    pub status: String,
}

pub struct LyricsWorker {
    requests: Sender<(TrackId, Query, bool)>,
    answers: Receiver<Answer>,
    asked: Option<TrackId>,
    /// Whether online lookups were allowed at the last request.
    online: bool,
}

impl LyricsWorker {
    pub fn spawn(cache_dir: &Path) -> Self {
        let (req_tx, req_rx) = crossbeam_channel::unbounded::<(TrackId, Query, bool)>();
        let (ans_tx, ans_rx) = crossbeam_channel::unbounded();
        let store = LyricsStore::new(cache_dir);
        let spawned = std::thread::Builder::new()
            .name("onset-lyrics".into())
            .spawn(move || {
                while let Ok(mut job) = req_rx.recv() {
                    // Only the newest request matters when the DJ skips through tracks.
                    while let Ok(newer) = req_rx.try_recv() {
                        job = newer;
                    }
                    let (track, query, online) = job;
                    let outcome = store.lookup(&query, online);
                    let status = outcome.describe();
                    tracing::info!(title = %query.title, %status, "lyrics");
                    let lyrics = match outcome {
                        Outcome::Synced(l) => Some(Arc::new(l)),
                        _ => None,
                    };
                    if ans_tx.send(Answer { track, lyrics, status }).is_err() {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "cannot start the lyrics worker");
        }
        Self {
            requests: req_tx,
            answers: ans_rx,
            asked: None,
            online: false,
        }
    }

    /// Asks for `track`'s lyrics once per track change. Returns true when a new request
    /// went out.
    pub fn follow(&mut self, track: Option<&TrackMeta>, online: bool) -> bool {
        let id = track.map(|t| t.id);
        if id == self.asked {
            return false;
        }
        self.asked = id;
        match track {
            Some(t) => self
                .requests
                .send((t.id, Query::from_meta(t), online))
                .is_ok(),
            None => false,
        }
    }

    /// Forgets the last request, so the current track is asked about again (after the
    /// online setting changes).
    pub fn retry(&mut self) {
        self.asked = None;
    }

    pub fn try_take(&self) -> Option<Answer> {
        self.answers.try_recv().ok()
    }

    /// Once a frame: asks about a new MASTER track, hands answers to the renderer, and asks
    /// again when online lookups are switched on.
    pub fn tend(
        &mut self,
        renderer: &mut onset_render::renderer::Renderer,
        track: Option<&TrackMeta>,
        online: bool,
    ) {
        if online != self.online {
            self.online = online;
            self.retry();
        }
        if self.follow(track, online)
            && let Some(t) = track
        {
            renderer.set_lyrics(t.id, None, "Looking for lyrics...".to_string());
        }
        while let Some(answer) = self.try_take() {
            renderer.set_lyrics(answer.track, answer.lyrics, answer.status);
        }
    }
}
