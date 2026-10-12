//! The `windows` crate, unmodified, talking COM to the host: `cargo xtask
//! build` compiles the windows-* crates as Windows code, so their
//! `extern "system"` is Microsoft's convention and their `link!`
//! declarations look the DLL up at run time (see xtask/src/host/rustcwrap/).
//! COM then needs nothing more: a vtable call is an ordinary indirect call,
//! and an object we implement is called by the host through the same kind
//! of vtable. What's checked here:
//!
//! - `CoCreateInstance` and vtable calls on the host's objects, with
//!   integer, pointer and `f32` arguments and an `f32` result
//! - an `IAudioEndpointVolumeCallback` implemented with
//!   `#[windows::core::implement]`, which the audio service calls from its
//!   own thread when the volume changes
//! - interfaces declared in this crate with `#[windows::core::interface]`:
//!   the host's task allocator called through one, and a malloc spy the
//!   host calls on every `CoTaskMemAlloc`. The macro writes the vtables'
//!   `extern "system"` into this crate, which isn't Windows code, so the
//!   macro itself is what gets compiled differently (see xtask/src/host/rustcwrap/)
//! - the crate's error type, whose message comes from `FormatMessageW`,
//!   and its string types over the host's wide strings
//!
//! The enumeration part needs no audio device; the volume part is skipped
//! without one. Windows only; elsewhere the example says so and exits.

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::core::{
    BOOL, Error, HRESULT, IUnknown, IUnknown_Vtbl, Interface, implement, interface,
};

static NOTIFIED: AtomicUsize = AtomicUsize::new(0);
static NOTIFY_THREADS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

// the host calls OnNotify on a thread of the audio service, holding a
// pointer to its own record of the change
#[windows::core::implement(IAudioEndpointVolumeCallback)]
struct VolumeWatcher;

impl IAudioEndpointVolumeCallback_Impl for VolumeWatcher_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        let data = unsafe { &*data };
        let tid = unsafe { GetCurrentThreadId() };
        println!(
            "  OnNotify on thread {tid}: volume {:.2}, {} channels, muted {}",
            data.fMasterVolume,
            data.nChannels,
            data.bMuted.as_bool()
        );
        NOTIFY_THREADS.lock().unwrap().push(tid);
        NOTIFIED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// IMalloc and IMallocSpy as this crate declares them
#[interface("00000002-0000-0000-c000-000000000046")]
unsafe trait IHostMalloc: IUnknown {
    fn Alloc(&self, cb: usize) -> *mut c_void;
    fn Realloc(&self, pv: *const c_void, cb: usize) -> *mut c_void;
    fn Free(&self, pv: *const c_void);
    fn GetSize(&self, pv: *const c_void) -> usize;
    fn DidAlloc(&self, pv: *const c_void) -> i32;
    fn HeapMinimize(&self);
}

#[interface("0000001d-0000-0000-c000-000000000046")]
unsafe trait IAllocSpy: IUnknown {
    fn PreAlloc(&self, cb: usize) -> usize;
    fn PostAlloc(&self, p: *mut c_void) -> *mut c_void;
    fn PreFree(&self, p: *mut c_void, spyed: BOOL) -> *mut c_void;
    fn PostFree(&self, spyed: BOOL);
    fn PreRealloc(&self, p: *mut c_void, cb: usize, newp: *mut *mut c_void, spyed: BOOL) -> usize;
    fn PostRealloc(&self, p: *mut c_void, spyed: BOOL) -> *mut c_void;
    fn PreGetSize(&self, p: *mut c_void, spyed: BOOL) -> *mut c_void;
    fn PostGetSize(&self, actual: usize, spyed: BOOL) -> usize;
    fn PreDidAlloc(&self, p: *mut c_void, spyed: BOOL) -> *mut c_void;
    fn PostDidAlloc(&self, p: *mut c_void, spyed: BOOL, actual: i32) -> i32;
    fn PreHeapMinimize(&self);
    fn PostHeapMinimize(&self);
}

static SPY_ALLOCS: AtomicUsize = AtomicUsize::new(0);
static SPY_FREES: AtomicUsize = AtomicUsize::new(0);
static SPY_SAW_4096: AtomicUsize = AtomicUsize::new(0);

#[implement(IAllocSpy)]
struct Spy;

impl IAllocSpy_Impl for Spy_Impl {
    unsafe fn PreAlloc(&self, cb: usize) -> usize {
        SPY_ALLOCS.fetch_add(1, Ordering::SeqCst);
        if cb == 4096 {
            SPY_SAW_4096.fetch_add(1, Ordering::SeqCst);
        }
        cb
    }
    unsafe fn PostAlloc(&self, p: *mut c_void) -> *mut c_void {
        p
    }
    unsafe fn PreFree(&self, p: *mut c_void, _spyed: BOOL) -> *mut c_void {
        SPY_FREES.fetch_add(1, Ordering::SeqCst);
        p
    }
    unsafe fn PostFree(&self, _spyed: BOOL) {}
    unsafe fn PreRealloc(
        &self,
        p: *mut c_void,
        cb: usize,
        newp: *mut *mut c_void,
        _spyed: BOOL,
    ) -> usize {
        unsafe { *newp = p };
        cb
    }
    unsafe fn PostRealloc(&self, p: *mut c_void, _spyed: BOOL) -> *mut c_void {
        p
    }
    unsafe fn PreGetSize(&self, p: *mut c_void, _spyed: BOOL) -> *mut c_void {
        p
    }
    unsafe fn PostGetSize(&self, actual: usize, _spyed: BOOL) -> usize {
        actual
    }
    unsafe fn PreDidAlloc(&self, p: *mut c_void, _spyed: BOOL) -> *mut c_void {
        p
    }
    unsafe fn PostDidAlloc(&self, _p: *mut c_void, _spyed: BOOL, actual: i32) -> i32 {
        actual
    }
    unsafe fn PreHeapMinimize(&self) {}
    unsafe fn PostHeapMinimize(&self) {}
}

