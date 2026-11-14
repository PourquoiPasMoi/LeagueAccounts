//! Encryption of app files that hold account data.
//!
//! Files are sealed with Windows Data Protection (DPAPI), the same service
//! Credential Manager uses: the key belongs to the signed-in Windows user and
//! never leaves Windows, so a copied file cannot be read on another PC or by
//! another Windows user. No password prompt, no key stored by the app.
//!
//! A sealed file stays valid JSON (`{"format": ..., "data": "<hex>"}`) so an
//! older version of the app sees a file it does not understand, keeps a copy
//! and refuses to write, instead of starting over with an empty list.

use serde::{Deserialize, Serialize};
use std::io;

const FORMAT: &str = "league-companion-dpapi-1";
/// Mixed into the key so a generic DPAPI dump of the user's blobs is not enough.
const ENTROPY: &[u8] = b"LeagueAccounts/protect/v1";

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    data: String,
}

/// What a file on disk turned out to be.
pub enum Opened {
    /// Not a sealed file: an older plain file (or damaged data).
    Plain,
    /// Sealed, and Windows gave the content back.
    Sealed(Vec<u8>),
    /// Sealed, but it cannot be opened here (another PC or Windows user, or damaged).
    Locked(io::Error),
}

/// Read a file's bytes: tells a sealed file from a plain one and decrypts it.
pub fn open(contents: &[u8]) -> Opened {
    let Ok(envelope) = serde_json::from_slice::<Envelope>(contents) else {
        return Opened::Plain;
    };
    if envelope.format != FORMAT {
        return Opened::Locked(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown protected file format",
        ));
    }
    let Some(cipher) = from_hex(&envelope.data) else {
        return Opened::Locked(io::Error::new(
            io::ErrorKind::InvalidData,
            "damaged protected file",
        ));
    };
    match dpapi(&cipher, false) {
        Ok(plain) => Opened::Sealed(plain),
        Err(error) => Opened::Locked(error),
    }
}

/// Encrypt `plain` into the bytes to write to disk. The result is decrypted
/// again and compared before it is returned, so a file that cannot be read
/// back is never written.
pub fn seal(plain: &[u8]) -> io::Result<Vec<u8>> {
    let cipher = dpapi(plain, true)?;
    if dpapi(&cipher, false)? != plain {
        return Err(io::Error::other("protected data did not read back"));
    }
    let envelope = Envelope {
        format: FORMAT.to_owned(),
        data: to_hex(&cipher),
    };
    serde_json::to_vec(&envelope).map_err(io::Error::other)
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let digit = |value: u8| (value as char).to_digit(16).map(|digit| digit as u8);
    text.chunks_exact(2)
        .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

#[cfg(windows)]
fn dpapi(input: &[u8], protect: bool) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let length = u32::try_from(input.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "data too large"))?;
    let source = CRYPT_INTEGER_BLOB {
        cbData: length,
        pbData: input.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: both input blobs point at live slices that Windows only reads;
    // `output` is filled by Windows with a LocalAlloc buffer that is copied and
    // freed below. No window is ever shown (UI_FORBIDDEN).
    let done = unsafe {
        if protect {
            CryptProtectData(
                &source,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &source,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if done == 0 {
        return Err(io::Error::last_os_error());
    }
    if output.pbData.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: on success Windows returned `cbData` readable bytes at `pbData`.
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    // SAFETY: the buffer came from LocalAlloc inside DPAPI and is not used again.
    unsafe {
        if !protect {
            // Do not leave the decrypted copy behind in freed memory.
            std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
        }
        LocalFree(output.pbData.cast());
    }
    Ok(bytes)
}

#[cfg(not(windows))]
fn dpapi(_input: &[u8], _protect: bool) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "file protection needs Windows",
    ))
}
