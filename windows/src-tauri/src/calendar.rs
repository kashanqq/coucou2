// Google Calendar — OAuth sign-in and the read-only sync.
//
// The sign-in is the "installed app" flow Google documents for desktop apps: the
// browser opens Google's consent page, Google redirects to a one-shot listener
// on 127.0.0.1, and the code is exchanged with PKCE. The user brings their own
// OAuth client (Google Cloud → Credentials → Desktop app), so no request ever
// goes through a server of ours — there is none.
//
// The client ID, client secret and refresh token live in the Credential Manager;
// the short-lived access token only in memory. The scope is read-only.
//
// Google's push notifications (events.watch) need a public HTTPS server to call,
// which a desktop app doesn't have, so the sync asks instead, but cheaply: a
// sync token per calendar lets a check ask "anything changed?" and get an empty
// answer back. The full week is downloaded only when something did change, when
// the window has moved on, or on the refresh button. The last week downloaded
// is kept in calendar.json, so the island still has it offline and at launch.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use windows::Win32::Security::Cryptography::{
    BCryptGenRandom, BCryptHash, BCRYPT_SHA256_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};

use crate::log;
use crate::secrets;

pub const CLIENT_ID_KEY: &str = "gcal-client-id";
pub const CLIENT_SECRET_KEY: &str = "gcal-client-secret";
/// Written only by the sign-in below — the front end can never set or read it.
pub const REFRESH_KEY: &str = "gcal-refresh-token";

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
const API: &str = "https://www.googleapis.com/calendar/v3";
const SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";

/// How often to ask whether anything changed (an empty answer when nothing did).
const CHECK_EVERY_MS: u64 = 5 * 60 * 1000;
/// How often the timer looks at the clock. Short, so a wake or a returning
/// network is noticed within a minute; each look costs nothing.
const TICK: Duration = Duration::from_secs(60);
const OFFLINE: &str = "Offline — showing the last sync.";
/// The window (a day and a half back, eight days ahead) slides with time, so
/// the week is downloaded again at least this often even if nothing changed.
const FULL_EVERY_MS: u64 = 6 * 3600 * 1000;
/// How long the browser has to come back with the code.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// Calendars beyond this are skipped: one request each per check.
const MAX_CALENDARS: usize = 12;
/// Pages of 250 events per calendar on a full download.
const MAX_PAGES: usize = 4;

