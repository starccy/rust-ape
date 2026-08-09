//! Ratatui + crossterm running as an APE binary, a real-world consumer of
//! the termios/ioctl shim.
//!
//! crossterm enables raw mode through rustix, which even in rustix_use_libc
//! mode goes through libc::ioctl(TCGETS2)/ioctl(TCSETS2) with kernel-layout
//! structs, so it lands in shim/termios.c's repack. The window size comes
//! from ioctl(TIOCGWINSZ).
//!
//! The event source is crossterm's default, which is mio, which is epoll,
//! which is shim/epoll.c. Keystrokes rather than sockets drive the interest
//! list here, and a registration that goes deaf after its first event costs
//! you every key but the first.
//!
//! Non-interactive runs (no tty) skip cleanly so the regression suite can
//! include it. Run from a real terminal to see it; auto-quits in 6 seconds,
//! or press q / Esc. Other keys are counted and shown, which is the part
//! worth watching: if the count stops climbing after the first one, the
//! epoll registration is being revoked when it fires.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Gauge, Paragraph};

fn main() -> std::io::Result<()> {
    if unsafe { libc::isatty(0) } != 1 || unsafe { libc::isatty(1) } != 1 {
        println!("tui_demo: no tty, skipped (run from a real terminal)");
        return Ok(());
    }

    let mut terminal = ratatui::init();
    let start = Instant::now();
    let mut frames = 0u32;
    let mut keys = 0u32;
    let mut quit_key = false;
    let result = loop {
        let elapsed = start.elapsed();
        if elapsed >= Duration::from_secs(6) {
            break Ok(());
        }
        let ratio = (elapsed.as_millis() as f64 / 6000.0).min(1.0);
        let draw = terminal.draw(|f| {
            let [top, mid, rest] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .areas(f.area());
            f.render_widget(
                Paragraph::new(format!("one binary, every OS. {keys} keys so far, q or Esc quits"))
                    .block(Block::bordered().title(" ratatui on APE ")),
                top,
            );
            f.render_widget(
                Gauge::default()
                    .block(Block::bordered().title(" auto-quit "))
                    .ratio(ratio),
                mid,
            );
            f.render_widget(
                Paragraph::new(format!("frame {frames}\nterminal {:?}", f.area())),
                rest,
            );
        });
        if let Err(e) = draw {
            break Err(e);
        }
        frames += 1;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(k) = event::read()? {
                keys += 1;
                if matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                    quit_key = true;
                    break Ok(());
                }
            }
        }
    };
    ratatui::restore();
    result?;
    println!(
        "tui_demo: drew {frames} frames, read {keys} keys, clean exit ({})",
        if quit_key { "key" } else { "timeout" }
    );
    Ok(())
}
