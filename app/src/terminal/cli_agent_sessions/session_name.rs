//! The name of a CLI agent session, shown as its row title in the vertical tabs panel (fork
//! feature).
//!
//! Claude Code puts the session's name in the terminal title: the `/rename` or `--name` title,
//! else the title it generates itself, behind a status glyph (`✳` when idle, `◐`/`◑` while
//! working). Until it has a name the title is just "Claude Code", which isn't one. A title
//! without that glyph was set by something else (e.g. the shell, when Claude Code's own titles
//! are turned off), so it isn't a name either. Codex doesn't put one there by default,
//! so its sessions are named after their first prompt, which is also what Codex itself calls a
//! thread until it is renamed.

use crate::terminal::CLIAgent;

/// Longest name kept, in characters. Rows fade long names out, and the full name is shown on
/// hover, so this only bounds what is saved with the session.
const MAX_SESSION_NAME_CHARS: usize = 200;

/// What Claude Code's title says before the session has a name.
const CLAUDE_UNNAMED_TITLE: &str = "Claude Code";

/// The session name in a terminal title set by `agent`, if that agent puts one there.
pub(crate) fn session_name_from_terminal_title(agent: CLIAgent, title: &str) -> Option<String> {
    match agent {
        CLIAgent::Claude => clean_session_name(strip_status_glyph(title)?)
            .filter(|name| name != CLAUDE_UNNAMED_TITLE),
        _ => None,
    }
}

/// A session name taken from a prompt: its first line, cleaned up.
pub(crate) fn session_name_from_prompt(prompt: &str) -> Option<String> {
    clean_session_name(prompt.lines().find(|line| !line.trim().is_empty())?)
}

/// `name` on one line, without control or bidi characters, at most
/// [`MAX_SESSION_NAME_CHARS`] long; `None` if nothing is left.
pub(crate) fn clean_session_name(name: &str) -> Option<String> {
    let name = name
        .chars()
        .filter(|&c| !is_bidi_control(c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }
    Some(name.chars().take(MAX_SESSION_NAME_CHARS).collect())
}

/// `title` without its leading status glyph and the space after it; `None` if it has none.
fn strip_status_glyph(title: &str) -> Option<&str> {
    let mut chars = title.trim_start().chars();
    match (chars.next(), chars.next()) {
        (Some(glyph), Some(' ')) if is_status_glyph(glyph) => Some(chars.as_str()),
        _ => None,
    }
}

/// Spinner and status marks agents put in front of a terminal title: braille spinner frames,
/// geometric shapes (`◐`, `●`), dingbat stars (`✳`, `✻`) and bullets.
fn is_status_glyph(c: char) -> bool {
    matches!(
        c,
        '*' | '\u{00B7}' | '\u{2022}' | '\u{25A0}'..='\u{25FF}' | '\u{2700}'..='\u{27BF}'
            | '\u{2800}'..='\u{28FF}'
    )
}

/// Characters that could reorder or hide displayed text.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
#[path = "session_name_tests.rs"]
mod tests;
