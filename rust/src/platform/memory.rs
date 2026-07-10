//! Cross-process memory reading + AOB pattern scanning of a module's `.text`
//! section. Port of the C# `Win32Api/Memory.cs`, with the handle leak fixed:
//! [`ProcessMemory`] owns its handle and closes it on `Drop`.

use std::io;

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
    pub fn read_i64(&self, addr: usize) -> io::Result<i64> {
        Ok(i64::from_le_bytes(self.read_arr::<8>(addr)?))
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
