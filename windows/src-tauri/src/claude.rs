// Claude API client — the same integration as ClaudeService.swift: multi-turn
// chat with web search, and files sent as document/image/text blocks.
//
// Everything happens here rather than in the island: the API key never leaves
// the Credential Manager, and file bytes never cross the IPC boundary.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::secrets;

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Server-side fallback: on a policy decline the API retries the same request on
/// a fallback model inside the same call, so the island never shows a dead end.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_TOKENS: u32 = 4096;
/// Text and code files are inlined; anything larger is skipped, as on macOS.
const MAX_INLINE_TEXT: u64 = 200_000;

pub const DEFAULT_MODEL: &str = "claude-opus-5";

const SYSTEM_PROMPT: &str = "You are Mochi, a personal AI assistant living at the top of the user's screen. \
You have web search access and can help with absolutely anything — research, coding, finding places, recommendations, tasks, questions. \
Respond in the user's language. Be thorough and complete — use as much detail as the task requires. \
No markdown formatting (no **, no ##, no bullet dashes). Use plain text with line breaks.";

#[derive(Default)]
pub struct Chat {
    /// Full multi-turn history, including tool_use / tool_result blocks.
    messages: Mutex<Vec<Value>>,
    /// Plain role/content history for OpenAI-compatible providers.
    plain: Mutex<Vec<Value>>,
    /// Session id of the local `claude` CLI conversation.
    cli_session: Mutex<Option<String>>,
}

/// Which backend answers the chat, resolved from the settings.
pub enum Provider {
    Anthropic { model: String },
    OpenAi { base_url: String, model: String },
    ClaudeCli,
}

impl Provider {
    pub fn from_settings(s: &crate::settings::Settings) -> Self {
        match s.provider.as_str() {
            "openai" => Provider::OpenAi {
                base_url: s.openai_base_url.clone(),
                model: s.openai_model.clone(),
            },
            "claudeCli" => Provider::ClaudeCli,
            _ => Provider::Anthropic { model: s.model.clone() },
        }
    }
}

impl Chat {
    pub fn reset(&self) {
        self.messages.lock().unwrap().clear();
        self.plain.lock().unwrap().clear();
        *self.cli_session.lock().unwrap() = None;
    }

    fn is_empty(&self) -> bool {
        self.messages.lock().unwrap().is_empty()
    }

    fn push(&self, message: Value) {
        self.messages.lock().unwrap().push(message);
    }

    fn pop(&self) {
        self.messages.lock().unwrap().pop();
    }