/// Access token and the moment it stops being usable.
static ACCESS: Mutex<Option<(String, Instant)>> = Mutex::new(None);
static SIGNING_IN: AtomicBool = AtomicBool::new(false);
/// What the last full download produced; mirrored in calendar.json.
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
/// The last update sent, for a window that asks after it was emitted.
static LAST: Mutex<Option<CalendarUpdate>> = Mutex::new(None);
/// One sync at a time: the timer and the refresh button can't overlap.
static SYNC_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// (wall-clock ms, error) of the last failed sync; cleared by a good one.
static FAILED: Mutex<Option<(u64, String)>> = Mutex::new(None);

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Cache {
    events: Vec<Value>,
    /// Unix ms of the last full download.
    fetched_at: u64,
    /// Unix ms of the last time Google confirmed it's current (download or check).
    #[serde(default)]
    checked_at: u64,
    /// Sync token of the calendar list, when Google gave one.
    list_token: Option<String>,
    /// (calendar id, its events sync token) for every calendar downloaded.
    tokens: Vec<(String, Option<String>)>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CalendarUpdate {
    pub connected: bool,
    /// None when the sync failed: the island keeps showing what it had.
    pub events: Option<Vec<Value>>,
    pub error: Option<String>,
    /// Unix ms of the last successful sync.
    pub synced_at: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarStatus {
    pub connected: bool,
    pub client_ready: bool,
    pub signing_in: bool,
}

pub fn status() -> CalendarStatus {
    CalendarStatus {
        connected: secrets::present(REFRESH_KEY),
        client_ready: secrets::present(CLIENT_ID_KEY),
        signing_in: SIGNING_IN.load(Ordering::Relaxed),
    }
}

fn emit(app: &AppHandle, update: CalendarUpdate) {
    *LAST.lock().unwrap() = Some(update.clone());
    // Both windows: the island shows the events, the settings window the status.
    let _ = app.emit("calendar", update);
}

/// What the island should show right now: the cached week before any network.
pub fn snapshot() -> CalendarUpdate {
    if let Some(last) = LAST.lock().unwrap().clone() {
        return last;
    }
    CalendarUpdate { events: Some(vec![]), ..cached_update(None) }
}

/// The cached week as an update. Without a cache, `events` is None, which
/// keeps whatever the island already shows.
fn cached_update(error: Option<String>) -> CalendarUpdate {
    let cache = CACHE.lock().unwrap().clone();
    CalendarUpdate {
        connected: secrets::present(REFRESH_KEY),
        events: cache.as_ref().map(|c| c.events.clone()),
        error,
        synced_at: cache.map(|c| c.checked_at.max(c.fetched_at)),
    }
}

fn cache_path() -> PathBuf {
    crate::settings::local_dir().join("calendar.json")
}

fn load_cache() -> Option<Cache> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Written next to itself and renamed, so a crash never leaves half a file.
fn save_cache(cache: &Cache) {
    let path = cache_path();
    let tmp = path.with_extension("json.tmp");
    let Ok(json) = serde_json::to_vec(cache) else { return };
    let _ = std::fs::create_dir_all(crate::settings::local_dir());
    if std::fs::write(&tmp, json).is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn forget_cache() {
    *CACHE.lock().unwrap() = None;
    let _ = std::fs::remove_file(cache_path());
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Shows the cached week at once, then checks for changes every five minutes
/// while connected. Paused means no network at all.
///
/// The five minutes are counted on the wall clock from the last check Google
/// answered, which calendar.json keeps: sleep, a shutdown or a day offline all
/// count, so the first tick after any of them checks straight away. A tick is
/// one comparison in memory; offline, each tick tries again (a failed DNS
/// lookup), so the week comes back within a minute of the network.
pub fn start(app: AppHandle) {
    if !secrets::present(REFRESH_KEY) {
        forget_cache();
    } else if let Some(cache) = load_cache() {
        *CACHE.lock().unwrap() = Some(cache);
        emit(&app, cached_update(None));
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(4)).await;
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if crate::integrations::PAUSED.load(Ordering::Relaxed) {
                continue;
            }
            let checked = CACHE.lock().unwrap().as_ref().map(|c| c.checked_at.max(c.fetched_at));
            let failed = FAILED.lock().unwrap().as_ref().map(|(at, error)| (*at, error == OFFLINE));
            if !is_due(now_ms(), checked, failed) || !secrets::present(REFRESH_KEY) {
                continue;
            }
            sync_now(&app, false).await;
        }
    });
}

/// Whether the timer should check now. `checked`: wall-clock ms of the last
/// check Google answered. `failed`: the last failed try, and whether it failed
/// for want of a network. A clock set back counts as due.
fn is_due(now: u64, checked: Option<u64>, failed: Option<(u64, bool)>) -> bool {
    let within = |at: u64| at <= now && now - at < CHECK_EVERY_MS;
    if checked.is_some_and(within) {
        return false;
    }
    // Google said no (not the network): wait the full interval before asking again.
    !matches!(failed, Some((at, false)) if within(at))
}

// ── Sign-in ──────────────────────────────────────────────────────────────────

/// Opens Google's consent page and waits for the browser to come back.
pub async fn connect(app: AppHandle) -> Result<(), String> {
    let client_id = secrets::get(CLIENT_ID_KEY)
        .ok_or("Save your OAuth client ID first.")?;
    let client_secret = secrets::get(CLIENT_SECRET_KEY).unwrap_or_default();
    if SIGNING_IN.swap(true, Ordering::SeqCst) {
        return Err("A sign-in is already waiting in your browser.".into());
    }
    let result = sign_in(&client_id, &client_secret).await;
    SIGNING_IN.store(false, Ordering::SeqCst);

    let (refresh, access, expires_in) = result?;
    secrets::set(REFRESH_KEY, &refresh)?;
    *ACCESS.lock().unwrap() = Some((access, Instant::now() + expires_in));
    log::line("google calendar connected".to_string());
    forget_cache();
    sync(&app).await;
    Ok(())
}

async fn sign_in(client_id: &str, client_secret: &str) -> Result<(String, String, Duration), String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Could not open the sign-in listener: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}");

    let verifier = random_token(48)?;
    let challenge = b64url(&sha256(verifier.as_bytes())?);
    let state = random_token(16)?;

    let url = reqwest::Url::parse_with_params(
        AUTH_URL,
        &[
            ("client_id", client_id),
            ("redirect_uri", redirect.as_str()),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("access_type", "offline"),
            // Without it Google only hands out a refresh token the very first time.
            ("prompt", "consent"),
        ],
    )
    .map_err(|e| e.to_string())?;
    crate::open_url(url.to_string());

    let code = tokio::time::timeout(SIGN_IN_TIMEOUT, wait_for_code(&listener, &state))
        .await
        .map_err(|_| "Sign-in timed out — try again.".to_string())??;

    let response = client()
        .post(TOKEN_URL)
        .form(&[
            ("code", code.as_str()),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("redirect_uri", redirect.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("No connection: {e}"))?;
    let ok = response.status().is_success();
    let json: Value = response.json().await.unwrap_or(json!({}));
    if !ok {
        return Err(google_error(&json, "Google refused the sign-in"));
    }
    let refresh = json
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or("Google sent no refresh token — remove Coucou from your Google account's third-party access and try again.")?;
    let access = json.get("access_token").and_then(Value::as_str).unwrap_or_default();
    let expires = json.get("expires_in").and_then(Value::as_u64).unwrap_or(3600);
    Ok((refresh.to_string(), access.to_string(), Duration::from_secs(expires.saturating_sub(60))))
}

/// Serves the one redirect Google makes and returns the `code` it carries.
/// Anything else hitting the port (a favicon request) gets a 404 and is ignored.
async fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut socket, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        let mut len = 0;
        while len < buf.len() {
            let n = socket.read(&mut buf[len..]).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            len += n;
            if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf[..len]);
        let target = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        let Ok(url) = reqwest::Url::parse(&format!("http://127.0.0.1{target}")) else {
            respond(&mut socket, 400, "Bad request").await;
            continue;
        };
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        if let Some(error) = param("error") {
            respond(&mut socket, 200, "Google Calendar was not connected. You can close this tab.").await;
            return Err(if error == "access_denied" {
                "Access was declined.".into()
            } else {
                format!("Google said: {error}")
            });
        }
        let Some(code) = param("code") else {
            respond(&mut socket, 404, "Not found").await;
            continue;
        };
        if param("state").as_deref() != Some(state) {
            respond(&mut socket, 400, "This sign-in link is stale. Start again from Coucou.").await;
            return Err("The sign-in answer did not match — try again.".into());
        }
        respond(&mut socket, 200, "Coucou is connected to Google Calendar. You can close this tab.").await;
        return Ok(code);
    }
}

