// Preferences, stored as plain JSON in %APPDATA%\Coucou\settings.json.
// No secret ever lands here — API keys live in the Windows Credential Manager.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub sound_enabled: bool,
    pub sound_volume: f64,
    pub auto_close_interval: f64,
    pub absence_interval: f64,
    pub active_integrations: Vec<String>,
    /// "primary" = the main display, "cursor" = whichever display the mouse is on.
    pub screen: String,
    pub autostart: bool,
    pub hooks_installed: bool,
    /// Claude model used by the chat. Changeable in the settings window.
    /// Defaulted explicitly so a settings.json written by an older build still loads.
    #[serde(default = "default_model")]
    pub model: String,
    /// "anthropic" | "openai" (any OpenAI-compatible API) | "claudeCli" (local claude CLI, i.e. the subscription).
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_openai_base_url")]
    pub openai_base_url: String,
    #[serde(default = "default_openai_model")]
    pub openai_model: String,
    /// Where the island sits along the top edge: "center" | "left" | "right".
    #[serde(default = "default_align")]
    pub island_align: String,
    /// Extra horizontal shift in logical px (positive = right), on top of the alignment.
    #[serde(default)]
    pub island_offset_x: f64,
    /// Hover time over the wake strip before a hidden island peeks out, in ms.
    #[serde(default = "default_wake_delay")]
    pub wake_delay: f64,
    /// Open the island on hover (closing as soon as the cursor leaves) instead of on click.
    #[serde(default)]
    pub hover_expand: bool,
    /// Gap between the screen's top edge and the island, in logical px (0 = glued to the edge).
    #[serde(default = "default_gap")]
    pub island_gap: f64,
    /// Width of the compact island, in logical px.
    #[serde(default = "default_compact_width")]
    pub compact_width: f64,
    /// Which screen edge the island lives on: "top" | "left" | "right".
    #[serde(default = "default_dock")]
    pub island_dock: String,
    /// Distance from the top of the screen when docked to a side edge, in logical px.
    #[serde(default = "default_offset_y")]
    pub island_offset_y: f64,
    /// "island" (floating cloud, shown on demand) | "bar" (thin black bar always on the top edge).
    #[serde(default = "default_style")]
    pub island_style: String,
    /// Local tasks.json with Bitrix24 deadlines, written by the user's own script. Empty = off.
    #[serde(default)]
    pub bitrix_tasks_path: String,
}

fn default_style() -> String {
    "island".into()
}

fn default_dock() -> String {
    "top".into()
}

fn default_offset_y() -> f64 {
    160.0
}

fn default_gap() -> f64 {
    8.0
}

fn default_compact_width() -> f64 {
    200.0
}

fn default_align() -> String {
    "center".into()
}

fn default_wake_delay() -> f64 {
    350.0
}

fn default_model() -> String {
    crate::claude::DEFAULT_MODEL.to_string()
}

fn default_provider() -> String {
    "anthropic".into()
}

fn default_openai_base_url() -> String {
    "https://api.deepseek.com".into()
}

fn default_openai_model() -> String {
    "deepseek-chat".into()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            sound_volume: 0.12,
            auto_close_interval: 15.0,
            absence_interval: 180.0,
            active_integrations: vec![
                "integration_resend".into(),
                "integration_n8n".into(),
                "integration_vercel".into(),
                "integration_github".into(),
            ],
            screen: "primary".into(),
            autostart: false,
            hooks_installed: false,
            model: default_model(),
            provider: default_provider(),
            openai_base_url: default_openai_base_url(),
            openai_model: default_openai_model(),
            island_align: default_align(),
            island_offset_x: 0.0,
            wake_delay: default_wake_delay(),
            hover_expand: false,
            island_gap: default_gap(),
            compact_width: default_compact_width(),
            island_dock: default_dock(),
            island_offset_y: default_offset_y(),
            island_style: default_style(),
            bitrix_tasks_path: String::new(),
        }
    }
}

/// %APPDATA%\Coucou
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Coucou")
}

/// %LOCALAPPDATA%\Coucou — where coucou-hook.exe and the log live.
pub fn local_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Coucou")
}

pub fn hook_exe_path() -> PathBuf {
    local_dir().join("bin").join("coucou-hook.exe")
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn load() -> Settings {
    match std::fs::read(settings_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(settings_path(), json)
}
