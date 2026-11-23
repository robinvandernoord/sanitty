//! Copying to the system clipboard.
//!
//! Locally a clipboard program does it (`wl-copy`, `xclip`, `xsel`, `pbcopy`),
//! which works in any terminal. Over SSH, or when none is installed, the text
//! goes to the terminal as OSC 52, which Ghostty (and tmux with
//! `set-clipboard on`) put on the clipboard of the machine you sit at; some
//! terminals ignore it.

use std::io::Write;
use std::process::Command;
use std::process::Stdio;

/// How the text was copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// A clipboard program on this machine: certain to have worked.
    Program(&'static str),
    /// OSC 52 through the terminal: works only if the terminal allows it.
    Terminal,
}

/// Put `text` on the clipboard. OSC 52 is always sent as well, so a terminal
/// that supports it gets the text even when a program also did.
pub fn copy(text: &str) -> Via {
    osc52(text);
    if over_ssh() {
        return Via::Terminal;
    }
    program_copy(text).map_or(Via::Terminal, Via::Program)
}

fn over_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some()
}

/// The clipboard programs to try here, in order.
fn programs() -> Vec<(&'static str, &'static [&'static str])> {
    let mut found: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if cfg!(target_os = "macos") {
        found.push(("pbcopy", &[]));
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        found.push(("wl-copy", &[]));
    }
    if std::env::var_os("DISPLAY").is_some() {
        found.push(("xclip", &["-selection", "clipboard"]));
        found.push(("xsel", &["--clipboard", "--input"]));
    }
    found
}

/// Hand `text` to the first clipboard program that runs.
fn program_copy(text: &str) -> Option<&'static str> {
    for (program, args) in programs() {
        let Ok(mut child) = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let written = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        // These programs keep serving the clipboard in the background; reap
        // the one we started without waiting for it here.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        if written {
            return Some(program);
        }
    }
    None
}

/// Copy through the terminal (OSC 52).
fn osc52(text: &str) {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte_at = |idx: usize| u32::from(chunk.get(idx).copied().unwrap_or(0));
        let triple = (byte_at(0) << 16) | (byte_at(1) << 8) | byte_at(2);
        // Four 6-bit groups, high to low; a short chunk pads the missing ones.
        for (pos, shift) in [18_u32, 12, 6, 0].into_iter().enumerate() {
            let digit = usize::try_from((triple >> shift) & 63)
                .ok()
                .and_then(|sextet| B64.get(sextet));
            match digit {
                Some(&digit) if pos <= chunk.len() => out.push(char::from(digit)),
                _ => out.push('='),
            }
        }
    }
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\x1b]52;c;{out}\x07");
    let _ = stdout.flush();
}
