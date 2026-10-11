//! A native window on Linux, Windows and macOS from the one binary, each
//! host's own GUI stack loaded at run time, the way a GUI program on cosmo
//! would do it.
//!
//! What it leans on, and shows off:
//!
//! - Linux: winit and softbuffer, unmodified, which reach X11 and Wayland
//!   through libloading's plain `dlopen`, so no cosmo-specific code at all.
//! - Windows: user32 and gdi32 through the `windows` crate, unmodified, and
//!   Direct3D 11 for presenting the frames (a hardware device, WARP when
//!   there's none, GDI when there's no Direct3D at all). The window
//!   procedure is an `#[ape::host_callback]`; the frames are made by a timer
//!   callback on one of Windows' own thread pool threads.
//! - macOS: AppKit, CoreVideo and CoreGraphics through the Objective-C
//!   runtime, written out here with `objc_msgSend` typed per call. The
//!   frames are made in a CVDisplayLink callback, on CoreVideo's thread,
//!   and the main thread is told with `performSelectorOnMainThread:`; the
//!   window's delegate is a class registered at run time whose methods are
//!   Rust; CoreGraphics frees each frame's pixels through a callback of
//!   ours; `NSDefaultRunLoopMode` is read as the variable it is.
//!
//! The picture itself is drawn on the CPU, a ball bouncing over rippling
//! water computed pixel by pixel, so one check works everywhere: a click on
//! the ball changes its colour, and the pixel at its centre has to say so.
//! The callbacks that drive the frames run on the host's threads and do
//! the drawing there, no polling in between; how long a frame takes to
//! reach the main thread is measured. Each frame is drawn in bands by
//! threads the callback's thread spawns and joins, `std::thread::scope` on
//! CoreVideo's or the pool's thread sixty times a second, and the self-test
//! times one frame drawn by one thread against the same frame drawn by all
//! of them.
//!
//! Without arguments it runs for about two seconds, clicks the ball itself,
//! checks and exits. `--stay` keeps the window until it's closed or Esc is
//! pressed. Skipped where there's no display.
//!
//! On Windows it's built for the GUI subsystem, so a double-click opens no
//! console window next to it; started from a shell it joins the shell's
//! console and prints there (the fork's winmain does that for any GUI
//! subsystem program), and redirections work as usual.

#![windows_subsystem = "windows"]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const WIDTH: u32 = 800;
const HEIGHT: u32 = 500;
const TITLE: &str = "rust-ape host gui";
// at 60 frames a second: when to click the ball, spawn the threads, stop
const CLICK_FRAME: u32 = 50;
const THREADS_FRAME: u32 = 70;
const LAST_FRAME: u32 = 120;
const RADIUS: i32 = 40;
const COLOURS: [(u32, &str); 3] = [
    (0xff_e0_50, "yellow"),
    (0x50_e0_a0, "green"),
    (0xf0_60_90, "pink"),
];

fn stay() -> bool {
    std::env::args().nth(1).as_deref() == Some("--stay")
}

// ---- the picture, drawn on the CPU ----

// what a frame is drawn from, taken from the scene under its lock
#[derive(Clone, Copy)]
struct Frame {
    w: usize,
    h: usize,
    frame: u32,
    x: f32,
    y: f32,
    colour: u32,
    threads: usize,
}

struct Scene {
    w: u32,
    h: u32,
    // the frame on screen
    pixels: Vec<u32>,
    frame: u32,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    colour: usize,
    // how many threads draw a frame
    threads: usize,
}

impl Scene {
    const fn empty() -> Scene {
        Scene {
            w: 0,
            h: 0,
            pixels: Vec::new(),
            frame: 0,
            x: 120.0,
            y: 100.0,
            vx: 3.2,
            vy: 2.1,
            colour: 0,
            threads: 1,
        }
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.w = w;
        self.h = h;
        self.pixels = vec![0; (w * h) as usize];
        self.threads = std::env::var("HOST_GUI_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map_or(2, |n| n.get())
                    .clamp(2, 8)
            })
            .max(1);
    }

    fn colour(&self) -> u32 {
        COLOURS[self.colour % COLOURS.len()].0
    }

    fn centre(&self) -> (i32, i32) {
        (self.x as i32, self.y as i32)
    }

    fn frame(&self) -> Frame {
        Frame {
            w: self.w as usize,
            h: self.h as usize,
            frame: self.frame,
            x: self.x,
            y: self.y,
            colour: self.colour(),
            threads: self.threads,
        }
    }

    // one frame on: moves the ball, says what to draw
    fn advance(&mut self) -> Option<Frame> {
        if self.w == 0 {
            return None;
        }
        self.frame += 1;
        let (w, h) = (self.w as f32, self.h as f32);
        let r = RADIUS as f32;
        self.x += self.vx;
        self.y += self.vy;
        if self.x < r || self.x > w - r {
            self.vx = -self.vx;
            self.x = self.x.clamp(r, w - r);
        }
        if self.y < r || self.y > h - r {
            self.vy = -self.vy;
            self.y = self.y.clamp(r, h - r);
        }
        Some(self.frame())
    }

    // a click: on the ball it changes colour, from the next frame on
    fn click(&mut self, x: i32, y: i32) -> bool {
        let (cx, cy) = self.centre();
        let hit = (x - cx).pow(2) + (y - cy).pow(2) <= RADIUS * RADIUS;
        if hit {
            self.colour += 1;
        }
        hit
    }
}

