//! The audio files rekordbox has open. A deck keeps its track's file open for as long as the
//! track is loaded, so the open handles name the loaded tracks without any pointer chain,
//! on every rekordbox version. Which deck holds which file is decided elsewhere.
//!
//! Enumerating another process's handles goes through `NtQuerySystemInformation` with the
//! extended handle class, duplicating each candidate into this process to ask its name.
//! Named pipes and some synchronous files hang `NtQueryObject`, so the access masks those
//! usually carry are skipped, as every handle viewer does.
#![allow(unsafe_code)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, NTSTATUS,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};

/// `SystemExtendedHandleInformation`.
const SYSTEM_EXTENDED_HANDLE_INFORMATION: i32 = 64;
const STATUS_INFO_LENGTH_MISMATCH: NTSTATUS = 0xC000_0004_u32.cast_signed();
/// `ObjectNameInformation` and `ObjectTypeInformation`.
const OBJECT_NAME_INFORMATION: i32 = 1;
const OBJECT_TYPE_INFORMATION: i32 = 2;
/// Access masks of handles that make `NtQueryObject` block (pipes, console, synchronous
/// files opened for waiting).
const SKIPPED_ACCESS: &[u32] = &[0x0012_019f, 0x0012_0189, 0x0010_0000, 0x001a_019f];

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "aiff", "aif", "m4a", "aac", "ogg", "wma", "alac",
];

#[repr(C)]
struct HandleEntry {
    object: usize,
    unique_process_id: usize,
    handle_value: usize,
    granted_access: u32,
    creator_back_trace_index: u16,
    object_type_index: u16,
    handle_attributes: u32,
    reserved: u32,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(
        class: i32,
        info: *mut core::ffi::c_void,
        len: u32,
        out_len: *mut u32,
    ) -> NTSTATUS;
    fn NtQueryObject(
        handle: HANDLE,
        class: i32,
        info: *mut core::ffi::c_void,
        len: u32,
        out_len: *mut u32,
    ) -> NTSTATUS;
}

/// The system's handle table as raw bytes: an 8-byte count, 8 reserved, then entries.
fn handle_table() -> anyhow::Result<Vec<u8>> {
    let mut size = 1usize << 24;
    loop {
        let mut buf = vec![0u8; size];
        let mut out = 0u32;
        // SAFETY: the buffer is valid for `size` bytes and the class is a documented one.
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_EXTENDED_HANDLE_INFORMATION,
                buf.as_mut_ptr().cast(),
                u32::try_from(size)?,
                &raw mut out,
            )
        };
        if status == 0 {
            return Ok(buf);
        }
        if status == STATUS_INFO_LENGTH_MISMATCH {
            size *= 2;
            continue;
        }
        anyhow::bail!("NtQuerySystemInformation failed: {status:#x}");
    }
}

/// The name of a duplicated handle when its type is `File`.
fn file_name(dup: HANDLE) -> Option<String> {
    let mut type_buf = vec![0u8; 1024];
    let mut len = 0u32;
    // SAFETY: buffers are valid for their lengths; the classes are documented.
    let status = unsafe {
        NtQueryObject(
            dup,
            OBJECT_TYPE_INFORMATION,
            type_buf.as_mut_ptr().cast(),
            1024,
            &raw mut len,
        )
    };
    if status != 0 {
        return None;
    }
    // OBJECT_TYPE_INFORMATION starts with a UNICODE_STRING: length, max, pad, buffer.
    let type_name = unicode_string_at(&type_buf)?;
    if type_name != "File" {
        return None;
    }
    let mut name_buf = vec![0u8; 8192];
    // SAFETY: as above.
    let status = unsafe {
        NtQueryObject(
            dup,
            OBJECT_NAME_INFORMATION,
            name_buf.as_mut_ptr().cast(),
            8192,
            &raw mut len,
        )
    };
    if status != 0 {
        return None;
    }
    unicode_string_at(&name_buf)
}

