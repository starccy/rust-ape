//! A web page in a native window, from the same binary on Linux, Windows
//! and macOS: WebKitGTK on Linux, WebView2 on Windows, WKWebView on macOS.
//! This is the layer a webview framework like Tauri stands on.
//!
//! The page sends Rust a message as soon as it loads, Rust answers, and the
//! page sends the answer back, so both directions are checked. Then the
//! window closes by itself, unless the first argument is `--stay`.
//!
//! On Windows the WebView2 runtime that ships with the system is used
//! directly, without Microsoft's WebView2Loader.dll: the runtime's location
//! is read from the registry and its EmbeddedBrowserWebView.dll is asked for
//! the environment, as the loader itself does. Setting WEBVIEW2_LOADER to the
//! path of a WebView2Loader.dll goes through that instead.
//!
//! On macOS the whole thing goes through the Objective-C runtime: AppKit and
//! WebKit are loaded like any library, objc_msgSend is called with each
//! method's own signature, and the object that receives the page's messages
//! is an NSObject subclass registered at run time, whose method is Rust.
//!
//! Skipped where there is no display, no WebKitGTK or no WebView2 runtime.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const PAGE: &str = r#"<!doctype html>
<html><body style="font: 20px sans-serif; margin: 2em">
<h1>rust-ape webview</h1>
<p id="msg">waiting for Rust</p>
<script>
const wk = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.rust;
const send = m => wk ? wk.postMessage(m) : window.chrome.webview.postMessage(m);
window.fromRust = m => {
  document.getElementById("msg").textContent = m;
  send("echo: " + m);
};
if (window.chrome && window.chrome.webview)
  window.chrome.webview.addEventListener("message", e => fromRust(e.data));
send("hello from " + navigator.userAgent);
</script>
</body></html>"#;

const REPLY: &str = "pong from rust";

// what the page sent, in order
static RECEIVED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static TIMED_OUT: AtomicBool = AtomicBool::new(false);

fn stay() -> bool {
    std::env::args().nth(1).as_deref() == Some("--stay")
}

// takes a message from the page, and says whether the exchange is over
fn received(msg: String) -> bool {
    println!("page says: {msg}");
    let mut r = RECEIVED.lock().unwrap();
    r.push(msg);
    r.len() >= 2
}

fn check() {
    let r = RECEIVED.lock().unwrap();
    assert!(
        !TIMED_OUT.load(Ordering::Relaxed),
        "no answer from the page in time: {r:?}"
    );
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(r[0].starts_with("hello from "), "{r:?}");
    assert_eq!(r[1], format!("echo: {REPLY}"));
    println!("\nwebview ok");
}

fn main() {
    if ape::is_linux() {
        linux::run();
    } else if ape::is_windows() {
        #[cfg(target_arch = "x86_64")]
        windows::run();
    } else if ape::is_xnu() {
        #[cfg(target_arch = "aarch64")]
        mac::run();
    } else {
        println!("no webview on this host yet, skipping");
    }
}

mod linux {
    use super::*;
    use ape::host::Callback;

    // everything below is reached through WebKitGTK, whose dependencies
    // GTK, GLib and JavaScriptCore are searched along with it
    #[ape::host_library(static WEBKIT = linux ["libwebkit2gtk-4.1.so.0", "libwebkit2gtk-4.0.so.37"])]
    unsafe extern "C" {
        fn gtk_init_check(argc: *mut c_int, argv: *mut c_void) -> c_int;
        fn gtk_window_new(kind: c_int) -> *mut c_void;
        fn gtk_window_set_title(w: *mut c_void, title: *const c_char);
        fn gtk_window_set_default_size(w: *mut c_void, width: c_int, height: c_int);
        fn gtk_container_add(c: *mut c_void, w: *mut c_void);
        fn gtk_widget_show_all(w: *mut c_void);
        fn gtk_widget_destroy(w: *mut c_void);
        fn gtk_main();
        fn gtk_main_quit();
        fn g_signal_connect_data(
            obj: *mut c_void,
            signal: *const c_char,
            handler: Callback,
            data: *mut c_void,
            destroy: Callback,
            flags: c_int,
        ) -> u64;
        fn g_timeout_add(ms: c_uint, f: Callback, data: *mut c_void) -> c_uint;
        fn g_free(p: *mut c_void);
        fn webkit_user_content_manager_new() -> *mut c_void;
        fn webkit_user_content_manager_register_script_message_handler(
            m: *mut c_void,
            name: *const c_char,
        ) -> c_int;
        fn webkit_web_view_new_with_user_content_manager(m: *mut c_void) -> *mut c_void;
        fn webkit_web_view_load_html(v: *mut c_void, html: *const c_char, base: *const c_char);
        fn webkit_web_view_run_javascript(
            v: *mut c_void,
            script: *const c_char,
            cancel: *mut c_void,
            done: Callback,
            data: *mut c_void,
        );
        fn webkit_javascript_result_get_js_value(r: *mut c_void) -> *mut c_void;
        fn jsc_value_to_string(v: *mut c_void) -> *mut c_char;
    }

