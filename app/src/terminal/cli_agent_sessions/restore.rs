//! Crash-recovery records for CLI agent sessions (Claude Code, Codex).
//!
//! The latest CLI agent session seen in a terminal pane is persisted with the app state so that,
//! after a crash or restart, the restored pane can offer to resume it.

use serde::{Deserialize, Deserializer, Serialize};

use super::{CLIAgentSession, CLIAgentSessionStatus};
use crate::terminal::CLIAgent;
use crate::terminal::shell::{ShellType, shell_quote_arg};

/// Longest title (in characters) kept for a persisted session. Plugins already cap prompts at
/// 200 characters; this guards against unexpectedly large values.
const MAX_TITLE_CHARS: usize = 200;

/// Longest accepted agent session id. Claude Code and Codex use 36-character UUIDs.
const MAX_SESSION_ID_LEN: usize = 128;

/// Version of the persisted JSON format, stored under [`VERSION_KEY`]. Records with another
/// version (e.g. written by a newer build) are ignored rather than misread.
const FORMAT_VERSION: u64 = 1;
const VERSION_KEY: &str = "v";

/// Status of a CLI agent session when it was last persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistedCLIAgentStatus {
    InProgress,
    Blocked,
    Success,
    Failed,
    Cancelled,
}

impl From<&CLIAgentSessionStatus> for PersistedCLIAgentStatus {
    fn from(status: &CLIAgentSessionStatus) -> Self {
        match status {
            CLIAgentSessionStatus::InProgress => Self::InProgress,
            CLIAgentSessionStatus::Blocked { .. } => Self::Blocked,
            CLIAgentSessionStatus::Success => Self::Success,
            CLIAgentSessionStatus::Failed { .. } => Self::Failed,
            CLIAgentSessionStatus::Cancelled => Self::Cancelled,
        }
    }
}

/// The latest CLI agent session seen in a terminal pane, persisted so it can be resumed later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedCLIAgentSession {
    pub agent: CLIAgent,
    /// The agent's own session id (Claude Code session id, Codex thread id).
    #[serde(default)]
    pub session_id: Option<String>,
    /// The first working directory reported for `session_id`. Agents look sessions up relative
    /// to the directory they were started in, so resuming `cd`s here first.
    #[serde(default)]
    pub launch_cwd: Option<String>,
    /// The latest user prompt, used as the session's title.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_status")]
    pub status: Option<PersistedCLIAgentStatus>,
    /// `user@host` when the agent ran over SSH. Such sessions can't be resumed from the fresh
    /// local shell of a restored pane, so only the command is offered.
    #[serde(default)]
    pub remote_host: Option<String>,
    #[serde(default)]
    pub updated_at_unix_ms: i64,
}

/// Unknown statuses (e.g. written by a newer build) are dropped instead of invalidating the
/// whole record.
fn deserialize_lenient_status<'de, D>(
    deserializer: D,
) -> Result<Option<PersistedCLIAgentStatus>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// Whether Warp knows how to resume sessions of this agent.
pub fn is_resumable_agent(agent: CLIAgent) -> bool {
    match agent {
        CLIAgent::Claude | CLIAgent::Codex => true,
        CLIAgent::Gemini
        | CLIAgent::Amp
        | CLIAgent::Droid
        | CLIAgent::OpenCode
        | CLIAgent::Copilot
        | CLIAgent::Pi
        | CLIAgent::OhMyPi
        | CLIAgent::Auggie
        | CLIAgent::CursorCli
        | CLIAgent::Goose
        | CLIAgent::Hermes
        | CLIAgent::Vibe
        | CLIAgent::Antigravity
        | CLIAgent::Grok
        | CLIAgent::Kiro
        | CLIAgent::Muse
        | CLIAgent::WarpTui
        | CLIAgent::Unknown => false,
    }
}

/// The valid session id the live session's agent has reported, if any.
pub(crate) fn reported_session_id(session: &CLIAgentSession) -> Option<&str> {
    session
        .session_context
        .session_id
        .as_deref()
        .and_then(valid_session_id)
}

/// Returns the trimmed value, or `None` if it is empty.
pub(crate) fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Returns the session id if it is safe to pass to an agent's resume command: a non-empty token
/// of ASCII alphanumerics, `-`, `_`, `.` or `:` that doesn't look like a flag.
pub(crate) fn valid_session_id(session_id: &str) -> Option<&str> {
    let session_id = session_id.trim();
    let is_valid = !session_id.is_empty()
        && session_id.len() <= MAX_SESSION_ID_LEN
        && !session_id.starts_with('-')
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    is_valid.then_some(session_id)
}

