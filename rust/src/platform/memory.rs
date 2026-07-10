//! Cross-process memory reading + AOB pattern scanning of a module's `.text`
//! section. Port of the C# `Win32Api/Memory.cs`, with the handle leak fixed:
//! [`ProcessMemory`] owns its handle and closes it on `Drop`.

use std::io;
use std::thread::sleep;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};
use windows_sys::Win32::System::Threading::{
    IsWow64Process, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};

use crate::pattern::{find_pattern_in, parse_signature};

/// `.text\0\0\0` read as a little-endian i64.
const DOT_TEXT: i64 = 0x0074_7865_742E;

/// How a playback-clock value is encoded in memory. Different NetEase builds use
/// different types/units, so [`ProcessMemory::find_playback_clock`] tries them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockFormat {
    F64Sec,
    F64Ms,
    F32Sec,
    F32Ms,
    I32Ms,
}

const CLOCK_FORMATS: [ClockFormat; 5] = [
    ClockFormat::F64Sec,
    ClockFormat::F64Ms,
    ClockFormat::F32Sec,
    ClockFormat::F32Ms,
    ClockFormat::I32Ms,
];

/// Interpret the bytes at `buf[i..]` as `fmt`, returning the value in seconds.
fn extract_seconds(buf: &[u8], i: usize, fmt: ClockFormat) -> Option<f64> {
    match fmt {
        ClockFormat::F64Sec | ClockFormat::F64Ms => {
            let b = buf.get(i..i + 8)?;
            let v = f64::from_le_bytes(b.try_into().ok()?);
            if !v.is_finite() {
                return None;
            }
            Some(if fmt == ClockFormat::F64Ms { v / 1000.0 } else { v })
        }
        ClockFormat::F32Sec | ClockFormat::F32Ms => {
            let b = buf.get(i..i + 4)?;
            let v = f32::from_le_bytes(b.try_into().ok()?) as f64;
            if !v.is_finite() {
                return None;
            }
            Some(if fmt == ClockFormat::F32Ms { v / 1000.0 } else { v })
        }
        ClockFormat::I32Ms => {
            let b = buf.get(i..i + 4)?;
            let v = i32::from_le_bytes(b.try_into().ok()?) as f64;
            Some(v / 1000.0)
        }
    }
}

/// A readable handle to another process. Closes the handle on drop.
pub struct ProcessMemory {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl ProcessMemory {
    pub fn open(pid: u32) -> io::Result<Self> {
        // SAFETY: FFI call; we check the returned handle before using it.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    pub fn read_bytes(&self, addr: usize, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        let mut read: usize = 0;
        // SAFETY: buf is `len` bytes; the kernel writes at most `len` and reports
        // the count in `read`.
        let ok = unsafe {
            ReadProcessMemory(
                self.handle,
                addr as *const core::ffi::c_void,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                len,
                &mut read as *mut usize,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(read);
        Ok(buf)
    }

    fn read_arr<const N: usize>(&self, addr: usize) -> io::Result<[u8; N]> {
        let v = self.read_bytes(addr, N)?;
        if v.len() < N {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read"));
        }
        let mut a = [0u8; N];
        a.copy_from_slice(&v[..N]);
        Ok(a)
    }

    pub fn read_i16(&self, addr: usize) -> io::Result<i16> {
        Ok(i16::from_le_bytes(self.read_arr::<2>(addr)?))
    }
    pub fn read_i32(&self, addr: usize) -> io::Result<i32> {
        Ok(i32::from_le_bytes(self.read_arr::<4>(addr)?))
    }
    pub fn read_u32(&self, addr: usize) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.read_arr::<4>(addr)?))
    }
    pub fn read_i64(&self, addr: usize) -> io::Result<i64> {
        Ok(i64::from_le_bytes(self.read_arr::<8>(addr)?))
    }
    pub fn read_f32(&self, addr: usize) -> io::Result<f32> {
        Ok(f32::from_le_bytes(self.read_arr::<4>(addr)?))
    }
    pub fn read_f64(&self, addr: usize) -> io::Result<f64> {
        Ok(f64::from_le_bytes(self.read_arr::<8>(addr)?))
    }

