// Runs the Bitrix24 sync service (windows/bitrix-sync) — a separate Python
// program, not part of this process. Coucou only starts it now and then and
// reads two things back: its exit code and one JSON status line. The tasks
// themselves travel through tasks.json, which bitrix.rs already watches.
//
// The service drives the installed Edge with a profile of its own, so the
// Bitrix24 session (and any 2FA "remember this device") lives in Edge's
// encrypted cookie store, never in Coucou. Sign-in is a visible Edge window,
// opened only from the Sign in button.
//
// Rare and polite, as the plan asks: every 45 min ± 10, never two at once,
// nothing while paused or without a portal. Cheap on the machine too: no Edge
// at all while offline or while the session needs a sign-in, and the whole
// process tree (Python, Playwright's driver, Edge) lives in a job object, so a
// timeout or Coucou quitting can't leave any of it behind.

use std::io::Read;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::WaitForSingleObject;

use crate::log;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SYNC_TIMEOUT: Duration = Duration::from_secs(3 * 60);
/// The service itself gives up after 10 minutes in the sign-in window.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(11 * 60);
const BASE_DELAY_S: u64 = 45 * 60;
const JITTER_S: u64 = 10 * 60;
/// How often the schedule looks at the clock, so a wake or a returning network
/// is noticed within a minute. While a run is due but the portal is
/// unreachable, each tick is one HEAD request, never an Edge.
const TICK: Duration = Duration::from_secs(60);
const OFFLINE: &str = "Offline — syncs as soon as the portal is reachable.";

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// "ok" | "needs-login" | "error" | "running" | "not-installed" | "off"
    pub state: String,
    pub message: Option<String>,
    /// Tasks written, on "ok".
    pub count: Option<u64>,
    /// Unix ms when this status was reached.
    pub at: Option<u64>,
}

impl SyncStatus {
    fn new(state: &str, message: Option<String>) -> Self {
        Self { state: state.into(), message, count: None, at: Some(now_ms()) }
    }
}

static RUNNING: AtomicBool = AtomicBool::new(false);
static LAST: Mutex<Option<SyncStatus>> = Mutex::new(None);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The service's last JSON line, checked against its exit code (0 ok, 3 login).
pub fn parse_status(stdout: &str, code: Option<i32>) -> SyncStatus {
    let line: Option<Value> = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l.trim()).ok().filter(Value::is_object));
    let field = |k: &str| line.as_ref().and_then(|v| v.get(k).cloned());
    let said = field("status").and_then(|v| v.as_str().map(str::to_string));
    let message = field("message").and_then(|v| v.as_str().map(str::to_string));
    let mut status = match (code, said.as_deref()) {
        (Some(0), Some("ok")) => SyncStatus::new("ok", None),
        (Some(3), _) | (_, Some("needs-login")) if code != Some(0) => SyncStatus::new("needs-login", None),
        (Some(_), Some("error")) => SyncStatus::new("error", message),
        (Some(c), _) => SyncStatus::new("error", Some(format!("The sync service stopped with exit code {c}."))),
        (None, _) => SyncStatus::new("error", Some("The sync service was stopped (timeout).".into())),
    };
    if status.state == "ok" {
        status.count = field("count").and_then(|v| v.as_u64());
    }
    status
}

/// 45 minutes ± 10, from any seed — so runs never line up on the clock.
pub fn next_delay(seed: u64) -> Duration {
    let spread = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32;
    Duration::from_secs(BASE_DELAY_S - JITTER_S + spread % (2 * JITTER_S + 1))
}

/// windows/bitrix-sync: next to the app once bundled, else in this checkout.
fn service_dir() -> Option<PathBuf> {
    let next_to_exe = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("bitrix-sync")));
    let checkout = Some(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../bitrix-sync")));
    [next_to_exe, checkout]
        .into_iter()
        .flatten()
        .find(|dir| dir.join(".venv").join("Scripts").join("python.exe").is_file())
}

fn profile_dir() -> PathBuf {
    crate::settings::local_dir().join("bitrix-profile")
}

fn portal(app: &AppHandle) -> String {
    let shared = app.state::<crate::Shared>();
    let portal = shared.settings.lock().unwrap().bitrix_portal.trim().to_string();
    portal
}

fn publish(app: &AppHandle, status: SyncStatus) {
    *LAST.lock().unwrap() = Some(status.clone());
    let _ = app.emit("bitrix-sync", status);
}

pub fn last_status(app: &AppHandle) -> SyncStatus {
    if portal(app).is_empty() {
        return SyncStatus::new("off", None);
    }
    if service_dir().is_none() {
        return SyncStatus::new("not-installed", None);
    }
    if RUNNING.load(Ordering::SeqCst) {
        return SyncStatus::new("running", None);
    }
    LAST.lock().unwrap().clone().unwrap_or_else(|| SyncStatus { at: None, ..SyncStatus::new("idle", None) })
}