async fn respond(socket: &mut tokio::net::TcpStream, code: u16, message: &str) {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Coucou</title>\
         <body style=\"font:15px system-ui;background:#0b0c0e;color:#f5f6f8;display:grid;place-items:center;height:90vh\">\
         <p>{message}</p></body>"
    );
    let reply = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(reply.as_bytes()).await;
    let _ = socket.shutdown().await;
}

/// Forgets the account here and tells Google to revoke the grant.
pub async fn disconnect(app: &AppHandle) {
    if let Some(token) = secrets::get(REFRESH_KEY) {
        let _ = client().post(REVOKE_URL).form(&[("token", token.as_str())]).send().await;
    }
    let _ = secrets::clear(REFRESH_KEY);
    *ACCESS.lock().unwrap() = None;
    forget_cache();
    log::line("google calendar disconnected".to_string());
    emit(app, CalendarUpdate { connected: false, events: Some(vec![]), error: None, synced_at: None });
}

async fn access_token(http: &reqwest::Client) -> Result<String, String> {
    if let Some((token, until)) = ACCESS.lock().unwrap().clone() {
        if Instant::now() < until {
            return Ok(token);
        }
    }
    let refresh = secrets::get(REFRESH_KEY).ok_or("Not connected.")?;
    let client_id = secrets::get(CLIENT_ID_KEY).ok_or("The OAuth client ID is missing.")?;
    let client_secret = secrets::get(CLIENT_SECRET_KEY).unwrap_or_default();
    let response = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("refresh_token", refresh.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .map_err(net_error)?;
    let status = response.status();
    let json: Value = response.json().await.unwrap_or(json!({}));
    if !status.is_success() {
        if json.get("error").and_then(Value::as_str) == Some("invalid_grant") {
            // Revoked from the Google account, or expired (unverified apps get
            // seven days): the token is dead, so stop pretending to be connected.
            let _ = secrets::clear(REFRESH_KEY);
            return Err("Google access expired or was revoked — connect again in Settings.".into());
        }
        return Err(google_error(&json, "Could not refresh the Google token"));
    }
    let token = json
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or("Google sent no access token.")?
        .to_string();
    let expires = json.get("expires_in").and_then(Value::as_u64).unwrap_or(3600);
    *ACCESS.lock().unwrap() =
        Some((token.clone(), Instant::now() + Duration::from_secs(expires.saturating_sub(60))));
    Ok(token)
}

fn google_error(json: &Value, fallback: &str) -> String {
    let detail = json
        .get("error_description")
        .and_then(Value::as_str)
        .or_else(|| json.get("error").and_then(|e| e.get("message")).and_then(Value::as_str))
        .or_else(|| json.get("error").and_then(Value::as_str));
    match detail {
        Some(d) => format!("{fallback}: {d}"),
        None => fallback.to_string(),
    }
}

// ── Sync ─────────────────────────────────────────────────────────────────────

/// The refresh button and a new connection: always the full week.
pub async fn sync(app: &AppHandle) {
    sync_now(app, true).await;
}

/// The timer's check: the full week only if something changed or it's stale.
async fn sync_now(app: &AppHandle, force: bool) {
    let _one_at_a_time = SYNC_LOCK.lock().await;
    if !secrets::present(REFRESH_KEY) {
        forget_cache();
        emit(app, CalendarUpdate { connected: false, events: Some(vec![]), error: None, synced_at: None });
        return;
    }
    let http = client();
    let result = async {
        let token = access_token(&http).await?;
        let cached = CACHE.lock().unwrap().clone();
        if let Some(cache) = cached.filter(|c| !force && now_ms().saturating_sub(c.fetched_at) < FULL_EVERY_MS) {
            if let Some(cache) = unchanged(&http, &token, cache).await? {
                return Ok(cache);
            }
        }
        fetch_all(&http, &token).await
    }
    .await;
    match result {
        Ok(mut cache) => {
            *FAILED.lock().unwrap() = None;
            cache.checked_at = now_ms();
            save_cache(&cache);
            *CACHE.lock().unwrap() = Some(cache);
            emit(app, cached_update(None));
        }
        Err(error) => {
            // Offline, this repeats every minute: log and re-send it only once.
            let repeat = {
                let mut failed = FAILED.lock().unwrap();
                let repeat = failed.as_ref().is_some_and(|(_, last)| *last == error);
                *failed = Some((now_ms(), error.clone()));
                repeat
            };
            if repeat {
                return;
            }
            log::line(format!("google calendar: {error}"));
            if !secrets::present(REFRESH_KEY) {
                forget_cache();
                emit(app, CalendarUpdate { connected: false, events: Some(vec![]), error: Some(error), synced_at: None });
                return;
            }
            // Keep showing the last week downloaded, with the reason it isn't newer.
            emit(app, cached_update(Some(error)));
        }
    }
}

/// Asks the calendar list and every calendar what changed since the last
/// download. Some(cache with fresh tokens) when nothing did; None when the week
/// must be downloaded again.
async fn unchanged(http: &reqwest::Client, token: &str, mut cache: Cache) -> Result<Option<Cache>, String> {
    let Some(list_token) = cache.list_token.clone() else { return Ok(None) };
    let mut url = reqwest::Url::parse(&format!("{API}/users/me/calendarList")).map_err(|e| e.to_string())?;
    url.query_pairs_mut().append_pair("syncToken", &list_token).append_pair("maxResults", "1");
    match changes(http, token, url).await? {
        Some(next) => cache.list_token = Some(next),
        None => return Ok(None),
    }
    for (id, sync_token) in cache.tokens.iter_mut() {
        let Some(current) = sync_token.clone() else { return Ok(None) };
        let mut url = events_url(id)?;
        url.query_pairs_mut()
            .append_pair("syncToken", &current)
            .append_pair("singleEvents", "true")
            .append_pair("maxResults", "1");
        match changes(http, token, url).await? {
            Some(next) => *sync_token = Some(next),
            None => return Ok(None),
        }
    }
    Ok(Some(cache))
}

/// One incremental request: Some(next sync token) when nothing changed, None
/// when something did or the token has expired (410 Gone).
async fn changes(http: &reqwest::Client, token: &str, url: reqwest::Url) -> Result<Option<String>, String> {
    let response = http.get(url).bearer_auth(token).send().await.map_err(net_error)?;
    let status = response.status();
    if status.as_u16() == 410 {
        return Ok(None);
    }
    let json: Value = response.json().await.unwrap_or(json!({}));
    if !status.is_success() {
        if status.as_u16() == 401 {
            *ACCESS.lock().unwrap() = None;
        }
        return Err(google_error(&json, &format!("Google Calendar error {}", status.as_u16())));
    }
    Ok(next_if_unchanged(&json))
}

/// The next sync token from an incremental answer that reports no change.
fn next_if_unchanged(json: &Value) -> Option<String> {
    let empty = json.get("items").and_then(Value::as_array).is_none_or(|items| items.is_empty());
    if !empty || json.get("nextPageToken").is_some() {
        return None;
    }
    json.get("nextSyncToken").and_then(Value::as_str).map(str::to_string)
}

fn net_error(e: reqwest::Error) -> String {
    if e.is_connect() || e.is_timeout() {
        OFFLINE.into()
    } else {
        format!("No connection: {e}")
    }
}

async fn get_json(http: &reqwest::Client, token: &str, url: reqwest::Url) -> Result<Value, String> {
    let response = http
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(net_error)?;
    let status = response.status();
    let json: Value = response.json().await.unwrap_or(json!({}));
    if status.is_success() {
        return Ok(json);
    }
    if status.as_u16() == 401 {
        *ACCESS.lock().unwrap() = None;
    }
    Err(google_error(&json, &format!("Google Calendar error {}", status.as_u16())))
}

fn events_url(calendar_id: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(&format!("{API}/calendars/")).map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "bad calendar URL".to_string())?
        .pop_if_empty()
        .push(calendar_id)
        .push("events");
    Ok(url)
}