// draws a frame into `pixels`, which holds `f.w * f.h`: the water in
// bands, one thread each, spawned and joined right here on whatever thread
// is drawing, then the ball and the bar
fn draw(pixels: &mut [u32], f: &Frame) {
    let (w, h) = (f.w, f.h);
    let t = f.frame as f32 * 0.04;
    let (cx, cy) = (f.x, f.y);
    let rows = h.div_ceil(f.threads.max(1)).max(1);
    std::thread::scope(|s| {
        for (i, band) in pixels.chunks_mut(rows * w).enumerate() {
            s.spawn(move || ripple(band, w, i * rows, t, cx, cy));
            THREADS_SPAWNED.fetch_add(1, Ordering::Relaxed);
        }
    });
    let (cx, cy) = (f.x as i32, f.y as i32);
    for dy in -RADIUS..=RADIUS {
        let y = cy + dy;
        if y < 0 || y >= h as i32 {
            continue;
        }
        let y = y as usize;
        let half = ((RADIUS * RADIUS - dy * dy) as f32).sqrt() as i32;
        let x0 = (cx - half).max(0) as usize;
        let x1 = (cx + half).min(w as i32 - 1) as usize;
        if x0 <= x1 {
            pixels[y * w + x0..y * w + x1 + 1].fill(f.colour);
        }
    }
    // a bar at the bottom that grows with the frame count
    let bar = (f.frame % 240) as usize * w / 240;
    for y in h.saturating_sub(6)..h {
        pixels[y * w..y * w + bar].fill(0xff_ff_ff);
    }
}

// one band of the water: y from `y0`, `band` holds whole rows
fn ripple(band: &mut [u32], w: usize, y0: usize, t: f32, cx: f32, cy: f32) {
    for (r, row) in band.chunks_mut(w).enumerate() {
        let y = (y0 + r) as f32;
        for (x, px) in row.iter_mut().enumerate() {
            let x = x as f32;
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            let v = (d * 0.06 - t * 2.0).sin() + (x * 0.02 + t).sin() + (y * 0.025 - t * 1.3).sin();
            // -3..3 into a dark blue to teal
            let s = (v + 3.0) / 6.0;
            let r = (12.0 + 30.0 * s) as u32;
            let g = (24.0 + 90.0 * s) as u32;
            let b = (60.0 + 120.0 * s) as u32;
            *px = r << 16 | g << 8 | b;
        }
    }
}

static SCENE: Mutex<Scene> = Mutex::new(Scene::empty());
// the buffer a frame is drawn into before it's swapped in, kept between frames
static BACK: Mutex<Vec<u32>> = Mutex::new(Vec::new());
// a frame drawn and not shown yet: the clock skips rather than queue
static PENDING: AtomicBool = AtomicBool::new(false);
static SKIPPED: AtomicU32 = AtomicU32::new(0);

// ---- what the hosts share ----

static PRESENTED: AtomicU32 = AtomicU32::new(0);
static LATENCY_NS: AtomicU64 = AtomicU64::new(0);
static LATENCY_N: AtomicU32 = AtomicU32::new(0);
static HIT: AtomicBool = AtomicBool::new(false);
static THREADS_SPAWNED: AtomicU64 = AtomicU64::new(0);
// one frame drawn by one thread and by all of them, in microseconds
static DRAW_ONE_US: AtomicU64 = AtomicU64::new(0);
static DRAW_ALL_US: AtomicU64 = AtomicU64::new(0);
// when the frame being shown was finished, by whichever thread drew it
static DRAWN_AT: Mutex<Option<Instant>> = Mutex::new(None);
// what the run went through, one line each, printed at the end
static SHOWN: Mutex<Vec<String>> = Mutex::new(Vec::new());
// the host's threads our callbacks ran on
static HOST_THREADS: Mutex<Vec<std::thread::ThreadId>> = Mutex::new(Vec::new());
static HOST_CALLS: AtomicU32 = AtomicU32::new(0);

fn shows(line: impl Into<String>) {
    SHOWN.lock().unwrap().push(line.into());
}

// a callback of ours is running on a thread the host made
fn on_host_thread() {
    HOST_CALLS.fetch_add(1, Ordering::Relaxed);
    let id = std::thread::current().id();
    let mut seen = HOST_THREADS.lock().unwrap();
    if !seen.contains(&id) {
        seen.push(id);
    }
}
// the run so far, and the stretch since the last rate line: when it began,
// frames shown, nanoseconds spent drawing
static STARTED: Mutex<Option<Instant>> = Mutex::new(None);
static STRETCH: Mutex<(Option<Instant>, u32, u64, u32)> = Mutex::new((None, 0, 0, 0));
static DRAW_NS: AtomicU64 = AtomicU64::new(0);
const RATE_EVERY: Duration = Duration::from_secs(5);

