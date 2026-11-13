//! Champion and skin pictures, downloaded from CommunityDragon the first time
//! they are shown and kept on disk afterwards, so each image costs bandwidth
//! once. Served to the web view through the `lacache` URI scheme.

use crate::logging::{self, Event, Reason};
use crate::utils::app_data_dir;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

const ORIGIN: &str = "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/";
/// Larger answers are refused: no picture the app shows is this big.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

static CLIENT: LazyLock<Option<reqwest::blocking::Client>> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .ok()
});

/// Only lower-case game-data paths to pictures: nothing that could reach
/// another host, climb directories or fetch something that is not an image.
pub fn valid_path(path: &str) -> bool {
    let path = path.trim_start_matches('/');
    !path.is_empty()
        && path.len() <= 300
        && !path.contains("..")
        && path.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'/' | b'_' | b'-' | b'.'))
        && content_type(path).is_some()
}

pub fn content_type(path: &str) -> Option<&'static str> {
    let extension = path.rsplit('.').next()?;
    match extension {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn cache_file(path: &str) -> Option<PathBuf> {
    let directory = app_data_dir().ok()?.join("cache").join("images");
    std::fs::create_dir_all(&directory).ok()?;
    let digest = Sha256::digest(path.as_bytes());
    let name: String = digest.iter().take(16).map(|byte| format!("{byte:02x}")).collect();
    let extension = path.rsplit('.').next().unwrap_or("img");
    Some(directory.join(format!("{name}.{extension}")))
}

/// The picture's bytes and content type, from disk or downloaded once.
pub fn load(path: &str) -> Option<(Vec<u8>, &'static str)> {
    let path = path.trim_start_matches('/');
    if !valid_path(path) {
        return None;
    }
    let mime = content_type(path)?;
    let file = cache_file(path)?;
    if let Ok(bytes) = std::fs::read(&file) {
        if !bytes.is_empty() {
            return Some((bytes, mime));
        }
    }
    // The running League client has every picture of its patch locally (no
    // bandwidth); CommunityDragon otherwise (it sometimes times out, 522).
    let bytes = match crate::riot_client::league_client().and_then(|client| client.game_asset(path, MAX_BYTES)) {
        Some(bytes) => bytes,
        None => download(path)?,
    };
    // A failed write only means the next view downloads it again.
    if let Ok(mut temporary) = tempfile::NamedTempFile::new_in(file.parent()?) {
        use std::io::Write;
        if temporary.write_all(&bytes).is_ok() {
            let _ = temporary.persist(&file);
        }
    }
    Some((bytes, mime))
}

/// One picture from CommunityDragon, tried twice (it sometimes answers 522).
fn download(path: &str) -> Option<Vec<u8>> {
    for attempt in 0..2 {
        let response = CLIENT.as_ref()?.get(format!("{ORIGIN}{path}")).send();
        match response {
            Ok(response) if response.status().is_success() => {
                let mut bytes = Vec::new();
                response.take(MAX_BYTES).read_to_end(&mut bytes).ok()?;
                return (!bytes.is_empty()).then_some(bytes);
            }
            Ok(response) if response.status().is_server_error() && attempt == 0 => std::thread::sleep(Duration::from_millis(800)),
            Ok(_) => return None,
            Err(error) => {
                logging::record(Event::ImageCacheFailed, Reason::from_request(&error));
                return None;
            }
        }
    }
    None
}
