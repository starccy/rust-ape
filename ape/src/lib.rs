//! Safe wrappers around the Cosmopolitan runtime interfaces.
//!
//! These symbols only exist in binaries linked by cosmocc, so this crate is
//! meant for projects built with `cargo xtask build`.

use std::ffi::CStr;
use std::ffi::{c_char, c_int, c_long, c_ulong};

// ------------------------------- host platform -------------------------------

/// The host operating system, mirroring cosmo's `__hostos` bitmask (libc/dce.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    /// Bare metal, booted straight from BIOS/UEFI with no OS under us.
    Metal,
    Windows,
    /// macOS and friends.
    Xnu,
    Openbsd,
    Freebsd,
    Netbsd,
    Unknown,
}

// _HOST* bit values from libc/dce.h.
const HOST_LINUX: c_int = 1;
const HOST_METAL: c_int = 2;
const HOST_WINDOWS: c_int = 4;
const HOST_XNU: c_int = 8;
const HOST_OPENBSD: c_int = 16;
const HOST_FREEBSD: c_int = 32;
const HOST_NETBSD: c_int = 64;

unsafe extern "C" {
    // Read-only global the APE loader fills in for the real host at startup.
    #[link_name = "__hostos"]
    static HOSTOS: c_int;

    fn IsQemuUser() -> c_int;
}

/// The host we're currently running on, read from cosmo's `__hostos`.
pub fn current_os() -> Os {
    let h = unsafe { HOSTOS };
    // Normally exactly one bit is set, so compare directly and fall back to a
    // bitwise test.
    match h {
        HOST_LINUX => Os::Linux,
        HOST_METAL => Os::Metal,
        HOST_WINDOWS => Os::Windows,
        HOST_XNU => Os::Xnu,
        HOST_OPENBSD => Os::Openbsd,
        HOST_FREEBSD => Os::Freebsd,
        HOST_NETBSD => Os::Netbsd,
        _ if h & HOST_WINDOWS != 0 => Os::Windows,
        _ if h & HOST_XNU != 0 => Os::Xnu,
        _ if h & HOST_LINUX != 0 => Os::Linux,
        _ => Os::Unknown,
    }
}

pub fn is_linux() -> bool {
    current_os() == Os::Linux
}

pub fn is_windows() -> bool {
    current_os() == Os::Windows
}

/// macOS, on either Intel or Apple Silicon.
pub fn is_xnu() -> bool {
    current_os() == Os::Xnu
}

pub fn is_bsd() -> bool {
    matches!(current_os(), Os::Openbsd | Os::Freebsd | Os::Netbsd)
}

pub fn is_metal() -> bool {
    current_os() == Os::Metal
}

/// Whether we're running under qemu-user rather than on real hardware.
pub fn is_qemu_user() -> bool {
    unsafe { IsQemuUser() != 0 }
}

// -------------------------------- system info --------------------------------

unsafe extern "C" {
    fn __get_cpu_count() -> c_int;
    fn __get_phys_pages() -> c_long;
    fn __get_avphys_pages() -> c_long;
    fn GetProgramExecutableName() -> *mut c_char;
    fn GetInterpreterExecutableName(buf: *mut c_char, size: usize) -> *mut c_char;
    fn __get_tmpdir() -> *mut c_char;
    fn getauxval(kind: c_ulong) -> c_ulong;
    fn getpagesize() -> c_int;
}

// GetCpuid* and __cpu_march are built on the x86 CPUID instruction; the symbols
// don't exist on aarch64.
#[cfg(target_arch = "x86_64")]
unsafe extern "C" {
    fn GetCpuidBrand(out: *mut c_char, leaf: u32);
    fn GetCpuidOs() -> *const c_char;
    fn GetCpuidEmulator() -> *const c_char;
    fn __cpu_march(id: std::ffi::c_uint) -> *const c_char;
}

/// Turns a C string from cosmo into a String; null becomes None.
unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