// draws the next frame, called wherever the host's clock ticks, and says
// whether there is one to show. Nothing is held while drawing, so the main
// thread keeps the window going however long a frame takes, and a frame
// the main thread hasn't shown yet means this tick is skipped, not queued
fn tick() -> bool {
    if PENDING.swap(true, Ordering::AcqRel) {
        SKIPPED.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let Some(f) = SCENE.lock().unwrap().advance() else {
        PENDING.store(false, Ordering::Release);
        return false;
    };
    let mut back = std::mem::take(&mut *BACK.lock().unwrap());
    back.resize(f.w * f.h, 0);
    let t = Instant::now();
    draw(&mut back, &f);
    DRAW_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    {
        let mut scene = SCENE.lock().unwrap();
        // unless the window changed size meanwhile
        if (scene.w as usize, scene.h as usize) == (f.w, f.h) {
            std::mem::swap(&mut scene.pixels, &mut back);
        }
    }
    *BACK.lock().unwrap() = back;
    *DRAWN_AT.lock().unwrap() = Some(Instant::now());
    true
}

// the main thread has put a frame on screen
fn presented() -> u32 {
    PENDING.store(false, Ordering::Release);
    if let Some(at) = DRAWN_AT.lock().unwrap().take() {
        LATENCY_NS.fetch_add(at.elapsed().as_nanos() as u64, Ordering::Relaxed);
        LATENCY_N.fetch_add(1, Ordering::Relaxed);
    }
    let shown = PRESENTED.fetch_add(1, Ordering::Relaxed) + 1;
    let now = Instant::now();
    STARTED.lock().unwrap().get_or_insert(now);
    // the rate, every few seconds
    let mut stretch = STRETCH.lock().unwrap();
    let drawn = DRAW_NS.load(Ordering::Relaxed);
    let skipped = SKIPPED.load(Ordering::Relaxed);
    match stretch.0 {
        None => *stretch = (Some(now), shown, drawn, skipped),
        Some(since) if now - since >= RATE_EVERY => {
            let secs = (now - since).as_secs_f64();
            let frames = shown - stretch.1;
            let missed = skipped - stretch.3;
            println!(
                "{frames} frames in {secs:.1} s: {:.1} per second, {:.1} ms drawing each{}",
                frames as f64 / secs,
                (drawn - stretch.2) as f64 / frames.max(1) as f64 / 1e6,
                if missed > 0 {
                    format!(", {missed} ticks skipped")
                } else {
                    String::new()
                }
            );
            *stretch = (Some(now), shown, drawn, skipped);
        }
        Some(_) => {}
    }
    shown
}

fn clicked(x: i32, y: i32) {
    let hit = SCENE.lock().unwrap().click(x, y);
    println!(
        "click at ({x}, {y}): {}",
        if hit { "hit the ball" } else { "missed" }
    );
    if hit {
        HIT.store(true, Ordering::Relaxed);
    }
}

enum Auto {
    Nothing,
    Click(i32, i32),
    Quit,
}

// the self-test's schedule, by frames shown
fn auto_step(frame: u32) -> Auto {
    if stay() {
        if frame == THREADS_FRAME {
            time_threads();
        }
        return Auto::Nothing;
    }
    match frame {
        CLICK_FRAME => {
            let (x, y) = SCENE.lock().unwrap().centre();
            Auto::Click(x, y)
        }
        THREADS_FRAME => {
            time_threads();
            Auto::Nothing
        }
        LAST_FRAME => Auto::Quit,
        _ => Auto::Nothing,
    }
}

// the same frame drawn by one thread and by all of them, while the host's
// loop is running and its callback thread keeps drawing the real ones
fn time_threads() {
    let f = SCENE.lock().unwrap().frame();
    if f.w == 0 {
        return;
    }
    let mut scratch = vec![0u32; f.w * f.h];
    let t = Instant::now();
    draw(&mut scratch, &Frame { threads: 1, ..f });
    let one = t.elapsed();
    let t = Instant::now();
    draw(&mut scratch, &f);
    let many = t.elapsed();
    DRAW_ONE_US.store(one.as_micros() as u64, Ordering::Relaxed);
    DRAW_ALL_US.store(many.as_micros() as u64, Ordering::Relaxed);
    println!(
        "one frame of {}x{}: {:.1} ms by one thread, {:.1} ms by {} threads",
        f.w,
        f.h,
        one.as_secs_f64() * 1000.0,
        many.as_secs_f64() * 1000.0,
        f.threads
    );
}

fn report(how: &str) {
    let frames = PRESENTED.load(Ordering::Relaxed);
    let n = LATENCY_N.load(Ordering::Relaxed);
    let spawned = THREADS_SPAWNED.load(Ordering::Relaxed);
    let calls = HOST_CALLS.load(Ordering::Relaxed);
    let host_threads = HOST_THREADS.lock().unwrap().len();
    if calls > 0 {
        shows(format!(
            "{calls} callbacks ran on {host_threads} thread{} the host made, each with std's thread state, drawing the frames there",
            if host_threads == 1 { "" } else { "s" }
        ));
    }
    shows(format!(
        "{spawned} threads spawned and joined by the thread drawing the frames, {} per frame",
        SCENE.lock().unwrap().threads
    ));
    println!("\n{how}");
    println!("what this run went through:");
    for line in SHOWN.lock().unwrap().iter() {
        println!("  - {line}");
    }
    let secs = STARTED
        .lock()
        .unwrap()
        .map_or(0.0, |t| t.elapsed().as_secs_f64());
    println!(
        "frames shown: {frames} in {secs:.1} s, {:.1} per second, {:.1} ms drawing each",
        frames as f64 / secs.max(1e-9),
        DRAW_NS.load(Ordering::Relaxed) as f64 / frames.max(1) as f64 / 1e6
    );
    if n > 0 {
        println!(
            "frame finished on the host's thread to shown by the main thread: {:.0} us on average over {n}",
            LATENCY_NS.load(Ordering::Relaxed) as f64 / n as f64 / 1000.0
        );
    }
    if stay() {
        return;
    }
    assert!(frames >= CLICK_FRAME + 10, "only {frames} frames shown");
    assert!(
        HIT.load(Ordering::Relaxed),
        "the click never reached the window"
    );
    let s = SCENE.lock().unwrap();
    let (cx, cy) = s.centre();
    let at = s.pixels[cy as usize * s.w as usize + cx as usize];
    assert_eq!(at, s.colour(), "the pixel at the ball's centre");
    println!(
        "the ball is {} now, as the pixel at its centre says",
        COLOURS[s.colour % COLOURS.len()].1
    );
    assert!(
        spawned >= frames as u64,
        "the frames weren't drawn by threads"
    );
    assert!(
        DRAW_ALL_US.load(Ordering::Relaxed) > 0,
        "the timing never ran"
    );
    println!("\nhost gui ok");
}

fn main() {
    if ape::is_linux() {
        linux::run();
    } else if ape::is_windows() {
        #[cfg(target_arch = "x86_64")]
        win::run();
    } else if ape::is_xnu() {
        #[cfg(target_arch = "aarch64")]
        mac::run();
    } else {
        println!("no GUI on this host, skipping");
    }
}

// ---- Linux: winit and softbuffer as they are ----

mod linux {
    use super::*;
    use std::num::NonZeroU32;
    use std::rc::Rc;
    use std::time::Duration;

    use winit::application::ApplicationHandler;
    use winit::dpi::LogicalSize;
    use winit::event::{ElementState, MouseButton, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
    use winit::keyboard::{Key, NamedKey};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::{Window, WindowId};

    // one frame has been drawn
    struct Tick;

    struct App {
        proxy: EventLoopProxy<Tick>,
        window: Option<Rc<Window>>,
        surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
        cursor: (f64, f64),
        ticking: bool,
    }

    impl App {
        fn present(&mut self) {
            let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
                return;
            };
            let size = window.inner_size();
            let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
            else {
                return;
            };
            let mut scene = SCENE.lock().unwrap();
            if scene.w != size.width || scene.h != size.height {
                scene.resize(size.width, size.height);
            }
            surface.resize(w, h).unwrap();
            let mut buffer = surface.buffer_mut().unwrap();
            buffer.copy_from_slice(&scene.pixels);
            drop(scene);
            buffer.present().unwrap();
        }
    }

    impl ApplicationHandler<Tick> for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attrs = Window::default_attributes()
                .with_title(TITLE)
                .with_inner_size(LogicalSize::new(WIDTH, HEIGHT));
            let window = Rc::new(event_loop.create_window(attrs).expect("a window"));
            let backend = match window.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Wayland(_)) => "Wayland (libwayland-client, libxkbcommon)",
                Ok(RawWindowHandle::Xlib(_)) | Ok(RawWindowHandle::Xcb(_)) => {
                    "X11 (libX11, libxcb, libxkbcommon)"
                }
                _ => "an unknown backend",
            };
            shows("winit 0.30 and softbuffer 0.4 as published, no cosmo code in either");
            shows(format!(
                "the window is on {backend}, libraries winit opened itself through libloading, which is plain dlopen here"
            ));
            shows("the frame clock is a thread of ours, since nothing on this host calls back");
            let context = softbuffer::Context::new(window.clone()).expect("a softbuffer context");
            self.surface =
                Some(softbuffer::Surface::new(&context, window.clone()).expect("a surface"));
            let size = window.inner_size();
            SCENE.lock().unwrap().resize(size.width, size.height);
            self.window = Some(window);
            if !self.ticking {
                self.ticking = true;
                // the clock: a thread of ours, since nothing on this host
                // calls back
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let period = Duration::from_micros(16_667);
                    let mut next = Instant::now() + period;
                    loop {
                        std::thread::sleep(next.saturating_duration_since(Instant::now()));
                        next = next.max(Instant::now() - period) + period;
                        if tick() && proxy.send_event(Tick).is_err() {
                            break;
                        }
                    }
                });
            }
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, _: Tick) {
            self.present();
            match auto_step(presented()) {
                // winit has no way to put a click into the queue, so the
                // self-test's click goes where a real one would arrive
                Auto::Click(x, y) => {
                    shows(
                        "the self-test's click went straight to the handler, winit can't post one",
                    );
                    clicked(x, y)
                }
                Auto::Quit => event_loop.exit(),
                Auto::Nothing => {}
            }
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::RedrawRequested => self.present(),
                WindowEvent::CursorMoved { position, .. } => self.cursor = (position.x, position.y),
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } => {
                    clicked(self.cursor.0 as i32, self.cursor.1 as i32);
                }
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && event.logical_key == Key::Named(NamedKey::Escape) =>
                {
                    event_loop.exit()
                }
                _ => {}
            }
        }
    }

    pub fn run() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
            println!("no display, skipping");
            return;
        }
        let event_loop = match EventLoop::<Tick>::with_user_event().build() {
            Ok(l) => l,
            Err(e) => {
                println!("no event loop: {e}, skipping");
                return;
            }
        };
        event_loop.set_control_flow(ControlFlow::Wait);
        let mut app = App {
            proxy: event_loop.create_proxy(),
            window: None,
            surface: None,
            cursor: (0.0, 0.0),
            ticking: false,
        };
        event_loop.run_app(&mut app).expect("the event loop");
        report("Linux: winit and softbuffer as they are");
    }
}

