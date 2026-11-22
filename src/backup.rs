//! Automatic backup of the accounts (with their passwords) to a folder the
//! player chooses, encrypted with a passphrase.
//!
//! The accounts file itself is sealed with Windows DPAPI (`protect`): it
//! cannot be read on another PC or after Windows is installed again. A backup
//! is the way back, so it must open anywhere, with the passphrase alone:
//! AES-256-GCM, key from PBKDF2-HMAC-SHA256 (Windows' own cryptography, no
//! added library). The passphrase is never stored: only the key made from it
//! is kept, in Windows Credential Manager, so backups can be written without
//! asking each time.

use crate::credentials;
use crate::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

const FORMAT: &str = "league-companion-backup-1";
const ITERATIONS: u32 = 200_000;
const SERVICE: &str = "LeagueAccounts";
const KEY_NAME: &str = "backup:key";
pub const EXTENSION: &str = "lcbackup";
const FILE_PREFIX: &str = "League Companion backup ";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    pub enabled: bool,
    pub folder: String,
    /// Days between backups (1, 7 or 30).
    pub every_days: u32,
    /// Backups kept in the folder (older ones are removed).
    pub keep: u32,
    /// Unix ms of the last backup written.
    pub last_at: u64,
    /// Salt of the stored key (hex); empty = no passphrase set yet.
    pub salt: String,
    /// Code of the last failure, empty after a success.
    pub last_error: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { enabled: false, folder: String::new(), every_days: 7, keep: 5, last_at: 0, salt: String::new(), last_error: String::new() }
    }
}

impl Prefs {
    fn sanitized(mut self) -> Self {
        if !matches!(self.every_days, 1 | 7 | 30) {
            self.every_days = 7;
        }
        self.keep = self.keep.clamp(1, 50);
        self.folder = self.folder.trim().to_owned();
        self
    }

    /// A passphrase was set (its key is in Credential Manager).
    pub fn ready(&self) -> bool {
        !self.salt.is_empty() && !self.folder.is_empty()
    }
}

fn prefs_path() -> Option<PathBuf> {
    app_data_dir().ok().map(|directory| directory.join("backup.json"))
}

pub fn prefs() -> Prefs {
    prefs_path().and_then(|path| std::fs::read(path).ok()).and_then(|bytes| serde_json::from_slice::<Prefs>(&bytes).ok()).unwrap_or_default().sanitized()
}

pub fn save_prefs(prefs: &Prefs) -> io::Result<()> {
    let path = prefs_path().ok_or_else(|| io::Error::other("no app folder"))?;
    std::fs::write(path, serde_json::to_vec_pretty(&prefs.clone().sanitized()).map_err(io::Error::other)?)
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    /// Unix ms.
    created: u64,
    iterations: u32,
    salt: String,
    nonce: String,
    /// Encrypted accounts followed by the 16-byte check tag (hex).
    data: String,
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let digit = |value: u8| (value as char).to_digit(16).map(|digit| digit as u8);
    text.chunks_exact(2).map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?)).collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64)
}

#[cfg(windows)]
mod crypto {
    use std::io;
    use windows_sys::Win32::Security::Cryptography::{
        BCryptCloseAlgorithmProvider, BCryptDecrypt, BCryptDeriveKeyPBKDF2, BCryptDestroyKey, BCryptEncrypt, BCryptGenRandom, BCryptGenerateSymmetricKey,
        BCryptOpenAlgorithmProvider, BCryptSetProperty, BCRYPT_AES_ALGORITHM, BCRYPT_ALG_HANDLE_HMAC_FLAG, BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO, BCRYPT_CHAINING_MODE,
        BCRYPT_CHAIN_MODE_GCM, BCRYPT_SHA256_ALGORITHM, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };

    fn check(status: i32) -> io::Result<()> {
        if status >= 0 { Ok(()) } else { Err(io::Error::other(format!("cryptography error {status:#x}"))) }
    }

    pub fn random(buffer: &mut [u8]) -> io::Result<()> {
        check(unsafe { BCryptGenRandom(std::ptr::null_mut(), buffer.as_mut_ptr(), buffer.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) })
    }

