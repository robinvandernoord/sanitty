//! The Terminal view (Alt+T): your shell, running inside sanitty in a
//! pseudo-terminal, in the workspace root. It keeps running while you are
//! back in the code, so Alt+T returns to the same session.
//!
//! A reader thread feeds the shell's output to a `vt100` parser (the screen
//! and its scrollback) and wakes the app to redraw; `tui-term` draws that
//! screen. Keys go to the shell as the bytes an xterm sends for them.

use std::io::Read as _;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use karet_core::ThemeRole;
use karet_theme::Theme;
use portable_pty::Child;
use portable_pty::CommandBuilder;
use portable_pty::MasterPty;
use portable_pty::PtySize;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc::UnboundedSender;
use tui_term::widget::Cursor;
use tui_term::widget::PseudoTerminal;

use crate::Event;

/// Lines kept above the screen for scrolling back.
const SCROLLBACK: usize = 5000;

pub struct Shell {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// Rows and columns the pseudo-terminal has now.
    size: (u16, u16),
    /// Set by the reader thread when the shell's output ends.
    exited: Arc<AtomicBool>,
    /// The program, for the title.
    pub program: String,
}

impl Shell {
    /// Start `$SHELL` (else `/bin/sh`) in `root`, `rows` by `cols`.
    pub fn spawn(
        root: &Path,
        rows: u16,
        cols: u16,
        tx: UnboundedSender<Event>,
    ) -> Result<Self, String> {
        let rows = rows.max(2);
        let cols = cols.max(10);
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = portable_pty::native_pty_system()
            .openpty(size)
            .map_err(|err| format!("could not open a terminal: {err}"))?;
        let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut command = CommandBuilder::new(&program);
        command.cwd(root);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("SANITTY", "1");
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| format!("could not start {program}: {err}"))?;
        // The child holds its own handle; ours would keep the terminal open
        // after the shell exits.
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| err.to_string())?;
        let writer = pair.master.take_writer().map_err(|err| err.to_string())?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let exited = Arc::new(AtomicBool::new(false));
        {
            let parser = Arc::clone(&parser);
            let exited = Arc::clone(&exited);
            std::thread::spawn(move || {
                let mut buffer = [0u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if let Ok(mut parser) = parser.lock() {
                                parser.process(buffer.get(..count).unwrap_or_default());
                            }
                            if tx.send(Event::ShellOutput).is_err() {
                                return;
                            }
                        }
                    }
                }
                exited.store(true, Ordering::Release);
                let _ = tx.send(Event::ShellOutput);
            });
        }
        let name = Path::new(&program).file_name().map_or_else(
            || program.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        Ok(Self {
            parser,
            writer,
            master: pair.master,
            child,
            size: (rows, cols),
            exited,
            program: name,
        })
    }

    /// Whether the shell has exited (Enter then starts a new one).
    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// The program running in the shell's foreground (a server, `cargo
    /// watch`, vim), if any: the terminal's foreground process group is then
    /// not the shell's own. An idle shell at its prompt gives `None`. The
    /// name comes from `/proc` where there is one.
    pub fn running(&self) -> Option<String> {
        if self.exited() {
            return None;
        }
        let leader = u32::try_from(self.master.process_group_leader()?).ok()?;
        if Some(leader) == self.child.process_id() {
            return None;
        }
        let name = std::fs::read_to_string(format!("/proc/{leader}/comm"))
            .map(|name| name.trim().to_string())
            .ok()
            .filter(|name| !name.is_empty());
        Some(name.unwrap_or_else(|| "a program".into()))
    }

    fn send(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            let _ = self.writer.write_all(bytes);
            let _ = self.writer.flush();
        }
    }

    /// A key: Shift+PageUp/PageDown scroll back through the output; any
    /// other key goes to the shell (and back to the bottom).
    pub fn key(&mut self, key: &KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if shift && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            let page = usize::from(self.size.0.saturating_sub(1).max(1));
            if let Ok(mut parser) = self.parser.lock() {
                let now = parser.screen().scrollback();
                let target = if key.code == KeyCode::PageUp {
                    now.saturating_add(page)
                } else {
                    now.saturating_sub(page)
                };
                parser.screen_mut().set_scrollback(target);
            }
            return;
        }
        let application_cursor = self.parser.lock().is_ok_and(|mut parser| {
            parser.screen_mut().set_scrollback(0);
            parser.screen().application_cursor()
        });
        let bytes = encode(key, application_cursor);
        self.send(&bytes);
    }

    /// Pasted text, marked as a paste when the program asked for that.
    pub fn paste(&mut self, text: &str) {
        let bracketed = self.parser.lock().is_ok_and(|mut parser| {
            parser.screen_mut().set_scrollback(0);
            parser.screen().bracketed_paste()
        });
        if bracketed {
            self.send(b"\x1b[200~");
            self.send(text.as_bytes());
            self.send(b"\x1b[201~");
        } else {
            self.send(text.as_bytes());
        }
    }

    /// Fit the pseudo-terminal to `rows` by `cols`.
    fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) == self.size || rows == 0 || cols == 0 {
            return;
        }
        self.size = (rows, cols);
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        self.resize(area.height, area.width);
        let base = crate::styles::surface(theme);
        frame.render_widget(ratatui::widgets::Block::new().style(base), area);
        let Ok(parser) = self.parser.lock() else {
            return;
        };
        let screen = parser.screen();
        let scrolled = screen.scrollback();
        let mut cursor = Cursor::default();
        if scrolled > 0 || self.exited() {
            cursor.hide();
        }
        frame.render_widget(PseudoTerminal::new(screen).cursor(cursor), area);
        let muted = theme.style(ThemeRole::Muted);
        let note = if self.exited() {
            Some(format!(
                " {} exited. Enter starts a new one. ",
                self.program
            ))
        } else if scrolled > 0 {
            Some(format!(
                " {scrolled} lines up. Shift+PageDown goes back down. "
            ))
        } else {
            None
        };
        if let Some(note) = note {
            let width = crate::util::to_u16(note.chars().count()).min(area.width);
            let rect = Rect {
                x: area.x + area.width.saturating_sub(width),
                y: area.y,
                width,
                height: 1.min(area.height),
            };
            frame.render_widget(Paragraph::new(Line::styled(note, muted)).style(base), rect);
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// The bytes an xterm sends for a key. `application_cursor` is the mode a
/// program (vim, less) can switch on, in which the arrows send `ESC O x`.
pub fn encode(key: &KeyEvent, application_cursor: bool) -> Vec<u8> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // xterm's modifier parameter: 1 plus Shift 1, Alt 2, Ctrl 4.
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let with_alt = |mut bytes: Vec<u8>| {
        if alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    // `ESC [ x` or, with modifiers, `ESC [ 1 ; m x`.
    let cursor_key = |letter: u8| {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{}", char::from(letter)).into_bytes()
        } else if application_cursor {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    };
    // `ESC [ n ~` or, with modifiers, `ESC [ n ; m ~`.
    let tilde_key = |number: u8| {
        if modifier > 1 {
            format!("\x1b[{number};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{number}~").into_bytes()
        }
    };
    match key.code {
        KeyCode::Char(chr) if ctrl => {
            with_alt(control(chr).map_or_else(Vec::new, |byte| vec![byte]))
        }
        KeyCode::Char(chr) => {
            let mut utf8 = [0u8; 4];
            with_alt(chr.encode_utf8(&mut utf8).as_bytes().to_vec())
        }
        KeyCode::Enter => with_alt(vec![b'\r']),
        KeyCode::Tab => with_alt(vec![b'\t']),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace if ctrl => with_alt(vec![0x08]),
        KeyCode::Backspace => with_alt(vec![0x7f]),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor_key(b'A'),
        KeyCode::Down => cursor_key(b'B'),
        KeyCode::Right => cursor_key(b'C'),
        KeyCode::Left => cursor_key(b'D'),
        KeyCode::Home => cursor_key(b'H'),
        KeyCode::End => cursor_key(b'F'),
        KeyCode::Insert => tilde_key(2),
        KeyCode::Delete => tilde_key(3),
        KeyCode::PageUp => tilde_key(5),
        KeyCode::PageDown => tilde_key(6),
        KeyCode::F(number @ 1..=4) => {
            let letter = b"PQRS"
                .get(usize::from(number - 1))
                .copied()
                .unwrap_or(b'P');
            if modifier > 1 {
                format!("\x1b[1;{modifier}{}", char::from(letter)).into_bytes()
            } else {
                vec![0x1b, b'O', letter]
            }
        }
        KeyCode::F(number @ 5..=12) => {
            let code = match number {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            tilde_key(code)
        }
        _ => Vec::new(),
    }
}

/// The control byte for Ctrl plus a character: Ctrl+C is 0x03, Ctrl+Space
/// NUL, Ctrl+[ ESC, Ctrl+? DEL.
fn control(chr: char) -> Option<u8> {
    let lower = chr.to_ascii_lowercase();
    match lower {
        'a'..='z' => u8::try_from(lower).ok().map(|byte| byte & 0x1f),
        ' ' | '@' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '7' | '/' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
        encode(&KeyEvent::new(code, modifiers), false)
    }

    #[test]
    fn keys_become_what_an_xterm_sends() {
        let none = KeyModifiers::NONE;
        assert_eq!(key(KeyCode::Char('a'), none), b"a");
        assert_eq!(key(KeyCode::Char('é'), none), "é".as_bytes());
        assert_eq!(key(KeyCode::Char('c'), KeyModifiers::CONTROL), [0x03]);
        assert_eq!(key(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1bb");
        assert_eq!(key(KeyCode::Enter, none), b"\r");
        assert_eq!(key(KeyCode::Backspace, none), [0x7f]);
        assert_eq!(key(KeyCode::Up, none), b"\x1b[A");
        assert_eq!(key(KeyCode::Right, KeyModifiers::CONTROL), b"\x1b[1;5C");
        assert_eq!(key(KeyCode::Delete, none), b"\x1b[3~");
        assert_eq!(key(KeyCode::F(5), none), b"\x1b[15~");
        assert_eq!(key(KeyCode::F(1), none), b"\x1bOP");
        // vim and less switch the arrows to application mode.
        assert_eq!(encode(&KeyEvent::new(KeyCode::Up, none), true), b"\x1bOA");
    }

    /// A real shell: what it prints reaches the screen.
    #[test]
    fn runs_a_shell_and_shows_its_output() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let dir = std::env::temp_dir();
        let Ok(mut shell) = Shell::spawn(&dir, 10, 40, tx) else {
            return;
        };
        shell.send(b"echo hello-from-$((40+2))\r");
        let found = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            while rx.try_recv().is_ok() {}
            shell
                .parser
                .lock()
                .is_ok_and(|parser| parser.screen().contents().contains("hello-from-42"))
        });
        assert!(found);

        // Idle at the prompt: nothing to warn about. A program in the
        // foreground is named, and gone again after Ctrl+C.
        let settle = |shell: &Shell, want: Option<&str>| {
            (0..50).any(|_| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                shell.running().as_deref() == want
            })
        };
        assert!(settle(&shell, None));
        shell.send(b"sleep 30\r");
        assert!(settle(&shell, Some("sleep")));
        shell.send(&[0x03]);
        assert!(settle(&shell, None));

        shell.send(b"exit\r");
        let gone = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            shell.exited()
        });
        assert!(gone);
    }
}