// ---- Windows: user32 and Direct3D 11 through the windows crate ----

#[cfg(target_arch = "x86_64")]
mod win {
    use super::*;
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::sync::atomic::AtomicUsize;

    use windows::Win32::Foundation::{
        HANDLE, HMODULE, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT, WPARAM,
    };
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::Console::{
        CONSOLE_MODE, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        CreateTimerQueueTimer, DeleteTimerQueueTimer, WT_EXECUTEDEFAULT,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{Interface, PCWSTR};

    const WM_FRAME: u32 = WM_APP + 1;

    static HWND_: AtomicUsize = AtomicUsize::new(0);
    static WNDPROC_CALLS: AtomicU32 = AtomicU32::new(0);
    // a frame with the ball's new colour is to be read back from Direct3D
    static CHECK_D3D: AtomicBool = AtomicBool::new(false);

    struct D3d {
        context: ID3D11DeviceContext,
        swapchain: IDXGISwapChain1,
        w: u32,
        h: u32,
    }

    thread_local! {
        // main thread only
        static D3D: RefCell<Option<D3d>> = const { RefCell::new(None) };
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    // where standard output goes: the parent shell's console, joined at
    // startup since this is a GUI subsystem program, or a pipe or file
    fn stdout_is() -> &'static str {
        let mut mode = CONSOLE_MODE(0);
        match unsafe { GetStdHandle(STD_OUTPUT_HANDLE) } {
            Ok(h) if unsafe { GetConsoleMode(h, &mut mode) }.is_ok() => {
                "the parent shell's console, joined at startup"
            }
            Ok(_) => "a pipe or a file, kept as handed over",
            Err(_) => "nowhere, nothing was handed over",
        }
    }

    fn hwnd() -> HWND {
        HWND(HWND_.load(Ordering::Relaxed) as *mut c_void)
    }

    fn client_size(hwnd: HWND) -> (u32, u32) {
        let mut r = RECT::default();
        unsafe { GetClientRect(hwnd, &mut r) }.unwrap();
        (
            (r.right - r.left).max(0) as u32,
            (r.bottom - r.top).max(0) as u32,
        )
    }

    // a device, hardware or WARP, and a flip model swap chain on the window
    fn d3d_init(hwnd: HWND) -> windows::core::Result<(D3d, String)> {
        let (w, h) = client_size(hwnd);
        let mut device = None;
        let mut context = None;
        let mut kind = "hardware";
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        let hw = unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        if hw.is_err() {
            kind = "WARP";
            unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    HMODULE::default(),
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )?
            };
        }
        let device: ID3D11Device = device.unwrap();
        let context = context.unwrap();
        let dxgi: IDXGIDevice = device.cast()?;
        let adapter = unsafe { dxgi.GetAdapter() }?;
        let desc = unsafe { adapter.GetDesc() }?;
        let name = String::from_utf16_lossy(&desc.Description)
            .trim_end_matches('\0')
            .to_string();
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }?;
        let sc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };
        let swapchain = unsafe { factory.CreateSwapChainForHwnd(&device, hwnd, &sc, None, None) }?;
        Ok((
            D3d {
                context,
                swapchain,
                w,
                h,
            },
            format!("{kind} device on {name}"),
        ))
    }

    // the frame onto the back buffer, which is handed back
    fn d3d_upload(d: &mut D3d, scene: &Scene) -> windows::core::Result<ID3D11Texture2D> {
        unsafe {
            if (d.w, d.h) != (scene.w, scene.h) {
                d.swapchain.ResizeBuffers(
                    0,
                    scene.w,
                    scene.h,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )?;
                d.w = scene.w;
                d.h = scene.h;
            }
            let back: ID3D11Texture2D = d.swapchain.GetBuffer(0)?;
            d.context.UpdateSubresource(
                &back,
                0,
                None,
                scene.pixels.as_ptr() as *const c_void,
                scene.w * 4,
                0,
            );
            Ok(back)
        }
    }

    fn d3d_present(d: &mut D3d, scene: &Scene) -> windows::core::Result<()> {
        d3d_upload(d, scene)?;
        unsafe { d.swapchain.Present(1, DXGI_PRESENT(0)).ok() }
    }

    // reads the pixel at (x, y) back from the back buffer, through a
    // staging texture, so the Direct3D path is checked end to end; before
    // Present, which in the flip model moves on to the next buffer
    fn d3d_read_pixel(
        d: &D3d,
        back: &ID3D11Texture2D,
        x: u32,
        y: u32,
    ) -> windows::core::Result<u32> {
        unsafe {
            let device = d.context.GetDevice()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut staging = None;
            device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            let staging: ID3D11Texture2D = staging.unwrap();
            d.context.CopyResource(&staging, back);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            d.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let row = (mapped.pData as *const u8).add(y as usize * mapped.RowPitch as usize)
                as *const u32;
            let px = *row.add(x as usize) & 0xff_ff_ff;
            d.context.Unmap(&staging, 0);
            Ok(px)
        }
    }

    fn gdi_present(hwnd: HWND, scene: &Scene) {
        let mut ps = PAINTSTRUCT::default();
        unsafe {
            let hdc = BeginPaint(hwnd, &mut ps);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: scene.w as i32,
                    biHeight: -(scene.h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            StretchDIBits(
                hdc,
                0,
                0,
                scene.w as i32,
                scene.h as i32,
                0,
                0,
                scene.w as i32,
                scene.h as i32,
                Some(scene.pixels.as_ptr() as *const c_void),
                &bmi,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            let _ = EndPaint(hwnd, &ps);
        }
    }

    // the frame after the click: once it shows the new colour, the pixel at
    // the ball's centre is read back from the back buffer before it goes up;
    // true when a frame was presented here
    fn check_d3d() -> bool {
        let checked = D3D.with(|d| {
            let mut d = d.borrow_mut();
            let Some(d) = d.as_mut() else { return Ok(None) };
            let scene = SCENE.lock().unwrap();
            let (cx, cy) = scene.centre();
            let want = scene.colour();
            if scene.w == 0 || scene.pixels[cy as usize * scene.w as usize + cx as usize] != want {
                return Ok(None);
            }
            let back = d3d_upload(d, &scene)?;
            let px = d3d_read_pixel(d, &back, cx as u32, cy as u32)?;
            unsafe { d.swapchain.Present(1, DXGI_PRESENT(0)).ok()? };
            Ok::<_, windows::core::Error>(Some((px, want)))
        });
        match checked {
            Ok(Some((px, want))) => {
                CHECK_D3D.store(false, Ordering::Relaxed);
                assert_eq!(px, want, "the back buffer's pixel at the ball's centre");
                println!("the Direct3D back buffer has the new colour: {px:06x}");
                shows(format!(
                    "the back buffer was read back through a staging texture: {px:06x}, the new colour"
                ));
                true
            }
            Ok(None) => false,
            Err(e) => {
                CHECK_D3D.store(false, Ordering::Relaxed);
                println!("couldn't read the back buffer: {e}");
                false
            }
        }
    }

    // puts the current frame on screen, Direct3D or GDI
    fn present(hwnd: HWND) {
        let scene = SCENE.lock().unwrap();
        if scene.w == 0 {
            return;
        }
        let done = D3D.with(|d| {
            let mut d = d.borrow_mut();
            match d.as_mut() {
                Some(d3d) => {
                    if let Err(e) = d3d_present(d3d, &scene) {
                        println!("Direct3D present failed: {e}, GDI from here on");
                        *d = None;
                        false
                    } else {
                        true
                    }
                }
                None => false,
            }
        });
        if !done {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    }

    // the clock: a timer queue callback, on a thread of Windows' pool
    #[ape::host_callback]
    fn on_timer(_ctx: *mut c_void, _fired: bool) {
        on_host_thread();
        if tick() {
            let _ = unsafe { PostMessageW(Some(hwnd()), WM_FRAME, WPARAM(0), LPARAM(0)) };
        }
    }

    #[ape::host_callback]
    fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        WNDPROC_CALLS.fetch_add(1, Ordering::Relaxed);
        match msg {
            WM_FRAME => {
                // the readback presents the frame itself when it runs
                let shown = CHECK_D3D.load(Ordering::Relaxed) && check_d3d();
                if !shown {
                    present(hwnd);
                }
                match auto_step(presented()) {
                    Auto::Click(x, y) => unsafe {
                        // a real click, through the queue
                        let pos = LPARAM((y as isize) << 16 | (x as isize & 0xffff));
                        let _ = PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), pos);
                        let _ = PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), pos);
                    },
                    Auto::Quit => unsafe {
                        let _ = DestroyWindow(hwnd);
                    },
                    Auto::Nothing => {}
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let scene = SCENE.lock().unwrap();
                if D3D.with(|d| d.borrow().is_none()) && scene.w > 0 {
                    gdi_present(hwnd, &scene);
                } else {
                    unsafe {
                        let _ = ValidateRect(Some(hwnd), None);
                    }
                }
                LRESULT(0)
            }
            WM_SIZE => {
                let w = (lparam.0 & 0xffff) as u32;
                let h = ((lparam.0 >> 16) & 0xffff) as u32;
                SCENE.lock().unwrap().resize(w, h);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (lparam.0 & 0xffff) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
                if !stay() {
                    shows(
                        "the self-test's click was posted to the window's queue as WM_LBUTTONDOWN and came back through user32",
                    );
                }
                clicked(x, y);
                if HIT.load(Ordering::Relaxed) && !stay() {
                    CHECK_D3D.store(true, Ordering::Relaxed);
                }
                LRESULT(0)
            }
            WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    pub fn run() {
        unsafe {
            let instance = GetModuleHandleW(None).expect("GetModuleHandleW");
            let class_name = wide("RustApeHostGui");
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: std::mem::transmute::<*const c_void, WNDPROC>(window_proc::ptr().0),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).expect("a cursor"),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                ..Default::default()
            };
            assert!(RegisterClassExW(&class) != 0, "RegisterClassExW");
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: WIDTH as i32,
                bottom: HEIGHT as i32,
            };
            let _ = AdjustWindowRect(&mut rect, WS_OVERLAPPEDWINDOW, false);
            let title = wide(TITLE);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .expect("CreateWindowExW");
            HWND_.store(hwnd.0 as usize, Ordering::Relaxed);
            let (w, h) = client_size(hwnd);
            SCENE.lock().unwrap().resize(w, h);
            shows(
                "user32, gdi32, d3d11 and dxgi through the windows crate as published, compiled as Windows code",
            );
            shows(
                "the window procedure is an #[ape::host_callback], handed to RegisterClassExW as any WNDPROC",
            );
            shows(
                "the frame clock is CreateTimerQueueTimer, its callback runs on Windows' thread pool",
            );
            match d3d_init(hwnd) {
                Ok((d, how)) => {
                    shows(format!(
                        "frames go up through Direct3D 11 COM interfaces, a flip model swap chain on a {how}"
                    ));
                    D3D.with(|slot| *slot.borrow_mut() = Some(d));
                }
                Err(e) => {
                    shows(format!(
                        "frames go up through GDI's StretchDIBits; no Direct3D 11 in this session ({e})"
                    ));
                }
            }
            shows(format!(
                "built for the GUI subsystem, no console of its own; this output goes to {}",
                stdout_is()
            ));
            let mut timer = HANDLE::default();
            let callback = std::mem::transmute::<
                *const c_void,
                windows::Win32::System::Threading::WAITORTIMERCALLBACK,
            >(on_timer::ptr().0);
            CreateTimerQueueTimer(&mut timer, None, callback, None, 16, 16, WT_EXECUTEDEFAULT)
                .expect("CreateTimerQueueTimer");

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // waits for a callback in flight
            let _ = DeleteTimerQueueTimer(None, timer, Some(INVALID_HANDLE_VALUE));
            D3D.with(|slot| *slot.borrow_mut() = None);
        }
        shows(format!(
            "user32 called the window procedure {} times",
            WNDPROC_CALLS.load(Ordering::Relaxed)
        ));
        report("Windows: user32 and Direct3D 11 through the windows crate");
    }
}

