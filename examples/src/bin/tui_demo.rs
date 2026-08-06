//! Ratatui + crossterm running as an APE binary, a real-world consumer of
//! the termios/ioctl shim.
//!
//! crossterm enables raw mode through rustix, which even in rustix_use_libc
//! mode goes through libc::ioctl(TCGETS2)/ioctl(TCSETS2) with kernel-layout
//! structs, so it lands in shim/termios.c's repack. The window size comes
//! from ioctl(TIOCGWINSZ), and the use-dev-tty event source polls /dev/tty
//! via the shimmed poll.
//!
//! Non-interactive runs (no tty) skip cleanly so the regression suite can
//! include it. Run from a real terminal to see it; auto-quits in 3 seconds,
//! or press q / Esc.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
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
    let mut quit_key = false;
    let result = loop {
        let elapsed = start.elapsed();
        if elapsed >= Duration::from_secs(3) {
            break Ok(());
        }
        let ratio = (elapsed.as_millis() as f64 / 3000.0).min(1.0);
        let draw = terminal.draw(|f| {
            let [top, mid, rest] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .areas(f.area());
            f.render_widget(
                Paragraph::new("one binary, every OS — press q or Esc to quit")
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
        "tui_demo: drew {frames} frames, clean exit ({})",
        if quit_key { "key" } else { "timeout" }
    );
    Ok(())
}
