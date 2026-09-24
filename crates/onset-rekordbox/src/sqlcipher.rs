//! Read-only `SQLCipher` 4 page decryption for rekordbox's `master.db`, with WAL replay.
use std::fs;
use std::path::Path;

use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::NoPadding};
use hmac::{Hmac, Mac};
use sha2::Sha512;

/// The passphrase every rekordbox 6/7 install uses for `master.db` (public knowledge).
pub const REKORDBOX_KEY: &str = "402fd482c38817c35ffa8ffb8c7d93143b749e7d315df7a81732a1ff43608497";

const PAGE_SIZE: usize = 4096;
const RESERVE: usize = 80; // 16-byte IV + 64-byte HMAC-SHA512
const SALT_LEN: usize = 16;
const KDF_ITER: u32 = 256_000;
const HMAC_KDF_ITER: u32 = 2;
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type HmacSha512 = Hmac<Sha512>;

#[derive(Debug, thiserror::Error)]
pub enum SqlcipherError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("database is not a whole number of {PAGE_SIZE}-byte pages ({0} bytes)")]
    BadSize(usize),
    #[error("HMAC mismatch on page {page}: wrong key or unsupported SQLCipher settings")]
    HmacMismatch { page: u32 },
    #[error("WAL header is malformed")]
    BadWal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecryptStats {
    pub pages: u32,
    pub wal_frames_applied: u32,
}

struct Keys {
    key: [u8; 32],
    hmac_key: [u8; 32],
}

fn derive_keys(passphrase: &str, salt: &[u8]) -> Keys {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha512>(passphrase.as_bytes(), salt, KDF_ITER, &mut key);
    let hmac_salt: Vec<u8> = salt.iter().map(|b| b ^ 0x3a).collect();
    let mut hmac_key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha512>(&key, &hmac_salt, HMAC_KDF_ITER, &mut hmac_key);
    Keys { key, hmac_key }
}

/// Decrypt one page in place-of-copy. `page_no` is 1-based. Returns the plaintext page
/// (header restored for page 1, reserve zeroed).
fn decrypt_page(keys: &Keys, page_no: u32, page: &[u8]) -> Result<Vec<u8>, SqlcipherError> {
    let data_start = if page_no == 1 { SALT_LEN } else { 0 };
    let data_end = PAGE_SIZE - RESERVE;
    let ciphertext = &page[data_start..data_end];
    let iv = &page[data_end..data_end + 16];
    let mac = &page[data_end + 16..data_end + 16 + 64];

    let mut h = <HmacSha512 as Mac>::new_from_slice(&keys.hmac_key).expect("hmac key length");
    h.update(ciphertext);
    h.update(iv);
    h.update(&page_no.to_le_bytes());
    if h.verify_slice(mac).is_err() {
        return Err(SqlcipherError::HmacMismatch { page: page_no });
    }

    let mut buf = ciphertext.to_vec();
    Aes256CbcDec::new(&keys.key.into(), iv.into())
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|_| SqlcipherError::HmacMismatch { page: page_no })?;

    let mut out = Vec::with_capacity(PAGE_SIZE);
    if page_no == 1 {
        out.extend_from_slice(SQLITE_HEADER);
    }
    out.extend_from_slice(&buf);
    out.resize(PAGE_SIZE, 0);
    Ok(out)
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Decrypts every page of the main file; fails on the first HMAC mismatch.
fn decrypt_all_pages(keys: &Keys, bytes: &[u8]) -> Result<Vec<Vec<u8>>, SqlcipherError> {
    // `chunks_exact` (not the nightly-only `as_chunks`) keeps this on stable Rust.
    #[allow(clippy::chunks_exact_to_as_chunks)]
    bytes
        .chunks_exact(PAGE_SIZE)
        .enumerate()
        .map(|(i, p)| decrypt_page(keys, u32::try_from(i + 1).expect("page count"), p))
        .collect()
}

pub fn decrypt_database(
    db: &Path,
    wal: Option<&Path>,
    passphrase: &str,
    out: &Path,
) -> Result<DecryptStats, SqlcipherError> {
    // rekordbox may checkpoint while we read; a torn page past the first shows up as an HMAC
    // failure. Re-read a few times before giving up. Page 1 failing means the wrong key.
    let (keys, mut pages) = {
        let mut attempt = 0;
        loop {
            let bytes = fs::read(db)?;
            if bytes.len() < PAGE_SIZE || bytes.len() % PAGE_SIZE != 0 {
                return Err(SqlcipherError::BadSize(bytes.len()));
            }
            let keys = derive_keys(passphrase, &bytes[..SALT_LEN]);
            match decrypt_all_pages(&keys, &bytes) {
                Ok(pages) => break (keys, pages),
                Err(SqlcipherError::HmacMismatch { page }) if page > 1 && attempt < 3 => {
                    attempt += 1;
                    tracing::warn!(page, attempt, "torn page while reading master.db; retrying");
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
    };

    let mut applied = 0u32;
    if let Some(wal_path) = wal.filter(|p| p.is_file()) {
        let w = fs::read(wal_path)?;
        if w.len() >= 32 {
            let magic = be_u32(&w[0..4]);
            if magic != 0x377f_0682 && magic != 0x377f_0683 {
                return Err(SqlcipherError::BadWal);
            }
            if be_u32(&w[8..12]) as usize != PAGE_SIZE {
                return Err(SqlcipherError::BadWal);
            }
            let (salt1, salt2) = (be_u32(&w[16..20]), be_u32(&w[20..24]));
            let frame_len = 24 + PAGE_SIZE;
            // Collect valid frames up to the last commit frame.
            let mut pending: Vec<(u32, Vec<u8>)> = Vec::new();
            let mut committed: Vec<(u32, Vec<u8>)> = Vec::new();
            let mut off = 32;
            while off + frame_len <= w.len() {
                let fh = &w[off..off + 24];
                if be_u32(&fh[8..12]) != salt1 || be_u32(&fh[12..16]) != salt2 {
                    break;
                }
                let page_no = be_u32(&fh[0..4]);
                let commit_size = be_u32(&fh[4..8]);
                // SQLite writes a frame header and its page separately; a header with good
                // salts over stale bytes is a torn frame. Like SQLite, treat it as end-of-log.
                let page = match decrypt_page(&keys, page_no, &w[off + 24..off + frame_len]) {
                    Ok(page) => page,
                    Err(SqlcipherError::HmacMismatch { .. }) => {
                        tracing::warn!(frame_offset = off, "torn WAL frame; ending log here");
                        break;
                    }
                    Err(e) => return Err(e),
                };
                pending.push((page_no, page));
                if commit_size != 0 {
                    committed.append(&mut pending);
                }
                off += frame_len;
            }
            for (page_no, page) in committed {
                let idx = page_no as usize - 1;
                if idx >= pages.len() {
                    pages.resize(idx + 1, vec![0u8; PAGE_SIZE]);
                }
                pages[idx] = page;
                applied += 1;
            }
        }
    }

    // The header's "database size in pages" (offset 28) must match the file we write.
    let page_count = u32::try_from(pages.len()).expect("page count");
    pages[0][28..32].copy_from_slice(&page_count.to_be_bytes());

    let mut plain = Vec::with_capacity(pages.len() * PAGE_SIZE);
    for p in &pages {
        plain.extend_from_slice(p);
    }
    fs::write(out, plain)?;
    Ok(DecryptStats {
        pages: page_count,
        wal_frames_applied: applied,
    })
}