/// Events from every calendar ticked in Google Calendar, from a day and a half
/// ago (so "today" is covered in any time zone) to eight days ahead, plus the
/// sync tokens the next checks start from. No orderBy and no minAccessRole:
/// neither can be combined with a sync token, and the island sorts anyway.
async fn fetch_all(http: &reqwest::Client, token: &str) -> Result<Cache, String> {
    let list_url = reqwest::Url::parse(&format!("{API}/users/me/calendarList")).map_err(|e| e.to_string())?;
    let list = get_json(http, token, list_url).await?;
    let list_token = list
        .get("nextSyncToken")
        .and_then(Value::as_str)
        .filter(|_| list.get("nextPageToken").is_none())
        .map(str::to_string);
    let calendars: Vec<(String, String, String)> = list
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|c| {
                    // Free/busy-only calendars have no events to read.
                    matches!(c.get("accessRole").and_then(Value::as_str), Some("reader" | "writer" | "owner"))
                })
                .filter(|c| {
                    c.get("selected").and_then(Value::as_bool).unwrap_or(false)
                        || c.get("primary").and_then(Value::as_bool).unwrap_or(false)
                })
                .filter_map(|c| {
                    Some((
                        c.get("id")?.as_str()?.to_string(),
                        c.get("summaryOverride")
                            .or_else(|| c.get("summary"))
                            .and_then(Value::as_str)
                            .unwrap_or("Calendar")
                            .to_string(),
                        c.get("backgroundColor").and_then(Value::as_str).unwrap_or("#4285F4").to_string(),
                    ))
                })
                .take(MAX_CALENDARS)
                .collect()
        })
        .unwrap_or_default();

    let now = now_ms() / 1000;
    let time_min = rfc3339(now.saturating_sub(36 * 3600));
    let time_max = rfc3339(now + 8 * 86400);

    let mut events = Vec::new();
    let mut tokens = Vec::new();
    for (id, name, color) in &calendars {
        let mut page: Option<String> = None;
        let mut sync_token = None;
        for _ in 0..MAX_PAGES {
            let mut url = events_url(id)?;
            url.query_pairs_mut()
                .append_pair("timeMin", &time_min)
                .append_pair("timeMax", &time_max)
                .append_pair("singleEvents", "true")
                .append_pair("maxResults", "250");
            if let Some(page) = &page {
                url.query_pairs_mut().append_pair("pageToken", page);
            }
            let json = get_json(http, token, url).await?;
            if let Some(items) = json.get("items").and_then(Value::as_array) {
                events.extend(items.iter().filter_map(|e| parse_event(e, id, name, color)));
            }
            page = json.get("nextPageToken").and_then(Value::as_str).map(str::to_string);
            if page.is_none() {
                // Only the last page carries it; without one, every check downloads again.
                sync_token = json.get("nextSyncToken").and_then(Value::as_str).map(str::to_string);
                break;
            }
        }
        tokens.push((id.clone(), sync_token));
    }
    Ok(Cache { events, fetched_at: now_ms(), checked_at: 0, list_token, tokens })
}

