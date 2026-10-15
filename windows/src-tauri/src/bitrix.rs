// Bitrix24 tasks — read from a local tasks.json, never from the portal.
//
// The portal gives ordinary employees no REST access, so something else on the
// machine (a script of the user's own) writes the file and Coucou only reads it:
// no session, no credentials, no network. The file is checked every 30 s and
// re-read only when its size or modification time changes.
//
// Expected shape — an array (or {"tasks": [...]}) of
//   {"id"?: string|number, "title": string, "url"?: string, "deadline": RFC 3339 with a zone}

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::log;

const CHECK_EVERY: Duration = Duration::from_secs(30);
/// Anything bigger is not a task list.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_TASKS: usize = 500;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BitrixTask {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
    pub deadline: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BitrixUpdate {
    /// False when no file is set in Settings.
    pub configured: bool,
    /// None when the file couldn't be read: the island keeps what it had.
    pub tasks: Option<Vec<BitrixTask>>,
    pub error: Option<String>,
    /// Unix ms of the file's last modification — when the list was produced.
    pub synced_at: Option<u64>,
}

/// (path, modified ms, size) of what was last sent, so an unchanged file is never re-read.
static SEEN: Mutex<Option<(String, u64, u64)>> = Mutex::new(None);

pub fn parse_tasks(text: &str) -> Result<Vec<BitrixTask>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let root: Value =
        serde_json::from_str(text).map_err(|e| format!("tasks.json isn't valid JSON: {e}"))?;
    let items = match &root {
        Value::Array(items) => items,
        Value::Object(obj) => match obj.get("tasks") {
            Some(Value::Array(items)) => items,
            _ => return Err("tasks.json must be an array of tasks (or {\"tasks\": [...]}).".into()),
        },
        _ => return Err("tasks.json must be an array of tasks (or {\"tasks\": [...]}).".into()),
    };
    Ok(items.iter().filter_map(parse_task).take(MAX_TASKS).collect())
}

fn parse_task(item: &Value) -> Option<BitrixTask> {
    let title = item.get("title")?.as_str()?.trim();
    let deadline = item.get("deadline")?.as_str()?.trim();
    if title.is_empty() || !is_date_time_with_zone(deadline) {
        return None;
    }
    // Only links the island may open.
    let url = item
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(str::to_string);
    let id = match item.get("id") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => url.clone().unwrap_or_else(|| title.to_string()),
    };
    Some(BitrixTask { id, title: title.to_string(), url, deadline: deadline.to_string() })
}

/// `YYYY-MM-DDTHH:MM[:SS[.fff]]` followed by `Z` or `±HH:MM`. A date alone or a
/// time without a zone would be read in the wrong time zone, so it's skipped.
fn is_date_time_with_zone(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |from: usize, n: usize| {
        b.len() >= from + n && b[from..from + n].iter().all(u8::is_ascii_digit)
    };
    let num = |from: usize, n: usize| -> u32 {
        s[from..from + n].parse().unwrap_or(u32::MAX)
    };
    let date_time = digits(0, 4) && b.get(4) == Some(&b'-') && digits(5, 2) && b.get(7) == Some(&b'-')
        && digits(8, 2) && matches!(b.get(10), Some(b'T' | b't')) && digits(11, 2)
        && b.get(13) == Some(&b':') && digits(14, 2);
    if !date_time || !(1..=12).contains(&num(5, 2)) || !(1..=31).contains(&num(8, 2))
        || num(11, 2) > 23 || num(14, 2) > 59
    {
        return false;
    }
    let mut i = 16;
    if b.get(i) == Some(&b':') {
        if !digits(i + 1, 2) {
            return false;
        }
        i += 3;
        if b.get(i) == Some(&b'.') {
            i += 1;
            let start = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == start {
                return false;
            }
        }
    }
    match b.get(i) {
        Some(b'Z' | b'z') => i + 1 == b.len(),
        Some(b'+' | b'-') => {
            i + 6 == b.len() && digits(i + 1, 2) && b[i + 3] == b':' && digits(i + 4, 2)
        }
        _ => false,
    }
}

/// Where tasks.json lives: the path from Settings (without the quotes Explorer's
/// "Copy as path" adds), or — when only a portal is set — the file Coucou's own
/// sync service writes. Empty when there is neither.
pub fn tasks_path(app: &AppHandle) -> String {
    let shared = app.state::<crate::Shared>();
    let settings = shared.settings.lock().unwrap();
    let raw = settings.bitrix_tasks_path.trim().trim_matches('"').trim();
    if !raw.is_empty() {
        return raw.to_string();
    }
    if settings.bitrix_portal.trim().is_empty() {
        return String::new();
    }
    crate::settings::local_dir().join("bitrix").join("tasks.json").to_string_lossy().into_owned()
}

fn emit(app: &AppHandle, update: BitrixUpdate) {
    let _ = app.emit("bitrix", update);
}

/// Checks the file every 30 s. Paused means not even that.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let mut ticker = tokio::time::interval(CHECK_EVERY);
        loop {
            ticker.tick().await;
            if crate::integrations::PAUSED.load(Ordering::Relaxed) {
                continue;
            }
            check(&app, false);
        }
    });
}