fn truncate_title(title: &str) -> String {
    title.chars().take(MAX_TITLE_CHARS).collect()
}

impl PersistedCLIAgentSession {
    /// Parses a persisted record. Returns `None` (logging a warning) for malformed JSON, an
    /// unknown format version, an agent that can't be resumed, or a missing or unsafe session id.
    /// Other empty or invalid fields are dropped rather than rejecting the record.
    pub fn from_json(json: &str) -> Option<Self> {
        let value = match serde_json::from_str::<serde_json::Value>(json) {
            Ok(value) => value,
            Err(err) => {
                log::warn!("Ignoring malformed persisted CLI agent session: {err}");
                return None;
            }
        };
        // Records without a version predate versioning and use the first format.
        let version = match value.get(VERSION_KEY) {
            None => Some(FORMAT_VERSION),
            Some(version) => version.as_u64(),
        };
        if version != Some(FORMAT_VERSION) {
            log::warn!("Ignoring persisted CLI agent session with unknown format version");
            return None;
        }
        match serde_json::from_value::<Self>(value) {
            Ok(record) => record.sanitized(),
            Err(err) => {
                log::warn!("Ignoring malformed persisted CLI agent session: {err}");
                None
            }
        }
    }

    pub fn to_json(&self) -> Option<String> {
        let mut value = serde_json::to_value(self)
            .inspect_err(|err| log::warn!("Failed to serialize CLI agent session: {err}"))
            .ok()?;
        let Some(object) = value.as_object_mut() else {
            log::warn!("CLI agent session didn't serialize to a JSON object");
            return None;
        };
        object.insert(VERSION_KEY.to_owned(), FORMAT_VERSION.into());
        Some(value.to_string())
    }

    fn sanitized(self) -> Option<Self> {
        if !is_resumable_agent(self.agent) {
            return None;
        }
        let Some(session_id) = self.session_id.as_deref().and_then(valid_session_id) else {
            log::warn!("Ignoring persisted CLI agent session without a usable session id");
            return None;
        };
        let owned = |value: Option<String>| non_empty(value.as_deref()).map(str::to_owned);
        Some(Self {
            agent: self.agent,
            session_id: Some(session_id.to_owned()),
            launch_cwd: owned(self.launch_cwd),
            title: non_empty(self.title.as_deref()).map(truncate_title),
            transcript_path: owned(self.transcript_path),
            status: self.status,
            remote_host: owned(self.remote_host),
            updated_at_unix_ms: self.updated_at_unix_ms,
        })
    }

    /// Builds the record for a live session, or `None` if it isn't worth resuming.
    ///
    /// A session is only persisted once it has a session id and has done something beyond
    /// starting (a prompt, a status change, a summary or a response): resuming a session that
    /// never received a prompt is pointless, and without an id the only fallback
    /// (`claude --continue` / `codex resume --last`) could pick another pane's session.
    ///
    /// Fields the live session hasn't reported yet are carried over from `previous` when it
    /// describes the same agent session (e.g. a restored record that is being resumed), and the
    /// launch directory stays pinned to the first one seen for a session id. `pane_cwd` is the
    /// fallback launch directory for agents that don't report one. `updated_at_unix_ms` only
    /// advances when the content changes.
    pub fn from_live_session(
        session: &CLIAgentSession,
        pane_cwd: Option<&str>,
        previous: Option<&Self>,
        now_unix_ms: i64,
    ) -> Option<Self> {
        let agent = session.agent;
        if !is_resumable_agent(agent) {
            return None;
        }
        let context = &session.session_context;
        let previous_same_agent = previous.filter(|previous| previous.agent == agent);
        let session_id = reported_session_id(session)
            .map(str::to_owned)
            .or_else(|| previous_same_agent.and_then(|previous| previous.session_id.clone()))?;
        let previous_same_session = previous_same_agent
            .filter(|previous| previous.session_id.as_deref() == Some(session_id.as_str()));

        let launch_cwd = previous_same_session
            .and_then(|previous| previous.launch_cwd.clone())
            .or_else(|| non_empty(context.launch_cwd.as_deref()).map(str::to_owned))
            .or_else(|| non_empty(pane_cwd).map(str::to_owned));
        let title = context
            .latest_user_prompt()
            .map(|title| truncate_title(&title))
            .or_else(|| previous_same_session.and_then(|previous| previous.title.clone()));
        let transcript_path = non_empty(context.transcript_path.as_deref())
            .map(str::to_owned)
            .or_else(|| {
                previous_same_session.and_then(|previous| previous.transcript_path.clone())
            });

        let has_activity = title.is_some()
            || context.title_like_text().is_some()
            || non_empty(context.response.as_deref()).is_some()
            || !matches!(session.status, CLIAgentSessionStatus::InProgress);
        if !has_activity {
            return None;
        }

        let mut record = Self {
            agent,
            session_id: Some(session_id),
            launch_cwd,
            title,
            transcript_path,
            status: Some(PersistedCLIAgentStatus::from(&session.status)),
            remote_host: non_empty(session.remote_host.as_deref()).map(str::to_owned),
            updated_at_unix_ms: now_unix_ms,
        };
        if let Some(previous) = previous
            && previous.has_same_content(&record)
        {
            record.updated_at_unix_ms = previous.updated_at_unix_ms;
        }
        Some(record)
    }