fn interfaces_declared_here() {
    unsafe {
        let malloc: IHostMalloc = CoGetMalloc(1)
            .expect("CoGetMalloc")
            .cast()
            .expect("cast to IHostMalloc");
        let p = malloc.Alloc(100);
        assert!(!p.is_null());
        let size = malloc.GetSize(p);
        let did = malloc.DidAlloc(p);
        println!("IMalloc through #[interface]: Alloc(100) gave {size} bytes, DidAlloc {did}");
        assert!(size >= 100);
        assert_eq!(did, 1);
        malloc.Free(p);

        let spy: IAllocSpy = Spy.into();
        CoRegisterMallocSpy(&spy.cast::<IMallocSpy>().unwrap()).expect("CoRegisterMallocSpy");
        let q = CoTaskMemAlloc(4096);
        assert!(!q.is_null());
        CoTaskMemFree(Some(q));
        CoRevokeMallocSpy().expect("CoRevokeMallocSpy");
        let allocs = SPY_ALLOCS.load(Ordering::SeqCst);
        let frees = SPY_FREES.load(Ordering::SeqCst);
        let saw = SPY_SAW_4096.load(Ordering::SeqCst);
        println!(
            "malloc spy from #[interface] and #[implement]: {allocs} PreAlloc ({saw} of 4096 bytes), {frees} PreFree"
        );
        assert!(saw >= 1 && frees >= 1, "the host didn't call the spy");
    }
}

fn friendly_name(device: &IMMDevice) -> String {
    unsafe {
        let store = device
            .OpenPropertyStore(STGM_READ)
            .expect("OpenPropertyStore");
        let value = store.GetValue(&PKEY_Device_FriendlyName).expect("GetValue");
        value.to_string()
    }
}

fn main() {
    if !ape::is_windows() {
        println!("not Windows, nothing to do here");
        return;
    }
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .expect("CoInitializeEx");
    }

    // an error made by the host, with the host's text for it
    let err = Error::from(HRESULT(0x80070490u32 as i32));
    println!("error text from the host: {err}");
    assert!(err.message().contains("not found"), "{err}");

    interfaces_declared_here();

    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .expect("CoCreateInstance");
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
        .expect("EnumAudioEndpoints");
    let n = unsafe { devices.GetCount() }.expect("GetCount");
    println!("{n} active render endpoints");
    for i in 0..n {
        let device = unsafe { devices.Item(i) }.expect("Item");
        let id = unsafe { device.GetId() }.expect("GetId");
        let id = unsafe { id.to_string() }.unwrap();
        println!("  {:?} {id}", friendly_name(&device));
        // a wide string measured with the C library's 32-bit wcslen comes
        // out cut short or running on into whatever follows it
        assert!(
            id.starts_with('{') && id.ends_with('}') && id.matches('{').count() == 2,
            "endpoint id {id:?} isn't whole"
        );
    }
    let default = match unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) } {
        Ok(d) => d,
        Err(e) => {
            println!("no default render endpoint ({e}), skipping the volume part");
            println!("\nhost com ok");
            return;
        }
    };
    println!("default endpoint: {:?}", friendly_name(&default));

    // f32 through a vtable, both ways
    let volume: IAudioEndpointVolume =
        unsafe { default.Activate(CLSCTX_ALL, None) }.expect("Activate");
    let before =
        unsafe { volume.GetMasterVolumeLevelScalar() }.expect("GetMasterVolumeLevelScalar");
    println!("master volume {before:.3}");
    let watcher: IAudioEndpointVolumeCallback = VolumeWatcher.into();
    unsafe { volume.RegisterControlChangeNotify(&watcher) }.expect("RegisterControlChangeNotify");
    let target = if before < 0.5 { 0.75 } else { 0.25 };
    unsafe { volume.SetMasterVolumeLevelScalar(target, std::ptr::null()) }
        .expect("SetMasterVolumeLevelScalar");
    let after = unsafe { volume.GetMasterVolumeLevelScalar() }.expect("GetMasterVolumeLevelScalar");
    println!("set to {target:.3}, read back {after:.3}");
    assert!((after - target).abs() < 0.01);

    // the notification arrives asynchronously
    let start = Instant::now();
    while NOTIFIED.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    unsafe { volume.SetMasterVolumeLevelScalar(before, std::ptr::null()) }
        .expect("restoring the volume");
    let start = Instant::now();
    while NOTIFIED.load(Ordering::SeqCst) < 2 && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    unsafe { volume.UnregisterControlChangeNotify(&watcher) }
        .expect("UnregisterControlChangeNotify");
    let notified = NOTIFIED.load(Ordering::SeqCst);
    let main_thread = unsafe { GetCurrentThreadId() };
    let threads = NOTIFY_THREADS.lock().unwrap();
    println!("{notified} notifications, main thread is {main_thread}, they came on {threads:?}");
    assert!(notified >= 2, "the volume callbacks didn't arrive");
    assert!(
        threads.iter().all(|&t| t != main_thread),
        "the host used our thread"
    );
    drop(threads);
    println!("\nhost com ok");
}