fn parse_event(e: &Value, calendar_id: &str, calendar: &str, color: &str) -> Option<Value> {
    if e.get("status").and_then(Value::as_str) == Some("cancelled") {
        return None;
    }
    // Invitations you said no to don't belong on your day.
    let declined = e
        .get("attendees")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter().any(|a| {
                a.get("self").and_then(Value::as_bool).unwrap_or(false)
                    && a.get("responseStatus").and_then(Value::as_str) == Some("declined")
            })
        })
        .unwrap_or(false);
    if declined {
        return None;
    }
    let when = |key: &str| -> Option<(String, bool)> {
        let t = e.get(key)?;
        if let Some(dt) = t.get("dateTime").and_then(Value::as_str) {
            return Some((dt.to_string(), false));
        }
        t.get("date").and_then(Value::as_str).map(|d| (d.to_string(), true))
    };
    let (start, all_day) = when("start")?;
    let (end, _) = when("end").unwrap_or((start.clone(), all_day));
    let meet = e.get("hangoutLink").and_then(Value::as_str).or_else(|| {
        e.get("conferenceData")
            .and_then(|c| c.get("entryPoints"))
            .and_then(Value::as_array)?
            .iter()
            .find(|p| p.get("entryPointType").and_then(Value::as_str) == Some("video"))?
            .get("uri")
            .and_then(Value::as_str)
    });
    Some(json!({
        "id": e.get("id")?.as_str()?,
        "calendarId": calendar_id,
        "calendar": calendar,
        "color": color,
        "title": e.get("summary").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("(No title)"),
        "start": start,
        "end": end,
        "allDay": all_day,
        "location": e.get("location").and_then(Value::as_str),
        "meetUrl": meet,
        "htmlLink": e.get("htmlLink").and_then(Value::as_str),
    }))
}