    static WINDOW: AtomicUsize = AtomicUsize::new(0);

    #[ape::host_callback]
    fn on_message(_m: *mut c_void, result: *mut c_void, view: *mut c_void) {
        let s = unsafe { jsc_value_to_string(webkit_javascript_result_get_js_value(result)) };
        let msg = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
        unsafe { g_free(s.cast()) };
        let first = RECEIVED.lock().unwrap().is_empty();
        let done = received(msg);
        if first {
            let js = std::ffi::CString::new(format!("fromRust({REPLY:?})")).unwrap();
            unsafe {
                webkit_web_view_run_javascript(
                    view,
                    js.as_ptr(),
                    std::ptr::null_mut(),
                    Callback::NULL,
                    std::ptr::null_mut(),
                )
            };
        } else if done && !stay() {
            unsafe { gtk_widget_destroy(WINDOW.load(Ordering::Relaxed) as *mut c_void) };
        }
    }

    #[ape::host_callback]
    fn on_destroy(_w: *mut c_void, _data: *mut c_void) {
        unsafe { gtk_main_quit() };
    }

    #[ape::host_callback]
    fn on_timeout(_data: *mut c_void) -> c_int {
        if !stay() && RECEIVED.lock().unwrap().len() < 2 {
            TIMED_OUT.store(true, Ordering::Relaxed);
            unsafe { gtk_widget_destroy(WINDOW.load(Ordering::Relaxed) as *mut c_void) };
        }
        0
    }

    pub fn run() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
            println!("no display, skipping");
            return;
        }
        if let Err(e) = WEBKIT.load() {
            println!("no WebKitGTK on this host, skipping: {e}");
            return;
        }
        unsafe {
            if gtk_init_check(std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                println!("GTK can't open the display, skipping");
                return;
            }
            let window = gtk_window_new(0);
            WINDOW.store(window as usize, Ordering::Relaxed);
            gtk_window_set_title(window, c"rust-ape webview".as_ptr());
            gtk_window_set_default_size(window, 800, 500);
            let manager = webkit_user_content_manager_new();
            webkit_user_content_manager_register_script_message_handler(manager, c"rust".as_ptr());
            let view = webkit_web_view_new_with_user_content_manager(manager);
            g_signal_connect_data(
                manager,
                c"script-message-received::rust".as_ptr(),
                on_message::ptr(),
                view,
                Callback::NULL,
                0,
            );
            g_signal_connect_data(
                window,
                c"destroy".as_ptr(),
                on_destroy::ptr(),
                std::ptr::null_mut(),
                Callback::NULL,
                0,
            );
            gtk_container_add(window, view);
            let page = std::ffi::CString::new(PAGE).unwrap();
            webkit_web_view_load_html(view, page.as_ptr(), c"about:blank".as_ptr());
            gtk_widget_show_all(window);
            g_timeout_add(30_000, on_timeout::ptr(), std::ptr::null_mut());
            gtk_main();
        }
        check();
    }
}

#[cfg(target_arch = "x86_64")]
mod windows {
    use super::*;
    use ape::host::Callback;

    type Hwnd = *mut c_void;