    fn snapshot(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatContext {
    File { name: String, path: String },
    Window { app_name: String, title: String, url: Option<String> },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub text: String,
}

pub async fn dispatch(
    chat: &Chat,
    provider: &Provider,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    match provider {
        Provider::Anthropic { model } => send(chat, model, query, context).await,
        Provider::OpenAi { base_url, model } => send_openai(chat, base_url, model, query, context).await,
        Provider::ClaudeCli => send_cli(chat, query, context).await,
    }
}

/// File / window context as plain text, for providers without Anthropic content
/// blocks. Images and PDFs are not available there.
fn plain_context(context: &Option<ChatContext>, with_path: bool) -> Option<String> {
    match context.as_ref()? {
        ChatContext::Window { app_name, title, url } => {
            let mut text = format!("Context — App: {app_name}, Window: {title}");
            if let Some(url) = url {
                text.push_str(&format!(", URL: {url}"));
            }
            Some(text)
        }
        ChatContext::File { name, path } => {
            if let Some(block) = file_block(path) {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    let t = block.get("text").and_then(Value::as_str).unwrap_or("");
                    return Some(format!("File: {name}\n{t}"));
                }
            }
            if with_path {
                Some(format!("File: {path}"))
            } else {
                Some(format!("File: {name} (content not available with this provider)"))
            }
        }
    }
}

// ── OpenAI-compatible providers (DeepSeek, OpenRouter, Groq, Ollama…) ─────────

async fn send_openai(
    chat: &Chat,
    base_url: &str,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let first = chat.plain.lock().unwrap().is_empty();
    let mut user_text = query;
    let mut image_url: Option<String> = None;
    if first {
        // Images go as an OpenAI `image_url` data URI (Gemini, GPT-4o, Ollama vision…).
        if let Some(ChatContext::File { path, .. }) = &context {
            if let Some(block) = file_block(path) {
                if block.get("type").and_then(Value::as_str) == Some("image") {
                    let media = block.pointer("/source/media_type").and_then(Value::as_str).unwrap_or("image/png");
                    let data = block.pointer("/source/data").and_then(Value::as_str).unwrap_or("");
                    image_url = Some(format!("data:{media};base64,{data}"));
                }
            }
        }
        if image_url.is_none() {
            if let Some(ctx) = plain_context(&context, false) {
                user_text = format!("{ctx}\n\n{user_text}");
            }
        }
    }
    let content = match image_url {
        Some(url) => json!([
            { "type": "image_url", "image_url": { "url": url } },
            { "type": "text", "text": user_text },
        ]),
        None => json!(user_text),
    };
    chat.plain.lock().unwrap().push(json!({ "role": "user", "content": content }));

    let result = openai_call(chat, base_url, model).await;
    match result {
        Ok(text) => {
            chat.plain.lock().unwrap().push(json!({ "role": "assistant", "content": text }));
            Ok(ChatReply { text })
        }
        Err(err) => {
            chat.plain.lock().unwrap().pop();
            Err(err)
        }
    }
}

/// `model` may be a comma-separated chain ("gemini-3.8-flash, gemini-3.7-flash"):
/// when one is out of quota (429), overloaded (503) or out of credit (402), the
/// next one answers the same request.
async fn openai_call(chat: &Chat, base_url: &str, model: &str) -> Result<String, String> {
    let url = format!("{}/chat/completions", base_url.trim().trim_end_matches('/'));
    let system = SYSTEM_PROMPT.replace("You have web search access and ", "You ") + " You have no web access.";
    let mut messages = vec![json!({ "role": "system", "content": system })];
    messages.extend(chat.plain.lock().unwrap().clone());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;
    let key = secrets::get("openai-compat-key");

    let models: Vec<&str> = model.split(',').map(str::trim).filter(|m| !m.is_empty()).collect();
    let mut last_err = String::from("No model configured.");
    for m in models {
        let body = json!({ "model": m, "max_tokens": MAX_TOKENS, "messages": messages });
        let mut req = client.post(&url).json(&body);
        if let Some(key) = &key {
            req = req.bearer_auth(key);
        }
        let response = req.send().await.map_err(|e| format!("Network error: {e}"))?;
        let status = response.status();
        let text = response.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| text.chars().take(200).collect());
            last_err = format!("{m}: API {status}: {detail}");
            if matches!(status.as_u16(), 402 | 429 | 503) {
                continue; // quota / overload: try the next model in the chain
            }
            return Err(last_err);
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("Bad API response: {e}"))?;
        let out = v
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if out.is_empty() {
            return Err("No response text.".into());
        }
        return Ok(out);
    }
    Err(last_err)
}

// ── Claude subscription through the local `claude` CLI ────────────────────────

async fn send_cli(chat: &Chat, query: String, context: Option<ChatContext>) -> Result<ChatReply, String> {
    let session = chat.cli_session.lock().unwrap().clone();
    // Prompt goes through stdin: nothing user-controlled is ever put on a command line.
    let mut prompt = query;
    if session.is_none() {
        if let Some(ctx) = plain_context(&context, true) {
            prompt = format!("{ctx}\n\n{prompt}");
        }
        prompt = format!("{SYSTEM_PROMPT}\n\n{prompt}");
    }

    let (text, new_session) = tokio::task::spawn_blocking(move || run_claude_cli(&prompt, session.as_deref()))
        .await
        .map_err(|e| e.to_string())??;
    if let Some(id) = new_session {
        *chat.cli_session.lock().unwrap() = Some(id);
    }
    Ok(ChatReply { text })
}