// ---- macOS: AppKit, CoreVideo and CoreGraphics through the Objective-C runtime ----

#[cfg(target_arch = "aarch64")]
mod mac {
    use super::*;
    use std::ffi::{CStr, c_char, c_void};
    use std::sync::atomic::AtomicUsize;

    use ape::host::{Callback, Library};

    type Id = *mut c_void;
    type Sel = *mut c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Size {
        w: f64,
        h: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Size,
    }

    #[ape::host_library(static OBJC = macos ["/usr/lib/libobjc.A.dylib"])]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
        fn objc_registerClassPair(cls: Id);
        fn class_addMethod(cls: Id, sel: Sel, imp: Callback, types: *const c_char) -> bool;
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    #[ape::host_library(static CG = macos ["/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics"])]
    unsafe extern "C" {
        fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
        fn CGDataProviderCreateWithData(
            info: *mut c_void,
            data: *const c_void,
            size: usize,
            release: Callback,
        ) -> *mut c_void;
        fn CGDataProviderRelease(provider: *mut c_void);
        fn CGImageCreate(
            width: usize,
            height: usize,
            bits_per_component: usize,
            bits_per_pixel: usize,
            bytes_per_row: usize,
            space: *mut c_void,
            bitmap_info: u32,
            provider: *mut c_void,
            decode: *const f64,
            interpolate: bool,
            intent: i32,
        ) -> *mut c_void;
        fn CGImageRelease(image: *mut c_void);
    }