    /// PBKDF2-HMAC-SHA256.
    pub fn derive(passphrase: &str, salt: &[u8], iterations: u32) -> io::Result<[u8; 32]> {
        let mut key = [0u8; 32];
        let mut algorithm = std::ptr::null_mut();
        unsafe {
            check(BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_SHA256_ALGORITHM, std::ptr::null(), BCRYPT_ALG_HANDLE_HMAC_FLAG))?;
            let status = BCryptDeriveKeyPBKDF2(algorithm, passphrase.as_ptr(), passphrase.len() as u32, salt.as_ptr(), salt.len() as u32, u64::from(iterations), key.as_mut_ptr(), 32, 0);
            BCryptCloseAlgorithmProvider(algorithm, 0);
            check(status)?;
        }
        Ok(key)
    }

    /// AES-256-GCM. Encrypting returns the data followed by its 16-byte tag;
    /// decrypting takes that and fails when the key or the data is wrong.
    pub fn aes_gcm(key: &[u8; 32], nonce: &[u8; 12], input: &[u8], encrypt: bool) -> io::Result<Vec<u8>> {
        let (body, mut tag) = if encrypt {
            (input, [0u8; 16])
        } else {
            let split = input.len().checked_sub(16).ok_or_else(|| io::Error::other("backup too short"))?;
            let mut tag = [0u8; 16];
            tag.copy_from_slice(&input[split..]);
            (&input[..split], tag)
        };
        let mut output = vec![0u8; body.len()];
        let mut nonce = *nonce;
        unsafe {
            let mut algorithm = std::ptr::null_mut();
            check(BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_AES_ALGORITHM, std::ptr::null(), 0))?;
            let mode: Vec<u16> = {
                let mut length = 0;
                while *BCRYPT_CHAIN_MODE_GCM.add(length) != 0 {
                    length += 1;
                }
                std::slice::from_raw_parts(BCRYPT_CHAIN_MODE_GCM, length + 1).to_vec()
            };
            let mut status = BCryptSetProperty(algorithm, BCRYPT_CHAINING_MODE, mode.as_ptr().cast(), (mode.len() * 2) as u32, 0);
            let mut handle = std::ptr::null_mut();
            if status >= 0 {
                status = BCryptGenerateSymmetricKey(algorithm, &mut handle, std::ptr::null_mut(), 0, key.as_ptr(), 32, 0);
            }
            if status >= 0 {
                let mut info: BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO = std::mem::zeroed();
                info.cbSize = std::mem::size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32;
                info.dwInfoVersion = 1;
                info.pbNonce = nonce.as_mut_ptr();
                info.cbNonce = 12;
                info.pbTag = tag.as_mut_ptr();
                info.cbTag = 16;
                let mut written = 0u32;
                let info_pointer = (&raw const info).cast();
                status = if encrypt {
                    BCryptEncrypt(handle, body.as_ptr(), body.len() as u32, info_pointer, std::ptr::null_mut(), 0, output.as_mut_ptr(), output.len() as u32, &mut written, 0)
                } else {
                    BCryptDecrypt(handle, body.as_ptr(), body.len() as u32, info_pointer, std::ptr::null_mut(), 0, output.as_mut_ptr(), output.len() as u32, &mut written, 0)
                };
            }
            if !handle.is_null() {
                BCryptDestroyKey(handle);
            }
            BCryptCloseAlgorithmProvider(algorithm, 0);
            check(status)?;
        }
        if encrypt {
            output.extend_from_slice(&tag);
        }
        Ok(output)
    }
}

#[cfg(not(windows))]
mod crypto {
    use std::io;
    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "backups need Windows"))
    }
    pub fn random(_: &mut [u8]) -> io::Result<()> {
        unsupported()
    }
    pub fn derive(_: &str, _: &[u8], _: u32) -> io::Result<[u8; 32]> {
        unsupported()
    }
    pub fn aes_gcm(_: &[u8; 32], _: &[u8; 12], _: &[u8], _: bool) -> io::Result<Vec<u8>> {
        unsupported()
    }
}

/// Random bytes from Windows (keys, PINs).
pub fn random_bytes(buffer: &mut [u8]) -> io::Result<()> {
    crypto::random(buffer)
}

/// Encrypt `plain` with a key already made from the passphrase and `salt`.
fn seal_with(key: &[u8; 32], salt: &[u8], plain: &[u8]) -> io::Result<Vec<u8>> {
    let mut nonce = [0u8; 12];
    crypto::random(&mut nonce)?;
    let data = crypto::aes_gcm(key, &nonce, plain, true)?;
    // Never write a backup that does not read back.
    if crypto::aes_gcm(key, &nonce, &data, false)? != plain {
        return Err(io::Error::other("backup did not read back"));
    }
    let envelope = Envelope { format: FORMAT.into(), created: now_ms(), iterations: ITERATIONS, salt: to_hex(salt), nonce: to_hex(&nonce), data: to_hex(&data) };
    serde_json::to_vec(&envelope).map_err(io::Error::other)
}

/// Encrypt with a passphrase (a new salt).
pub fn seal(plain: &[u8], passphrase: &str) -> io::Result<Vec<u8>> {
    let mut salt = [0u8; 16];
    crypto::random(&mut salt)?;
    seal_with(&crypto::derive(passphrase, &salt, ITERATIONS)?, &salt, plain)
}

