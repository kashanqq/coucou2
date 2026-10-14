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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
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

const SYNC_EVERY: Duration = Duration::from_secs(120);
/// How long the browser has to come back with the code.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// Calendars beyond this are skipped: one request each, every two minutes.
const MAX_CALENDARS: usize = 12;

/// Access token and the moment it stops being usable.
static ACCESS: Mutex<Option<(String, Instant)>> = Mutex::new(None);
static SIGNING_IN: AtomicBool = AtomicBool::new(false);

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
    // Both windows: the island shows the events, the settings window the status.
    let _ = app.emit("calendar", update);
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

/// Syncs every two minutes while connected. Paused means no network at all.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(4)).await;
        let mut ticker = tokio::time::interval(SYNC_EVERY);
        loop {
            ticker.tick().await;
            if crate::integrations::PAUSED.load(Ordering::Relaxed) || !secrets::present(REFRESH_KEY) {
                continue;
            }
            sync(&app).await;
        }
    });
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
        .map_err(|e| format!("No connection: {e}"))?;
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

pub async fn sync(app: &AppHandle) {
    if !secrets::present(REFRESH_KEY) {
        emit(app, CalendarUpdate { connected: false, events: Some(vec![]), error: None, synced_at: None });
        return;
    }
    match fetch_events().await {
        Ok(events) => emit(app, CalendarUpdate {
            connected: true,
            events: Some(events),
            error: None,
            synced_at: Some(now_ms()),
        }),
        Err(error) => {
            log::line(format!("google calendar: {error}"));
            emit(app, CalendarUpdate {
                connected: secrets::present(REFRESH_KEY),
                events: None,
                error: Some(error),
                synced_at: None,
            });
        }
    }
}

async fn get_json(http: &reqwest::Client, token: &str, url: reqwest::Url) -> Result<Value, String> {
    let response = http
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("No connection: {e}"))?;
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

/// Events from every calendar ticked in Google Calendar, from a day and a half
/// ago (so "today" is covered in any time zone) to eight days ahead.
async fn fetch_events() -> Result<Vec<Value>, String> {
    let http = client();
    let token = access_token(&http).await?;

    let list_url = reqwest::Url::parse_with_params(
        &format!("{API}/users/me/calendarList"),
        &[("minAccessRole", "reader")],
    )
    .map_err(|e| e.to_string())?;
    let list = get_json(&http, &token, list_url).await?;
    let calendars: Vec<(String, String, String)> = list
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
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
    for (id, name, color) in &calendars {
        let mut url = reqwest::Url::parse(&format!("{API}/calendars/")).map_err(|e| e.to_string())?;
        url.path_segments_mut()
            .map_err(|_| "bad calendar URL".to_string())?
            .pop_if_empty()
            .push(id)
            .push("events");
        url.query_pairs_mut()
            .append_pair("timeMin", &time_min)
            .append_pair("timeMax", &time_max)
            .append_pair("singleEvents", "true")
            .append_pair("orderBy", "startTime")
            .append_pair("maxResults", "100");
        let json = get_json(&http, &token, url).await?;
        let Some(items) = json.get("items").and_then(Value::as_array) else { continue };
        events.extend(items.iter().filter_map(|e| parse_event(e, id, name, color)));
    }
    Ok(events)
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
