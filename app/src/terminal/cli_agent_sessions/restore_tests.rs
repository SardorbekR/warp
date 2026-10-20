use super::*;
use crate::terminal::cli_agent_sessions::{CLIAgentInputState, CLIAgentSessionContext};

const CLAUDE_ID: &str = "3f2a9c1e-8d4b-4f6a-9e2d-1b7c5a0f8e3d";
const CODEX_ID: &str = "019a6c2e-7b1d-7e42-a5f3-2c9d8e1b4a60";

fn record(
    agent: CLIAgent,
    session_id: Option<&str>,
    launch_cwd: Option<&str>,
) -> PersistedCLIAgentSession {
    PersistedCLIAgentSession {
        agent,
        session_id: session_id.map(str::to_owned),
        launch_cwd: launch_cwd.map(str::to_owned),
        title: Some("fix the login bug".to_owned()),
        transcript_path: None,
        status: None,
        remote_host: None,
        launched_locally: true,
        updated_at_unix_ms: 1_000,
    }
}

fn live_session(agent: CLIAgent, session_context: CLIAgentSessionContext) -> CLIAgentSession {
    CLIAgentSession {
        agent,
        status: CLIAgentSessionStatus::InProgress,
        session_context,
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        remote_host: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification: true,
    }
}

fn context(
    session_id: Option<&str>,
    launch_cwd: Option<&str>,
    query: Option<&str>,
) -> CLIAgentSessionContext {
    CLIAgentSessionContext {
        session_id: session_id.map(str::to_owned),
        resume_session_id: session_id.map(str::to_owned),
        cwd: launch_cwd.map(str::to_owned),
        launch_cwd: launch_cwd.map(str::to_owned),
        query: query.map(str::to_owned),
        ..Default::default()
    }
}

#[test]
fn json_round_trip() {
    let mut original = record(CLIAgent::Claude, Some(CLAUDE_ID), Some("/Users/me/proj"));
    original.transcript_path = Some("/Users/me/.claude/projects/x/y.jsonl".to_owned());
    original.remote_host = Some("me@devbox".to_owned());
    original.status = Some(PersistedCLIAgentStatus::Blocked);

    let json = original.to_json().expect("record should serialize");
    assert_eq!(PersistedCLIAgentSession::from_json(&json), Some(original));
}

#[test]
fn malformed_json_is_none() {
    for json in [
        "",
        "{",
        "null",
        "[]",
        "42",
        r#"{"agent":"Claude","session_id":7}"#,
    ] {
        assert_eq!(PersistedCLIAgentSession::from_json(json), None, "{json}");
    }
}

