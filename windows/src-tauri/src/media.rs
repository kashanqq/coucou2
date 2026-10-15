// Now playing — what Windows' own media flyout knows: Spotify, a YouTube tab
// in Edge or Chrome, any player that reports to the system media controls.
//
// Push only: Windows calls back when a session appears or goes, a track changes
// or playback pauses, and nothing runs in between (no timeline events, which
// tick every second). Read-only — Coucou never plays, pauses or skips.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

use crate::log;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    /// The player's app id ("Spotify.exe", "MSEdge", …), for the colour of the dot.
    pub app: String,
}

static APP: Mutex<Option<AppHandle>> = Mutex::new(None);
static MANAGER: Mutex<Option<Manager>> = Mutex::new(None);
/// Every session listened to, with its two handler tokens (to remove them).
static WATCHED: Mutex<Vec<(Session, i64, i64)>> = Mutex::new(Vec::new());
/// What was last sent; also serialises refreshes coming from several callbacks.
static CURRENT: Mutex<Option<NowPlaying>> = Mutex::new(None);

pub fn now_playing() -> Option<NowPlaying> {
    CURRENT.lock().unwrap().clone()
}

pub fn start(app: AppHandle) {
    *APP.lock().unwrap() = Some(app);
    // RequestAsync().get() blocks; keep it off the main thread.
    std::thread::spawn(|| {
        let manager = match Manager::RequestAsync().and_then(|op| op.get()) {
            Ok(manager) => manager,
            Err(e) => {
                log::line(format!("media: system media controls unavailable: {e}"));
                return;
            }
        };
        let _ = manager.SessionsChanged(&TypedEventHandler::new(|_, _| {
            watch_sessions();
            Ok(())
        }));
        let _ = manager.CurrentSessionChanged(&TypedEventHandler::new(|_, _| {
            refresh();
            Ok(())
        }));
        *MANAGER.lock().unwrap() = Some(manager);
        watch_sessions();
    });
}

fn manager() -> Option<Manager> {
    MANAGER.lock().unwrap().clone()
}

/// Listens to every session there is now, dropping the ones that are gone.
fn watch_sessions() {
    let Some(manager) = manager() else { return };
    {
        let mut watched = WATCHED.lock().unwrap();
        for (session, a, b) in watched.drain(..) {
            let _ = session.RemovePlaybackInfoChanged(a);
            let _ = session.RemoveMediaPropertiesChanged(b);
        }
        for session in sessions(&manager) {
            let playback = session.PlaybackInfoChanged(&TypedEventHandler::new(|_, _| {
                refresh();
                Ok(())
            }));
            let props = session.MediaPropertiesChanged(&TypedEventHandler::new(|_, _| {
                refresh();
                Ok(())
            }));
            if let (Ok(a), Ok(b)) = (playback, props) {
                watched.push((session, a, b));
            }
        }
    }
    refresh();
}

fn sessions(manager: &Manager) -> Vec<Session> {
    let Ok(list) = manager.GetSessions() else { return vec![] };
    let n = list.Size().unwrap_or(0);
    (0..n).filter_map(|i| list.GetAt(i).ok()).collect()
}

fn playing(session: &Session) -> bool {
    session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
        .is_ok_and(|status| status == Status::Playing)
}

/// The session that is playing — Windows' current one first — as a track.
fn pick(manager: &Manager) -> Option<NowPlaying> {
    let current = manager.GetCurrentSession().ok();
    let mut candidates: Vec<Session> = current.into_iter().collect();
    candidates.extend(sessions(manager));
    let session = candidates.into_iter().find(playing)?;
    let props = session.TryGetMediaPropertiesAsync().and_then(|op| op.get()).ok()?;
    let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
    let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
    let app = session.SourceAppUserModelId().map(|s| s.to_string()).unwrap_or_default();
    track(&title, &artist, &app)
}

/// Trimmed, and None without a title: a player that's just loading says nothing.
fn track(title: &str, artist: &str, app: &str) -> Option<NowPlaying> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    Some(NowPlaying { title: title.to_string(), artist: artist.trim().to_string(), app: app.to_string() })
}

fn refresh() {
    let Some(manager) = manager() else { return };
    let mut current = CURRENT.lock().unwrap();
    let next = pick(&manager);
    if *current == next {
        return;
    }
    *current = next.clone();
    drop(current);
    if let Some(app) = APP.lock().unwrap().as_ref() {
        let _ = app.emit("media", next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_needs_a_title() {
        assert_eq!(track("  ", "Artist", "Spotify.exe"), None);
        let t = track(" Song ", " Band ", "Spotify.exe").unwrap();
        assert_eq!((t.title.as_str(), t.artist.as_str()), ("Song", "Band"));
    }
}
