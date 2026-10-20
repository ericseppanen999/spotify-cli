//! An app-side "up next" list layered on Spotify's own queue, which the Web
//! API can only append to -- no removing or reordering. Songs wait here,
//! where they can be freely edited, and each is handed to Spotify's queue
//! only shortly before it's due: playback stays gapless, and the album or
//! playlist that was playing picks back up once this list runs out. It
//! lives in the background daemon (see daemon.rs), so it keeps going after
//! the TUI exits.
use crate::api::{Api, Playback, Track};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// How close to the end of the playing song the next one is handed off.
/// Comfortably more than the TUI's 5s poll interval, so a poll always lands
/// inside the window.
const HANDOFF_MS: u64 = 15_000;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Queue {
    /// Waiting here; free to reorder or remove.
    pub pending: Vec<Track>,
    /// Already in Spotify's queue to play next, so it can't be taken back.
    pub handed: Option<Track>,
    /// Spotify's own view of what plays next, for display. Refetched rather
    /// than saved or sent between processes.
    #[serde(skip)]
    pub upcoming: Vec<Track>,
}
impl Queue {
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.handed.is_none()
    }
    pub fn add(&mut self, track: Track) {
        self.pending.push(track);
    }
    pub fn remove(&mut self, index: usize) {
        if index < self.pending.len() {
            self.pending.remove(index);
        }
    }
    /// Swaps the song at `index` with its neighbor `delta` away.
    pub fn shift(&mut self, index: usize, delta: isize) {
        let Some(target) = index.checked_add_signed(delta) else {
            return;
        };
        if index < self.pending.len() && target < self.pending.len() {
            self.pending.swap(index, target);
        }
    }
    async fn hand_off(&mut self, api: &mut Api) -> Result<bool> {
        if self.handed.is_some() || self.pending.is_empty() {
            return Ok(false);
        }
        api.queue(&self.pending[0].uri).await?;
        self.handed = Some(self.pending.remove(0));
        Ok(true)
    }
    /// Skips ahead, to the head of this list when there is one -- Spotify
    /// plays its own queue before resuming the album/playlist.
    pub async fn next(&mut self, api: &mut Api) -> Result<()> {
        self.hand_off(api).await?;
        api.next().await
    }
    pub async fn play_now(&mut self, api: &mut Api, index: usize) -> Result<()> {
        if index >= self.pending.len() {
            return Ok(());
        }
        let track = self.pending.remove(index);
        self.pending.insert(0, track);
        anyhow::ensure!(
            self.handed.is_none(),
            "The next song is already in Spotify's queue; this one is now right after it"
        );
        self.next(api).await
    }
    /// Run on every playback poll. Returns whether anything shown changed.
    pub async fn sync(
        &mut self,
        api: &mut Api,
        playback: &Playback,
        track_changed: bool,
    ) -> Result<bool> {
        let mut changed = false;
        if track_changed {
            let playing = playback.item.as_ref().map(|t| t.uri.as_str());
            if self.handed.as_ref().map(|t| t.uri.as_str()) == playing {
                self.handed = None;
            }
            if let Ok(upcoming) = api.upcoming().await {
                // Dropped from Spotify's queue elsewhere (another app, say):
                // stop waiting on it, or nothing else would get handed off.
                if let Some(handed) = &self.handed {
                    if !upcoming.iter().any(|t| t.uri == handed.uri) {
                        self.handed = None;
                    }
                }
                self.upcoming = upcoming;
            }
            changed = true;
        }
        if let (true, Some(track), Some(progress)) =
            (playback.is_playing, &playback.item, playback.progress_ms)
        {
            if track.duration_ms.saturating_sub(progress) <= HANDOFF_MS
                && self.hand_off(api).await?
            {
                if let Ok(upcoming) = api.upcoming().await {
                    self.upcoming = upcoming;
                }
                changed = true;
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(uri: &str) -> Track {
        Track {
            uri: uri.into(),
            ..Default::default()
        }
    }
    fn uris(queue: &Queue) -> Vec<&str> {
        queue.pending.iter().map(|t| t.uri.as_str()).collect()
    }

    #[test]
    fn edits_stay_in_bounds() {
        let mut queue = Queue::default();
        for uri in ["a", "b", "c"] {
            queue.add(track(uri));
        }
        queue.shift(0, 1);
        assert_eq!(uris(&queue), ["b", "a", "c"]);
        queue.shift(0, -1);
        queue.shift(2, 1);
        assert_eq!(uris(&queue), ["b", "a", "c"]);
        queue.remove(1);
        queue.remove(9);
        assert_eq!(uris(&queue), ["b", "c"]);
    }
}
