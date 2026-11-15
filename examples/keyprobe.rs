//! Prints the raw bytes the terminal sends for each key press.
//! Run with `--plain` to skip the kitty keyboard protocol that acp-tui enables when supported.

use crossterm::event::{KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement};
use std::io::{Read, Write};

fn escaped(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x1b => "\\e".to_string(),
            0x20..=0x7e => (b as char).to_string(),
            _ => format!("\\x{b:02x}"),
        })
        .collect()
}

fn main() -> std::io::Result<()> {
    let plain = std::env::args().any(|a| a == "--plain");
    enable_raw_mode()?;
    let supported = supports_keyboard_enhancement().unwrap_or(false);
    let enhanced = supported && !plain;
    let mut out = std::io::stdout();
    if enhanced {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    write!(out, "kitty protocol supported={supported} enabled={enhanced} (press q to quit)\r\n")?;
    out.flush()?;
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 64];
    loop {
        let n = stdin.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        write!(out, "{}\r\n", escaped(chunk))?;
        out.flush()?;
        if chunk == b"q" {
            break;
        }
    }
    if enhanced {
        execute!(out, PopKeyboardEnhancementFlags)?;
    }
    disable_raw_mode()
}
