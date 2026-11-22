//! Phone remote (off by default, Settings › Phone remote): a tiny web page
//! served on the home network to accept a match and go through champ select
//! (ban, pick, runes, spells) from a phone.
//!
//! - The server only exists while League is in the ready check or in champ
//!   select (or for a few minutes when the player opens it to pair a phone).
//!   Outside that nothing listens and nothing is sent.
//! - Everything goes through the League client's local API, as the client's
//!   own buttons do. The game is never touched.
//! - A phone is paired once with the PIN shown in Settings; it then holds a
//!   random key. Without a key nothing can be read or done.
//! - `leaguecompanion.local` is answered by a small mDNS responder (no other
//!   program needed); the PC's address works too.
//! - The page waits on one request that the server answers the moment
//!   something changes, so the phone follows the client within a fraction of
//!   a second without polling.

use crate::builds::{self, Mode};
use crate::riot_client::{self, LeagueApi};
use crate::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const HOST: &str = "leaguecompanion.local";
/// Tried in order: 80 gives an address without a port number.
const PORTS: [u16; 3] = [80, 8765, 8766];
const PAGE: &str = include_str!("remote.html");
/// The server stays a moment after champ select ends (dodge → next match).
const LINGER: Duration = Duration::from_secs(6);
/// A waiting phone gets an (unchanged) answer after this long and asks again.
const LONG_POLL: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------- preferences

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
struct Prefs {
    enabled: bool,
    /// Six digits typed once on a phone to pair it.
    pin: String,
    /// Keys of the paired phones.
    keys: Vec<String>,
}

fn prefs_path() -> Option<PathBuf> {
    app_data_dir().ok().map(|directory| directory.join("remote.json"))
}

static PREFS: Mutex<Option<Prefs>> = Mutex::new(None);

fn prefs() -> Prefs {
    let mut cached = PREFS.lock().unwrap_or_else(|error| error.into_inner());
    cached
        .get_or_insert_with(|| {
            // Sealed like the accounts file: it holds the phones' keys.
            let stored = prefs_path().and_then(|path| std::fs::read(path).ok()).and_then(|bytes| match crate::protect::open(&bytes) {
                crate::protect::Opened::Sealed(plain) => serde_json::from_slice::<Prefs>(&plain).ok(),
                _ => None,
            });
            stored.unwrap_or_default()
        })
        .clone()
}

fn save(prefs: &Prefs) {
    if let Ok(mut cached) = PREFS.lock() {
        *cached = Some(prefs.clone());
    }
    let Some(path) = prefs_path() else { return };
    if let Some(sealed) = serde_json::to_vec(prefs).ok().and_then(|plain| crate::protect::seal(&plain).ok()) {
        let _ = std::fs::write(path, sealed);
    }
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    let _ = crate::backup::random_bytes(&mut buffer);
    buffer.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn new_pin() -> String {
    let mut buffer = [0u8; 4];
    let _ = crate::backup::random_bytes(&mut buffer);
    format!("{:06}", u32::from_le_bytes(buffer) % 1_000_000)
}

// ---------------------------------------------------------------- shared state

/// What the phones see, and the bookkeeping around it.
struct Shared {
    /// (version, state as JSON): the version goes up when the state changes.
    state: Mutex<(u64, String)>,
    changed: Condvar,
    /// Phones waiting for a change right now.
    waiting: AtomicUsize,
    /// Unix ms of the last request (the state is read faster while a phone is there).
    last_request: AtomicU64,
    /// An action was just sent: read the client again at once.
    refresh: AtomicBool,
    /// Champion and spell names (in the client's language), read once per start.
    names: Mutex<String>,
    /// Wrong PINs in a row, and when pairing opens again.
    failures: Mutex<(u32, Option<Instant>)>,
}

static SHARED: Mutex<Option<Arc<Shared>>> = Mutex::new(None);
/// The running server: its stop switch and port.
static SERVER: Mutex<Option<(Arc<AtomicBool>, u16)>> = Mutex::new(None);
/// "Open now" from Settings: keep the server up until then (Unix ms).
static OPEN_UNTIL: AtomicU64 = AtomicU64::new(0);
/// The page's texts in the interface language (sent by the app's window).
static TEXTS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64)
}

fn shared() -> Arc<Shared> {
    let mut slot = SHARED.lock().unwrap_or_else(|error| error.into_inner());
    Arc::clone(slot.get_or_insert_with(|| {
        Arc::new(Shared {
            state: Mutex::new((1, json!({ "phase": "idle" }).to_string())),
            changed: Condvar::new(),
            waiting: AtomicUsize::new(0),
            last_request: AtomicU64::new(0),
            refresh: AtomicBool::new(false),
            names: Mutex::new(String::new()),
            failures: Mutex::new((0, None)),
        })
    }))
}

// ---------------------------------------------------------------- League client → state

/// Slow-changing parts, read every few seconds (and right after an action).
#[derive(Default)]
struct Extras {
    read: Option<Instant>,
    pickable: Value,
    bannable: Value,
    pages: Value,
    /// The selected rune page (whoever made it: the player, Blitz, ...).
    rune: Value,
    /// The champions this player may choose from when the mode offers only a few (ARAM cards).
    subset: Value,
}

