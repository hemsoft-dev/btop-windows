//! Interactive Windows Terminal VT smoke demo for `btop-term`.
//!
//! Run with: `cargo run -p btop-term --example vt_smoke`
//! Exits cleanly on `Ctrl+C` or `q`, restoring console modes, cursor, and primary screen.

use std::process::ExitCode;
use std::time::Duration;

use btop_term::{
    Color, CrosstermInput, DiffEncoder, Grid, InputSource, KeyCode, Rgb, Size, TermEvent,
    TerminalGuard,
};

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(Rgb { r, g, b })
}

fn render_smoke(size: Size, last_event: &str, frame_count: u64) -> Grid {
    let mut grid = Grid::new(size);
    if size.width < 20 || size.height < 8 {
        grid.write_str(0, 0, "Resize larger", Color::Reset, Color::Reset, true);
        return grid;
    }

    let bg = rgb(18, 20, 28);
    let border_fg = rgb(85, 135, 105);
    let title_fg = rgb(238, 238, 238);
    let accent_fg = rgb(119, 202, 155);
    let warn_fg = rgb(220, 192, 108);
    let muted_fg = rgb(150, 160, 180);

    // Fill background
    let blank_row = " ".repeat(usize::from(size.width));
    for y in 0..size.height {
        grid.write_str(0, y, &blank_row, title_fg, bg, false);
    }

    // Rounded border
    let inner_w = usize::from(size.width.saturating_sub(2));
    let top = format!("╭{}╮", "─".repeat(inner_w));
    let bottom = format!("╰{}╯", "─".repeat(inner_w));
    grid.write_str(0, 0, &top, border_fg, bg, false);
    grid.write_str(0, size.height - 1, &bottom, border_fg, bg, false);
    for y in 1..size.height - 1 {
        grid.write_str(0, y, "│", border_fg, bg, false);
        grid.write_str(size.width - 1, y, "│", border_fg, bg, false);
    }

    // Embedded btop-style title (clipped so column `width - 1` right border stays intact)
    let max_title = usize::from(size.width.saturating_sub(3));
    let title: String = "┐ btop-windows VT smoke ┌"
        .chars()
        .take(max_title)
        .collect();
    grid.write_str(2, 0, &title, title_fg, bg, true);

    let max_text = usize::from(size.width.saturating_sub(4));
    let clip = |s: &str| -> String { s.chars().take(max_text).collect() };

    grid.write_str(
        3,
        2,
        &clip(&format!(
            "Viewport: {}x{}  |  DEC 2026: ON  |  TrueColor: 24-bit",
            size.width, size.height
        )),
        accent_fg,
        bg,
        true,
    );
    grid.write_str(
        3,
        4,
        &clip("Braille : ⣀⣤⣶⣿⣷⣶⣤⣀⣀⣤⣶⣿⣷⣶⣤⣀⣀⣤⣶⣿⣷⣶⣤⣀"),
        accent_fg,
        bg,
        false,
    );
    grid.write_str(
        3,
        5,
        &clip("Block   : ▁▂▃▄▅▆▇█▇▆▅▄▃▂▁▂▃▄▅▆▇█▇▆▅▄▃▂▁▂▃▄▅▆▇█"),
        warn_fg,
        bg,
        false,
    );
    grid.write_str(
        3,
        7,
        &clip(&format!("Frames rendered: {frame_count}")),
        muted_fg,
        bg,
        false,
    );
    if size.height > 9 {
        grid.write_str(
            3,
            8,
            &clip(&format!("Last event     : {last_event}")),
            title_fg,
            bg,
            false,
        );
    }
    grid.write_str(
        3,
        size.height - 2,
        &clip("Resize window, click/scroll, or press Ctrl+C / q to exit cleanly."),
        muted_fg,
        bg,
        false,
    );

    grid
}

fn main() -> ExitCode {
    let mut guard = match TerminalGuard::enter() {
        Ok(g) => g,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut input = CrosstermInput;
    let encoder = DiffEncoder { sync_output: true };
    let mut size = guard.size().unwrap_or(Size {
        width: 80,
        height: 24,
    });
    let mut prev_grid: Option<Grid> = None;
    let mut last_event = "Startup (waiting for input/resize)".to_owned();
    let mut frames = 0u64;

    loop {
        frames += 1;
        let next_grid = render_smoke(size, &last_event, frames);
        let payload = encoder.encode(prev_grid.as_ref(), &next_grid);
        if let Err(err) = guard.write_frame(&payload) {
            drop(guard);
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
        prev_grid = Some(next_grid);

        match input.poll(Duration::from_millis(250)) {
            Ok(Some(TermEvent::Key(k))) if k.is_ctrl_c() || k.code == KeyCode::Char('q') => {
                break;
            }
            Ok(Some(TermEvent::Key(k))) => {
                last_event = format!("Key({:?}, ctrl={}, shift={})", k.code, k.ctrl, k.shift);
            }
            Ok(Some(TermEvent::Mouse(m))) => {
                last_event = format!("Mouse({:?} at {},{})", m.kind, m.col, m.row);
            }
            Ok(Some(TermEvent::Resize(new_size))) => {
                size = guard.size().unwrap_or(new_size);
                last_event = format!("Resize({}x{})", size.width, size.height);
                prev_grid = None;
            }
            Ok(None) => {
                if let Ok(live) = guard.size()
                    && live != size
                {
                    size = live;
                    last_event = format!("Resize({}x{})", size.width, size.height);
                    prev_grid = None;
                }
            }
            Err(err) => {
                drop(guard);
                eprintln!("error: {err}");
                return ExitCode::FAILURE;
            }
        }
    }

    drop(guard);
    println!("btop-term vt_smoke exited cleanly ({frames} frames).");
    ExitCode::SUCCESS
}