// ── Small helpers (no extra crates) ──────────────────────────────────────────

fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
        .ok()
        .map_err(|e| format!("No randomness available: {e}"))?;
    Ok(buf)
}

fn random_token(n: usize) -> Result<String, String> {
    Ok(b64url(&random_bytes(n)?))
}

fn sha256(input: &[u8]) -> Result<[u8; 32], String> {
    let mut out = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, input, &mut out) }
        .ok()
        .map_err(|e| format!("SHA-256 failed: {e}"))?;
    Ok(out)
}

fn b64url(bytes: &[u8]) -> String {
    crate::claude::base64_for(bytes)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

/// Unix seconds → `2026-10-14T09:30:00Z`.
fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_incremental_answer_means_unchanged() {
        let json = json!({"items": [], "nextSyncToken": "t2"});
        assert_eq!(next_if_unchanged(&json), Some("t2".into()));
    }

    #[test]
    fn any_item_means_changed() {
        let json = json!({"items": [{"id": "e1", "status": "cancelled"}], "nextSyncToken": "t2"});
        assert_eq!(next_if_unchanged(&json), None);
    }

    #[test]
    fn more_pages_mean_changed() {
        assert_eq!(next_if_unchanged(&json!({"items": [], "nextPageToken": "p2"})), None);
    }

    #[test]
    fn no_token_means_download_again() {
        assert_eq!(next_if_unchanged(&json!({"items": []})), None);
    }

    #[test]
    fn the_cache_survives_a_round_trip() {
        let cache = Cache {
            events: vec![json!({"id": "e1", "title": "Standup"})],
            fetched_at: 1,
            checked_at: 2,
            list_token: Some("l".into()),
            tokens: vec![("primary".into(), Some("t".into())), ("other".into(), None)],
        };
        let back: Cache = serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(back.events, cache.events);
        assert_eq!(back.tokens, cache.tokens);
        assert_eq!((back.fetched_at, back.checked_at, back.list_token), (1, 2, Some("l".into())));
    }

    const MIN: u64 = 60_000;

    #[test]
    fn due_once_five_wall_clock_minutes_have_passed() {
        let now = 1_000 * MIN;
        assert!(is_due(now, None, None), "never synced");
        assert!(!is_due(now, Some(now - 4 * MIN), None));
        assert!(is_due(now, Some(now - 5 * MIN), None));
        // Checked 20 min before an hour of sleep: due on the first tick after.
        assert!(is_due(now, Some(now - 80 * MIN), None));
    }

    #[test]
    fn offline_tries_every_tick_but_google_errors_wait() {
        let now = 1_000 * MIN;
        let old = Some(now - 30 * MIN);
        assert!(is_due(now, old, Some((now - MIN, true))), "offline: try again");
        assert!(!is_due(now, old, Some((now - MIN, false))), "Google refused: wait");
        assert!(is_due(now, old, Some((now - 6 * MIN, false))));
    }

    #[test]
    fn a_clock_set_back_counts_as_due() {
        let now = 1_000 * MIN;
        assert!(is_due(now, Some(now + 60 * MIN), None));
    }

    #[test]
    fn rfc3339_formats_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_792_050_600), "2026-10-15T07:50:00Z");
    }
}