/// Reads the `UNICODE_STRING` at the start of a query buffer; its data pointer points into
/// the same buffer, which is still alive.
fn unicode_string_at(buf: &[u8]) -> Option<String> {
    let len = usize::from(u16::from_le_bytes([buf[0], buf[1]]));
    let ptr = u64::from_le_bytes(buf[8..16].try_into().ok()?) as usize;
    if len == 0 || ptr == 0 {
        return None;
    }
    // SAFETY: the pointer targets `len` bytes inside `buf`, which outlives this read.
    let wide = unsafe { std::slice::from_raw_parts(ptr as *const u16, len / 2) };
    Some(String::from_utf16_lossy(wide))
}

/// NT device paths (`\Device\HarddiskVolume3\Users\...`) become drive paths by matching the
/// device prefix of each drive letter.
fn nt_path_to_dos(nt: &str) -> PathBuf {
    use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;
    for letter in b'A'..=b'Z' {
        let drive: Vec<u16> = format!("{}:\0", letter as char).encode_utf16().collect();
        let mut target = vec![0u16; 512];
        // SAFETY: both buffers are valid for their lengths.
        let n = unsafe { QueryDosDeviceW(drive.as_ptr(), target.as_mut_ptr(), 512) };
        if n == 0 {
            continue;
        }
        let device =
            String::from_utf16_lossy(&target[..target.iter().position(|c| *c == 0).unwrap_or(0)]);
        if let Some(rest) = nt.strip_prefix(&device) {
            return PathBuf::from(format!("{}:{rest}", letter as char));
        }
    }
    PathBuf::from(nt)
}

/// The audio files the process with `pid` has open, in handle order (oldest first), each
/// once.
pub fn open_audio_files(pid: u32) -> anyhow::Result<Vec<PathBuf>> {
    // SAFETY: plain API calls with valid arguments; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, pid) };
    if process.is_null() {
        anyhow::bail!("cannot open pid {pid} for handle duplication");
    }
    let table = handle_table();
    let result = table.and_then(|table| {
        let count = usize::try_from(u64::from_le_bytes(table[..8].try_into()?))?;
        let entry_size = std::mem::size_of::<HandleEntry>();
        let entries = &table[16..];
        let me = unsafe { GetCurrentProcess() };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for i in 0..count.min(entries.len() / entry_size) {
            let slice = &entries[i * entry_size..(i + 1) * entry_size];
            // SAFETY: the slice has exactly the struct's size and the layout is repr(C).
            let entry: &HandleEntry = unsafe { &*slice.as_ptr().cast() };
            if entry.unique_process_id != pid as usize
                || SKIPPED_ACCESS.contains(&entry.granted_access)
            {
                continue;
            }
            let mut dup: HANDLE = std::ptr::null_mut();
            // SAFETY: valid source process and handle value; the duplicate is closed below.
            let ok = unsafe {
                DuplicateHandle(
                    process,
                    entry.handle_value as HANDLE,
                    me,
                    &raw mut dup,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if ok == 0 {
                continue;
            }
            let name = file_name(dup);
            // SAFETY: we own the duplicate.
            unsafe { CloseHandle(dup) };
            let Some(name) = name else { continue };
            let lower = name.to_lowercase();
            if AUDIO_EXTENSIONS
                .iter()
                .any(|ext| lower.ends_with(&format!(".{ext}")))
                && seen.insert(lower)
            {
                out.push(nt_path_to_dos(&name));
            }
        }
        Ok(out)
    });
    // SAFETY: we own the process handle.
    unsafe { CloseHandle(process) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_string_reads_length_and_pointer() {
        let text: Vec<u16> = "File".encode_utf16().collect();
        let mut buf = vec![0u8; 64];
        buf[0..2].copy_from_slice(&8u16.to_le_bytes());
        buf[8..16].copy_from_slice(&(text.as_ptr() as u64).to_le_bytes());
        assert_eq!(unicode_string_at(&buf).as_deref(), Some("File"));
        buf[0..2].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(unicode_string_at(&buf), None);
    }
}