fn ids(value: Option<Value>) -> Value {
    value.filter(Value::is_array).unwrap_or_else(|| json!([]))
}

fn read_extras(client: &LeagueApi) -> Extras {
    let pages: Vec<Value> = client
        .get_json("/lol-perks/v1/pages")
        .and_then(|pages| pages.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|page| page["isValid"].as_bool() != Some(false))
        .map(|page| json!({ "id": page["id"], "name": page["name"], "cur": page["current"].as_bool().unwrap_or(false) }))
        .collect();
    let rune = client.get_json("/lol-perks/v1/currentpage").map_or(Value::Null, |page| {
        json!({
            "id": page["id"],
            "name": page["name"],
            "primary": page["primaryStyleId"],
            "sub": page["subStyleId"],
            "perks": page["selectedPerkIds"],
        })
    });
    Extras {
        read: Some(Instant::now()),
        pickable: ids(client.get_json("/lol-champ-select/v1/pickable-champion-ids")),
        bannable: ids(client.get_json("/lol-champ-select/v1/bannable-champion-ids")),
        pages: Value::Array(pages),
        rune,
        subset: ids(client.get_json("/lol-lobby-team-builder/champ-select/v1/subset-champion-list")),
    }
}

/// Champ select as the page needs it. Times are absolute (PC clock, ms), so
/// the state only changes when something really changed.
fn select_state(client: &LeagueApi, extras: &Extras) -> Option<Value> {
    let session = client.get_json("/lol-champ-select/v1/session")?;
    let cell = session["localPlayerCellId"].as_i64()?;
    let list = |value: &Value| value.as_array().cloned().unwrap_or_default();
    let actions: Vec<Value> = list(&session["actions"]).iter().flat_map(list).collect();
    // My turn: the first of my actions that is not done (the one in progress first).
    let mine: Vec<&Value> = actions.iter().filter(|action| action["actorCellId"].as_i64() == Some(cell) && action["completed"].as_bool() != Some(true)).collect();
    let action = mine.iter().find(|action| action["isInProgress"].as_bool() == Some(true)).or(mine.first()).map(|action| {
        json!({ "id": action["id"], "type": action["type"], "active": action["isInProgress"].as_bool().unwrap_or(false), "champ": action["championId"] })
    });
    let player = |member: &Value| {
        json!({
            "champ": member["championId"].as_i64().unwrap_or(0),
            "intent": member["championPickIntent"].as_i64().unwrap_or(0),
            "pos": member["assignedPosition"].as_str().unwrap_or_default(),
            "me": member["cellId"].as_i64() == Some(cell),
        })
    };
    let me = list(&session["myTeam"]).into_iter().find(|member| member["cellId"].as_i64() == Some(cell)).unwrap_or(Value::Null);
    let banned: Vec<i64> = actions
        .iter()
        .filter(|action| action["type"].as_str() == Some("ban") && action["completed"].as_bool() == Some(true))
        .filter_map(|action| action["championId"].as_i64().filter(|id| *id > 0))
        .collect();
    let timer = &session["timer"];
    // Custom games can have no time limit: no countdown then.
    let end = if timer["isInfinite"].as_bool() == Some(true) { 0 } else { timer["internalNowInEpochMs"].as_i64().unwrap_or(0) + timer["adjustedTimeLeftInPhase"].as_i64().unwrap_or(0) };
    Some(json!({
        "phase": "select",
        "step": timer["phase"].as_str().unwrap_or_default(),
        // Rounded: the client's two numbers jitter by a few ms between reads.
        "end": (end / 250) * 250,
        "action": action,
        "spells": [me["spell1Id"].as_i64().unwrap_or(0), me["spell2Id"].as_i64().unwrap_or(0)],
        "team": list(&session["myTeam"]).iter().map(player).collect::<Vec<_>>(),
        "enemy": list(&session["theirTeam"]).iter().map(player).collect::<Vec<_>>(),
        "bans": banned,
        "pickable": extras.pickable,
        "bannable": extras.bannable,
        "pages": extras.pages,
        "rune": extras.rune,
        "subset": extras.subset,
        // ARAM: the champion I hold, the bench anyone may take from, rerolls left.
        "mine": me["championId"].as_i64().unwrap_or(0),
        "bench": if session["benchEnabled"].as_bool() == Some(true) {
            Value::Array(list(&session["benchChampions"]).iter().filter_map(|champion| champion["championId"].as_i64()).map(Value::from).collect())
        } else {
            Value::Null
        },
        "rerolls": session["rerollsRemaining"].as_i64().unwrap_or(0),
    }))
}