/// The file is one of our backups (it needs its passphrase to be read).
pub fn is_backup(contents: &[u8]) -> bool {
    serde_json::from_slice::<Envelope>(contents).is_ok_and(|envelope| envelope.format == FORMAT)
}

/// Decrypt a backup. `Err(InvalidInput)`: wrong passphrase (or damaged file).
pub fn open(contents: &[u8], passphrase: &str) -> io::Result<Vec<u8>> {
    let envelope: Envelope = serde_json::from_slice(contents).map_err(io::Error::other)?;
    if envelope.format != FORMAT || !(1_000..=10_000_000).contains(&envelope.iterations) {
        return Err(io::Error::other("unknown backup format"));
    }
    let damaged = || io::Error::other("damaged backup");
    let salt = from_hex(&envelope.salt).ok_or_else(damaged)?;
    let nonce: [u8; 12] = from_hex(&envelope.nonce).and_then(|nonce| nonce.try_into().ok()).ok_or_else(damaged)?;
    let data = from_hex(&envelope.data).ok_or_else(damaged)?;
    let key = crypto::derive(passphrase, &salt, envelope.iterations)?;
    crypto::aes_gcm(&key, &nonce, &data, false).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "wrong passphrase"))
}

/// Choose the passphrase of the automatic backups: its key goes to Windows
/// Credential Manager, its salt to the preferences.
pub fn set_passphrase(passphrase: &str) -> io::Result<Prefs> {
    let mut salt = [0u8; 16];
    crypto::random(&mut salt)?;
    let key = crypto::derive(passphrase, &salt, ITERATIONS)?;
    credentials::set_password(SERVICE, KEY_NAME, &to_hex(&key)).map_err(|error| io::Error::other(error.to_string()))?;
    let mut prefs = prefs();
    prefs.salt = to_hex(&salt);
    save_prefs(&prefs)?;
    Ok(prefs)
}

fn stored_key(prefs: &Prefs) -> io::Result<([u8; 32], Vec<u8>)> {
    let missing = || io::Error::new(io::ErrorKind::NotFound, "no backup passphrase");
    let salt = from_hex(&prefs.salt).filter(|salt| !salt.is_empty()).ok_or_else(missing)?;
    let key = credentials::get_password(SERVICE, KEY_NAME).ok().flatten().and_then(|hex| from_hex(&hex)).and_then(|key| <[u8; 32]>::try_from(key).ok()).ok_or_else(missing)?;
    Ok((key, salt))
}

fn today() -> String {
    #[cfg(windows)]
    {
        let time = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!("{:04}-{:02}-{:02}", time.wYear, time.wMonth, time.wDay)
    }
    #[cfg(not(windows))]
    {
        String::from("today")
    }
}

/// Our backup files in a folder, newest first.
pub fn list(folder: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|file| file.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION))
                && path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(FILE_PREFIX))
        })
        .collect();
    // The date is in the name: the newest sorts last.
    files.sort();
    files.reverse();
    files
}

/// Write a backup of `accounts_json` now (one file a day, replaced when
/// written again), then drop the oldest ones over the number to keep.
pub fn write(accounts_json: &str) -> io::Result<PathBuf> {
    let mut prefs = prefs();
    let result = (|| {
        if prefs.folder.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no backup folder"));
        }
        let (key, salt) = stored_key(&prefs)?;
        let folder = PathBuf::from(&prefs.folder);
        std::fs::create_dir_all(&folder)?;
        let body = seal_with(&key, &salt, accounts_json.as_bytes())?;
        let target = folder.join(format!("{FILE_PREFIX}{}.{EXTENSION}", today()));
        let temporary = target.with_extension("tmp");
        std::fs::write(&temporary, body)?;
        std::fs::rename(&temporary, &target).inspect_err(|_| {
            let _ = std::fs::remove_file(&temporary);
        })?;
        for old in list(&folder).into_iter().skip(prefs.keep as usize) {
            let _ = std::fs::remove_file(old);
        }
        Ok(target)
    })();
    match &result {
        Ok(_) => {
            prefs.last_at = now_ms();
            prefs.last_error.clear();
        }
        Err(error) => {
            prefs.last_error = match error.kind() {
                io::ErrorKind::NotFound => "backup_not_set",
                _ => "backup_failed",
            }
            .into();
        }
    }
    let _ = save_prefs(&prefs);
    result
}

/// A backup is due (on, set up, and the last one is old enough).
pub fn due() -> bool {
    let prefs = prefs();
    prefs.enabled && prefs.ready() && now_ms().saturating_sub(prefs.last_at) >= u64::from(prefs.every_days) * 86_400_000
}