#[test]
fn json_is_versioned() {
    let original = record(CLIAgent::Codex, Some(CODEX_ID), Some("/proj"));
    let json = original.to_json().expect("record should serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(value.get("v"), Some(&serde_json::json!(1)));

    // Records written before versioning are read as the first version.
    let unversioned = format!(r#"{{"agent":"Codex","session_id":"{CODEX_ID}"}}"#);
    assert!(PersistedCLIAgentSession::from_json(&unversioned).is_some());

    for version in ["2", "0", r#""1""#, "null"] {
        let json = format!(r#"{{"v":{version},"agent":"Codex","session_id":"{CODEX_ID}"}}"#);
        assert_eq!(PersistedCLIAgentSession::from_json(&json), None, "{json}");
    }
}

#[test]
fn missing_optional_fields_default() {
    let json = format!(r#"{{"v":1,"agent":"Codex","session_id":"{CODEX_ID}"}}"#);
    let restored =
        PersistedCLIAgentSession::from_json(&json).expect("agent and session id are enough");
    assert_eq!(restored.agent, CLIAgent::Codex);
    assert_eq!(restored.session_id.as_deref(), Some(CODEX_ID));
    assert_eq!(restored.launch_cwd, None);
    assert_eq!(restored.title, None);
    assert_eq!(restored.status, None);
    assert_eq!(restored.updated_at_unix_ms, 0);
}

#[test]
fn unknown_status_and_fields_are_ignored() {
    let json = format!(
        r#"{{"agent":"Claude","session_id":"{CLAUDE_ID}","status":"thinking_hard","future_field":{{"a":1}}}}"#
    );
    let restored = PersistedCLIAgentSession::from_json(&json).expect("record should parse");
    assert_eq!(restored.session_id.as_deref(), Some(CLAUDE_ID));
    assert_eq!(restored.status, None);
}

#[test]
fn unknown_or_unsupported_agents_are_none() {
    assert_eq!(
        PersistedCLIAgentSession::from_json(r#"{"agent":"NotAnAgent"}"#),
        None
    );
    assert_eq!(
        PersistedCLIAgentSession::from_json(r#"{"agent":"Gemini"}"#),
        None
    );
    assert_eq!(
        PersistedCLIAgentSession::from_json(r#"{"session_id":"abc"}"#),
        None
    );
}

#[test]
fn records_without_a_usable_session_id_are_none() {
    for session_id in [
        "null",
        r#""""#,
        r#""   ""#,
        r#""--dangerously-skip-permissions""#,
        r#""abc; rm -rf ~""#,
    ] {
        let json = format!(r#"{{"agent":"Claude","session_id":{session_id}}}"#);
        assert_eq!(PersistedCLIAgentSession::from_json(&json), None, "{json}");
    }
    assert_eq!(
        PersistedCLIAgentSession::from_json(r#"{"agent":"Claude"}"#),
        None
    );
}

#[test]
fn empty_fields_are_dropped() {
    let json = format!(
        r#"{{"agent":"Claude","session_id":" {CLAUDE_ID} ","launch_cwd":"  ","title":"","transcript_path":"","remote_host":""}}"#
    );
    let restored = PersistedCLIAgentSession::from_json(&json).expect("record should parse");
    assert_eq!(restored.session_id.as_deref(), Some(CLAUDE_ID));
    assert_eq!(restored.launch_cwd, None);
    assert_eq!(restored.title, None);
    assert_eq!(restored.transcript_path, None);
    assert_eq!(restored.remote_host, None);
}

#[test]
fn session_id_validation() {
    assert_eq!(valid_session_id(CLAUDE_ID), Some(CLAUDE_ID));
    assert_eq!(valid_session_id(" abc_1.2:3 "), Some("abc_1.2:3"));
    assert_eq!(valid_session_id(""), None);
    assert_eq!(valid_session_id("-r"), None);
    assert_eq!(valid_session_id("abc; rm -rf ~"), None);
    assert_eq!(valid_session_id("$(whoami)"), None);
    assert_eq!(valid_session_id(&"a".repeat(MAX_SESSION_ID_LEN + 1)), None);
}

#[test]
fn long_titles_are_truncated_on_char_boundaries() {
    let title = "é".repeat(MAX_TITLE_CHARS + 50);
    let json = serde_json::json!({ "agent": "Claude", "session_id": CLAUDE_ID, "title": title })
        .to_string();
    let restored = PersistedCLIAgentSession::from_json(&json).expect("record should parse");
    assert_eq!(
        restored.title.map(|title| title.chars().count()),
        Some(MAX_TITLE_CHARS)
    );
}

#[test]
fn display_title_collapses_whitespace() {
    let mut restored = record(CLIAgent::Claude, None, None);
    restored.title = Some("  fix\nthe   login\tbug ".to_owned());
    assert_eq!(
        restored.display_title().as_deref(),
        Some("fix the login bug")
    );
    restored.title = Some(" \n ".to_owned());
    assert_eq!(restored.display_title(), None);
}

#[test]
fn live_session_record() {
    let session = live_session(
        CLIAgent::Claude,
        CLIAgentSessionContext {
            transcript_path: Some("/t.jsonl".to_owned()),
            ..context(Some(CLAUDE_ID), Some("/proj"), Some("  add tests  "))
        },
    );
    let live =
        PersistedCLIAgentSession::from_live_session(&session, Some("/elsewhere"), None, true, 5)
            .expect("Claude sessions are persisted");
    assert_eq!(live.agent, CLIAgent::Claude);
    assert_eq!(live.session_id.as_deref(), Some(CLAUDE_ID));
    assert_eq!(live.launch_cwd.as_deref(), Some("/proj"));
    assert_eq!(live.title.as_deref(), Some("add tests"));
    assert_eq!(live.transcript_path.as_deref(), Some("/t.jsonl"));
    // Status flips are frequent and nothing reads them back, so they aren't persisted.
    assert_eq!(live.status, None);
    assert!(live.launched_locally);
    assert!(live.can_resume_here());
    assert_eq!(live.updated_at_unix_ms, 5);
}

#[test]
fn live_session_of_unsupported_agent_is_not_persisted() {
    let session = live_session(CLIAgent::Gemini, context(Some("abc"), Some("/proj"), None));
    assert_eq!(
        PersistedCLIAgentSession::from_live_session(&session, None, None, true, 5),
        None
    );
}

#[test]
fn live_session_falls_back_to_pane_cwd() {
    let session = live_session(
        CLIAgent::Codex,
        CLIAgentSessionContext {
            session_id: Some(CODEX_ID.to_owned()),
            resume_session_id: Some(CODEX_ID.to_owned()),
            query: Some("refactor".to_owned()),
            ..Default::default()
        },
    );
    let live = PersistedCLIAgentSession::from_live_session(&session, Some("/pane"), None, true, 5)
        .expect("Codex sessions are persisted");
    assert_eq!(live.session_id.as_deref(), Some(CODEX_ID));
    assert_eq!(live.launch_cwd.as_deref(), Some("/pane"));
}

#[test]
fn live_session_without_session_id_is_not_persisted() {
    let session = live_session(
        CLIAgent::Claude,
        context(None, Some("/proj"), Some("a prompt")),
    );
    assert_eq!(
        PersistedCLIAgentSession::from_live_session(&session, None, None, true, 5),
        None
    );
}

#[test]
fn fresh_session_without_activity_is_not_persisted() {
    // Only `SessionStart` was seen: the session is `InProgress` with an id but no prompt.
    let session = live_session(
        CLIAgent::Claude,
        context(Some(CLAUDE_ID), Some("/proj"), None),
    );
    assert_eq!(
        PersistedCLIAgentSession::from_live_session(&session, None, None, true, 5),
        None
    );
}

#[test]
fn status_or_summary_counts_as_activity() {
    let mut session = live_session(
        CLIAgent::Claude,
        context(Some(CLAUDE_ID), Some("/proj"), None),
    );
    session.status = CLIAgentSessionStatus::Blocked {
        message: None,
        source: crate::terminal::cli_agent_sessions::BlockedSource::PermissionRequest,
    };
    let live = PersistedCLIAgentSession::from_live_session(&session, None, None, true, 5)
        .expect("a blocked session has done something");
    assert_eq!(live.title, None);

    let mut session = live_session(
        CLIAgent::Claude,
        context(Some(CLAUDE_ID), Some("/proj"), None),
    );
    session.session_context.summary = Some("Wants to run Bash".to_owned());
    assert!(PersistedCLIAgentSession::from_live_session(&session, None, None, true, 5).is_some());
}

#[test]
fn resumed_session_inherits_restored_record_until_plugin_reports() {
    let restored = record(CLIAgent::Claude, Some(CLAUDE_ID), Some("/proj"));
    // Command detection creates the session before the plugin reports anything.
    let session = live_session(CLIAgent::Claude, CLIAgentSessionContext::default());
    let live = PersistedCLIAgentSession::from_live_session(
        &session,
        Some("/other"),
        Some(&restored),
        true,
        9,
    )
    .expect("Claude sessions are persisted");
    assert_eq!(live.session_id.as_deref(), Some(CLAUDE_ID));
    assert_eq!(live.launch_cwd.as_deref(), Some("/proj"));
    assert_eq!(live.title.as_deref(), Some("fix the login bug"));
    // Same content as the restored record, so the timestamp doesn't move.
    assert_eq!(live.updated_at_unix_ms, restored.updated_at_unix_ms);
}

#[test]
fn launch_cwd_stays_pinned_for_the_same_session_id() {
    let previous = record(CLIAgent::Claude, Some(CLAUDE_ID), Some("/proj"));
    let session = live_session(
        CLIAgent::Claude,
        context(Some(CLAUDE_ID), Some("/proj/subdir"), Some("next prompt")),
    );
    let live =
        PersistedCLIAgentSession::from_live_session(&session, None, Some(&previous), true, 9)
            .expect("Claude sessions are persisted");
    assert_eq!(live.launch_cwd.as_deref(), Some("/proj"));
    assert_eq!(live.title.as_deref(), Some("next prompt"));
    assert_eq!(live.updated_at_unix_ms, 9);
}

#[test]
fn new_session_id_replaces_previous_session_fields() {
    let mut previous = record(CLIAgent::Claude, Some(CLAUDE_ID), Some("/proj"));
    previous.transcript_path = Some("/old.jsonl".to_owned());
    let session = live_session(
        CLIAgent::Claude,
        context(
            Some("new-session"),
            Some("/proj/subdir"),
            Some("first prompt"),
        ),
    );
    let live =
        PersistedCLIAgentSession::from_live_session(&session, None, Some(&previous), true, 9)
            .expect("Claude sessions are persisted");
    assert_eq!(live.session_id.as_deref(), Some("new-session"));
    assert_eq!(live.launch_cwd.as_deref(), Some("/proj/subdir"));
    assert_eq!(live.title.as_deref(), Some("first prompt"));
    assert_eq!(live.transcript_path, None);

    // Until the new session does something, it isn't worth persisting.
    let session = live_session(
        CLIAgent::Claude,
        context(Some("new-session"), Some("/proj/subdir"), None),
    );
    assert_eq!(
        PersistedCLIAgentSession::from_live_session(&session, None, Some(&previous), true, 9),
        None
    );
}

#[test]
fn different_agent_does_not_inherit_previous_record() {
    let previous = record(CLIAgent::Claude, Some(CLAUDE_ID), Some("/proj"));
    let session = live_session(CLIAgent::Codex, CLIAgentSessionContext::default());
    // Without its own session id the Codex session can't be persisted.
    assert_eq!(
        PersistedCLIAgentSession::from_live_session(
            &session,
            Some("/pane"),
            Some(&previous),
            true,
            9
        ),
        None
    );
}

#[test]
fn status_change_alone_is_not_a_content_change() {
    let previous = record(CLIAgent::Claude, Some(CLAUDE_ID), Some("/proj"));
    let mut session = live_session(CLIAgent::Claude, CLIAgentSessionContext::default());
    for status in [
        CLIAgentSessionStatus::Success,
        CLIAgentSessionStatus::InProgress,
        CLIAgentSessionStatus::Cancelled,
    ] {
        session.status = status;
        let live =
            PersistedCLIAgentSession::from_live_session(&session, None, Some(&previous), true, 9)
                .expect("Claude sessions are persisted");
        assert!(live.has_same_content(&previous));
        assert_eq!(live.updated_at_unix_ms, previous.updated_at_unix_ms);
    }
}

#[test]
fn nested_agent_prompt_keeps_the_title() {
    let mut session = live_session(
        CLIAgent::Claude,
        context(Some(CLAUDE_ID), Some("/repo"), Some("fix the build")),
    );
    let record = PersistedCLIAgentSession::from_live_session(&session, None, None, true, 5)
        .expect("Claude sessions are persisted");
    assert_eq!(record.title.as_deref(), Some("fix the build"));

    // e.g. a `claude -p` run by the agent's shell tool reported its own session and prompt.
    session.session_context.session_id = Some("nested".to_owned());
    session.session_context.query = Some("summarize the logs".to_owned());
    let live = PersistedCLIAgentSession::from_live_session(&session, None, Some(&record), true, 9)
        .expect("Claude sessions are persisted");
    assert!(live.has_same_content(&record));

    // The agent's own next prompt retitles the record.
    session.session_context.session_id = Some(CLAUDE_ID.to_owned());
    session.session_context.query = Some("now add tests".to_owned());
    let live = PersistedCLIAgentSession::from_live_session(&session, None, Some(&record), true, 9)
        .expect("Claude sessions are persisted");
    assert_eq!(live.session_id.as_deref(), Some(CLAUDE_ID));
    assert_eq!(live.title.as_deref(), Some("now add tests"));
}

#[test]
fn sessions_not_started_from_the_local_command_line_are_copy_only() {
    let session = live_session(
        CLIAgent::Claude,
        context(Some(CLAUDE_ID), Some("/srv/app"), Some("deploy")),
    );
    // e.g. the plugin reported from an SSH session Warp didn't take over, or from tmux.
    let live = PersistedCLIAgentSession::from_live_session(&session, None, None, false, 5)
        .expect("the session is still persisted, to show and copy");
    assert!(!live.launched_locally);
    assert!(!live.is_remote());
    assert!(!live.can_resume_here());

    // Once seen running locally, the session stays resumable while it's tracked.
    let local = PersistedCLIAgentSession::from_live_session(&session, None, None, true, 5)
        .expect("Claude sessions are persisted");
    let later = PersistedCLIAgentSession::from_live_session(&session, None, Some(&local), false, 6)
        .expect("Claude sessions are persisted");
    assert!(later.can_resume_here());

    // A remote host always makes it copy-only.
    let mut remote = local.clone();
    remote.remote_host = Some("me@devbox".to_owned());
    assert!(!remote.can_resume_here());

    // Records from before this field existed are copy-only.
    let json = format!(r#"{{"v":1,"agent":"Claude","session_id":"{CLAUDE_ID}"}}"#);
    let restored = PersistedCLIAgentSession::from_json(&json).expect("record should parse");
    assert!(!restored.can_resume_here());
}

#[test]
fn paths_with_control_characters_are_dropped() {
    let json = serde_json::json!({
        "agent": "Claude",
        "session_id": CLAUDE_ID,
        "launch_cwd": "/tmp/x\n; rm -rf ~",
        "transcript_path": "/t/\u{1b}[2J.jsonl",
        "remote_host": "me@host\u{7}",
        "launched_locally": true,
    })
    .to_string();
    let restored = PersistedCLIAgentSession::from_json(&json).expect("record should parse");
    assert_eq!(restored.launch_cwd, None);
    assert_eq!(restored.transcript_path, None);
    assert_eq!(restored.remote_host, None);
    assert_eq!(
        restored.resume_command_steps(ShellType::Zsh, Some("/home/me")),
        Some(vec![format!("claude --resume {CLAUDE_ID}")])
    );

    let session = live_session(
        CLIAgent::Claude,
        CLIAgentSessionContext {
            launch_cwd: Some("/repo\r\u{1b}]0;x\u{7}".to_owned()),
            ..context(Some(CLAUDE_ID), None, Some("a prompt"))
        },
    );
    let live =
        PersistedCLIAgentSession::from_live_session(&session, Some("/pane\u{0}"), None, true, 5)
            .expect("Claude sessions are persisted");
    assert_eq!(live.launch_cwd, None);
    let live = PersistedCLIAgentSession::from_live_session(&session, Some("/pane"), None, true, 5)
        .expect("Claude sessions are persisted");
    assert_eq!(live.launch_cwd.as_deref(), Some("/pane"));
}

#[test]
fn display_title_drops_control_and_bidi_characters() {
    let mut restored = record(CLIAgent::Claude, Some(CLAUDE_ID), None);
    restored.title = Some("fix \u{1b}[31mthe\u{7} \u{202E}bug\u{2066}".to_owned());
    assert_eq!(restored.display_title().as_deref(), Some("fix [31mthe bug"));
    restored.title = Some("\u{1b}\u{7}".to_owned());
    assert_eq!(restored.display_title(), None);
}

#[test]
fn json_omits_unset_status() {
    let original = record(CLIAgent::Claude, Some(CLAUDE_ID), Some("/proj"));
    let json = original.to_json().expect("record should serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(value.get("status"), None);
    assert_eq!(
        value.get("launched_locally"),
        Some(&serde_json::json!(true))
    );
}

#[test]
fn resume_steps_skip_cd_when_pane_is_already_there() {
    let restored = record(CLIAgent::Claude, Some(CLAUDE_ID), Some("/Users/me/proj"));
    for pane_cwd in ["/Users/me/proj", "/Users/me/proj/"] {
        assert_eq!(
            restored.resume_command_steps(ShellType::Zsh, Some(pane_cwd)),
            Some(vec![format!("claude --resume {CLAUDE_ID}")])
        );
    }
}

#[test]
fn resume_steps_cd_to_launch_directory_first() {
    let restored = record(CLIAgent::Codex, Some(CODEX_ID), Some("/Users/me/my proj"));
    assert_eq!(
        restored.resume_command_steps(ShellType::Bash, Some("/Users/me")),
        Some(vec![
            "cd '/Users/me/my proj'".to_owned(),
            format!("codex resume {CODEX_ID}"),
        ])
    );
    assert_eq!(
        restored.resume_command_steps(ShellType::Bash, None),
        Some(vec![
            "cd '/Users/me/my proj'".to_owned(),
            format!("codex resume {CODEX_ID}"),
        ])
    );
}

#[test]
fn resume_without_session_id_uses_most_recent_session() {
    let claude = record(CLIAgent::Claude, None, None);
    assert_eq!(
        claude.resume_command_steps(ShellType::Zsh, Some("/proj")),
        Some(vec!["claude --continue".to_owned()])
    );
    let codex = record(CLIAgent::Codex, None, Some("/proj"));
    assert_eq!(
        codex.resume_command_line(ShellType::Fish),
        Some("cd /proj && codex resume --last".to_owned())
    );
}

#[test]
fn resume_ignores_unsafe_session_ids() {
    let restored = record(CLIAgent::Claude, Some("x; rm -rf ~"), None);
    assert_eq!(
        restored.resume_command_line(ShellType::Bash),
        Some("claude --continue".to_owned())
    );
}

#[test]
fn resume_command_line_per_shell() {
    let restored = record(
        CLIAgent::Claude,
        Some(CLAUDE_ID),
        Some("/Users/me/it's here"),
    );
    assert_eq!(
        restored.resume_command_line(ShellType::Bash),
        Some(format!(
            r#"cd '/Users/me/it'"'"'s here' && claude --resume {CLAUDE_ID}"#
        ))
    );
    assert_eq!(
        restored.resume_command_line(ShellType::Zsh),
        Some(format!(
            r#"cd '/Users/me/it'"'"'s here' && claude --resume {CLAUDE_ID}"#
        ))
    );
    assert_eq!(
        restored.resume_command_line(ShellType::Fish),
        Some(format!(
            r"cd '/Users/me/it\'s here' && claude --resume {CLAUDE_ID}"
        ))
    );
    assert_eq!(
        restored.resume_command_line(ShellType::PowerShell),
        Some(format!(
            "Set-Location -LiteralPath '/Users/me/it''s here'; if ($?) {{ claude --resume '{CLAUDE_ID}' }}"
        ))
    );
}

#[test]
fn resume_cd_into_dash_prefixed_relative_path() {
    let restored = record(CLIAgent::Claude, None, Some("-weird"));
    assert_eq!(
        restored.resume_command_steps(ShellType::Bash, None),
        Some(vec![
            "cd ./-weird".to_owned(),
            "claude --continue".to_owned()
        ])
    );
}

#[test]
fn quote_shell_arg_leaves_simple_paths_bare() {
    for shell_type in [ShellType::Bash, ShellType::Zsh, ShellType::Fish] {
        assert_eq!(
            quote_shell_arg("/Users/me/code/warp-fork_2.0", shell_type),
            "/Users/me/code/warp-fork_2.0"
        );
    }
    assert_eq!(quote_shell_arg("/tmp", ShellType::PowerShell), "'/tmp'");
}

#[test]
fn quote_shell_arg_quotes_special_characters() {
    for (value, expected) in [
        ("my proj", "'my proj'"),
        ("~/proj", "'~/proj'"),
        ("$HOME", "'$HOME'"),
        ("a;b", "'a;b'"),
        ("a\"b", "'a\"b'"),
        ("a'b", r#"'a'"'"'b'"#),
        ("a\\b", r"'a\b'"),
        ("=cmd", "'=cmd'"),
        ("", "''"),
    ] {
        assert_eq!(quote_shell_arg(value, ShellType::Bash), expected, "{value}");
        assert_eq!(quote_shell_arg(value, ShellType::Zsh), expected, "{value}");
    }
}

#[test]
fn quote_shell_arg_escapes_backslashes_for_fish() {
    assert_eq!(
        quote_shell_arg(r"C:\proj dir\", ShellType::Fish),
        r"'C:\\proj dir\\'"
    );
    assert_eq!(quote_shell_arg(r"a\'b", ShellType::Fish), r"'a\\\'b'");
    assert_eq!(quote_shell_arg("my proj", ShellType::Fish), "'my proj'");
}

#[test]
fn quote_shell_arg_doubles_all_powershell_quote_characters() {
    assert_eq!(
        quote_shell_arg("C:\\Users\\me\\it's", ShellType::PowerShell),
        "'C:\\Users\\me\\it''s'"
    );
    assert_eq!(
        quote_shell_arg("Bob’s $proj", ShellType::PowerShell),
        "'Bob’’s $proj'"
    );
}