    /// Whether both records describe the same session state, ignoring when they were updated.
    pub fn has_same_content(&self, other: &Self) -> bool {
        self.agent == other.agent
            && self.session_id == other.session_id
            && self.launch_cwd == other.launch_cwd
            && self.title == other.title
            && self.transcript_path == other.transcript_path
            && self.status == other.status
            && self.remote_host == other.remote_host
    }

    pub fn is_remote(&self) -> bool {
        self.remote_host.is_some()
    }

    /// Single-line title for display, if the session has one.
    pub fn display_title(&self) -> Option<String> {
        let title = non_empty(self.title.as_deref())?;
        Some(title.split_whitespace().collect::<Vec<_>>().join(" "))
    }

    /// The agent command that resumes this session, without changing directories.
    pub fn agent_resume_command(&self, shell_type: ShellType) -> Option<String> {
        let session_id = self.session_id.as_deref().and_then(valid_session_id);
        let command = match (self.agent, session_id) {
            (CLIAgent::Claude, Some(id)) => {
                format!("claude --resume {}", quote_shell_arg(id, shell_type))
            }
            (CLIAgent::Claude, None) => "claude --continue".to_owned(),
            (CLIAgent::Codex, Some(id)) => {
                format!("codex resume {}", quote_shell_arg(id, shell_type))
            }
            (CLIAgent::Codex, None) => "codex resume --last".to_owned(),
            (
                CLIAgent::Gemini
                | CLIAgent::Amp
                | CLIAgent::Droid
                | CLIAgent::OpenCode
                | CLIAgent::Copilot
                | CLIAgent::Pi
                | CLIAgent::OhMyPi
                | CLIAgent::Auggie
                | CLIAgent::CursorCli
                | CLIAgent::Goose
                | CLIAgent::Hermes
                | CLIAgent::Vibe
                | CLIAgent::Antigravity
                | CLIAgent::Grok
                | CLIAgent::Kiro
                | CLIAgent::Muse
                | CLIAgent::WarpTui
                | CLIAgent::Unknown,
                _,
            ) => return None,
        };
        Some(command)
    }

    /// The commands that resume this session in a shell whose working directory is `pane_cwd`:
    /// a `cd` to the launch directory (omitted when the pane is already there), then the agent
    /// command. They're meant to run as separate blocks, the agent only if `cd` succeeds, so
    /// Warp's command detection recognizes the agent.
    pub fn resume_command_steps(
        &self,
        shell_type: ShellType,
        pane_cwd: Option<&str>,
    ) -> Option<Vec<String>> {
        let agent_command = self.agent_resume_command(shell_type)?;
        let mut steps = Vec::with_capacity(2);
        if let Some(launch_cwd) = non_empty(self.launch_cwd.as_deref())
            && !paths_match(launch_cwd, pane_cwd)
        {
            steps.push(change_directory_command(launch_cwd, shell_type));
        }
        steps.push(agent_command);
        Some(steps)
    }

    /// A single command line that resumes this session from any directory.
    pub fn resume_command_line(&self, shell_type: ShellType) -> Option<String> {
        let agent_command = self.agent_resume_command(shell_type)?;
        let Some(launch_cwd) = non_empty(self.launch_cwd.as_deref()) else {
            return Some(agent_command);
        };
        let cd = change_directory_command(launch_cwd, shell_type);
        Some(match shell_type {
            // Windows PowerShell 5.1 doesn't support `&&`.
            ShellType::PowerShell => format!("{cd}; if ($?) {{ {agent_command} }}"),
            ShellType::Bash | ShellType::Zsh | ShellType::Fish => {
                format!("{cd} && {agent_command}")
            }
        })
    }
}