/// For working out a game mode's champ select (ARAM: Mayhem cards): while the
/// file `capture-champselect.on` exists in the app's folder, what the client
/// reports during champ select is kept in `champselect-capture.json` (the
/// last 60 different snapshots). Local only, never sent anywhere.
fn capture(client: &LeagueApi) {
    let Ok(directory) = app_data_dir() else { return };
    if !directory.join("capture-champselect.on").exists() {
        return;
    }
    let paths = [
        "/lol-champ-select/v1/session",
        "/lol-champ-select/v1/pickable-champion-ids",
        "/lol-lobby-team-builder/champ-select/v1/subset-champion-list",
        "/lol-champ-select/v1/current-champion",
        "/lol-champ-select/v1/session/my-selection",
    ];
    let mut snapshot = serde_json::Map::new();
    for path in paths {
        snapshot.insert(path.to_owned(), client.get_json(path).unwrap_or(Value::Null));
    }
    let flow = client.get_json("/lol-gameflow/v1/session").unwrap_or(Value::Null);
    snapshot.insert("queue".to_owned(), flow["gameData"]["queue"].clone());
    let snapshot = Value::Object(snapshot);
    let file = directory.join("champselect-capture.json");
    let mut list: Vec<Value> = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    // The countdown changes every time: compare without it.
    let plain = |value: &Value| {
        let mut copy = value.clone();
        if let Some(session) = copy.get_mut("/lol-champ-select/v1/session").and_then(Value::as_object_mut) {
            session.remove("timer");
        }
        copy
    };
    if list.last().is_some_and(|last| plain(&last["data"]) == plain(&snapshot)) {
        return;
    }
    list.push(json!({ "at": now_ms(), "data": snapshot }));
    let extra = list.len().saturating_sub(60);
    list.drain(..extra);
    if let Ok(body) = serde_json::to_vec(&list) {
        let _ = std::fs::write(file, body);
    }
}

/// Auto accept as set in the app (`autoaccept.json`): (on, seconds waited first).
fn auto_accept() -> (bool, u64) {
    let prefs: Value = app_data_dir().ok().and_then(|directory| std::fs::read(directory.join("autoaccept.json")).ok()).and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
    (prefs["enabled"].as_bool().unwrap_or(false), prefs["delaySeconds"].as_u64().unwrap_or(0))
}

fn ready_state(client: &LeagueApi) -> Value {
    let check = client.get_json("/lol-matchmaking/v1/ready-check").unwrap_or(Value::Null);
    let shown = check["timer"].as_f64().unwrap_or(0.0);
    json!({
        "phase": "ready",
        // The app accepts by itself: the phone says so instead of asking.
        "auto": auto_accept().0,
        "answer": check["playerResponse"].as_str().unwrap_or("None"),
        // When the check ends (about 10 s after it appeared), to the second.
        "end": ((now_ms() as f64 - shown * 1000.0 + 10_000.0) / 1000.0).round() as u64 * 1000,
    })
}

/// Champions, the summoner spells of this game mode and the rune trees, with
/// names in the client's language and the game's own pictures, as JSON.
fn read_names(client: &LeagueApi) -> String {
    let champions: Vec<Value> = client
        .get_json("/lol-game-data/assets/v1/champion-summary.json")
        .and_then(|list| list.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|champion| champion["id"].as_i64().is_some_and(|id| id > 0))
        .map(|champion| json!({ "id": champion["id"], "name": champion["name"] }))
        .collect();
    let assets = builds::game_assets(Some(client));
    // Custom games, ARAM and the practice tool have their own spell lists.
    let flow = client.get_json("/lol-gameflow/v1/session").unwrap_or(Value::Null);
    let mode = flow["map"]["gameMode"].as_str().or_else(|| flow["gameData"]["queue"]["gameMode"].as_str()).filter(|mode| !mode.is_empty()).unwrap_or("CLASSIC").to_owned();
    let all_spells: Vec<Value> = client.get_json("/lol-game-data/assets/v1/summoner-spells.json").and_then(|list| list.as_array().cloned()).unwrap_or_default();
    let spells_for = |wanted: &str| -> Vec<Value> {
        all_spells
            .iter()
            .filter(|spell| spell["gameModes"].as_array().is_some_and(|modes| modes.iter().any(|mode| mode.as_str() == Some(wanted))))
            .filter_map(|spell| {
                let id = spell["id"].as_i64()?;
                Some(json!({ "id": id, "name": spell["name"], "icon": assets.spells.get(&id).map(|asset| asset.icon.clone()).unwrap_or_default() }))
            })
            .collect()
    };
    let mut spells = spells_for(&mode);
    if spells.is_empty() {
        spells = spells_for("CLASSIC");
    }
    let mut styles: Vec<(&i64, &builds::RuneStyle)> = assets.styles.iter().collect();
    styles.sort_by_key(|(id, _)| **id);
    let mut perks = serde_json::Map::new();
    for (_, style) in &styles {
        for id in style.slots.iter().flatten() {
            if let Some(asset) = assets.perks.get(id) {
                perks.insert(id.to_string(), json!({ "name": asset.name, "icon": asset.icon }));
            }
        }
    }
    let styles: Vec<Value> = styles.iter().map(|(id, style)| json!({ "id": id, "name": style.name, "icon": style.icon, "slots": style.slots })).collect();
    json!({ "champions": champions, "spells": spells, "styles": styles, "perks": perks }).to_string()
}

