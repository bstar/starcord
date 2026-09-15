//! Attaching a file by typing where it is.
//!
//! There is no file browser. A terminal client is being driven by somebody who
//! already has a shell, a path on their clipboard and a tab-completing prompt
//! one window away; a directory listing reimplemented inside a chat client
//! would be a worse version of all three.
//!
//! What this does do is check before it accepts: `~` is expanded, the file has
//! to exist, and it has to be under `[media] max_attachment_mib`. A chip that
//! appears and then fails at send time is a message somebody thinks they sent.

use std::path::{Path, PathBuf};

use starkit::chrome::overlay::{self, Anchor};
use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starkit::input::{Edit, TextInput};
use starkit::ratatui::buffer::Buffer;
use starkit::ratatui::layout::Rect;
use starkit::ratatui::style::Style;

use crate::ui::panels::{fit, rgb};
use crate::ui::theme::Theme;

/// What a key asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Taken,
    Close,
    /// A path that exists and is small enough.
    Attach(PathBuf),
    Quit,
}

#[derive(Debug)]
pub struct Attach {
    pub path: TextInput,
    /// Why the last attempt was refused.
    pub error: Option<String>,
    /// The cap, in mebibytes.
    limit_mib: u64,
}

impl Attach {
    pub fn new(limit_mib: u64) -> Self {
        Self {
            path: TextInput::single(),
            error: None,
            limit_mib,
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        match self.path.handle(key) {
            Edit::Submit => match check(self.path.text(), self.limit_mib) {
                Ok(path) => Action::Attach(path),
                Err(reason) => {
                    self.error = Some(reason);
                    Action::Taken
                }
            },
            Edit::Cancel => Action::Close,
            Edit::Consumed => {
                self.error = None;
                Action::Taken
            }
            Edit::Ignored => Action::Taken,
        }
    }

    pub fn paste(&mut self, text: &str) {
        // A path pasted out of a file manager arrives quoted or with a
        // trailing newline more often than not.
        let text = text.trim().trim_matches(['"', '\''].as_slice());
        self.path.paste(text);
        self.error = None;
    }
}

/// `~` as the home directory, and only at the front: a file really called
/// `~backup` in the current directory is a file called `~backup`.
pub fn expand(text: &str) -> PathBuf {
    let text = text.trim();
    let Some(rest) = text.strip_prefix('~') else {
        return PathBuf::from(text);
    };
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
        return PathBuf::from(text);
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    if rest.is_empty() {
        PathBuf::from(home)
    } else {
        PathBuf::from(home).join(rest)
    }
}

/// Whether this is a file worth making a chip of.
pub fn check(text: &str, limit_mib: u64) -> Result<PathBuf, String> {
    if text.trim().is_empty() {
        return Err("nothing typed".into());
    }
    let path = expand(text);
    let meta = std::fs::metadata(&path).map_err(|_| format!("no such file: {}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let limit = limit_mib.saturating_mul(1024 * 1024);
    if limit > 0 && meta.len() > limit {
        return Err(format!(
            "{} is {}, over the {limit_mib} MiB limit",
            name_of(&path),
            human(meta.len())
        ));
    }
    Ok(path)
}

pub fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// A size somebody can read.
pub fn human(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Where the box lands: the same shape every overlay opens in, upper-anchored
/// where the typing boxes sit.
pub fn rect(area: Rect) -> Rect {
    overlay::rect(area, (30, 72), 5, 5, Anchor::Upper)
}

pub fn render(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    attach: &mut Attach,
) -> Option<(u16, u16)> {
    let r = rect(area);
    if r.width < 12 || r.height < 4 {
        return None;
    }

    let t = theme;
    // The core theme type -- a struct literal is not a coercion site, so the
    // deref from this crate's own `Theme` is spelled out here.
    let core: &starkit::theme::Theme = t;
    let inner = overlay::render(
        r,
        buf,
        &overlay::Overlay {
            theme: core,
            title: "attach a file",
            detail: None,
            footer: Some("enter attach \u{b7} esc cancel"),
        },
    );
    if inner.width < 4 || inner.height == 0 {
        return None;
    }

    buf.set_string(
        inner.x,
        inner.y,
        "\u{203a} ",
        Style::default().fg(rgb(t.accent)),
    );
    let caret = attach.path.render(
        Rect {
            x: inner.x + 2,
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: 1,
        },
        buf,
        Style::default().fg(rgb(t.fg)),
    );
    if inner.height > 2 {
        let (text, colour) = match &attach.error {
            Some(reason) => (reason.clone(), t.error),
            None => ("a path, with ~ for your home directory".to_string(), t.dim),
        };
        buf.set_string(
            inner.x,
            inner.y + 2,
            fit(&text, inner.width),
            Style::default().fg(rgb(colour)),
        );
    }
    caret
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests_support::theme;

    #[test]
    fn a_tilde_is_the_home_directory_and_only_at_the_front() {
        let home = std::env::var_os("HOME").unwrap_or_default();
        if home.is_empty() {
            return;
        }
        assert!(expand("~/x.png").is_absolute());
        assert!(expand("~/x.png").ends_with("x.png"));
        assert_eq!(expand("/tmp/~x"), PathBuf::from("/tmp/~x"));
        assert_eq!(expand("  /tmp/a.png  "), PathBuf::from("/tmp/a.png"));
    }

    /// The whole point of checking here: a chip that appears and then fails at
    /// send time is a message somebody believes they sent.
    #[test]
    fn a_file_that_is_too_big_is_refused_before_it_becomes_a_chip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        std::fs::write(&path, vec![0u8; 3 * 1024 * 1024]).unwrap();

        let refused = check(path.to_str().unwrap(), 1).expect_err("three MiB under a one MiB cap");
        assert!(refused.contains("over the 1 MiB limit"), "{refused}");
        assert!(refused.contains("big.bin"), "{refused}");

        let ok = check(path.to_str().unwrap(), 25).expect("under the real cap");
        assert_eq!(ok, path);
    }

    #[test]
    fn a_file_that_is_not_there_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nothing.png");
        let e = check(missing.to_str().unwrap(), 25).unwrap_err();
        assert!(e.contains("no such file"), "{e}");

        let e = check(dir.path().to_str().unwrap(), 25).unwrap_err();
        assert!(e.contains("not a file"), "{e}");

        assert!(check("   ", 25).is_err());
    }

    #[test]
    fn the_refusal_is_shown_rather_than_swallowed() {
        let mut a = Attach::new(25);
        for c in "/nonexistent/x.png".chars() {
            a.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(
            a.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Taken,
            "it stays open so the path can be fixed"
        );
        assert!(a.error.is_some());

        let t = theme("terminal");
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &t, &mut a);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("ATTACH A FILE"), "{text}");
        assert!(text.contains("no such file"), "{text}");
    }

    #[test]
    fn sizes_read_as_sizes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2 KiB");
        assert_eq!(human(3 * 1024 * 1024), "3.0 MiB");
    }

    /// A path pasted out of a file manager arrives quoted more often than not.
    #[test]
    fn a_pasted_path_loses_its_quotes() {
        let mut a = Attach::new(25);
        a.paste("\"/tmp/a b.png\"\n");
        assert_eq!(a.path.text(), "/tmp/a b.png");
    }
}
