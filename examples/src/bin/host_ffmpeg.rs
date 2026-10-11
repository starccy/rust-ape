//! Decoding video with the host's ffmpeg, through bindings that `cargo xtask
//! host-bindings` made from bindgen output (src/hostlibs/ffmpeg.rs). A big
//! library with big structs: the code below reaches into AVFormatContext,
//! AVCodecContext, AVPacket and AVFrame directly, and turns on frame
//! threading so libavcodec starts its own worker threads on the host.
//!
//! Three ways the library calls back into us are covered:
//!
//! - the log callback, which gets the format string and a `va_list` the
//!   host built; the callback both hands the list on to the host's own
//!   `av_log_format_line2` and reads it itself with `ape::host::VaList`,
//!   after `host_variadic!(av_log(..))` fed it known arguments
//! - a custom `AVIOContext`, so the demuxer reads the clip through our
//!   read and seek callbacks instead of a file
//! - frame threads, which run the decoder on threads the host started
//!
//! The struct layouts are those of the ffmpeg major the bindings were made
//! from, so the example skips on any other. The clip is a one second test
//! pattern, 30 frames of 320x240 H.264 that build.rs has ffmpeg make and
//! the binary carries; without an ffmpeg on the build machine there is no
//! clip and the decoding is skipped.

#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]

#[path = "../hostlibs/ffmpeg.rs"]
mod ffmpeg;

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use ffmpeg::*;

const CLIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/testsrc.mp4"));
// FFERRTAG('E','O','F',' ')
const AVERROR_EOF: c_int = -0x20464F45;

static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PARSED: Mutex<Option<String>> = Mutex::new(None);
static READS: AtomicUsize = AtomicUsize::new(0);
static SEEKS: AtomicUsize = AtomicUsize::new(0);

#[ape::host_callback]
fn log_line(ptr: *mut c_void, level: c_int, fmt: *const c_char, vl: *mut c_void) {
    let f = unsafe { CStr::from_ptr(fmt) }.to_string_lossy();
    if let Some(rest) = f.strip_prefix("ape-vatest ") {
        // "%d %s %f %lld %c\n", read with the host's va_list layout
        let mut list = unsafe { ape::host::VaList::from_host(vl) };
        let d: c_int = unsafe { list.arg() };
        let s: *const c_char = unsafe { list.arg() };
        let x: f64 = unsafe { list.arg() };
        let ll: i64 = unsafe { list.arg() };
        let c: c_int = unsafe { list.arg() };
        let s = unsafe { CStr::from_ptr(s) }.to_string_lossy();
        *PARSED.lock().unwrap() = Some(format!("{rest} -> {d} {s} {x} {ll} {}", c as u8 as char));
        return;
    }
    // everything else is formatted by the library, from the same list
    let list = unsafe { ape::host::VaList::from_host(vl) };
    let mut line = [0u8; 512];
    let mut prefix = 1;
    let n = unsafe {
        av_log_format_line2(
            ptr,
            level,
            fmt,
            list.as_ptr(),
            line.as_mut_ptr() as *mut c_char,
            line.len() as c_int,
            &mut prefix,
        )
    };
    if n > 0 {
        let n = (n as usize).min(line.len() - 1);
        LOG.lock()
            .unwrap()
            .push(String::from_utf8_lossy(&line[..n]).trim_end().to_string());
    }
}

// the demuxer reading the clip out of our memory
#[ape::host_callback]
fn read_packet(opaque: *mut c_void, buf: *mut u8, size: c_int) -> c_int {
    let pos = unsafe { &mut *(opaque as *mut usize) };
    let n = (CLIP.len() - *pos).min(size as usize);
    if n == 0 {
        return AVERROR_EOF;
    }
    unsafe { ptr::copy_nonoverlapping(CLIP.as_ptr().add(*pos), buf, n) };
    *pos += n;
    READS.fetch_add(1, Ordering::Relaxed);
    n as c_int
}