fn publish(shared: &Shared, state: &Value) {
    let text = state.to_string();
    let mut current = shared.state.lock().unwrap_or_else(|error| error.into_inner());
    if current.1 != text {
        current.0 += 1;
        current.1 = text;
        shared.changed.notify_all();
    }
}

// ---------------------------------------------------------------- actions from the phone

/// The client only takes a `POST` that says it carries JSON, even an empty
/// one (a lock in sent without a body was ignored): every request has a body.
fn sent(client: &LeagueApi, method: &str, path: &str, body: Option<&Value>) -> bool {
    let empty = json!({});
    client.send(method, path, Some(body.unwrap_or(&empty))).is_some_and(|(status, _)| (200..300).contains(&status))
}

fn act(body: &Value) -> Result<(), &'static str> {
    let client = riot_client::league_client().ok_or("no_client")?;
    let number = |name: &str| body[name].as_i64().filter(|value| *value >= 0);
    let done = |ok: bool| if ok { Ok(()) } else { Err("refused") };
    match body["t"].as_str().unwrap_or_default() {
        "accept" => done(sent(&client, "POST", "/lol-matchmaking/v1/ready-check/accept", None)),
        "decline" => done(sent(&client, "POST", "/lol-matchmaking/v1/ready-check/decline", None)),
        // Show the champion to the team (as a click on it in the client does).
        "hover" => {
            let (action, champion) = (number("action").ok_or("bad")?, number("id").ok_or("bad")?);
            done(sent(&client, "PATCH", &format!("/lol-champ-select/v1/session/actions/{action}"), Some(&json!({ "championId": champion }))))
        }
        // Ban or lock in.
        "lock" => {
            let (action, champion) = (number("action").ok_or("bad")?, number("id").ok_or("bad")?);
            let path = format!("/lol-champ-select/v1/session/actions/{action}");
            // The champion and "done" in one request; the client's own two-step way when that is refused.
            if sent(&client, "PATCH", &path, Some(&json!({ "championId": champion, "completed": true }))) {
                return Ok(());
            }
            sent(&client, "PATCH", &path, Some(&json!({ "championId": champion })));
            done(sent(&client, "POST", &format!("{path}/complete"), None))
        }
        "page" => done(sent(&client, "PUT", "/lol-perks/v1/currentpage", Some(&json!(number("id").ok_or("bad")?)))),
        // ARAM: take a champion from the bench (mine goes there), or roll a new one.
        "swap" => done(sent(&client, "POST", &format!("/lol-champ-select/v1/session/bench/swap/{}", number("id").ok_or("bad")?), None)),
        "reroll" => done(sent(&client, "POST", "/lol-champ-select/v1/session/my-selection/reroll", None)),
        "spells" => {
            let (first, second) = (number("a").ok_or("bad")?, number("b").ok_or("bad")?);
            done(sent(&client, "PATCH", "/lol-champ-select/v1/session/my-selection", Some(&json!({ "spell1Id": first, "spell2Id": second }))))
        }
        // The page edited on the phone: the selected page is changed in place
        // (it keeps its name, also when another app made it); a page that
        // cannot be edited is left alone and ours is made instead.
        "rune" => {
            let perks: Vec<i64> = body["perks"].as_array().map(|list| list.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
            let (primary, sub) = (number("primary").ok_or("bad")?, number("sub").ok_or("bad")?);
            if perks.len() != 9 || primary == sub {
                return Err("bad");
            }
            let current = client.get_json("/lol-perks/v1/currentpage").unwrap_or(Value::Null);
            let editable = current["isEditable"].as_bool() == Some(true);
            if let Some(id) = current["id"].as_i64().filter(|_| editable) {
                let page = json!({ "name": current["name"], "primaryStyleId": primary, "subStyleId": sub, "selectedPerkIds": perks, "current": true });
                if sent(&client, "PUT", &format!("/lol-perks/v1/pages/{id}"), Some(&page)) {
                    return Ok(());
                }
            }
            let page = builds::RunePage { primary_style: primary, sub_style: sub, perks, rates: builds::Rates { games: 0, win_rate: 0.0, pick_rate: 0.0 } };
            builds::apply_runes(&client, "League Companion", &page).map(|_| ()).map_err(|_| "refused")
        }
        // Asked for by a button only, never by itself: the most used runes and
        // spells for the champion and lane (OP.GG, a few KB).
        "auto" => {
            let select = builds::champ_select(&client);
            if !select.active || select.champion_id <= 0 {
                return Err("no_champion");
            }
            let build = builds::champion_build(select.champion_id, select.mode.unwrap_or(Mode::Ranked), &select.position).map_err(|_| "no_build")?;
            let page = build.rune_pages.first().ok_or("no_build")?;
            builds::apply_runes(&client, "League Companion", page).map_err(|_| "refused")?;
            if let Some(spells) = build.spells.first() {
                let _ = builds::apply_spells(&client, &spells.ids);
            }
            Ok(())
        }
        _ => Err("bad"),
    }
}

// ---------------------------------------------------------------- HTTP

