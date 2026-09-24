//! Read-only access to another process on Windows: open by name, find the main module,
//! read the file version, enumerate memory regions, read bytes. Nothing here writes.
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use anyhow::{Context, bail};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, PROCESSENTRY32W, Process32FirstW,
    Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_IMAGE, MEM_MAPPED, MEM_PRIVATE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ,
    PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE,
    PAGE_WRITECOPY, VirtualQueryEx,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};

/// Regions are read in chunks of this size so one unreadable page loses little.
pub const READ_CHUNK_BYTES: usize = 16 * 1024 * 1024;

/// Largest region the scanners will read in full; the rest of a bigger one is skipped.
pub const MAX_REGION_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// Part of a loaded executable or DLL image.
    Image,
    /// Heap, stacks, anything the process allocated.
    Private,
    /// File mappings.
    Mapped,
}

#[derive(Debug, Clone, Copy)]
pub struct Region {
    pub base: u64,
    pub size: u64,
    pub kind: RegionKind,
    pub writable: bool,
}

impl Region {
    pub fn end(&self) -> u64 {
        self.base + self.size
    }

    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr < self.end()
    }
}

pub struct Process {
    handle: HANDLE,
    pub pid: u32,
    pub name: String,
    pub exe_path: PathBuf,
    /// Base address of the main module (`rekordbox.exe`).
    pub module_base: u64,
    pub module_size: u64,
}

// The handle is only used for reads and is closed once; it is safe to move between threads.
unsafe impl Send for Process {}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed exactly once here.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..end])
        .to_string_lossy()
        .into_owned()
}