fn run_claude_cli(prompt: &str, session: Option<&str>) -> Result<(String, Option<String>), String> {
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    // `cmd /C` so claude.cmd (the npm shim) resolves like in a terminal.
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "claude", "-p", "--output-format", "json"]);
    if let Some(id) = session {
        // Session ids come from claude's own output; refuse anything unexpected.
        if id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            cmd.args(["--resume", id]);
        }
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn()
        .map_err(|e| format!("Cannot launch claude: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;

    let parsed: Option<Value> = serde_json::from_slice(&output.stdout).ok();
    let Some(v) = parsed else {
        let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if msg.is_empty() {
            "claude CLI not found or not logged in — run `claude` once in a terminal.".into()
        } else {
            msg
        });
    };
    let text = v.get("result").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if v.get("is_error").and_then(Value::as_bool) == Some(true) {
        return Err(text);
    }
    if text.is_empty() {
        return Err("No response text.".into());
    }
    Ok((text, v.get("session_id").and_then(Value::as_str).map(str::to_string)))
}

/// One chat turn against the Anthropic API. Returns the assistant's text, or a
/// message the island shows in the note view.
pub async fn send(
    chat: &Chat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let key = secrets::get("anthropic-api-key")
        .ok_or_else(|| "API key missing. Open settings.".to_string())?;

    let mut content: Vec<Value> = Vec::new();

    // File / window context rides along with the first message only, exactly
    // like ClaudeService.chat().
    if chat.is_empty() {
        match &context {
            Some(ChatContext::File { name, path }) => {
                if let Some(block) = file_block(path) {
                    content.push(block);
                }
                content.push(json!({ "type": "text", "text": format!("File: {name}") }));
            }
            Some(ChatContext::Window { app_name, title, url }) => {
                let mut text = format!("Context — App: {app_name}, Window: {title}");
                if let Some(url) = url {
                    text.push_str(&format!(", URL: {url}"));
                }
                content.push(json!({ "type": "text", "text": text }));
            }
            None => {}
        }
    }
    content.push(json!({ "type": "text", "text": query }));

    chat.push(json!({ "role": "user", "content": content }));

    let body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": SYSTEM_PROMPT,
        "tools": [{ "type": "web_search_20260209", "name": "web_search", "max_uses": 5 }],
        "fallbacks": "default",
        "messages": chat.snapshot(),
    });

    let response = match call(&key, &body).await {
        Ok(v) => v,
        Err(err) => {
            chat.pop(); // keep the history consistent with what the model saw
            return Err(err);
        }
    };

    // A policy decline comes back as HTTP 200 with stop_reason "refusal".
    if response.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        chat.pop();
        let why = response
            .get("stop_details")
            .and_then(|d| d.get("explanation"))
            .and_then(Value::as_str)
            .unwrap_or("Claude declined this one.");
        return Err(why.to_string());
    }

    let Some(blocks) = response.get("content").and_then(Value::as_array).cloned() else {
        chat.pop();
        return Err("Unexpected API response.".into());
    };

    // Store the whole content — tool_use / tool_result blocks included — so the
    // next turn has the right context.
    chat.push(json!({ "role": "assistant", "content": blocks.clone() }));

    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if text.is_empty() {
        return Err("No response text.".into());
    }
    Ok(ChatReply { text })
}

async fn call(key: &str, body: &Value) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .post(ENDPOINT)
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("anthropic-beta", FALLBACK_BETA)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        // Surface the API's own message, which is what makes a bad key obvious.
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(format!("Claude API {status}: {detail}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("Bad API response: {e}"))
}

/// PDF → document block, image → image block, text/code → inline text.
/// Mirrors readFileAsBlock() in ClaudeService.swift.
fn file_block(path: &str) -> Option<Value> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let media_type = match ext.as_str() {
        "pdf" => Some(("document", "application/pdf")),
        "jpg" | "jpeg" => Some(("image", "image/jpeg")),
        "png" => Some(("image", "image/png")),
        "gif" => Some(("image", "image/gif")),
        "webp" => Some(("image", "image/webp")),
        _ => None,
    };

    if let Some((block_type, media)) = media_type {
        let bytes = std::fs::read(path).ok()?;
        return Some(json!({
            "type": block_type,
            "source": { "type": "base64", "media_type": media, "data": base64(&bytes) },
        }));
    }

    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_INLINE_TEXT {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(json!({ "type": "text", "text": format!("File contents:\n{text}") }))
}

/// Small standalone base64 encoder — not worth another dependency.
/// Also used for Stripe's basic auth.
pub(crate) fn base64_for(bytes: &[u8]) -> String {
    base64(bytes)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