struct Request {
    method: String,
    path: String,
    query: String,
    key: String,
    body: Vec<u8>,
}

/// One request from the connection (None: closed, too large or not HTTP).
fn read_request(stream: &mut TcpStream, pending: &mut Vec<u8>) -> Option<Request> {
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        if let Some(position) = pending.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if pending.len() > 16 * 1024 {
            return None;
        }
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        pending.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&pending[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_owned(), first.next()?.to_owned());
    let mut length = 0usize;
    let mut key = String::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().ok()?,
            "x-key" => key = value.trim().to_owned(),
            _ => {}
        }
    }
    if length > 4096 {
        return None;
    }
    while pending.len() < head_end + length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        pending.extend_from_slice(&chunk[..read]);
    }
    let body = pending[head_end..head_end + length].to_vec();
    pending.drain(..head_end + length);
    let (path, query) = target.split_once('?').map_or((target.clone(), String::new()), |(path, query)| (path.to_owned(), query.to_owned()));
    Some(Request { method, path, query, key, body })
}

fn respond(stream: &mut TcpStream, status: &str, kind: &str, cache: &str, body: &[u8]) -> bool {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: {cache}\r\nX-Content-Type-Options: nosniff\r\nConnection: keep-alive\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).is_ok() && stream.write_all(body).is_ok()
}

fn respond_json(stream: &mut TcpStream, status: &str, body: &Value) -> bool {
    respond(stream, status, "application/json", "no-store", body.to_string().as_bytes())
}

fn paired(key: &str) -> bool {
    key.len() >= 16 && prefs().keys.iter().any(|known| known == key)
}

/// PIN → a key for this phone. Five wrong PINs close pairing for a minute.
fn pair(shared: &Shared, body: &Value) -> Result<String, &'static str> {
    let mut failures = shared.failures.lock().unwrap_or_else(|error| error.into_inner());
    if failures.1.is_some_and(|until| Instant::now() < until) {
        return Err("locked");
    }
    let mut prefs = prefs();
    let pin = body["pin"].as_str().unwrap_or_default();
    if prefs.pin.len() != 6 || pin != prefs.pin {
        failures.0 += 1;
        if failures.0 >= 5 {
            *failures = (0, Some(Instant::now() + Duration::from_secs(60)));
        }
        return Err("pin");
    }
    *failures = (0, None);
    let key = random_hex(16);
    prefs.keys.push(key.clone());
    // The oldest phones go when the list grows long.
    let extra = prefs.keys.len().saturating_sub(8);
    prefs.keys.drain(..extra);
    save(&prefs);
    Ok(key)
}

fn handle(mut stream: TcpStream, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut pending = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let Some(request) = read_request(&mut stream, &mut pending) else { return };
        shared.last_request.store(now_ms(), Ordering::Relaxed);
        let get = request.method == "GET";
        let post = request.method == "POST";
        let ok = match request.path.as_str() {
            "/" | "/index.html" if get => respond(&mut stream, "200 OK", "text/html; charset=utf-8", "no-cache", PAGE.as_bytes()),
            "/texts" if get => {
                let texts = TEXTS.lock().ok().and_then(|texts| texts.clone()).unwrap_or_default();
                respond_json(&mut stream, "200 OK", &json!(texts))
            }
            "/pair" if post => match pair(&shared, &serde_json::from_slice(&request.body).unwrap_or(Value::Null)) {
                Ok(key) => respond_json(&mut stream, "200 OK", &json!({ "key": key })),
                Err(code) => respond_json(&mut stream, "403 Forbidden", &json!({ "error": code })),
            },
            path if get && path.starts_with("/c/") && path.ends_with(".png") => {
                // Champion squares: the same pictures the app shows (from the League client or its cache).
                let id: Option<u32> = path[3..path.len() - 4].parse().ok();
                match id.and_then(|id| crate::image_cache::load(&format!("/v1/champion-icons/{id}.png"))) {
                    Some((bytes, mime)) => respond(&mut stream, "200 OK", mime, "public, max-age=604800, immutable", &bytes),
                    None => respond(&mut stream, "404 Not Found", "text/plain", "no-store", b""),
                }
            }
            path if get && path.starts_with("/g/") => {
                // Rune and spell pictures: lower-case game-data paths to images only (checked by the cache).
                match Some(&path[2..]).filter(|path| crate::image_cache::valid_path(path)).and_then(crate::image_cache::load) {
                    Some((bytes, mime)) => respond(&mut stream, "200 OK", mime, "public, max-age=604800, immutable", &bytes),
                    None => respond(&mut stream, "404 Not Found", "text/plain", "no-store", b""),
                }
            }
            _ if !paired(&request.key) => respond_json(&mut stream, "401 Unauthorized", &json!({ "error": "key" })),
            "/names" if get => {
                let names = shared.names.lock().map(|names| names.clone()).unwrap_or_default();
                respond(&mut stream, "200 OK", "application/json", "no-store", if names.is_empty() { b"{}" } else { names.as_bytes() })
            }
            "/state" if get => {
                // Answered at once when the phone's version is old, else when something changes.
                let seen: u64 = request.query.split('&').find_map(|part| part.strip_prefix("v=")).and_then(|value| value.parse().ok()).unwrap_or(0);
                shared.waiting.fetch_add(1, Ordering::Relaxed);
                let started = Instant::now();
                let mut state = shared.state.lock().unwrap_or_else(|error| error.into_inner());
                while state.0 == seen && started.elapsed() < LONG_POLL && !stop.load(Ordering::Relaxed) {
                    state = shared.changed.wait_timeout(state, Duration::from_millis(500)).map(|(guard, _)| guard).unwrap_or_else(|error| error.into_inner().0);
                }
                let body = format!("{{\"v\":{},\"now\":{},\"s\":{}}}", state.0, now_ms(), state.1);
                drop(state);
                shared.waiting.fetch_sub(1, Ordering::Relaxed);
                respond(&mut stream, "200 OK", "application/json", "no-store", body.as_bytes())
            }
            "/act" if post => {
                let result = act(&serde_json::from_slice(&request.body).unwrap_or(Value::Null));
                shared.refresh.store(true, Ordering::Relaxed);
                match result {
                    Ok(()) => respond_json(&mut stream, "200 OK", &json!({ "ok": true })),
                    Err(code) => respond_json(&mut stream, "409 Conflict", &json!({ "error": code })),
                }
            }
            _ => respond(&mut stream, "404 Not Found", "text/plain", "no-store", b""),
        };
        if !ok {
            return;
        }
    }
}