fn change_directory_command(path: &str, shell_type: ShellType) -> String {
    // `cd` would treat a leading `-` as an option even when quoted.
    let path = if path.starts_with('-') {
        format!("./{path}")
    } else {
        path.to_owned()
    };
    let quoted = quote_shell_arg(&path, shell_type);
    match shell_type {
        ShellType::PowerShell => format!("Set-Location -LiteralPath {quoted}"),
        ShellType::Bash | ShellType::Zsh | ShellType::Fish => format!("cd {quoted}"),
    }
}

fn paths_match(launch_cwd: &str, pane_cwd: Option<&str>) -> bool {
    fn normalize(path: &str) -> &str {
        let trimmed = path.trim_end_matches(['/', '\\']);
        if trimmed.is_empty() { path } else { trimmed }
    }
    non_empty(pane_cwd).is_some_and(|pane_cwd| normalize(launch_cwd) == normalize(pane_cwd))
}

/// Whether `value` can be passed to `shell_type` unquoted. Deliberately conservative: anything
/// beyond a path-like token is quoted.
fn is_bare_word(value: &str, shell_type: ShellType) -> bool {
    match shell_type {
        ShellType::PowerShell => false,
        ShellType::Bash | ShellType::Zsh | ShellType::Fish => {
            !value.is_empty()
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':'))
        }
    }
}

/// Quotes `value` as a single literal argument for `shell_type`.
pub fn quote_shell_arg(value: &str, shell_type: ShellType) -> String {
    if is_bare_word(value, shell_type) {
        return value.to_owned();
    }
    match shell_type {
        // `shell_quote_arg` leaves backslashes alone, but fish treats `\\` and `\'` as escapes
        // inside single quotes.
        ShellType::Fish => format!("'{}'", value.replace('\\', r"\\").replace('\'', r"\'")),
        // PowerShell also treats typographic single quotes as quote characters; doubling any
        // of them inside a single-quoted string yields that character literally.
        ShellType::PowerShell => {
            let mut quoted = String::with_capacity(value.len() + 2);
            quoted.push('\'');
            for c in value.chars() {
                if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
                    quoted.push(c);
                }
                quoted.push(c);
            }
            quoted.push('\'');
            quoted
        }
        ShellType::Bash | ShellType::Zsh => shell_quote_arg(value, shell_type),
    }
}

/// Whether the agent's transcript for `record` exists on this machine: `Some(false)` when it's
/// known to be missing, `None` when it can't be determined. Does blocking file system I/O, so
/// it must run off the main thread.
#[cfg(not(target_family = "wasm"))]
pub fn transcript_exists(record: &PersistedCLIAgentSession) -> Option<bool> {
    use std::path::Path;

    use crate::ai::agent_sdk::driver::harness::{claude_transcript, codex_transcript};

    if record.is_remote() {
        return None;
    }
    if let Some(path) = non_empty(record.transcript_path.as_deref())
        && Path::new(path).is_file()
    {
        return Some(true);
    }
    let session_id = record.session_id.as_deref().and_then(valid_session_id)?;
    match record.agent {
        CLIAgent::Claude => {
            let projects_dir = claude_transcript::claude_config_dir()
                .ok()?
                .join("projects");
            let file_name = format!("{session_id}.jsonl");
            let entries = std::fs::read_dir(&projects_dir).ok()?;
            let found = entries
                .flatten()
                .any(|entry| entry.path().join(&file_name).is_file());
            Some(found)
        }
        CLIAgent::Codex => {
            let session_id = uuid::Uuid::parse_str(session_id).ok()?;
            let sessions_root = codex_transcript::codex_sessions_root().ok()?;
            Some(codex_transcript::find_session_file(&sessions_root, session_id).is_some())
        }
        CLIAgent::Gemini
        | CLIAgent::Amp
        | CLIAgent::Droid
        | CLIAgent::OpenCode
        | CLIAgent::Copilot
        | CLIAgent::Pi
        | CLIAgent::OhMyPi
        | CLIAgent::Auggie
        | CLIAgent::CursorCli
        | CLIAgent::Goose
        | CLIAgent::Hermes
        | CLIAgent::Vibe
        | CLIAgent::Antigravity
        | CLIAgent::Grok
        | CLIAgent::Kiro
        | CLIAgent::Muse
        | CLIAgent::WarpTui
        | CLIAgent::Unknown => None,
    }
}

#[cfg(test)]
#[path = "restore_tests.rs"]
mod tests;