    #[ape::host_library(static CV = macos ["/System/Library/Frameworks/CoreVideo.framework/CoreVideo"])]
    unsafe extern "C" {
        fn CVDisplayLinkCreateWithActiveCGDisplays(out: *mut *mut c_void) -> i32;
        fn CVDisplayLinkSetOutputCallback(
            link: *mut c_void,
            callback: Callback,
            ctx: *mut c_void,
        ) -> i32;
        fn CVDisplayLinkStart(link: *mut c_void) -> i32;
        fn CVDisplayLinkStop(link: *mut c_void) -> i32;
        fn CVDisplayLinkRelease(link: *mut c_void);
    }

    // kCGBitmapByteOrder32Little | kCGImageAlphaNoneSkipFirst: a pixel is
    // the u32 the scene draws, blue in the low byte
    const BITMAP_INFO: u32 = 2 << 12 | 6;
    const NS_LEFT_MOUSE_DOWN: u64 = 1;
    const NS_KEY_DOWN: u64 = 10;
    const NS_APPLICATION_DEFINED: u64 = 15;
    const KEY_ESCAPE: u16 = 53;

    static SEND: AtomicUsize = AtomicUsize::new(0);
    static APP: AtomicUsize = AtomicUsize::new(0);
    static WINDOW: AtomicUsize = AtomicUsize::new(0);
    static VIEW: AtomicUsize = AtomicUsize::new(0);
    static DELEGATE: AtomicUsize = AtomicUsize::new(0);
    static COLOUR_SPACE: AtomicUsize = AtomicUsize::new(0);
    static QUIT: AtomicBool = AtomicBool::new(false);
    static FRAME_READY: AtomicU32 = AtomicU32::new(0);
    static RELEASED: AtomicU32 = AtomicU32::new(0);

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

