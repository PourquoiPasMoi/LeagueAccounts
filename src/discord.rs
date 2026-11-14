//! Discord webhooks: named channels the player can post clips to. Kept on this
//! PC only (`discord.json` in the app folder). Posting sends the clip and a
//! message to Discord's webhook address, nothing else; mentions are disabled.

use crate::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Webhook {
    pub id: String,
    pub name: String,
    pub url: String,
}

fn file() -> Option<PathBuf> {
    app_data_dir().ok().map(|directory| directory.join("discord.json"))
}

pub fn load() -> Vec<Webhook> {
    file().and_then(|path| std::fs::read(path).ok()).and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn save(list: &[Webhook]) -> Result<(), &'static str> {
    let path = file().ok_or("file_write")?;
    let body = serde_json::to_vec_pretty(list).map_err(|_| "file_write")?;
    std::fs::write(path, body).map_err(|_| "file_write")
}

/// Only Discord's own webhook addresses.
pub fn valid_url(url: &str) -> bool {
    let url = url.trim();
    ["https://discord.com/api/webhooks/", "https://discordapp.com/api/webhooks/", "https://ptb.discord.com/api/webhooks/", "https://canary.discord.com/api/webhooks/"]
        .iter()
        .any(|prefix| url.starts_with(prefix) && url.len() > prefix.len() + 10)
        && !url.chars().any(|c| c.is_whitespace() || c == '"' || c == '<' || c == '>')
}

pub fn add(name: &str, url: &str) -> Result<Vec<Webhook>, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("discord_name_empty");
    }
    if !valid_url(url) {
        return Err("discord_url_invalid");
    }
    let mut list = load();
    let id = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_nanos()));
    list.push(Webhook { id, name: name.chars().take(60).collect(), url: url.trim().to_owned() });
    save(&list)?;
    Ok(list)
}

pub fn remove(id: &str) -> Result<Vec<Webhook>, &'static str> {
    let mut list = load();
    list.retain(|webhook| webhook.id != id);
    save(&list)?;
    Ok(list)
}

/// A file name Discord shows as is (ASCII, no quotes).
fn upload_name(path: &Path) -> String {
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let clean: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '(' | ')' | '.') { c } else { '_' }).collect();
    let clean = clean.trim().to_owned();
    format!("{}.mp4", if clean.is_empty() { "clip".to_owned() } else { clean })
}

/// The request body: the message as `payload_json`, then the video as `files[0]`.
pub fn multipart(boundary: &str, payload: &str, file_name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(bytes.len() + payload.len() + 512);
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\nContent-Type: application/json\r\n\r\n{payload}\r\n").as_bytes());
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"files[0]\"; filename=\"{file_name}\"\r\nContent-Type: video/mp4\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// Post the video and a message to one webhook. Error codes: `discord_too_big`
/// (over the server's upload limit), `discord_webhook_invalid` (deleted or
/// wrong address), `discord_failed`.
pub fn post(url: &str, video: &Path, message: &str) -> Result<(), &'static str> {
    if !valid_url(url) {
        return Err("discord_webhook_invalid");
    }
    let bytes = std::fs::read(video).map_err(|_| "recording_missing")?;
    let content: String = message.trim().chars().take(2000).collect();
    let payload = serde_json::json!({ "content": content, "allowed_mentions": { "parse": [] } }).to_string();
    let boundary = format!("LeagueCompanion{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_nanos()));
    let body = multipart(&boundary, &payload, &upload_name(video), &bytes);
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(300)).build().map_err(|_| "discord_failed")?;
    let response = client
        .post(format!("{}?wait=true", url.trim()))
        .header(reqwest::header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
        .body(body)
        .send()
        .map_err(|_| "discord_failed")?;
    match response.status().as_u16() {
        200..=299 => Ok(()),
        413 => Err("discord_too_big"),
        401 | 403 | 404 => Err("discord_webhook_invalid"),
        _ => Err("discord_failed"),
    }
}