    /// Is the target a 32-bit process running under WOW64? `Some(true)` means
    /// 32-bit; `Some(false)` means native (64-bit on a 64-bit OS); `None` if the
    /// query failed. This matters because the AOB patterns are bitness-specific.
    pub fn is_wow64(&self) -> Option<bool> {
        let mut wow64: i32 = 0;
        // SAFETY: FFI; handle is valid, we own the out param.
        let ok = unsafe { IsWow64Process(self.handle, &mut wow64) };
        if ok == 0 {
            return None;
        }
        Some(wow64 != 0)
    }

    /// Best-effort read of a whole region into one buffer; unreadable chunks are
    /// left as zeros (so they simply won't match anything in a scan).
    pub fn read_region_best_effort(&self, base: usize, size: usize) -> Vec<u8> {
        let mut buf = vec![0u8; size];
        let chunk = 0x40000usize; // 256 KiB
        let mut off = 0usize;
        while off < size {
            let len = chunk.min(size - off);
            if let Ok(bytes) = self.read_bytes(base + off, len) {
                let n = bytes.len().min(len);
                buf[off..off + n].copy_from_slice(&bytes[..n]);
            }
            off += len;
        }
        buf
    }

    /// Read a clock value, in seconds, under a given numeric format.
    pub fn read_clock_seconds(&self, addr: usize, fmt: ClockFormat) -> Option<f64> {
        match fmt {
            ClockFormat::F64Sec => self.read_f64(addr).ok(),
            ClockFormat::F64Ms => self.read_f64(addr).ok().map(|v| v / 1000.0),
            ClockFormat::F32Sec => self.read_f32(addr).ok().map(|v| v as f64),
            ClockFormat::F32Ms => self.read_f32(addr).ok().map(|v| v as f64 / 1000.0),
            ClockFormat::I32Ms => self.read_i32(addr).ok().map(|v| v as f64 / 1000.0),
        }
    }