/// Background schedule. The first run comes shortly after launch.
///
/// The next run is a wall-clock time kept on disk (bitrix-sync.json), not a
/// countdown: sleep, a shutdown or time offline all count towards it, and the
/// first tick after any of them runs the sync if it's due. A tick is a look at
/// the clock; only a due tick costs anything (a HEAD request, then Edge).
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            scheduled_run(&app).await;
        }
    });
}

/// One tick of the schedule: runs the sync if it's due and possible.
async fn scheduled_run(app: &AppHandle) {
    if crate::integrations::PAUSED.load(Ordering::Relaxed) || !is_due(now_ms(), load_next_at()) {
        return;
    }
    let portal = portal(app);
    if portal.is_empty() {
        return;
    }
    // A headless run can't sign in, it would only find that out again. The
    // Sign in button runs a sync itself once the user is through.
    if LAST.lock().unwrap().as_ref().is_some_and(|s| s.state == "needs-login") {
        return;
    }
    if !reachable(&portal).await {
        // Still due: the next tick tries again, so the sync follows the network back.
        let said = LAST.lock().unwrap().as_ref().and_then(|s| s.message.clone());
        if service_dir().is_some() && said.as_deref() != Some(OFFLINE) {
            publish(app, SyncStatus::new("error", Some(OFFLINE.into())));
        }
        return;
    }
    let status = run(app, false).await;
    // The network went away mid-run: keep it due rather than wait 45 minutes.
    if status.state == "error" && !reachable(&portal).await {
        save_next_at(now_ms());
    }
}

/// Due when the planned time has come, when there is none yet, or when it lies
/// further ahead than any delay could (the clock was set back).
fn is_due(now: u64, next_at: Option<u64>) -> bool {
    let longest = (BASE_DELAY_S + JITTER_S) * 1000;
    next_at.is_none_or(|at| at <= now || at > now + longest)
}

fn schedule_path() -> PathBuf {
    crate::settings::local_dir().join("bitrix-sync.json")
}

fn load_next_at() -> Option<u64> {
    let text = std::fs::read_to_string(schedule_path()).ok()?;
    serde_json::from_str::<Value>(&text).ok()?.get("nextAt")?.as_u64()
}

fn save_next_at(at: u64) {
    let _ = std::fs::create_dir_all(crate::settings::local_dir());
    let _ = std::fs::write(schedule_path(), serde_json::json!({ "nextAt": at }).to_string());
}

/// False only when the portal can't be reached at all (no network, no DNS).
/// Any HTTP answer, even an error page, means Edge would get through.
async fn reachable(portal: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_default();
    match client.head(portal).send().await {
        Ok(_) => true,
        Err(e) => !(e.is_connect() || e.is_timeout()),
    }
}

/// One run of `sync` (or `login`, which opens a visible Edge window). Returns
/// what it ended with; a second run while one is going is refused.
pub async fn run(app: &AppHandle, login: bool) -> SyncStatus {
    let portal = portal(app);
    if portal.is_empty() {
        return SyncStatus::new("off", None);
    }
    let Some(dir) = service_dir() else {
        let status = SyncStatus::new("not-installed", Some("Run windows\\bitrix-sync\\setup.ps1 once.".into()));
        publish(app, status.clone());
        return status;
    };
    if RUNNING.swap(true, Ordering::SeqCst) {
        return SyncStatus::new("running", None);
    }
    publish(app, SyncStatus::new("running", None));

    let out = crate::bitrix::tasks_path(app);
    let mut args = vec![
        "-m".to_string(), "bitrix_sync".into(),
        if login { "login".into() } else { "sync".into() },
        "--portal".into(), portal,
        "--profile".into(), profile_dir().to_string_lossy().into_owned(),
    ];
    if !login {
        args.extend(["--out".to_string(), out]);
    }
    let timeout = if login { LOGIN_TIMEOUT } else { SYNC_TIMEOUT };
    let result = tauri::async_runtime::spawn_blocking(move || run_service(&dir, &args, timeout)).await;
    RUNNING.store(false, Ordering::SeqCst);

    let status = match result {
        Ok(Ok((stdout, code))) => parse_status(&stdout, code),
        Ok(Err(err)) => SyncStatus::new("error", Some(err)),
        Err(err) => SyncStatus::new("error", Some(err.to_string())),
    };
    log::line(format!(
        "bitrix sync{}: {}{}",
        if login { " (login)" } else { "" },
        status.state,
        status.message.as_deref().map(|m| format!(" — {m}")).unwrap_or_default()
    ));
    publish(app, status.clone());
    if !login {
        // Any finished sync, scheduled or "Sync now", sets the next one.
        let now = now_ms();
        save_next_at(now + next_delay(now).as_millis() as u64);
    }
    if status.state == "ok" {
        // The file just changed; show it now rather than at the next 30 s check.
        crate::bitrix::check(app, false);
    }
    status
}