    #[ape::host_library(static USER32 = windows ["user32.dll"])]
    unsafe extern "C" {
        fn RegisterClassExW(class: *const WndClassEx) -> u16;
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: c_int,
            y: c_int,
            w: c_int,
            h: c_int,
            parent: Hwnd,
            menu: *mut c_void,
            inst: *mut c_void,
            param: *mut c_void,
        ) -> Hwnd;
        fn ShowWindow(w: Hwnd, cmd: c_int) -> c_int;
        fn DestroyWindow(w: Hwnd) -> c_int;
        fn DefWindowProcW(w: Hwnd, msg: u32, wp: usize, lp: isize) -> isize;
        fn GetMessageW(msg: *mut Msg, w: Hwnd, min: u32, max: u32) -> c_int;
        fn TranslateMessage(msg: *const Msg) -> c_int;
        fn DispatchMessageW(msg: *const Msg) -> isize;
        fn PostQuitMessage(code: c_int);
        fn GetClientRect(w: Hwnd, r: *mut Rect) -> c_int;
        fn SetTimer(w: Hwnd, id: usize, ms: u32, f: *mut c_void) -> usize;
    }

    #[ape::host_library(static KERNEL32 = windows ["kernel32.dll"])]
    unsafe extern "C" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    #[ape::host_library(static OLE32 = windows ["ole32.dll"])]
    unsafe extern "C" {
        fn CoInitializeEx(reserved: *mut c_void, model: u32) -> i32;
        fn CoTaskMemFree(p: *mut c_void);
    }

    #[ape::host_library(static ADVAPI32 = windows ["advapi32.dll"])]
    unsafe extern "C" {
        fn RegGetValueW(
            key: isize,
            sub: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            size: *mut u32,
        ) -> i32;
    }

    #[repr(C)]
    struct WndClassEx {
        size: u32,
        style: u32,
        proc_: Callback,
        cls_extra: c_int,
        wnd_extra: c_int,
        instance: *mut c_void,
        icon: *mut c_void,
        cursor: *mut c_void,
        background: *mut c_void,
        menu: *const u16,
        class: *const u16,
        icon_sm: *mut c_void,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct Msg {
        hwnd: Hwnd,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt: [i32; 2],
        private: u32,
    }

    const WM_DESTROY: u32 = 0x0002;
    const WM_SIZE: u32 = 0x0005;
    const WM_TIMER: u32 = 0x0113;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    static HWND: AtomicUsize = AtomicUsize::new(0);
    static CONTROLLER: AtomicUsize = AtomicUsize::new(0);
    static FAILED: Mutex<Option<String>> = Mutex::new(None);

    fn fail(why: String) {
        println!("{why}");
        *FAILED.lock().unwrap() = Some(why);
        unsafe { DestroyWindow(HWND.load(Ordering::Relaxed) as Hwnd) };
    }

    // method `i` of a COM object's vtable
    unsafe fn method<F: Copy>(obj: *mut c_void, i: usize) -> F {
        unsafe {
            let vtbl = *(obj as *const *const usize);
            std::mem::transmute_copy(&*vtbl.add(i))
        }
    }

    // the three completion and event handlers WebView2 calls, as COM
    // objects of our own. each lives for the whole run, so reference
    // counting has nothing to do
    #[repr(C)]
    struct Handler {
        vtbl: &'static HandlerVtbl,
    }

    #[repr(C)]
    struct HandlerVtbl {
        query_interface: extern "win64" fn(*mut Handler, *const u8, *mut *mut c_void) -> i32,
        add_ref: extern "win64" fn(*mut Handler) -> u32,
        release: extern "win64" fn(*mut Handler) -> u32,
        invoke: *const c_void,
    }

    unsafe impl Sync for HandlerVtbl {}

    extern "win64" fn query_interface(
        this: *mut Handler,
        _iid: *const u8,
        out: *mut *mut c_void,
    ) -> i32 {
        unsafe { *out = this.cast() };
        0
    }

    extern "win64" fn add_ref(_: *mut Handler) -> u32 {
        1
    }

    extern "win64" fn release(_: *mut Handler) -> u32 {
        1
    }

    static ENV_DONE: HandlerVtbl = HandlerVtbl {
        query_interface,
        add_ref,
        release,
        invoke: env_created as *const c_void,
    };
    static CONTROLLER_DONE: HandlerVtbl = HandlerVtbl {
        query_interface,
        add_ref,
        release,
        invoke: controller_created as *const c_void,
    };
    static MESSAGE: HandlerVtbl = HandlerVtbl {
        query_interface,
        add_ref,
        release,
        invoke: message_received as *const c_void,
    };

    struct Handlers([Handler; 3]);
    unsafe impl Sync for Handlers {}
    static HANDLERS: Handlers = Handlers([
        Handler { vtbl: &ENV_DONE },
        Handler {
            vtbl: &CONTROLLER_DONE,
        },
        Handler { vtbl: &MESSAGE },
    ]);

    fn handler(i: usize) -> *mut c_void {
        &HANDLERS.0[i] as *const Handler as *mut c_void
    }

    // ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler::Invoke
    extern "win64" fn env_created(_this: *mut Handler, hr: i32, env: *mut c_void) -> i32 {
        if hr < 0 || env.is_null() {
            fail(format!("creating the WebView2 environment failed: {hr:#x}"));
            return 0;
        }
        // ICoreWebView2Environment::CreateCoreWebView2Controller
        let create: extern "win64" fn(*mut c_void, Hwnd, *mut c_void) -> i32 =
            unsafe { method(env, 3) };
        let hr = create(env, HWND.load(Ordering::Relaxed) as Hwnd, handler(1));
        if hr < 0 {
            fail(format!("CreateCoreWebView2Controller failed: {hr:#x}"));
        }
        0
    }

    fn fit(controller: *mut c_void) {
        let mut r = Rect::default();
        unsafe { GetClientRect(HWND.load(Ordering::Relaxed) as Hwnd, &mut r) };
        // ICoreWebView2Controller::put_Bounds
        let put_bounds: extern "win64" fn(*mut c_void, Rect) -> i32 =
            unsafe { method(controller, 6) };
        put_bounds(controller, r);
    }

    // ICoreWebView2CreateCoreWebView2ControllerCompletedHandler::Invoke
    extern "win64" fn controller_created(
        _this: *mut Handler,
        hr: i32,
        controller: *mut c_void,
    ) -> i32 {
        if hr < 0 || controller.is_null() {
            fail(format!("creating the WebView2 controller failed: {hr:#x}"));
            return 0;
        }
        unsafe {
            // IUnknown::AddRef, the controller is kept
            let add: extern "win64" fn(*mut c_void) -> u32 = method(controller, 1);
            add(controller);
            CONTROLLER.store(controller as usize, Ordering::Relaxed);
            fit(controller);
            // ICoreWebView2Controller::get_CoreWebView2
            let get_view: extern "win64" fn(*mut c_void, *mut *mut c_void) -> i32 =
                method(controller, 25);
            let mut view = std::ptr::null_mut();
            get_view(controller, &mut view);
            // ICoreWebView2::add_WebMessageReceived
            let add_msg: extern "win64" fn(*mut c_void, *mut c_void, *mut i64) -> i32 =
                method(view, 34);
            let mut token = 0i64;
            add_msg(view, handler(2), &mut token);
            // ICoreWebView2::NavigateToString
            let navigate: extern "win64" fn(*mut c_void, *const u16) -> i32 = method(view, 6);
            let hr = navigate(view, wide(PAGE).as_ptr());
            if hr < 0 {
                fail(format!("NavigateToString failed: {hr:#x}"));
            }
        }
        0
    }

    // ICoreWebView2WebMessageReceivedEventHandler::Invoke
    extern "win64" fn message_received(
        _this: *mut Handler,
        view: *mut c_void,
        args: *mut c_void,
    ) -> i32 {
        unsafe {
            // ICoreWebView2WebMessageReceivedEventArgs::TryGetWebMessageAsString
            let get: extern "win64" fn(*mut c_void, *mut *mut u16) -> i32 = method(args, 5);
            let mut s = std::ptr::null_mut();
            if get(args, &mut s) < 0 || s.is_null() {
                return 0;
            }
            let n = (0..).take_while(|&i| *s.add(i) != 0).count();
            let msg = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            CoTaskMemFree(s.cast());
            let first = RECEIVED.lock().unwrap().is_empty();
            let done = received(msg);
            if first {
                // ICoreWebView2::PostWebMessageAsString
                let post: extern "win64" fn(*mut c_void, *const u16) -> i32 = method(view, 33);
                post(view, wide(REPLY).as_ptr());
            } else if done && !stay() {
                DestroyWindow(HWND.load(Ordering::Relaxed) as Hwnd);
            }
        }
        0
    }

    #[ape::host_callback]
    fn window_proc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize {
        match msg {
            WM_SIZE => {
                let c = CONTROLLER.load(Ordering::Relaxed);
                if c != 0 {
                    fit(c as *mut c_void);
                }
                0
            }
            WM_TIMER => {
                if !stay() && RECEIVED.lock().unwrap().len() < 2 {
                    TIMED_OUT.store(true, Ordering::Relaxed);
                    unsafe { DestroyWindow(hwnd) };
                }
                0
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                0
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    type CreateInternal =
        extern "win64" fn(bool, c_int, *const u16, *mut c_void, *mut c_void) -> i32;
    type CreateWithOptions =
        extern "win64" fn(*const u16, *const u16, *mut c_void, *mut c_void) -> i32;

    // the installed runtime's directory, as the registry records it for the
    // stable channel, machine-wide first and then per user
    fn runtime_dir() -> Option<String> {
        const RRF_RT_REG_SZ: u32 = 0x2;
        const RRF_SUBKEY_WOW6432KEY: u32 = 0x20000;
        let sub = wide(
            r"SOFTWARE\Microsoft\EdgeUpdate\ClientState\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
        );
        let value = wide("EBWebView");
        for key in [
            0x8000_0002u32 as i32 as isize,
            0x8000_0001u32 as i32 as isize,
        ] {
            let mut buf = [0u16; 1024];
            let mut size = (buf.len() * 2) as u32;
            let rc = unsafe {
                RegGetValueW(
                    key,
                    sub.as_ptr(),
                    value.as_ptr(),
                    RRF_RT_REG_SZ | RRF_SUBKEY_WOW6432KEY,
                    std::ptr::null_mut(),
                    buf.as_mut_ptr().cast(),
                    &mut size,
                )
            };
            if rc == 0 {
                let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
                return Some(String::from_utf16_lossy(&buf[..n]));
            }
        }
        None
    }

    pub fn run() {
        let data_dir = std::env::temp_dir().join("rust-ape-webview");
        let _ = std::fs::create_dir_all(&data_dir);
        let data_dir = ape::host::host_path(&data_dir).unwrap();
        let data_dir: Vec<u16> = wide(&data_dir.to_string_lossy());

        // either the runtime's own entry point, or Microsoft's loader
        let (lib, start): (ape::host::Library, Box<dyn Fn() -> i32>) =
            match std::env::var("WEBVIEW2_LOADER") {
                Ok(path) => {
                    let lib = ape::host::Library::open(&path).unwrap_or_else(|e| panic!("{e}"));
                    let f: CreateWithOptions = unsafe {
                        std::mem::transmute(
                            lib.func("CreateCoreWebView2EnvironmentWithOptions")
                                .expect("loader entry"),
                        )
                    };
                    println!("WebView2 through {path}");
                    let dir = data_dir.clone();
                    (
                        lib,
                        Box::new(move || {
                            f(
                                std::ptr::null(),
                                dir.as_ptr(),
                                std::ptr::null_mut(),
                                handler(0),
                            )
                        }),
                    )
                }
                Err(_) => {
                    let Some(dir) = runtime_dir() else {
                        println!("no WebView2 runtime on this host, skipping");
                        return;
                    };
                    let dll = format!(r"{dir}\EBWebView\x64\EmbeddedBrowserWebView.dll");
                    let lib = match ape::host::Library::open(&dll) {
                        Ok(lib) => lib,
                        Err(e) => {
                            println!("the WebView2 runtime won't load, skipping: {e}");
                            return;
                        }
                    };
                    let f: CreateInternal = unsafe {
                        std::mem::transmute(
                            lib.func("CreateWebViewEnvironmentWithOptionsInternal")
                                .expect("runtime entry"),
                        )
                    };
                    println!("WebView2 runtime {dir}, no loader DLL");
                    let dir = data_dir.clone();
                    (
                        lib,
                        Box::new(move || {
                            f(true, 0, dir.as_ptr(), std::ptr::null_mut(), handler(0))
                        }),
                    )
                }
            };

        unsafe {
            CoInitializeEx(std::ptr::null_mut(), 2);
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("rust-ape-webview");
            let wc = WndClassEx {
                size: std::mem::size_of::<WndClassEx>() as u32,
                style: 0,
                proc_: window_proc::ptr(),
                cls_extra: 0,
                wnd_extra: 0,
                instance,
                icon: std::ptr::null_mut(),
                cursor: std::ptr::null_mut(),
                background: std::ptr::null_mut(),
                menu: std::ptr::null(),
                class: class.as_ptr(),
                icon_sm: std::ptr::null_mut(),
            };
            RegisterClassExW(&wc);
            let title = wide("rust-ape webview");
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                0x00CF_0000,
                0x8000_0000u32 as i32,
                0x8000_0000u32 as i32,
                800,
                500,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            );
            HWND.store(hwnd as usize, Ordering::Relaxed);
            ShowWindow(hwnd, 5);
            SetTimer(hwnd, 1, 30_000, std::ptr::null_mut());
            let hr = start();
            if hr < 0 {
                fail(format!("starting WebView2 failed: {hr:#x}"));
            }
            let mut msg: Msg = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // the runtime keeps threads of its own going until the process ends
        std::mem::forget(lib);
        if let Some(why) = FAILED.lock().unwrap().take() {
            panic!("{why}");
        }
        check();
    }
}

#[cfg(target_arch = "aarch64")]
mod mac {
    use super::*;
    use ape::host::{Callback, Library};

    type Id = *mut c_void;
    type Sel = *mut c_void;

    #[ape::host_library(static OBJC = macos ["/usr/lib/libobjc.A.dylib"])]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn objc_getProtocol(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
        fn objc_registerClassPair(cls: Id);
        fn class_addMethod(cls: Id, sel: Sel, imp: Callback, types: *const c_char) -> bool;
        fn class_addProtocol(cls: Id, protocol: Id) -> bool;
        fn objc_autoreleasePoolPush() -> *mut c_void;
    }

    #[ape::host_library(static SYSTEM = macos ["/usr/lib/libSystem.B.dylib"])]
    unsafe extern "C" {
        fn dispatch_time(when: u64, delta: i64) -> u64;
        fn dispatch_after_f(when: u64, queue: *mut c_void, ctx: *mut c_void, work: Callback);
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }

    static SEND: AtomicUsize = AtomicUsize::new(0);
    static APP: AtomicUsize = AtomicUsize::new(0);
    static VIEW: AtomicUsize = AtomicUsize::new(0);

    fn sel(name: &CStr) -> Sel {
        unsafe { sel_registerName(name.as_ptr()) }
    }

    fn class(name: &CStr) -> Id {
        unsafe { objc_getClass(name.as_ptr()) }
    }

    // objc_msgSend, typed as the method being called
    fn send<F: Copy>() -> F {
        let p = SEND.load(Ordering::Relaxed);
        unsafe { std::mem::transmute_copy(&p) }
    }

    fn msg0(obj: Id, name: &CStr) -> Id {
        send::<extern "C" fn(Id, Sel) -> Id>()(obj, sel(name))
    }

    fn msg1(obj: Id, name: &CStr, a: Id) -> Id {
        send::<extern "C" fn(Id, Sel, Id) -> Id>()(obj, sel(name), a)
    }

    fn nsstring(s: &str) -> Id {
        let c = std::ffi::CString::new(s).unwrap();
        send::<extern "C" fn(Id, Sel, *const c_char) -> Id>()(
            class(c"NSString"),
            sel(c"stringWithUTF8String:"),
            c.as_ptr(),
        )
    }

    // ends [NSApp run]: stop only takes effect once an event comes in, so
    // an empty one is posted as well
    fn stop() {
        let app = APP.load(Ordering::Relaxed) as Id;
        msg1(app, c"stop:", std::ptr::null_mut());
        let event = send::<extern "C" fn(Id, Sel, u64, Point, u64, f64, i64, Id, i16, i64, i64) -> Id>()(
            class(c"NSEvent"),
            sel(c"otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:"),
            15, // NSEventTypeApplicationDefined
            Point { x: 0.0, y: 0.0 }, 0, 0.0, 0, std::ptr::null_mut(), 0, 0, 0,
        );
        send::<extern "C" fn(Id, Sel, Id, bool)>()(app, sel(c"postEvent:atStart:"), event, true);
    }

    // -[RustApeScriptHandler userContentController:didReceiveScriptMessage:]
    #[ape::host_callback]
    fn on_message(_this: Id, _sel: Sel, _controller: Id, message: Id) {
        let body = msg0(message, c"body");
        let utf8 = send::<extern "C" fn(Id, Sel) -> *const c_char>()(body, sel(c"UTF8String"));
        let text = if utf8.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(utf8) }
                .to_string_lossy()
                .into_owned()
        };
        let first = RECEIVED.lock().unwrap().is_empty();
        let done = received(text);
        if first {
            let view = VIEW.load(Ordering::Relaxed) as Id;
            let js = nsstring(&format!("fromRust({REPLY:?})"));
            send::<extern "C" fn(Id, Sel, Id, Id)>()(
                view,
                sel(c"evaluateJavaScript:completionHandler:"),
                js,
                std::ptr::null_mut(),
            );
        } else if done && !stay() {
            stop();
        }
    }

    #[ape::host_callback]
    fn on_timeout(_ctx: *mut c_void) {
        if !stay() && RECEIVED.lock().unwrap().len() < 2 {
            TIMED_OUT.store(true, Ordering::Relaxed);
            stop();
        }
    }

    pub fn run() {
        let (Ok(objc), Ok(system)) = (OBJC.load(), SYSTEM.load()) else {
            println!("no Objective-C runtime, skipping");
            return;
        };
        for framework in ["AppKit", "WebKit"] {
            let path = format!("/System/Library/Frameworks/{framework}.framework/{framework}");
            match Library::open(&path) {
                Ok(lib) => std::mem::forget(lib),
                Err(e) => {
                    println!("no {framework} on this host, skipping: {e}");
                    return;
                }
            }
        }
        SEND.store(
            objc.sym("objc_msgSend").expect("objc_msgSend") as usize,
            Ordering::Relaxed,
        );
        let main_queue = system.sym("_dispatch_main_q").expect("main queue");

        unsafe {
            objc_autoreleasePoolPush();

            // a class whose instances take the page's messages
            let cls =
                objc_allocateClassPair(class(c"NSObject"), c"RustApeScriptHandler".as_ptr(), 0);
            class_addMethod(
                cls,
                sel(c"userContentController:didReceiveScriptMessage:"),
                on_message::ptr(),
                c"v@:@@".as_ptr(),
            );
            let protocol = objc_getProtocol(c"WKScriptMessageHandler".as_ptr());
            if !protocol.is_null() {
                class_addProtocol(cls, protocol);
            }
            objc_registerClassPair(cls);
            let handler = msg0(msg0(cls, c"alloc"), c"init");

            let app = msg0(class(c"NSApplication"), c"sharedApplication");
            APP.store(app as usize, Ordering::Relaxed);
            send::<extern "C" fn(Id, Sel, i64) -> bool>()(app, sel(c"setActivationPolicy:"), 0);

            let frame = Rect {
                x: 0.0,
                y: 0.0,
                w: 800.0,
                h: 500.0,
            };
            // titled, closable, miniaturizable, resizable; buffered
            let window = send::<extern "C" fn(Id, Sel, Rect, u64, u64, bool) -> Id>()(
                msg0(class(c"NSWindow"), c"alloc"),
                sel(c"initWithContentRect:styleMask:backing:defer:"),
                frame,
                15,
                2,
                false,
            );
            msg1(window, c"setTitle:", nsstring("rust-ape webview"));

            let config = msg0(msg0(class(c"WKWebViewConfiguration"), c"alloc"), c"init");
            let controller = msg0(config, c"userContentController");
            send::<extern "C" fn(Id, Sel, Id, Id)>()(
                controller,
                sel(c"addScriptMessageHandler:name:"),
                handler,
                nsstring("rust"),
            );
            let view = send::<extern "C" fn(Id, Sel, Rect, Id) -> Id>()(
                msg0(class(c"WKWebView"), c"alloc"),
                sel(c"initWithFrame:configuration:"),
                frame,
                config,
            );
            VIEW.store(view as usize, Ordering::Relaxed);
            msg1(window, c"setContentView:", view);
            send::<extern "C" fn(Id, Sel, Id, Id) -> Id>()(
                view,
                sel(c"loadHTMLString:baseURL:"),
                nsstring(PAGE),
                std::ptr::null_mut(),
            );

            msg0(window, c"center");
            msg1(window, c"makeKeyAndOrderFront:", std::ptr::null_mut());
            send::<extern "C" fn(Id, Sel, bool)>()(app, sel(c"activateIgnoringOtherApps:"), true);

            // DISPATCH_TIME_NOW plus 30 seconds
            dispatch_after_f(
                dispatch_time(0, 30_000_000_000),
                main_queue,
                std::ptr::null_mut(),
                on_timeout::ptr(),
            );
            msg0(app, c"run");
        }
        check();
    }
}