    /// Enumerate committed, readable+writable regions of the target's address
    /// space (where a mutable playback clock would live), as `(base, size)`.
    fn enumerate_writable_regions(&self) -> Vec<(usize, usize)> {
        use windows_sys::Win32::System::Memory::{
            VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READWRITE,
            PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS, PAGE_READWRITE, PAGE_WRITECOPY,
        };

        let mut regions = Vec::new();
        let mut addr: usize = 0;
        let max_addr: usize = 0xFFFF_FFFF; // 32-bit user space (large-address-aware)

        loop {
            let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { core::mem::zeroed() };
            let ret = unsafe {
                VirtualQueryEx(
                    self.handle,
                    addr as *const core::ffi::c_void,
                    &mut mbi,
                    core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if ret == 0 {
                break;
            }

            let region_base = mbi.BaseAddress as usize;
            let region_size = mbi.RegionSize;
            if region_size == 0 {
                break;
            }

            let writable = mbi.Protect
                & (PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY)
                != 0;
            let inaccessible = mbi.Protect & (PAGE_GUARD | PAGE_NOACCESS) != 0;
            if mbi.State == MEM_COMMIT && writable && !inaccessible {
                regions.push((region_base, region_size));
            }

            match region_base.checked_add(region_size) {
                Some(next) if next > addr && next <= max_addr => addr = next,
                _ => break,
            }
        }

        regions
    }

    /// Auto-discover the playback-position "clock" without any version-specific
    /// offset: a value that, read as some common numeric format, sits within
    /// `[0, duration]` seconds and advances ~1.0 per real second. Scans **all**
    /// writable process memory (the clock lives on the heap, not in the module),
    /// trying `double`/`float`/`int32` in seconds and milliseconds. Requires the
    /// song to be **playing** during the ~5s scan; returns `None` if nothing fits.
    pub fn find_playback_clock(&self, duration: f64) -> Option<(usize, ClockFormat)> {
        const CAP: usize = 512 * 1024 * 1024; // bound memory/time for the scan
        let dur_max = if duration > 0.0 { duration + 2.0 } else { 100_000.0 };

        let regions = self.enumerate_writable_regions();
        let total: usize = regions.iter().map(|&(_, s)| s).sum();
        crate::diag!(
            "[clock] {} writable region(s), {} MiB committed; scanning up to {} MiB (a few seconds)",
            regions.len(),
            total >> 20,
            CAP >> 20
        );

        // Snapshot A of each region (bounded by CAP).
        let mut snaps: Vec<(usize, Vec<u8>)> = Vec::new();
        let mut used = 0usize;
        for (rbase, rsize) in regions {
            if used >= CAP {
                crate::diag!("[clock] reached {} MiB cap; higher memory not scanned", CAP >> 20);
                break;
            }
            let sz = rsize.min(CAP - used);
            snaps.push((rbase, self.read_region_best_effort(rbase, sz)));
            used += sz;
        }

        let t0 = Instant::now();
        sleep(Duration::from_millis(1200));
        let dt = t0.elapsed().as_secs_f64();

        let advances = |sa: f64, sb: f64, dt: f64| {
            sa >= 0.0 && sa <= dur_max && (sb - sa) > 0.0 && ((sb - sa) - dt).abs() < dt * 0.4
        };

        let mut candidates: Vec<(usize, ClockFormat)> = Vec::new();
        let mut per_format = [0usize; CLOCK_FORMATS.len()];
        for (rbase, a) in &snaps {
            let b = self.read_region_best_effort(*rbase, a.len());
            let limit = a.len().min(b.len());
            let mut i = 0usize;
            while i + 4 <= limit {
                for (fi, &fmt) in CLOCK_FORMATS.iter().enumerate() {
                    if let (Some(sa), Some(sb)) =
                        (extract_seconds(a, i, fmt), extract_seconds(&b, i, fmt))
                    {
                        if advances(sa, sb, dt) {
                            candidates.push((rbase + i, fmt));
                            per_format[fi] += 1;
                        }
                    }
                }
                i += 4;
            }
        }
        drop(snaps);

        for (fi, &fmt) in CLOCK_FORMATS.iter().enumerate() {
            if per_format[fi] > 0 {
                crate::diag!("[clock] round 1: {} candidate(s) as {fmt:?}", per_format[fi]);
            }
        }
        crate::diag!("[clock] round 1: {} candidate(s) total (dt={dt:.2}s)", candidates.len());
        if candidates.is_empty() {
            return None;
        }

        // Disambiguate with a second timed round; keep the best-tracking survivor.
        let before: Vec<f64> = candidates
            .iter()
            .map(|&(addr, fmt)| self.read_clock_seconds(addr, fmt).unwrap_or(f64::NAN))
            .collect();
        let t1 = Instant::now();
        sleep(Duration::from_millis(1200));
        let dt2 = t1.elapsed().as_secs_f64();

        let mut survivors: Vec<(usize, ClockFormat, f64, f64)> = Vec::new(); // addr, fmt, value, rate error
        for (idx, &(addr, fmt)) in candidates.iter().enumerate() {
            let after = self.read_clock_seconds(addr, fmt).unwrap_or(f64::NAN);
            let bef = before[idx];
            if bef.is_finite() && after.is_finite() && advances(bef, after, dt2) {
                survivors.push((addr, fmt, after, ((after - bef) - dt2).abs()));
            }
        }
        crate::diag!("[clock] round 2: {} survivor(s)", survivors.len());
        for &(addr, fmt, value, _) in survivors.iter().take(8) {
            crate::diag!("[clock]   0x{addr:X} {fmt:?} = {value:.2}s");
        }

        // Best = the one whose rate tracks real time most precisely.
        survivors
            .iter()
            .min_by(|a, b| a.3.total_cmp(&b.3))
            .map(|&(addr, fmt, _, _)| {
                crate::diag!("[clock] using 0x{addr:X} as {fmt:?}");
                (addr, fmt)
            })
    }

    /// Scan the `.text` section of the module based at `module_base` for an AOB
    /// signature, returning the absolute address of the match.
    ///
    /// The PE headers are read *through* the target process (as the C# does),
    /// so `module_base` is the remote module base address.
    pub fn find_pattern(&self, signature: &str, module_base: usize) -> io::Result<Option<usize>> {
        let nt_offset = self.read_i32(module_base + 0x3C)? as usize;
        let nt_header = module_base + nt_offset;
        let file_header = nt_header + 4;

        // Machine field of IMAGE_FILE_HEADER: 0x8664 = x64, 0x14C = x86.
        let machine = self.read_i16(file_header)? as u16;
        let sections = self.read_i16(nt_header + 6)? as usize; // NumberOfSections
        let opt_size = self.read_i16(file_header + 16)? as usize; // SizeOfOptionalHeader
        let opt_header = file_header + 20;
        let mut cursor = opt_header + opt_size; // first section header

        crate::diag!(
            "[scan] machine=0x{machine:X} ({}) sections={sections} opt_hdr=0x{opt_size:X}",
            machine_name(machine)
        );

        for i in 0..sections {
            let name = self.read_i64(cursor)?;
            let virt_size = self.read_i32(cursor + 8)? as usize; // VirtualSize
            let virt_addr = self.read_i32(cursor + 12)? as usize; // VirtualAddress (RVA)
            crate::diag!(
                "[scan] section[{i}] {:?} rva=0x{virt_addr:X} vsize=0x{virt_size:X}",
                section_name(name)
            );

            if name == DOT_TEXT {
                let start = module_base + virt_addr;
                let block = self.read_bytes(start, virt_size)?;
                let pat = parse_signature(signature);
                let found = find_pattern_in(&block, &pat).map(|off| start + off);
                crate::diag!(
                    "[scan] .text @0x{start:X} read {} of 0x{virt_size:X} bytes; match={found:X?}",
                    block.len()
                );
                return Ok(found);
            }
            cursor += 40; // sizeof(IMAGE_SECTION_HEADER)
        }

        crate::diag!("[scan] no .text section found among {sections} sections");
        Ok(None)
    }
}

impl Drop for ProcessMemory {
    fn drop(&mut self) {
        // SAFETY: handle was produced by OpenProcess and is only closed here.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Read a remote MSVC `std::string` from a **64-bit** process (NetEase).
pub fn read_std_string_x64(mem: &ProcessMemory, base: usize) -> Option<String> {
    let len = mem.read_i64(base + 0x10).ok()?;
    if len <= 0 {
        return Some(String::new());
    }
    let len = len as usize;
    let bytes = if len <= 15 {
        mem.read_bytes(base, len).ok()?
    } else {
        let ptr = mem.read_i64(base).ok()? as usize;
        mem.read_bytes(ptr, len).ok()?
    };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Read a remote MSVC `std::string` from a **32-bit** process (QQ Music).
pub fn read_std_string_x86(mem: &ProcessMemory, base: usize) -> Option<String> {
    let len = mem.read_i32(base + 0x10).ok()?;
    if len <= 0 {
        return Some(String::new());
    }
    let len = len as usize;
    let bytes = if len <= 15 {
        mem.read_bytes(base, len).ok()?
    } else {
        let ptr = mem.read_u32(base).ok()? as usize;
        mem.read_bytes(ptr, len).ok()?
    };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Locate a loaded module by name (case-insensitive) and return its base address
/// and image size.
pub fn module_base_size(pid: u32, module_name: &str) -> io::Result<(usize, usize)> {
    // SAFETY: FFI; snapshot is validated and always closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    let mut entry: MODULEENTRY32W = unsafe { core::mem::zeroed() };
    entry.dwSize = core::mem::size_of::<MODULEENTRY32W>() as u32;

    let target = module_name.to_ascii_lowercase();
    let mut result = None;

    let mut ok = unsafe { Module32FirstW(snapshot, &mut entry) };
    while ok != 0 {
        let name = u16_buf_to_string(&entry.szModule);
        if name.to_ascii_lowercase() == target {
            result = Some((entry.modBaseAddr as usize, entry.modBaseSize as usize));
            break;
        }
        ok = unsafe { Module32NextW(snapshot, &mut entry) };
    }

    unsafe {
        CloseHandle(snapshot);
    }

    result.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, format!("module {module_name} not found"))
    })
}

fn u16_buf_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Decode an 8-byte PE section name (read as a little-endian i64) to text.
fn section_name(name_le: i64) -> String {
    let bytes = name_le.to_le_bytes();
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn machine_name(machine: u16) -> &'static str {
    match machine {
        0x8664 => "x64",
        0x014C => "x86",
        0xAA64 => "arm64",
        _ => "unknown",
    }
}