/// Kills every process still in it when dropped: whatever the service started
/// and left running (Playwright's driver, Edge) goes with it.
struct Job(HANDLE);

impl Job {
    fn new() -> Option<Self> {
        let job = Job(unsafe { CreateJobObjectW(None, None) }.ok()?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .ok()?;
        Some(job)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Runs the service and waits, killing its whole tree past `timeout`. (stdout, exit code)
fn run_service(dir: &PathBuf, args: &[String], timeout: Duration) -> Result<(String, Option<i32>), String> {
    let python = dir.join(".venv").join("Scripts").join("python.exe");
    let mut child = Command::new(python)
        .args(args)
        .current_dir(dir)
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("Could not start the sync service: {e}"))?;
    let process = HANDLE(child.as_raw_handle());
    // Python takes far longer to start than this, so its children are born inside.
    let job = Job::new().filter(|job| unsafe { AssignProcessToJobObject(job.0, process) }.is_ok());
    if job.is_none() {
        log::line("bitrix sync: no job object, a timeout may leave Edge running".to_string());
    }

    // Drain both pipes on their own threads so a chatty child can't block.
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    // Sleeps in the kernel until the process ends or time is up: no polling.
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    let code = match unsafe { WaitForSingleObject(process, ms) } {
        WAIT_OBJECT_0 => child.wait().map_err(|e| e.to_string())?.code(),
        WAIT_TIMEOUT => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
        _ => {
            let _ = child.kill();
            child.wait().map_err(|e| e.to_string())?.code()
        }
    };
    // Whatever this run left running ends here, and with it any copy of the
    // pipes it held, so the readers below can finish.
    drop(job);
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if code != Some(0) && !stderr.trim().is_empty() {
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
        log::line(format!("bitrix sync stderr: {tail}"));
    }
    Ok((stdout, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_line_with_count() {
        let s = parse_status("noise\n{\"status\": \"ok\", \"count\": 4, \"out\": \"C:\\\\x\"}\n", Some(0));
        assert_eq!((s.state.as_str(), s.count, s.message), ("ok", Some(4), None));
    }

    #[test]
    fn needs_login() {
        let s = parse_status("{\"status\": \"needs-login\"}", Some(3));
        assert_eq!(s.state, "needs-login");
    }

    #[test]
    fn error_keeps_the_message() {
        let s = parse_status("{\"status\": \"error\", \"message\": \"Bitrix24 said: нет\"}", Some(1));
        assert_eq!((s.state.as_str(), s.message.as_deref()), ("error", Some("Bitrix24 said: нет")));
    }

    #[test]
    fn no_status_line_is_an_error() {
        let s = parse_status("Traceback (most recent call last):\n  boom", Some(1));
        assert_eq!(s.state, "error");
        assert!(s.message.unwrap().contains("exit code 1"));
    }

    #[test]
    fn killed_without_exit_code() {
        let s = parse_status("", None);
        assert_eq!(s.state, "error");
    }

    #[test]
    fn exit_code_wins_over_a_lying_line() {
        // A crash after printing "ok" must not look like success.
        let s = parse_status("{\"status\": \"ok\", \"count\": 1}", Some(1));
        assert_eq!(s.state, "error");
    }

    #[test]
    fn due_by_the_wall_clock() {
        let now = 10_000_000_000;
        assert!(is_due(now, None), "never ran");
        assert!(!is_due(now, Some(now + 60_000)));
        assert!(is_due(now, Some(now)));
        // Planned for 25 min after the laptop went to sleep for an hour.
        assert!(is_due(now, Some(now - 35 * 60_000)));
    }

    #[test]
    fn a_clock_set_back_counts_as_due() {
        let now = 10_000_000_000;
        assert!(!is_due(now, Some(now + 55 * 60_000)));
        assert!(is_due(now, Some(now + 3 * 3600_000)));
    }

    #[test]
    fn delay_is_45_minutes_give_or_take_10() {
        for seed in [0, 1, 7, 599, 600, 1_234_567, u64::MAX] {
            let d = next_delay(seed).as_secs();
            assert!((35 * 60..=55 * 60).contains(&d), "{seed} → {d}");
        }
        assert_ne!(next_delay(1), next_delay(2));
    }
}