// ---------------------------------------------------------------- name on the network (mDNS)

/// The PC's address on the home network (the one its default route uses).
fn lan_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    // No packet is sent: connecting a UDP socket only picks the route.
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(address) if !address.ip().is_loopback() && !address.ip().is_unspecified() => Some(*address.ip()),
        _ => None,
    }
}

/// The name in a DNS question (labels joined by dots, lower case) and where the question ends.
fn question_name(packet: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    loop {
        let length = *packet.get(at)? as usize;
        at += 1;
        if length == 0 {
            return Some((name, at));
        }
        // Compressed names do not appear in the questions we answer.
        if length > 63 {
            return None;
        }
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&String::from_utf8_lossy(packet.get(at..at + length)?).to_ascii_lowercase());
        at += length;
    }
}

/// Answer "who is leaguecompanion.local?" on the home network while the
/// server runs. Phones and PCs ask this by themselves for `.local` names.
fn answer_name(address: Ipv4Addr, stop: Arc<AtomicBool>) {
    use socket2::{Domain, Protocol, Socket, Type};
    const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
    let Ok(socket) = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) else { return };
    // Windows' own name service listens on the same port: share it.
    let _ = socket.set_reuse_address(true);
    if socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 5353).into()).is_err() {
        return;
    }
    let _ = socket.join_multicast_v4(&GROUP, &address);
    let _ = socket.set_multicast_if_v4(&address);
    let _ = socket.set_multicast_ttl_v4(255);
    let socket: UdpSocket = socket.into();
    let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
    let mut answer = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
    for label in HOST.split('.') {
        answer.push(label.len() as u8);
        answer.extend_from_slice(label.as_bytes());
    }
    // End of name, type A, class IN with the cache-flush bit, 120 s, 4 bytes.
    answer.extend_from_slice(&[0, 0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4]);
    answer.extend_from_slice(&address.octets());
    let mut packet = [0u8; 1500];
    while !stop.load(Ordering::Relaxed) {
        let Ok((length, from)) = socket.recv_from(&mut packet) else { continue };
        let packet = &packet[..length];
        // Questions only (not other devices' answers).
        if length < 12 || packet[2] & 0x80 != 0 {
            continue;
        }
        let questions = u16::from_be_bytes([packet[4], packet[5]]);
        let mut at = 12;
        for _ in 0..questions.min(8) {
            let Some((name, end)) = question_name(packet, at) else { break };
            let Some(kind) = packet.get(end..end + 4) else { break };
            at = end + 4;
            let wanted = u16::from_be_bytes([kind[0], kind[1]]);
            if name == HOST && (wanted == 1 || wanted == 255) {
                let _ = socket.send_to(&answer, SocketAddrV4::new(GROUP, 5353));
                // "Answer me directly" (the top bit of the class), as phones ask at first.
                if kind[2] & 0x80 != 0 {
                    let _ = socket.send_to(&answer, from);
                }
                break;
            }
        }
    }
}

// ---------------------------------------------------------------- server life