    fn id(a: &AtomicUsize) -> Id {
        a.load(Ordering::Relaxed) as Id
    }

    fn view_size() -> (u32, u32) {
        let bounds = send::<extern "C" fn(Id, Sel) -> Rect>()(id(&VIEW), sel(c"bounds"));
        (bounds.size.w as u32, bounds.size.h as u32)
    }

    // wakes the event loop, which is waiting for an event
    fn post_wakeup() {
        let event = send::<extern "C" fn(Id, Sel, u64, Point, u64, f64, i64, Id, i16, i64, i64) -> Id>()(
            class(c"NSEvent"),
            sel(c"otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:"),
            NS_APPLICATION_DEFINED, Point { x: 0.0, y: 0.0 }, 0, 0.0, 0, std::ptr::null_mut(), 0, 0, 0,
        );
        send::<extern "C" fn(Id, Sel, Id, bool)>()(
            id(&APP),
            sel(c"postEvent:atStart:"),
            event,
            true,
        );
    }

    // CVDisplayLinkOutputCallback, on CoreVideo's thread: draws the
    // frame there and tells the main thread
    #[ape::host_callback]
    fn display_link(
        _link: *mut c_void,
        _now: *const c_void,
        _out: *const c_void,
        _flags_in: u64,
        _flags_out: *mut u64,
        _ctx: *mut c_void,
    ) -> i32 {
        on_host_thread();
        if tick() {
            send::<extern "C" fn(Id, Sel, Sel, Id, bool)>()(
                id(&DELEGATE),
                sel(c"performSelectorOnMainThread:withObject:waitUntilDone:"),
                sel(c"frameReady"),
                std::ptr::null_mut(),
                false,
            );
        }
        0
    }

    // CGDataProviderReleaseDataCallback: CoreGraphics is done with a
    // frame's pixels
    #[ape::host_callback]
    fn release_pixels(_info: *mut c_void, data: *const c_void, size: usize) {
        RELEASED.fetch_add(1, Ordering::Relaxed);
        drop(unsafe { Vec::from_raw_parts(data as *mut u32, size / 4, size / 4) });
    }

    // the current frame as a CGImage, handed to the view's layer
    fn present() {
        let scene = SCENE.lock().unwrap();
        if scene.w == 0 {
            return;
        }
        let (w, h) = (scene.w as usize, scene.h as usize);
        let pixels = scene.pixels.clone();
        drop(scene);
        let len = pixels.len() * 4;
        let data = pixels.leak().as_ptr() as *const c_void;
        unsafe {
            let provider = CGDataProviderCreateWithData(
                std::ptr::null_mut(),
                data,
                len,
                release_pixels::ptr(),
            );
            let image = CGImageCreate(
                w,
                h,
                8,
                32,
                w * 4,
                id(&COLOUR_SPACE),
                BITMAP_INFO,
                provider,
                std::ptr::null(),
                false,
                0,
            );
            CGDataProviderRelease(provider);
            let layer = msg0(id(&VIEW), c"layer");
            msg1(layer, c"setContents:", image as Id);
            CGImageRelease(image);
        }
    }

    // -[RustApeGuiDelegate frameReady], on the main thread
    #[ape::host_callback]
    fn frame_ready(_this: Id, _sel: Sel) {
        FRAME_READY.fetch_add(1, Ordering::Relaxed);
        let (w, h) = view_size();
        {
            let mut scene = SCENE.lock().unwrap();
            if (scene.w, scene.h) != (w, h) {
                scene.resize(w, h);
            }
        }
        present();
        match auto_step(presented()) {
            Auto::Click(x, y) => {
                // a real click, through the queue; AppKit's y goes up
                let window = id(&WINDOW);
                let number = send::<extern "C" fn(Id, Sel) -> i64>()(window, sel(c"windowNumber"));
                let at = Point {
                    x: x as f64,
                    y: (h as i32 - y) as f64,
                };
                let event = send::<extern "C" fn(Id, Sel, u64, Point, u64, f64, i64, Id, i64, i64, f32) -> Id>()(
                    class(c"NSEvent"),
                    sel(c"mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:"),
                    NS_LEFT_MOUSE_DOWN, at, 0, 0.0, number, std::ptr::null_mut(), 0, 1, 1.0,
                );
                send::<extern "C" fn(Id, Sel, Id, bool)>()(
                    id(&APP),
                    sel(c"postEvent:atStart:"),
                    event,
                    false,
                );
            }
            Auto::Quit => {
                QUIT.store(true, Ordering::Relaxed);
                post_wakeup();
            }
            Auto::Nothing => {}
        }
    }

    // -[RustApeGuiDelegate windowWillClose:]
    #[ape::host_callback]
    fn window_will_close(_this: Id, _sel: Sel, _note: Id) {
        QUIT.store(true, Ordering::Relaxed);
    }