/// Same, for the CPUID registers, which hold leftover control-character junk
/// when there's nothing to report rather than an empty string.
unsafe fn cstr_printable(p: *const c_char) -> Option<String> {
    let s = unsafe { cstr(p) }?;
    let s = s.trim_matches(|c: char| !c.is_ascii_graphic() && c != ' ');
    (!s.is_empty()).then(|| s.to_owned())
}

/// Number of CPUs online.
pub fn cpu_count() -> i32 {
    unsafe { __get_cpu_count() }
}

/// Total physical memory, in pages.
pub fn phys_pages() -> i64 {
    unsafe { __get_phys_pages() as i64 }
}

/// Physical memory currently available, in pages.
pub fn avail_phys_pages() -> i64 {
    unsafe { __get_avphys_pages() as i64 }
}

/// Page size in bytes.
pub fn page_size() -> i64 {
    unsafe { getpagesize() as i64 }
}

/// Total physical memory in bytes.
pub fn total_ram_bytes() -> i64 {
    phys_pages().saturating_mul(page_size())
}

/// Available physical memory in bytes.
pub fn avail_ram_bytes() -> i64 {
    avail_phys_pages().saturating_mul(page_size())
}

/// Path to our own executable, resolved per host and normalized on Windows.
/// Prefer this over `std::env::current_exe`, which under APE hands back the
/// loader (a ~9KB stub) instead of the program that's running.
pub fn program_executable_name() -> Option<String> {
    unsafe { cstr(GetProgramExecutableName()) }
}

/// Path to the APE loader, when the program was started through one.
pub fn interpreter_executable_name() -> Option<String> {
    let mut buf = [0 as c_char; 1024];
    unsafe {
        let p = GetInterpreterExecutableName(buf.as_mut_ptr(), buf.len());
        if p.is_null() { None } else { cstr(p) }
    }
}

/// Temp directory, resolved from TMPDIR, TEMP and friends depending on host.
pub fn tmpdir() -> Option<String> {
    unsafe { cstr(__get_tmpdir()) }
}

/// One entry of the ELF auxiliary vector, e.g. `AT_HWCAP`. cosmo presents this
/// uniformly across hosts.
pub fn auxval(kind: u64) -> u64 {
    unsafe { getauxval(kind as c_ulong) as u64 }
}

/// CPU vendor string from CPUID leaf 0, e.g. "GenuineIntel". x86_64 only.
#[cfg(target_arch = "x86_64")]
pub fn cpuid_vendor() -> String {
    // cosmo's GetCpuidBrand(buf, 0) writes 12 bytes as EBX,ECX,EDX, but leaf 0
    // spells the vendor EBX,EDX,ECX. Swap the last two words to get it right.
    let mut raw = [0u8; 13];
    unsafe { GetCpuidBrand(raw.as_mut_ptr() as *mut c_char, 0) };
    let mut v = [0u8; 12];
    v[0..4].copy_from_slice(&raw[0..4]); // EBX
    v[4..8].copy_from_slice(&raw[8..12]); // EDX
    v[8..12].copy_from_slice(&raw[4..8]); // ECX
    String::from_utf8_lossy(&v).trim_end_matches('\0').to_string()
}

/// Host OS as reported through CPUID, which some hypervisors disclose; None on
/// bare hardware. x86_64 only.
#[cfg(target_arch = "x86_64")]
pub fn cpuid_os() -> Option<String> {
    unsafe { cstr_printable(GetCpuidOs()) }
}

/// Name of the detected emulator or hypervisor (blink, qemu, kvm, ...); None on
/// bare hardware. x86_64 only.
#[cfg(target_arch = "x86_64")]
pub fn cpuid_emulator() -> Option<String> {
    unsafe { cstr_printable(GetCpuidEmulator()) }
}

/// Maps an x86 microarchitecture id (`X86_MARCH_*`) to a name, e.g. 6 is
/// "haswell". x86_64 only.
#[cfg(target_arch = "x86_64")]
pub fn cpu_march_name(id: u32) -> Option<String> {
    unsafe { cstr(__cpu_march(id as std::ffi::c_uint)) }
}