fn start(shared: &Arc<Shared>) -> Option<(Arc<AtomicBool>, u16)> {
    // LC_REMOTE_LOCAL: this PC only (the test program; no firewall question).
    let local = std::env::var_os("LC_REMOTE_LOCAL").is_some();
    let host = if local { Ipv4Addr::LOCALHOST } else { Ipv4Addr::UNSPECIFIED };
    // The test program keeps away from the port of an app that is running.
    let ports: &[u16] = if local { &[8799] } else { &PORTS };
    let (listener, port) = ports.iter().find_map(|port| TcpListener::bind(SocketAddrV4::new(host, *port)).ok().map(|listener| (listener, *port)))?;
    listener.set_nonblocking(true).ok()?;
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (shared, stop) = (Arc::clone(shared), Arc::clone(&stop));
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let (shared, stop) = (Arc::clone(&shared), Arc::clone(&stop));
                        thread::spawn(move || handle(stream, shared, stop));
                    }
                    // Nobody is connecting: look again shortly (no CPU is used meanwhile).
                    Err(_) => thread::sleep(Duration::from_millis(25)),
                }
            }
        });
    }
    if let Some(address) = lan_address().filter(|_| !local) {
        let stop = Arc::clone(&stop);
        thread::spawn(move || answer_name(address, stop));
    }
    Some((stop, port))
}

fn running() -> Option<u16> {
    SERVER.lock().ok().and_then(|server| server.as_ref().map(|(_, port)| *port))
}

fn stop_server(status: &dyn Fn(bool)) {
    let stopped = SERVER.lock().ok().and_then(|mut server| server.take());
    if let Some((stop, _)) = stopped {
        stop.store(true, Ordering::Relaxed);
        shared().changed.notify_all();
        status(false);
    }
}

/// The app is quitting: close the page and the name answers at once (Windows
/// would close them with the process anyway; this makes it immediate).
pub fn shutdown() {
    OPEN_UNTIL.store(0, Ordering::Relaxed);
    stop_server(&|_| {});
}

/// Start the watcher. `status` hears when the server starts (true) or stops.
pub fn spawn(status: impl Fn(bool) + Send + 'static) {
    thread::spawn(move || {
        let shared = shared();
        let mut extras = Extras::default();
        let mut idle_since: Option<Instant> = None;
        loop {
            let prefs = prefs();
            if !prefs.enabled {
                stop_server(&status);
                thread::sleep(Duration::from_secs(3));
                continue;
            }
            let client = riot_client::league_client();
            let phase = client.as_ref().and_then(|client| client.get_json("/lol-gameflow/v1/gameflow-phase")).and_then(|phase| phase.as_str().map(str::to_owned)).unwrap_or_default();
            // On for as long as the setting is on (the owner's choice: a server that
            // came and went left the phone guessing). It closes with the switch and with the app.
            let live = true;
            let open = OPEN_UNTIL.load(Ordering::Relaxed) > now_ms();
            if !live && !open {
                // Not needed: close it (after a moment, for a dodge) and look again later.
                if running().is_some() && idle_since.get_or_insert_with(Instant::now).elapsed() >= LINGER {
                    stop_server(&status);
                }
                if running().is_some() {
                    publish(&shared, &json!({ "phase": "idle" }));
                }
                thread::sleep(Duration::from_secs(if running().is_some() { 1 } else { 2 }));
                continue;
            }
            idle_since = None;
            if running().is_none() {
                let Some(server) = start(&shared) else {
                    thread::sleep(Duration::from_secs(5));
                    continue;
                };
                if let Ok(mut slot) = SERVER.lock() {
                    *slot = Some(server);
                }
                extras = Extras::default();
                if let Ok(mut names) = shared.names.lock() {
                    names.clear();
                }
                status(true);
            }
            if let Some(client) = &client {
                if shared.names.lock().is_ok_and(|names| names.is_empty()) {
                    let names = read_names(client);
                    if let Ok(mut slot) = shared.names.lock() {
                        *slot = names;
                    }
                }
                let state = match phase.as_str() {
                    "ReadyCheck" => ready_state(client),
                    "ChampSelect" => {
                        // The lists of champions and rune pages change rarely: every 3 s, and after an action.
                        let forced = shared.refresh.swap(false, Ordering::Relaxed);
                        if forced || extras.read.is_none_or(|read| read.elapsed() >= Duration::from_secs(3)) {
                            extras = read_extras(client);
                            capture(client);
                        }
                        select_state(client, &extras).unwrap_or_else(|| json!({ "phase": "idle" }))
                    }
                    // Lobby or queue: the page says which while it waits.
                    other => json!({ "phase": "idle", "flow": other }),
                };
                publish(&shared, &state);
            } else {
                publish(&shared, &json!({ "phase": "idle" }));
            }
            // A phone is there: follow the client closely. Nobody: once a second is enough.
            let watched = shared.waiting.load(Ordering::Relaxed) > 0 || now_ms().saturating_sub(shared.last_request.load(Ordering::Relaxed)) < 5_000;
            // Only the ready check and champ select need a close watch; the rest
            // of the time one look a second (two without a client) is plenty.
            let busy = matches!(phase.as_str(), "ReadyCheck" | "ChampSelect");
            let pause = if client.is_none() { 2000 } else if busy && watched { 250 } else { 1000 };
            // An action from the phone cuts the pause short.
            for _ in 0..pause / 50 {
                if shared.refresh.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    });
}

// ---------------------------------------------------------------- commands

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteView {
    pub enabled: bool,
    pub pin: String,
    /// Phones paired so far.
    pub paired: usize,
    /// The server is up right now, and on which port (80 = no port in the address).
    pub running: bool,
    pub port: u16,
    /// `leaguecompanion.local` and the PC's address, as typed on the phone.
    pub name: String,
    pub address: String,
}

fn view() -> RemoteView {
    let prefs = prefs();
    let port = running().unwrap_or(PORTS[0]);
    let with_port = |host: String| if port == 80 { format!("http://{host}") } else { format!("http://{host}:{port}") };
    RemoteView {
        enabled: prefs.enabled,
        pin: prefs.pin.clone(),
        paired: prefs.keys.len(),
        running: running().is_some(),
        port,
        name: with_port(HOST.to_owned()),
        address: lan_address().map(|address| with_port(address.to_string())).unwrap_or_default(),
    }
}

pub fn info() -> RemoteView {
    view()
}

pub fn set_enabled(enabled: bool) -> RemoteView {
    let mut prefs = prefs();
    prefs.enabled = enabled;
    if enabled && prefs.pin.len() != 6 {
        prefs.pin = new_pin();
    }
    if !enabled {
        OPEN_UNTIL.store(0, Ordering::Relaxed);
    }
    save(&prefs);
    view()
}

/// A new PIN; with `forget`, every paired phone must pair again.
pub fn reset(forget: bool) -> RemoteView {
    let mut prefs = prefs();
    prefs.pin = new_pin();
    if forget {
        prefs.keys.clear();
    }
    save(&prefs);
    view()
}

/// Keep the page reachable for a few minutes outside champ select (to pair a
/// phone or look at it). Waits a moment for the server to be up.
pub fn open(minutes: u64) -> RemoteView {
    OPEN_UNTIL.store(now_ms() + minutes.clamp(1, 10) * 60_000, Ordering::Relaxed);
    for _ in 0..40 {
        if running().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    view()
}

/// Ask the network "who is leaguecompanion.local?" as a phone does and wait
/// for the answer (Windows' own lookup also asks for an IPv6 address and
/// waits several seconds for it, which made a working name look missing).
fn name_answers(address: Ipv4Addr) -> bool {
    use socket2::{Domain, Protocol, Socket, Type};
    const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
    let Ok(socket) = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) else { return false };
    let _ = socket.set_reuse_address(true);
    if socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 5353).into()).is_err() {
        return false;
    }
    let _ = socket.join_multicast_v4(&GROUP, &address);
    let _ = socket.set_multicast_if_v4(&address);
    let socket: UdpSocket = socket.into();
    let _ = socket.set_read_timeout(Some(Duration::from_millis(300)));
    let mut question = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in HOST.split('.') {
        question.push(label.len() as u8);
        question.extend_from_slice(label.as_bytes());
    }
    question.extend_from_slice(&[0, 0, 1, 0, 1]);
    if socket.send_to(&question, SocketAddrV4::new(GROUP, 5353)).is_err() {
        return false;
    }
    let started = Instant::now();
    let mut packet = [0u8; 1500];
    while started.elapsed() < Duration::from_secs(2) {
        let Ok((length, _)) = socket.recv_from(&mut packet) else { continue };
        let packet = &packet[..length];
        // An answer (not a question) for our name that ends with this PC's address.
        if length > 12 && packet[2] & 0x80 != 0 && question_name(packet, 12).is_some_and(|(name, _)| name == HOST) && packet.ends_with(&address.octets()) {
            return true;
        }
    }
    false
}