/// PID of the first process whose executable name matches (case-insensitive).
pub fn find_pid(exe_name: &str) -> Option<u32> {
    // SAFETY: plain Toolhelp calls with correctly sized structs; the snapshot handle is closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        if Process32FirstW(snap, &raw mut entry) != 0 {
            loop {
                if from_wide(&entry.szExeFile).eq_ignore_ascii_case(exe_name) {
                    found = Some(entry.th32ProcessID);
                    break;
                }
                if Process32NextW(snap, &raw mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        found
    }
}

impl Process {
    /// Opens `exe_name` (for example `rekordbox.exe`) for reading. Fails when it is not
    /// running or when this process may not read it (an elevated target, for instance).
    pub fn open(exe_name: &str) -> anyhow::Result<Self> {
        let pid = find_pid(exe_name).with_context(|| format!("{exe_name} is not running"))?;
        // SAFETY: OpenProcess with read-only rights; the handle is owned by `Process`.
        let handle = unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid) };
        if handle.is_null() {
            bail!(
                "cannot open {exe_name} (pid {pid}) for reading: {}",
                last_error()
            );
        }
        let mut process = Self {
            handle,
            pid,
            name: exe_name.to_string(),
            exe_path: PathBuf::new(),
            module_base: 0,
            module_size: 0,
        };
        process.locate_main_module()?;
        Ok(process)
    }

    fn locate_main_module(&mut self) -> anyhow::Result<()> {
        // Module snapshots fail with ERROR_BAD_LENGTH while the target is loading; retry.
        let mut last = String::new();
        for _ in 0..20 {
            // SAFETY: Toolhelp module snapshot for our pid; struct sized before the call.
            unsafe {
                let snap =
                    CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, self.pid);
                if snap == INVALID_HANDLE_VALUE {
                    last = last_error();
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                let mut entry: MODULEENTRY32W = std::mem::zeroed();
                entry.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
                let ok = Module32FirstW(snap, &raw mut entry) != 0;
                CloseHandle(snap);
                if ok {
                    self.module_base = entry.modBaseAddr as u64;
                    self.module_size = u64::from(entry.modBaseSize);
                    self.exe_path = PathBuf::from(from_wide(&entry.szExePath));
                    return Ok(());
                }
                last = last_error();
            }
        }
        bail!("cannot enumerate modules of pid {}: {last}", self.pid)
    }

    /// `major.minor.patch` from the executable's version resource (`7.2.18`), or the four
    /// numbers when the build field is not zero.
    pub fn file_version(&self) -> Option<String> {
        let exe_wide = wide(&self.exe_path.to_string_lossy());
        // SAFETY: standard version-info calls with a buffer sized by the first call; the
        // pointer VerQueryValueW returns points into that buffer.
        unsafe {
            let mut handle = 0u32;
            let size = GetFileVersionInfoSizeW(exe_wide.as_ptr(), &raw mut handle);
            if size == 0 {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            if GetFileVersionInfoW(exe_wide.as_ptr(), 0, size, buf.as_mut_ptr().cast()) == 0 {
                return None;
            }
            let root = wide("\\");
            let mut info: *mut VS_FIXEDFILEINFO = std::ptr::null_mut();
            let mut len = 0u32;
            if VerQueryValueW(
                buf.as_ptr().cast(),
                root.as_ptr(),
                (&raw mut info).cast(),
                &raw mut len,
            ) == 0
                || info.is_null()
            {
                return None;
            }
            let ms = (*info).dwFileVersionMS;
            let ls = (*info).dwFileVersionLS;
            let (major, minor, patch, build_no) = (ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff);
            Some(if build_no == 0 {
                format!("{major}.{minor}.{patch}")
            } else {
                format!("{major}.{minor}.{patch}.{build_no}")
            })
        }
    }

    pub fn is_in_module(&self, addr: u64) -> bool {
        addr >= self.module_base && addr < self.module_base + self.module_size
    }

    /// Reads exactly `buf.len()` bytes or fails.
    pub fn read_exact(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let mut got = 0usize;
        // SAFETY: the destination is a valid, writable slice of the stated length.
        let ok = unsafe {
            ReadProcessMemory(
                self.handle,
                addr as *const core::ffi::c_void,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &raw mut got,
            )
        };
        if ok == 0 || got != buf.len() {
            bail!(
                "read of {} bytes at {addr:#x} failed: {}",
                buf.len(),
                last_error()
            );
        }
        Ok(())
    }

    /// Reads as much of the region as the OS allows, chunk by chunk (a page may have
    /// vanished since the enumeration). `None` when nothing could be read.
    pub fn read_region(&self, region: &Region) -> Option<Vec<u8>> {
        let len = usize::try_from(region.size).ok()?.min(MAX_REGION_BYTES);
        let mut buf = vec![0u8; len];
        let mut done = 0usize;
        while done < len {
            let chunk = (len - done).min(READ_CHUNK_BYTES);
            let mut got = 0usize;
            // SAFETY: the destination slice is valid for `chunk` bytes.
            let ok = unsafe {
                ReadProcessMemory(
                    self.handle,
                    (region.base + done as u64) as *const core::ffi::c_void,
                    buf.as_mut_ptr().add(done).cast(),
                    chunk,
                    &raw mut got,
                )
            };
            if ok == 0 && got == 0 {
                break;
            }
            done += got;
            if got < chunk {
                break;
            }
        }
        if done == 0 {
            return None;
        }
        buf.truncate(done);
        Some(buf)
    }

    pub fn read_u64(&self, addr: u64) -> anyhow::Result<u64> {
        let mut b = [0u8; 8];
        self.read_exact(addr, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    pub fn read_i64(&self, addr: u64) -> anyhow::Result<i64> {
        let mut b = [0u8; 8];
        self.read_exact(addr, &mut b)?;
        Ok(i64::from_le_bytes(b))
    }

    pub fn read_f32(&self, addr: u64) -> anyhow::Result<f32> {
        let mut b = [0u8; 4];
        self.read_exact(addr, &mut b)?;
        Ok(f32::from_le_bytes(b))
    }

    pub fn read_f64(&self, addr: u64) -> anyhow::Result<f64> {
        let mut b = [0u8; 8];
        self.read_exact(addr, &mut b)?;
        Ok(f64::from_le_bytes(b))
    }

    pub fn read_u8(&self, addr: u64) -> anyhow::Result<u8> {
        let mut b = [0u8; 1];
        self.read_exact(addr, &mut b)?;
        Ok(b[0])
    }

    /// Committed, readable, non-guard regions of the whole address space.
    pub fn regions(&self) -> Vec<Region> {
        let mut out = Vec::new();
        let mut addr: u64 = 0;
        loop {
            // SAFETY: VirtualQueryEx fills a struct of the size we pass.
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            let got = unsafe {
                VirtualQueryEx(
                    self.handle,
                    addr as *const core::ffi::c_void,
                    &raw mut info,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if got == 0 {
                break;
            }
            let base = info.BaseAddress as u64;
            let size = info.RegionSize as u64;
            let protect = info.Protect;
            let readable = matches!(
                protect & 0xff,
                PAGE_READONLY
                    | PAGE_READWRITE
                    | PAGE_WRITECOPY
                    | PAGE_EXECUTE_READ
                    | PAGE_EXECUTE_READWRITE
                    | PAGE_EXECUTE_WRITECOPY
            );
            if info.State == MEM_COMMIT && readable && protect & PAGE_GUARD == 0 {
                let kind = match info.Type {
                    MEM_IMAGE => RegionKind::Image,
                    MEM_MAPPED => RegionKind::Mapped,
                    _ if info.Type == MEM_PRIVATE => RegionKind::Private,
                    _ => RegionKind::Private,
                };
                let writable = matches!(
                    protect & 0xff,
                    PAGE_READWRITE
                        | PAGE_WRITECOPY
                        | PAGE_EXECUTE_READWRITE
                        | PAGE_EXECUTE_WRITECOPY
                );
                out.push(Region {
                    base,
                    size,
                    kind,
                    writable,
                });
            }
            let Some(next) = base.checked_add(size) else {
                break;
            };
            if next >= 0x7FFF_FFFF_FFFF {
                break;
            }
            addr = next;
        }
        out
    }

    /// Writable regions inside the main module: where static pointer roots live.
    pub fn static_regions(&self) -> Vec<Region> {
        self.regions()
            .into_iter()
            .filter(|r| r.kind == RegionKind::Image && r.writable && self.is_in_module(r.base))
            .collect()
    }
}

fn last_error() -> String {
    std::io::Error::last_os_error().to_string()
}