#[ape::host_callback]
fn seek_packet(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let pos = unsafe { &mut *(opaque as *mut usize) };
    SEEKS.fetch_add(1, Ordering::Relaxed);
    if whence as u32 & AVSEEK_SIZE != 0 {
        return CLIP.len() as i64;
    }
    let base = match whence & 3 {
        0 => 0,
        1 => *pos as i64,
        _ => CLIP.len() as i64,
    };
    let to = base + offset;
    if to < 0 || to > CLIP.len() as i64 {
        return -1;
    }
    *pos = to as usize;
    to
}

// decodes every video frame of `ic`, checking the count and order
fn decode(ic: *mut AVFormatContext) {
    let rc = unsafe { avformat_find_stream_info(ic, ptr::null_mut()) };
    assert!(rc >= 0, "avformat_find_stream_info: {rc}");

    let streams = unsafe { std::slice::from_raw_parts((*ic).streams, (*ic).nb_streams as usize) };
    let video = streams
        .iter()
        .copied()
        .find(|&s| unsafe { (*(*s).codecpar).codec_type } == AVMediaType_AVMEDIA_TYPE_VIDEO)
        .expect("a video stream");
    let par = unsafe { (*video).codecpar };
    let decoder = unsafe { avcodec_find_decoder((*par).codec_id) };
    assert!(!decoder.is_null(), "no decoder for the clip");
    let name = unsafe { CStr::from_ptr(avcodec_get_name((*par).codec_id)) }.to_string_lossy();
    println!(
        "{} streams, video is {name} {}x{}",
        streams.len(),
        unsafe { (*par).width },
        unsafe { (*par).height }
    );

    let ctx = unsafe { avcodec_alloc_context3(decoder) };
    assert!(!ctx.is_null());
    assert_eq!(unsafe { avcodec_parameters_to_context(ctx, par) }, 0);
    unsafe {
        (*ctx).thread_count = 4;
        (*ctx).thread_type = FF_THREAD_FRAME as i32;
    }
    let rc = unsafe { avcodec_open2(ctx, decoder, ptr::null_mut()) };
    assert_eq!(rc, 0, "avcodec_open2");
    println!("decoder opened with {} frame threads", unsafe {
        (*ctx).thread_count
    });

    let pkt = unsafe { av_packet_alloc() };
    let frame = unsafe { av_frame_alloc() };
    let mut frames = 0;
    let mut packets = 0;
    let mut last_pts = i64::MIN;
    let mut drain = |ctx: *mut AVCodecContext| loop {
        let rc = unsafe { avcodec_receive_frame(ctx, frame) };
        if rc < 0 {
            break;
        }
        let (w, pts) = unsafe { ((*frame).width, (*frame).pts) };
        assert_eq!(w, 320);
        assert!(
            pts > last_pts,
            "frames out of order: {pts} after {last_pts}"
        );
        last_pts = pts;
        frames += 1;
        unsafe { av_frame_unref(frame) };
    };
    loop {
        let rc = unsafe { av_read_frame(ic, pkt) };
        if rc < 0 {
            break;
        }
        if unsafe { (*pkt).stream_index } == unsafe { (*video).index } {
            packets += 1;
            let rc = unsafe { avcodec_send_packet(ctx, pkt) };
            assert_eq!(rc, 0, "avcodec_send_packet");
            drain(ctx);
        }
        unsafe { av_packet_unref(pkt) };
    }
    assert_eq!(unsafe { avcodec_send_packet(ctx, ptr::null()) }, 0);
    drain(ctx);
    println!("decoded {frames} frames from {packets} packets, last pts {last_pts}");
    assert_eq!(packets, 30);
    assert_eq!(frames, 30);

    let mut pkt = pkt;
    let mut frame = frame;
    let mut ctx = ctx;
    unsafe {
        av_packet_free(&mut pkt);
        av_frame_free(&mut frame);
        avcodec_free_context(&mut ctx);
    }
}