/// What a test from this PC found.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    /// The page answered on the PC's network address.
    pub page: bool,
    /// `leaguecompanion.local` was found (by this PC's own name lookup).
    pub name: bool,
    pub view: RemoteView,
}

/// Open the page for a few minutes and check from this PC that it answers on
/// the network address and that the name is found. (A phone can still be
/// stopped by the Windows firewall or the router: this cannot see that.)
pub fn test() -> TestResult {
    let view = open(5);
    let page = lan_address().is_some_and(|address| {
        let target = SocketAddr::V4(SocketAddrV4::new(address, view.port));
        let Ok(mut stream) = TcpStream::connect_timeout(&target, Duration::from_secs(2)) else { return false };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut answer = [0u8; 16];
        stream.write_all(b"GET /texts HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").is_ok() && stream.read(&mut answer).is_ok_and(|read| answer[..read].starts_with(b"HTTP/1.1 200"))
    });
    let name = view.running && lan_address().is_some_and(name_answers);
    TestResult { page, name, view: self::view() }
}

/// The page's texts in the interface language.
pub fn set_texts(texts: HashMap<String, String>) {
    let texts: HashMap<String, String> = texts.into_iter().filter(|(key, value)| key.len() <= 40 && value.chars().count() <= 300).take(80).collect();
    if let Ok(mut slot) = TEXTS.lock() {
        *slot = Some(texts);
    }
}

/// For the test program: does this name question ask for our name?
pub fn asks_for_us(packet: &[u8]) -> Option<bool> {
    let (name, end) = question_name(packet, 12)?;
    let kind = packet.get(end..end + 2)?;
    Some(name == HOST && matches!(u16::from_be_bytes([kind[0], kind[1]]), 1 | 255))
}