/// Sends the current list. `force` re-sends even when nothing changed — the
/// island asks for it at launch and the refresh button uses it.
pub fn check(app: &AppHandle, force: bool) {
    let path = tasks_path(app);
    if path.is_empty() {
        if remember((String::new(), 0, 0), force) {
            emit(app, BitrixUpdate { configured: false, tasks: Some(vec![]), error: None, synced_at: None });
        }
        return;
    }

    let meta = match std::fs::metadata(PathBuf::from(&path)) {
        Ok(meta) if meta.is_file() => meta,
        _ => {
            if remember((path.clone(), 0, u64::MAX), force) {
                log::line(format!("bitrix: {path} not found"));
                emit(app, BitrixUpdate {
                    configured: true,
                    tasks: Some(vec![]),
                    error: Some(format!("Can't find {path}")),
                    synced_at: None,
                });
            }
            return;
        }
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if !remember((path.clone(), modified, meta.len()), force) {
        return;
    }

    let result = if meta.len() > MAX_BYTES {
        Err("tasks.json is larger than 2 MB — that doesn't look like a task list.".to_string())
    } else {
        std::fs::read_to_string(&path)
            .map_err(|e| format!("Can't read {path}: {e}"))
            .and_then(|text| parse_tasks(&text))
    };
    match result {
        Ok(tasks) => emit(app, BitrixUpdate {
            configured: true,
            tasks: Some(tasks),
            error: None,
            synced_at: Some(modified),
        }),
        Err(err) => {
            log::line(format!("bitrix: {err}"));
            emit(app, BitrixUpdate { configured: true, tasks: None, error: Some(err), synced_at: None });
        }
    }
}

/// True when `seen` differs from what was last sent (or `force`), and records it.
fn remember(seen: (String, u64, u64), force: bool) -> bool {
    let mut last = SEEN.lock().unwrap();
    if !force && last.as_ref() == Some(&seen) {
        return false;
    }
    *last = Some(seen);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(text: &str) -> Vec<String> {
        parse_tasks(text).unwrap().into_iter().map(|t| t.id).collect()
    }

    #[test]
    fn reads_the_planned_format() {
        let tasks = parse_tasks(
            r#"[{"title":"Report","url":"https://umschool.bitrix24.kz/company/personal/user/1/tasks/task/view/42/","deadline":"2026-10-20T18:00:00+05:00"}]"#,
        )
        .unwrap();
        assert_eq!(
            tasks,
            vec![BitrixTask {
                id: "https://umschool.bitrix24.kz/company/personal/user/1/tasks/task/view/42/".into(),
                title: "Report".into(),
                url: Some("https://umschool.bitrix24.kz/company/personal/user/1/tasks/task/view/42/".into()),
                deadline: "2026-10-20T18:00:00+05:00".into(),
            }]
        );
    }

    #[test]
    fn takes_an_explicit_id_string_or_number() {
        let text = r#"[
            {"id": 42, "title": "A", "deadline": "2026-10-20T18:00:00Z"},
            {"id": "x7", "title": "B", "deadline": "2026-10-20T18:00:00Z"}
        ]"#;
        assert_eq!(ids(text), vec!["42", "x7"]);
    }

    #[test]
    fn falls_back_to_the_title_without_id_or_url() {
        assert_eq!(ids(r#"[{"title":"A","deadline":"2026-10-20T18:00:00Z"}]"#), vec!["A"]);
    }

    #[test]
    fn accepts_an_object_with_a_tasks_array() {
        assert_eq!(ids(r#"{"tasks":[{"id":1,"title":"A","deadline":"2026-10-20T18:00:00Z"}]}"#), vec!["1"]);
    }

    #[test]
    fn skips_entries_without_title_or_deadline() {
        let text = r#"[
            {"id": 1, "title": "", "deadline": "2026-10-20T18:00:00Z"},
            {"id": 2, "title": "No deadline"},
            {"id": 3, "title": "Null deadline", "deadline": null},
            "not an object",
            {"id": 4, "title": "  Kept  ", "deadline": "2026-10-20T18:00:00Z"}
        ]"#;
        let tasks = parse_tasks(text).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Kept");
    }

    #[test]
    fn needs_a_full_date_time_with_a_time_zone() {
        let text = r#"[
            {"id": 1, "title": "A", "deadline": "2026-10-20T18:00:00+05:00"},
            {"id": 2, "title": "B", "deadline": "2026-10-20T13:00:00Z"},
            {"id": 3, "title": "C", "deadline": "2026-10-20T13:00:00.250-03:30"},
            {"id": 4, "title": "D", "deadline": "2026-10-20T18:00+05:00"},
            {"id": 5, "title": "Date only", "deadline": "2026-10-20"},
            {"id": 6, "title": "No zone", "deadline": "2026-10-20T18:00:00"},
            {"id": 7, "title": "Words", "deadline": "tomorrow"},
            {"id": 8, "title": "Bad month", "deadline": "2026-13-20T18:00:00Z"}
        ]"#;
        assert_eq!(ids(text), vec!["1", "2", "3", "4"]);
    }

    #[test]
    fn drops_links_that_are_not_http() {
        let tasks = parse_tasks(
            r#"[{"id":1,"title":"A","url":"javascript:alert(1)","deadline":"2026-10-20T18:00:00Z"}]"#,
        )
        .unwrap();
        assert_eq!(tasks[0].url, None);
    }

    #[test]
    fn tolerates_a_utf8_bom() {
        assert_eq!(ids("\u{feff}[{\"id\":1,\"title\":\"A\",\"deadline\":\"2026-10-20T18:00:00Z\"}]"), vec!["1"]);
    }

    #[test]
    fn rejects_invalid_json_and_other_shapes() {
        assert!(parse_tasks("{oops").unwrap_err().contains("valid JSON"));
        assert!(parse_tasks(r#"{"title":"A"}"#).unwrap_err().contains("array"));
    }

    #[test]
    fn caps_the_number_of_tasks() {
        let one = r#"{"title":"A","deadline":"2026-10-20T18:00:00Z"}"#;
        let text = format!("[{}]", vec![one; MAX_TASKS + 10].join(","));
        assert_eq!(parse_tasks(&text).unwrap().len(), MAX_TASKS);
    }
}