fn main() {
    if let Err(e) = FFMPEG.load() {
        println!("no ffmpeg on this host, skipping: {e}");
        return;
    }
    let (fmt, codec) = unsafe { (avformat_version() >> 16, avcodec_version() >> 16) };
    println!(
        "host ffmpeg: {}, libavformat {fmt}, libavcodec {codec}",
        unsafe { CStr::from_ptr(av_version_info()) }.to_string_lossy()
    );
    if fmt != LIBAVFORMAT_VERSION_MAJOR || codec != LIBAVCODEC_VERSION_MAJOR {
        println!(
            "bindings are for libavformat {LIBAVFORMAT_VERSION_MAJOR} / libavcodec {LIBAVCODEC_VERSION_MAJOR}, skipping"
        );
        return;
    }

    // the log callback, with a va_list built by the host: first read by
    // us, from a message we put known arguments into
    unsafe {
        av_log_set_level(AV_LOG_INFO as c_int);
        av_log_set_callback(log_line::ptr());
    }
    let word = c"pear";
    unsafe {
        ape::host_variadic!(av_log(
            ptr::null_mut::<c_void>(),
            AV_LOG_INFO as c_int,
            c"ape-vatest %d %s %f %lld %c\n".as_ptr(),
            42 as c_int,
            word.as_ptr(),
            3.5f64,
            1234567890123i64,
            'x' as c_int
        ))
    };
    let parsed = PARSED.lock().unwrap().take().expect("the log callback ran");
    println!("va_list read: {parsed}");
    assert_eq!(parsed, "%d %s %f %lld %c\n -> 42 pear 3.5 1234567890123 x");
    // then formatted by the library from the same list
    unsafe {
        ape::host_variadic!(av_log(
            ptr::null_mut::<c_void>(),
            AV_LOG_INFO as c_int,
            c"formatted by the host: %s %d %.2f\n".as_ptr(),
            word.as_ptr(),
            7 as c_int,
            2.25f64
        ))
    };
    let last = LOG
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("a formatted line");
    println!("va_list passed on: {last:?}");
    assert_eq!(last, "formatted by the host: pear 7 2.25");

    if CLIP.is_empty() {
        println!("no test clip, ffmpeg wasn't on the build machine; skipping the decoding");
        println!("\nhost ffmpeg ok");
        return;
    }

    // 1. the clip from a file
    let path =
        std::env::temp_dir().join(format!("rust-ape-host_ffmpeg-{}.mp4", std::process::id()));
    std::fs::write(&path, CLIP).expect("writing the clip");
    // spelled the host's way, C:\... on Windows
    let cpath = ape::host::host_path(&path).expect("host path");
    println!("clip at {:?}, handed to ffmpeg as {cpath:?}", path);
    let mut ic: *mut AVFormatContext = ptr::null_mut();
    let rc = unsafe { avformat_open_input(&mut ic, cpath.as_ptr(), ptr::null(), ptr::null_mut()) };
    assert_eq!(rc, 0, "avformat_open_input");
    decode(ic);
    unsafe { avformat_close_input(&mut ic) };
    let _ = std::fs::remove_file(&path);

    // 2. the clip from memory, through our AVIO callbacks
    let mut pos: usize = 0;
    let buffer = unsafe { av_malloc(4096) } as *mut u8;
    let avio = unsafe {
        avio_alloc_context(
            buffer,
            4096,
            0,
            &mut pos as *mut usize as *mut c_void,
            read_packet::ptr(),
            ape::host::Callback::NULL,
            seek_packet::ptr(),
        )
    };
    assert!(!avio.is_null());
    let mut ic = unsafe { avformat_alloc_context() };
    unsafe { (*ic).pb = avio };
    let rc = unsafe { avformat_open_input(&mut ic, c"".as_ptr(), ptr::null(), ptr::null_mut()) };
    assert_eq!(rc, 0, "avformat_open_input from memory");
    decode(ic);
    unsafe { avformat_close_input(&mut ic) };
    let mut avio = avio;
    unsafe {
        av_free((*avio).buffer as *mut c_void);
        avio_context_free(&mut avio);
    }
    println!(
        "read through our callbacks: {} reads, {} seeks",
        READS.load(Ordering::Relaxed),
        SEEKS.load(Ordering::Relaxed)
    );
    assert!(READS.load(Ordering::Relaxed) > 0 && SEEKS.load(Ordering::Relaxed) > 0);

    let log = LOG.lock().unwrap();
    println!("{} lines went through the log callback", log.len());
    for l in log.iter().take(3) {
        println!("  {l}");
    }
    println!("\nhost ffmpeg ok");
}