    pub fn run() {
        let (Ok(objc), Ok(_), Ok(_)) = (OBJC.load(), CG.load(), CV.load()) else {
            println!("no Objective-C runtime, skipping");
            return;
        };
        let appkit = match Library::open("/System/Library/Frameworks/AppKit.framework/AppKit") {
            Ok(lib) => lib,
            Err(e) => {
                println!("no AppKit on this host, skipping: {e}");
                return;
            }
        };
        SEND.store(
            objc.sym("objc_msgSend").expect("objc_msgSend") as usize,
            Ordering::Relaxed,
        );
        // a variable of the framework, read as one
        let default_mode = unsafe {
            *(appkit
                .sym("NSDefaultRunLoopMode")
                .expect("NSDefaultRunLoopMode") as *const Id)
        };
        std::mem::forget(appkit);
        shows(
            "libobjc, AppKit, CoreVideo and CoreGraphics opened at run time; objc_msgSend typed per call",
        );
        shows("NSDefaultRunLoopMode read as the variable of AppKit it is");
        shows(
            "RustApeGuiDelegate registered with the Objective-C runtime, its methods Rust: windowWillClose:, frameReady",
        );
        shows(
            "the frame clock is CVDisplayLink, its callback runs on CoreVideo's thread and draws there",
        );
        shows("the main thread is told with performSelectorOnMainThread:, no polling");

        unsafe {
            let pool = objc_autoreleasePoolPush();
            COLOUR_SPACE.store(CGColorSpaceCreateDeviceRGB() as usize, Ordering::Relaxed);

            let app = msg0(class(c"NSApplication"), c"sharedApplication");
            APP.store(app as usize, Ordering::Relaxed);
            send::<extern "C" fn(Id, Sel, i64) -> bool>()(app, sel(c"setActivationPolicy:"), 0);

            // the window's delegate: a class made here, its methods Rust
            let cls = objc_allocateClassPair(class(c"NSObject"), c"RustApeGuiDelegate".as_ptr(), 0);
            class_addMethod(
                cls,
                sel(c"windowWillClose:"),
                window_will_close::ptr(),
                c"v@:@".as_ptr(),
            );
            class_addMethod(cls, sel(c"frameReady"), frame_ready::ptr(), c"v@:".as_ptr());
            objc_registerClassPair(cls);
            let delegate = msg0(msg0(cls, c"alloc"), c"init");
            DELEGATE.store(delegate as usize, Ordering::Relaxed);

            let frame = Rect {
                origin: Point { x: 200.0, y: 200.0 },
                size: Size {
                    w: WIDTH as f64,
                    h: HEIGHT as f64,
                },
            };
            // titled, closable, miniaturizable, resizable; buffered; not deferred
            let window = send::<extern "C" fn(Id, Sel, Rect, u64, u64, bool) -> Id>()(
                msg0(class(c"NSWindow"), c"alloc"),
                sel(c"initWithContentRect:styleMask:backing:defer:"),
                frame,
                1 | 2 | 4 | 8,
                2,
                false,
            );
            WINDOW.store(window as usize, Ordering::Relaxed);
            msg1(window, c"setTitle:", nsstring(TITLE));
            msg1(window, c"setDelegate:", delegate);
            send::<extern "C" fn(Id, Sel, bool)>()(window, sel(c"setReleasedWhenClosed:"), false);
            let view = msg0(window, c"contentView");
            VIEW.store(view as usize, Ordering::Relaxed);
            send::<extern "C" fn(Id, Sel, bool)>()(view, sel(c"setWantsLayer:"), true);
            let (w, h) = view_size();
            SCENE.lock().unwrap().resize(w, h);
            present();

            msg0(app, c"finishLaunching");
            send::<extern "C" fn(Id, Sel, bool)>()(app, sel(c"activateIgnoringOtherApps:"), true);
            msg1(window, c"makeKeyAndOrderFront:", std::ptr::null_mut());

            // the clock: the display's own, on CoreVideo's thread
            let mut link = std::ptr::null_mut();
            assert_eq!(
                CVDisplayLinkCreateWithActiveCGDisplays(&mut link),
                0,
                "CVDisplayLinkCreateWithActiveCGDisplays"
            );
            assert_eq!(
                CVDisplayLinkSetOutputCallback(link, display_link::ptr(), std::ptr::null_mut()),
                0
            );
            assert_eq!(CVDisplayLinkStart(link), 0, "CVDisplayLinkStart");

            let distant_future = msg0(class(c"NSDate"), c"distantFuture");
            while !QUIT.load(Ordering::Relaxed) {
                let inner = objc_autoreleasePoolPush();
                let event = send::<extern "C" fn(Id, Sel, u64, Id, Id, bool) -> Id>()(
                    app,
                    sel(c"nextEventMatchingMask:untilDate:inMode:dequeue:"),
                    u64::MAX,
                    distant_future,
                    default_mode,
                    true,
                );
                if !event.is_null() {
                    let kind = send::<extern "C" fn(Id, Sel) -> u64>()(event, sel(c"type"));
                    if kind == NS_LEFT_MOUSE_DOWN {
                        let at = send::<extern "C" fn(Id, Sel) -> Point>()(
                            event,
                            sel(c"locationInWindow"),
                        );
                        let (_, h) = view_size();
                        if !stay() {
                            shows(
                                "the self-test's click was posted to NSApp's queue as an NSEvent and came back from nextEventMatchingMask:",
                            );
                        }
                        clicked(at.x as i32, h as i32 - at.y as i32);
                    } else if kind == NS_KEY_DOWN {
                        let code = send::<extern "C" fn(Id, Sel) -> u16>()(event, sel(c"keyCode"));
                        if code == KEY_ESCAPE {
                            QUIT.store(true, Ordering::Relaxed);
                        }
                    }
                    msg1(app, c"sendEvent:", event);
                }
                objc_autoreleasePoolPop(inner);
            }

            CVDisplayLinkStop(link);
            CVDisplayLinkRelease(link);
            msg0(window, c"close");
            objc_autoreleasePoolPop(pool);
        }
        shows(format!(
            "frameReady was called {} times by the runtime on the main thread",
            FRAME_READY.load(Ordering::Relaxed)
        ));
        shows(format!(
            "CoreGraphics freed {} frames' pixels through our release callback",
            RELEASED.load(Ordering::Relaxed)
        ));
        report("macOS: AppKit, CoreVideo and CoreGraphics through the Objective-C runtime");
    }
}
