use std::time::Duration;

use warp_core::execution_mode::{AppExecutionMode, ExecutionMode};
use warpui::r#async::Timer;
use warpui::{App, EntityId};

use super::event::{
    CLIAgentEvent, CLIAgentEventPayload, CLIAgentEventSource, CLIAgentEventType, parse_event,
};
use super::{
    BlockedSource, CLIAgentInputEntrypoint, CLIAgentInputState, CLIAgentSession,
    CLIAgentSessionContext, CLIAgentSessionStatus, CLIAgentSessionsModel,
    CLIAgentSessionsModelEvent,
};
use crate::ai::blocklist::{InputConfig, InputType};
use crate::terminal::CLIAgent;

#[test]
fn parse_stop_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe","transcript_path":"/tmp/t.jsonl"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.v, 1);
    assert_eq!(notif.agent, CLIAgent::Claude);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.session_id.as_deref(), Some("abc"));
    assert_eq!(notif.cwd.as_deref(), Some("/tmp/proj"));
    assert_eq!(notif.project.as_deref(), Some("proj"));
    assert_eq!(notif.payload.query.as_deref(), Some("write a haiku"));
    assert_eq!(notif.payload.response.as_deref(), Some("Memory is safe"));
    assert_eq!(
        notif.payload.transcript_path.as_deref(),
        Some("/tmp/t.jsonl")
    );
}

#[test]
fn cli_agent_session_context_title_like_text_uses_trimmed_summary() {
    let context = CLIAgentSessionContext {
        summary: Some("  Reviewing changes  ".to_string()),
        query: Some("Latest prompt".to_string()),
        ..Default::default()
    };

    assert_eq!(
        context.title_like_text(),
        Some("Reviewing changes".to_string())
    );
}

#[test]
fn cli_agent_session_context_latest_user_prompt_uses_trimmed_query() {
    let context = CLIAgentSessionContext {
        summary: Some("Reviewing changes".to_string()),
        query: Some("  Latest prompt  ".to_string()),
        ..Default::default()
    };

    assert_eq!(
        context.latest_user_prompt(),
        Some("Latest prompt".to_string())
    );
}

#[test]
fn cli_agent_session_context_title_helpers_ignore_empty_text() {
    let context = CLIAgentSessionContext {
        summary: Some("  ".to_string()),
        query: Some("".to_string()),
        ..Default::default()
    };

    assert_eq!(context.title_like_text(), None);
    assert_eq!(context.latest_user_prompt(), None);
}

#[test]
fn parse_permission_request_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"permission_request","session_id":"abc","cwd":"/tmp/proj","project":"proj","summary":"Wants to run Bash: rm -rf /tmp","tool_name":"Bash","tool_input":{"command":"rm -rf /tmp"}}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::PermissionRequest);
    assert_eq!(
        notif.payload.summary.as_deref(),
        Some("Wants to run Bash: rm -rf /tmp")
    );
    assert_eq!(notif.payload.tool_name.as_deref(), Some("Bash"));
    assert_eq!(
        notif.payload.tool_input_preview.as_deref(),
        Some("rm -rf /tmp")
    );
}

#[test]
fn parse_permission_request_with_file_path() {
    let body = r#"{"v":1,"agent":"claude","event":"permission_request","session_id":"abc","cwd":"/tmp","project":"tmp","tool_name":"Write","tool_input":{"file_path":"/tmp/test.py","content":"print('hi')"}}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(
        notif.payload.tool_input_preview.as_deref(),
        Some("/tmp/test.py")
    );
}

#[test]
fn parse_idle_prompt_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"idle_prompt","session_id":"abc","cwd":"/tmp","project":"tmp","summary":"Claude is waiting for your input"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::IdlePrompt);
    assert_eq!(
        notif.payload.summary.as_deref(),
        Some("Claude is waiting for your input")
    );
}

#[test]
fn parse_agent_needs_input_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"agent_needs_input","session_id":"abc","cwd":"/tmp","project":"tmp","summary":"Some dialog text"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::NeedsInput);
    assert_eq!(notif.payload.summary.as_deref(), Some("Some dialog text"));
}

#[test]
fn parse_session_start_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"session_start","session_id":"abc","cwd":"/tmp","project":"tmp","plugin_version":"1.1.0"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::SessionStart);
    assert_eq!(notif.payload.plugin_version.as_deref(), Some("1.1.0"));
}

#[test]
fn returns_none_for_wrong_sentinel() {
    let body = r#"{"v":1,"agent":"claude","event":"stop"}"#;
    assert!(parse_event(Some("Claude Code"), body).is_none());
}

#[test]
fn returns_none_for_missing_title() {
    let body = r#"{"v":1,"agent":"claude","event":"stop"}"#;
    assert!(parse_event(None, body).is_none());
}

#[test]
fn returns_none_for_invalid_json() {
    assert!(parse_event(Some("warp://cli-agent"), "not json").is_none());
}

#[test]
fn handles_unknown_event_type() {
    let body = r#"{"v":1,"agent":"claude","event":"some_future_event"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();
    assert_eq!(
        notif.event,
        CLIAgentEventType::Unknown("some_future_event".to_string())
    );
}

#[test]
fn handles_missing_optional_fields() {
    let body = r#"{"event":"stop"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.v, 1);
    assert_eq!(notif.agent, CLIAgent::Unknown);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert!(notif.session_id.is_none());
    assert!(notif.cwd.is_none());
    assert!(notif.project.is_none());
    assert!(notif.payload.query.is_none());
}

#[test]
fn handles_special_characters_in_values() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","query":"what does \"hello\" mean?","response":"It means greeting. Use: printf(\"hello\")"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(
        notif.payload.query.as_deref(),
        Some("what does \"hello\" mean?")
    );
    assert_eq!(
        notif.payload.response.as_deref(),
        Some("It means greeting. Use: printf(\"hello\")")
    );
}

#[test]
fn rejects_unsupported_schema_version() {
    let body = r#"{"v":2,"agent":"claude","event":"stop"}"#;
    assert!(parse_event(Some("warp://cli-agent"), body).is_none());
}

#[test]
fn defaults_to_v1_when_version_missing() {
    let body = r#"{"agent":"claude","event":"stop","query":"hi"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();
    assert_eq!(notif.v, 1);
    assert_eq!(notif.payload.query.as_deref(), Some("hi"));
}

#[test]
fn explicit_v1_parses_correctly() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","query":"test"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();
    assert_eq!(notif.v, 1);
    assert_eq!(notif.payload.query.as_deref(), Some("test"));
}

#[test]
fn parse_prompt_submit_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"prompt_submit","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"fix the bug"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::PromptSubmit);
    assert_eq!(notif.payload.query.as_deref(), Some("fix the bug"));
}

#[test]
fn parse_tool_complete_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"tool_complete","session_id":"abc","cwd":"/tmp/proj","project":"proj","tool_name":"Bash"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::ToolComplete);
    assert_eq!(notif.payload.tool_name.as_deref(), Some("Bash"));
}

#[test]
fn parse_auggie_stop_notification() {
    // Mirrors what the community auggie-warp plugin emits on the Stop hook.
    let body = r#"{"v":1,"agent":"auggie","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.agent, CLIAgent::Auggie);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.payload.query.as_deref(), Some("write a haiku"));
    assert_eq!(notif.payload.response.as_deref(), Some("Memory is safe"));
}

#[test]
fn parse_pi_stop_notification() {
    // Mirrors what the community pi-mono plugin emits on the Stop hook —
    // matches the Auggie shape and uses `"agent":"pi"`, which `resolve_agent`
    // already maps to `CLIAgent::Pi` via `command_prefix()`.
    let body = r#"{"v":1,"agent":"pi","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.agent, CLIAgent::Pi);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.payload.query.as_deref(), Some("write a haiku"));
    assert_eq!(notif.payload.response.as_deref(), Some("Memory is safe"));
}

#[test]
fn parse_droid_stop_notification() {
    // Droid is already a known CLI agent, so structured OSC 777 events using
    // `"agent":"droid"` should resolve through the existing command prefix
    // parser without any Droid-specific parser logic.
    let body = r#"{"v":1,"agent":"droid","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.agent, CLIAgent::Droid);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.payload.query.as_deref(), Some("write a haiku"));
    assert_eq!(notif.payload.response.as_deref(), Some("Memory is safe"));
}

#[test]
fn apply_event_preserves_input_session() {
    let input_state = CLIAgentInputState::Open {
        entrypoint: CLIAgentInputEntrypoint::CtrlG,
        previous_input_config: InputConfig {
            input_type: InputType::Shell,
            is_locked: false,
        },
        previous_was_lock_set_with_empty_buffer: true,
    };
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state,
        should_auto_toggle_input: false,
        listener: None,
        remote_host: None,
        plugin_version: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::PermissionRequest,
        session_id: Some("abc".to_string()),
        cwd: Some("/tmp/proj".to_string()),
        project: Some("proj".to_string()),
        payload: CLIAgentEventPayload {
            summary: Some("Needs approval".to_string()),
            ..Default::default()
        },
    };

    session.apply_event(&event);

    assert_eq!(session.input_state, input_state);
}

#[test]
fn is_remote_returns_true_when_remote_host_is_set() {
    let session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: Some("user@devbox".to_owned()),
        custom_command_prefix: None,
        received_rich_notification: false,
    };
    assert!(session.is_remote());
}

#[test]
fn is_remote_returns_false_when_remote_host_is_none() {
    let session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        remote_host: None,
        plugin_version: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };
    assert!(!session.is_remote());
}

#[test]
fn local_failure_is_shared_across_local_sessions() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, None);

    assert!(model.has_plugin_auto_failed(CLIAgent::Claude, &None));
}

#[test]
fn local_failure_does_not_affect_remote_host() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, None);

    let remote = Some("user@devbox".to_owned());
    assert!(!model.has_plugin_auto_failed(CLIAgent::Claude, &remote));
}

#[test]
fn remote_failure_does_not_affect_local() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, Some("user@devbox".to_owned()));

    assert!(!model.has_plugin_auto_failed(CLIAgent::Claude, &None));
}

#[test]
fn remote_failures_are_independent_per_host() {
    let mut model = CLIAgentSessionsModel::new();

    let host_a = Some("user@host-a".to_owned());
    let host_b = Some("user@host-b".to_owned());

    model.record_plugin_auto_failure(CLIAgent::Claude, host_a.clone());

    assert!(model.has_plugin_auto_failed(CLIAgent::Claude, &host_a));
    assert!(!model.has_plugin_auto_failed(CLIAgent::Claude, &host_b));
}

#[test]
fn failure_tracking_is_independent_per_agent() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, None);

    assert!(model.has_plugin_auto_failed(CLIAgent::Claude, &None));
    assert!(!model.has_plugin_auto_failed(CLIAgent::Gemini, &None));
}

#[test]
fn session_start_sets_plugin_version() {
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::SessionStart,
        session_id: Some("abc".to_owned()),
        cwd: Some("/tmp".to_owned()),
        project: Some("proj".to_owned()),
        payload: CLIAgentEventPayload {
            plugin_version: Some("1.5.0".to_owned()),
            ..Default::default()
        },
    };

    session.apply_event(&event);
    assert_eq!(session.plugin_version.as_deref(), Some("1.5.0"));
}

#[test]
fn session_start_without_plugin_version_leaves_none() {
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::SessionStart,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };

    session.apply_event(&event);
    assert_eq!(session.plugin_version, None);
}

#[test]
fn codex_session_not_rich_until_rich_notification() {
    // Codex's OSC 9 fallback never sets `received_rich_notification`, so the
    // session must not claim rich status even when a fallback listener exists.
    let mut session = CLIAgentSession {
        agent: CLIAgent::Codex,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        remote_host: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };
    assert!(!session.supports_rich_status());

    // A structured OSC 777 notification latches the flag -> rich status.
    session.received_rich_notification = true;
    assert!(session.supports_rich_status());
}

#[test]
fn non_codex_session_rich_after_rich_notification() {
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        remote_host: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };
    // No listener and no rich notification yet.
    assert!(!session.supports_rich_status());

    session.received_rich_notification = true;
    assert!(session.supports_rich_status());
}

/// Constructs a session with permission-scoped state already populated, as if
/// a `PermissionRequest` had just been received and the agent is now Blocked.
/// Used by the GH-9525 regression tests below.
fn blocked_claude_session_with_permission_state() -> CLIAgentSession {
    CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::Blocked {
            message: Some("Wants to run bash: rm -rf /tmp".to_owned()),
            source: BlockedSource::PermissionRequest,
        },
        session_context: CLIAgentSessionContext {
            summary: Some("Wants to run bash: rm -rf /tmp".to_owned()),
            tool_name: Some("Bash".to_owned()),
            tool_input_preview: Some("rm -rf /tmp".to_owned()),
            ..Default::default()
        },
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    }
}

#[test]
fn stop_clears_permission_scoped_state() {
    // GH-9525: after a PermissionRequest sets `summary`, the Stop event must
    // clear it. Otherwise the tab title falls back to the stale permission
    // text instead of reflecting the now-completed session.
    let mut session = blocked_claude_session_with_permission_state();

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::Stop,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload {
            query: Some("write a haiku".to_owned()),
            response: Some("Memory is safe".to_owned()),
            ..Default::default()
        },
    };

    session.apply_event(&event);

    assert_eq!(session.session_context.summary, None);
    assert_eq!(session.session_context.tool_name, None);
    assert_eq!(session.session_context.tool_input_preview, None);
    assert_eq!(
        session.session_context.query.as_deref(),
        Some("write a haiku"),
    );
    assert_eq!(
        session.session_context.response.as_deref(),
        Some("Memory is safe"),
    );
    assert!(matches!(session.status, CLIAgentSessionStatus::Success));
}

#[test]
fn permission_replied_clears_permission_scoped_state() {
    // When the user replies to a permission prompt the agent transitions back
    // to InProgress; the now-stale summary/tool fields must be cleared so they
    // don't leak into UI surfaces during the next turn.
    let mut session = blocked_claude_session_with_permission_state();

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::PermissionReplied,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };

    session.apply_event(&event);

    assert_eq!(session.session_context.summary, None);
    assert_eq!(session.session_context.tool_name, None);
    assert_eq!(session.session_context.tool_input_preview, None);
    assert!(matches!(session.status, CLIAgentSessionStatus::InProgress));
}

#[test]
fn prompt_submit_clears_permission_scoped_state() {
    // PromptSubmit already clears `response`; clearing the permission-scoped
    // fields keeps the same hygiene if the user manages to start a new turn
    // while permission state is still populated (e.g. an abandoned permission
    // flow that was not closed by an explicit PermissionReplied).
    let mut session = blocked_claude_session_with_permission_state();
    session.session_context.response = Some("stale response".to_owned());

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::PromptSubmit,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload {
            query: Some("next prompt".to_owned()),
            ..Default::default()
        },
    };

    session.apply_event(&event);

    assert_eq!(session.session_context.summary, None);
    assert_eq!(session.session_context.tool_name, None);
    assert_eq!(session.session_context.tool_input_preview, None);
    assert_eq!(session.session_context.response, None);
    assert_eq!(
        session.session_context.query.as_deref(),
        Some("next prompt")
    );
    assert!(matches!(session.status, CLIAgentSessionStatus::InProgress));
}

#[test]
fn tool_complete_clears_permission_scoped_state() {
    // GH-11082: answering an AskUserQuestion emits only ToolComplete (the
    // plugin sends no PermissionReplied for it), so the Blocked -> InProgress
    // transition here must also clear the stale summary. Otherwise the tab
    // title keeps showing "Wants to run AskUserQuestion: ..." until the next
    // prompt or Stop.
    let mut session = blocked_claude_session_with_permission_state();

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::ToolComplete,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };

    session.apply_event(&event);

    assert_eq!(session.session_context.summary, None);
    assert_eq!(session.session_context.tool_name, None);
    assert_eq!(session.session_context.tool_input_preview, None);
    assert!(matches!(session.status, CLIAgentSessionStatus::InProgress));
}

#[test]
fn permission_request_still_populates_summary_and_tool_fields() {
    // Sanity: clearing permission-scoped state on Stop/Reply/Submit must not
    // also break the PermissionRequest path that initially populates them.
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::PermissionRequest,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload {
            summary: Some("Wants to run bash: rm -rf /tmp".to_owned()),
            tool_name: Some("Bash".to_owned()),
            tool_input_preview: Some("rm -rf /tmp".to_owned()),
            ..Default::default()
        },
    };

    session.apply_event(&event);

    assert_eq!(
        session.session_context.summary.as_deref(),
        Some("Wants to run bash: rm -rf /tmp"),
    );
    assert_eq!(session.session_context.tool_name.as_deref(), Some("Bash"));
    assert_eq!(
        session.session_context.tool_input_preview.as_deref(),
        Some("rm -rf /tmp"),
    );
    assert!(matches!(
        session.status,
        CLIAgentSessionStatus::Blocked { .. },
    ));
}

#[test]
fn needs_input_blocks_session_with_the_event_summary() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);

    let event = CLIAgentEvent {
        payload: CLIAgentEventPayload {
            summary: Some("Some dialog text".to_owned()),
            ..Default::default()
        },
        ..rich_event(CLIAgentEventType::NeedsInput)
    };

    let new_status = session.apply_event(&event);

    let expected = CLIAgentSessionStatus::Blocked {
        message: Some("Some dialog text".to_owned()),
        source: BlockedSource::NeedsInput,
    };
    assert_eq!(new_status, Some(expected.clone()));
    assert_eq!(session.status, expected);
    assert!(session.is_blocked_on_needs_input());
}

#[test]
fn needs_input_without_a_summary_blocks_session_with_the_generic_message() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);

    let new_status = session.apply_event(&rich_event(CLIAgentEventType::NeedsInput));

    let expected = CLIAgentSessionStatus::Blocked {
        message: Some("Agent requires user input".to_owned()),
        source: BlockedSource::NeedsInput,
    };
    assert_eq!(new_status, Some(expected));
    assert!(session.is_blocked_on_needs_input());
}

#[test]
fn blocked_status_records_the_event_that_caused_it() {
    for (event, expected_source) in [
        (
            CLIAgentEventType::PermissionRequest,
            BlockedSource::PermissionRequest,
        ),
        (
            CLIAgentEventType::QuestionAsked,
            BlockedSource::QuestionAsked,
        ),
        (CLIAgentEventType::NeedsInput, BlockedSource::NeedsInput),
    ] {
        let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);

        session.apply_event(&rich_event(event));

        let CLIAgentSessionStatus::Blocked { source, .. } = session.status else {
            panic!("expected a blocked status");
        };
        assert_eq!(source, expected_source);
        assert_eq!(
            session.is_blocked_on_needs_input(),
            expected_source == BlockedSource::NeedsInput
        );
    }
}

#[test]
fn permission_request_after_needs_input_is_no_longer_blocked_on_needs_input() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&rich_event(CLIAgentEventType::NeedsInput));

    session.apply_event(&rich_event(CLIAgentEventType::PermissionRequest));

    assert!(!session.is_blocked_on_needs_input());
}

#[test]
fn tool_complete_after_needs_input_returns_session_to_in_progress() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&rich_event(CLIAgentEventType::NeedsInput));

    session.apply_event(&rich_event(CLIAgentEventType::ToolComplete));

    assert_eq!(session.status, CLIAgentSessionStatus::InProgress);
    assert!(!session.is_blocked_on_needs_input());
}

#[test]
fn idle_prompt_after_needs_input_keeps_session_blocked() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&rich_event(CLIAgentEventType::NeedsInput));

    let new_status = session.apply_event(&rich_event(CLIAgentEventType::IdlePrompt));

    assert_eq!(new_status, None);
    assert!(matches!(
        session.status,
        CLIAgentSessionStatus::Blocked { .. }
    ));
    assert!(session.is_blocked_on_needs_input());
}

fn assert_needs_input_blocks_session_in_mode(mode: ExecutionMode, expect_blocked: bool) {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(mode, false, ctx));
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });

        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::NeedsInput), ctx);
        });

        let status = model.read(&app, |m, _| m.session(view_id).map(|s| s.status.clone()));
        assert_eq!(
            matches!(status, Some(CLIAgentSessionStatus::Blocked { .. })),
            expect_blocked
        );
    });
}

#[test]
fn needs_input_blocks_autonomous_sessions() {
    assert_needs_input_blocks_session_in_mode(ExecutionMode::Sdk, true);
}

#[test]
fn needs_input_is_ignored_in_the_desktop_app() {
    assert_needs_input_blocks_session_in_mode(ExecutionMode::App, false);
}

#[test]
fn stop_with_pending_background_work_keeps_session_in_progress() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","query":"build it","response":"Build started in the background.","background_task_count":1,"session_cron_count":2}"#;
    let event = parse_event(Some("warp://cli-agent"), body).unwrap();
    assert_eq!(event.payload.pending_background_work_count, Some(3));
    assert!(event.payload.has_pending_background_work());
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);

    assert_eq!(session.apply_event(&event), None);

    assert_eq!(session.status, CLIAgentSessionStatus::InProgress);
    assert_eq!(
        session.session_context.response.as_deref(),
        Some("Build started in the background.")
    );
}

#[test]
fn stop_without_pending_background_work_succeeds() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","background_task_count":0,"session_cron_count":0}"#;
    let event = parse_event(Some("warp://cli-agent"), body).unwrap();
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);

    assert_eq!(
        session.apply_event(&event),
        Some(CLIAgentSessionStatus::Success)
    );
}

// --- Ctrl-C pending-cancel state machine ---

/// Grace window used by the tests below. Long enough to comfortably
/// distinguish "fired at the original deadline" from "reset" under CI
/// scheduling jitter, short enough to keep the suite fast.
const TEST_WINDOW: Duration = Duration::from_millis(200);
/// Extra margin added when waiting for `TEST_WINDOW` to lapse.
const TEST_WINDOW_BUFFER: Duration = Duration::from_millis(150);

fn cli_agent_session(
    status: CLIAgentSessionStatus,
    received_rich_notification: bool,
) -> CLIAgentSession {
    CLIAgentSession {
        agent: CLIAgent::Claude,
        status,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        remote_host: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification,
    }
}

fn plugin_event(source: CLIAgentEventSource, event: CLIAgentEventType) -> CLIAgentEvent {
    CLIAgentEvent {
        source,
        v: 1,
        agent: CLIAgent::Claude,
        event,
        session_id: None,
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    }
}

fn rich_event(event: CLIAgentEventType) -> CLIAgentEvent {
    plugin_event(CLIAgentEventSource::RichPlugin, event)
}

#[test]
fn ctrl_c_does_not_arm_on_optimistic_in_progress_before_prompt_submit() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });

        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        assert!(
            !model.read(&app, |m, _| m
                .has_pending_or_resolved_ctrl_c_cancel(view_id)),
            "must not arm on the optimistic InProgress set at registration, before any turn started"
        );
    });
}

#[test]
fn ctrl_c_arms_when_in_progress_rich_and_prompt_submitted() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });

        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        assert!(model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));
    });
}

#[test]
fn ctrl_c_arms_when_blocked_rich_and_prompt_submitted() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(
                view_id,
                &rich_event(CLIAgentEventType::PermissionRequest),
                ctx,
            );
        });

        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        assert!(model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));
    });
}

#[test]
fn ctrl_c_does_not_arm_when_status_is_success() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::Stop), ctx);
        });

        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        assert!(!model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));
    });
}

#[test]
fn ctrl_c_does_not_arm_for_codex_osc9_fallback_session() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, false),
                ctx,
            );
        });
        // Codex's OSC 9 fallback never latches `received_rich_notification`,
        // even though `has_seen_prompt_submit` is still tracked.
        model.update(&mut app, |m, ctx| {
            m.update_from_event(
                view_id,
                &plugin_event(
                    CLIAgentEventSource::CodexOsc9Fallback,
                    CLIAgentEventType::PromptSubmit,
                ),
                ctx,
            );
        });

        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        assert!(
            !model.read(&app, |m, _| m
                .has_pending_or_resolved_ctrl_c_cancel(view_id)),
            "a Codex OSC 9 fallback session must never arm the Ctrl-C cancel window"
        );
    });
}

#[test]
fn window_lapse_transitions_session_to_cancelled() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled)
            );
            assert!(m.has_pending_or_resolved_ctrl_c_cancel(view_id));
        });
    });
}

/// Arms a window for an InProgress+rich session that has seen `prompt_submit`,
/// then applies `disarming_event` immediately and asserts the window no
/// longer resolves to `Cancelled` once it would have lapsed.
fn assert_event_disarms_pending_cancel(disarming_event: CLIAgentEvent) {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::Sdk, false, ctx));
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        assert!(model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));

        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &disarming_event, ctx);
        });
        assert!(!model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));

        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;

        model.read(&app, |m, _| {
            assert_ne!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled),
                "a disarming event must prevent the lapsed window from cancelling the session"
            );
        });
    });
}

#[test]
fn stop_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::Stop));
}

#[test]
fn stop_failure_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::StopFailure));
}

#[test]
fn permission_request_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::PermissionRequest));
}

#[test]
fn question_asked_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::QuestionAsked));
}

#[test]
fn needs_input_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::NeedsInput));
}

#[test]
fn prompt_submit_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::PromptSubmit));
}

#[test]
fn tool_complete_event_disarms_pending_cancel() {
    // ToolComplete only drives a status transition when the session is
    // Blocked, but any plugin traffic must still disarm the window.
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::ToolComplete));
}

#[test]
fn idle_prompt_does_not_disarm_pending_cancel() {
    // Unlike other plugin events, `IdlePrompt` means the CLI is sitting
    // idle at its interactive prompt -- evidence of idleness, not
    // aliveness. If it disarmed the window, an idle notification arriving
    // instead of a genuine `stop`/`stop_failure` after an interrupt would
    // leave the session stuck exactly like the bug this feature exists to
    // fix, so the window must survive it and still resolve to `Cancelled`
    // once it lapses.
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        assert!(model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));

        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::IdlePrompt), ctx);
        });
        assert!(
            model.read(&app, |m, _| {
                m.has_pending_or_resolved_ctrl_c_cancel(view_id)
            }),
            "an IdlePrompt must not disarm the pending-cancel window"
        );

        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled),
                "an IdlePrompt must not prevent the lapsed window from cancelling the session"
            );
        });
    });
}

#[test]
fn late_stop_after_cancelled_flips_status_to_success() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;
        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled)
            );
        });

        // A `stop` hook that arrives after the window already marked the
        // session Cancelled must still flow through and flip to Success.
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::Stop), ctx);
        });

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Success)
            );
        });
    });
}

#[test]
fn prompt_submit_after_cancelled_returns_session_to_in_progress() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;
        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled)
            );
        });

        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::InProgress)
            );
        });
    });
}

#[test]
fn stale_timer_callback_after_disarming_event_does_not_overwrite_newer_status() {
    // WarpUI's `SpawnedFutureHandle::abort` only takes effect the next time
    // the future is polled: if the timer already completed and queued its
    // resolve callback before a disarming event arrives, that callback can
    // still run after `abort()` was called on it. The token guard in
    // `resolve_pending_cancel` must make the stale callback a no-op instead
    // of overwriting the disarming event's status.
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        let armed_token = model
            .read(&app, |m, _| {
                m.ctrl_c_cancel_state
                    .get(&view_id)
                    .and_then(|s| s.armed_token)
            })
            .expect("window should be armed");

        // A disarming event arrives before the queued callback runs.
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::Stop), ctx);
        });
        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Success)
            );
        });

        // The stale callback finally runs with the pre-disarm token. It must
        // be a no-op: the disarming event's Success status must survive.
        model.update(&mut app, |m, ctx| {
            m.resolve_pending_cancel(view_id, armed_token, ctx);
        });

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Success),
                "a stale timer callback must not overwrite a newer disarming event's status"
            );
        });
    });
}

#[test]
fn remove_session_clears_pending_cancel_state() {
    // The model is a process-lifetime singleton, so a closed session that
    // saw a `prompt_submit` (and possibly armed a window) must not leave an
    // orphaned `ctrl_c_cancel_state` entry behind.
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        assert!(model.read(&app, |m, _| m.ctrl_c_cancel_state.contains_key(&view_id)));

        model.update(&mut app, |m, ctx| {
            m.remove_session(view_id, ctx);
        });

        assert!(
            model.read(&app, |m, _| !m.ctrl_c_cancel_state.contains_key(&view_id)),
            "remove_session must clear the ctrl_c_cancel_state entry for the closed session"
        );
    });
}

#[test]
fn second_ctrl_c_while_armed_reuses_the_existing_window() {
    // Deliberately larger/looser than `TEST_WINDOW` so the checkpoint below
    // has a wide, unambiguous margin on both sides: comfortably after the
    // original deadline, and comfortably before what a reset (rather than
    // reused) window would require.
    let window = Duration::from_millis(300);
    let before_deadline = Duration::from_millis(250);
    let checkpoint_after_second_ctrl_c = Duration::from_millis(100);

    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, window, ctx);
        });

        // Second Ctrl-C at t=250ms, shortly before the original t=300ms
        // deadline. Check at t=350ms: 50ms past the original deadline, but
        // 200ms short of a reset deadline (250+300=550ms) — reuse and reset
        // are unambiguous at this checkpoint.
        Timer::after(before_deadline).await;
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, window, ctx);
        });
        Timer::after(checkpoint_after_second_ctrl_c).await;

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled),
                "a second Ctrl-C while armed must reuse the existing window, not reset it"
            );
        });
    });
}

fn session_event(
    event: CLIAgentEventType,
    session_id: Option<&str>,
    cwd: Option<&str>,
    payload: CLIAgentEventPayload,
) -> CLIAgentEvent {
    CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event,
        session_id: session_id.map(str::to_owned),
        cwd: cwd.map(str::to_owned),
        project: None,
        payload,
    }
}

#[test]
fn parse_treats_empty_strings_as_missing() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","session_id":"","cwd":"","project":"","transcript_path":""}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.session_id, None);
    assert_eq!(notif.cwd, None);
    assert_eq!(notif.project, None);
    assert_eq!(notif.payload.transcript_path, None);
}

#[test]
fn empty_session_id_does_not_erase_known_session_id() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));

    let body = r#"{"v":1,"agent":"claude","event":"stop","session_id":"","cwd":""}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();
    session.apply_event(&notif);

    assert_eq!(session.session_context.session_id.as_deref(), Some("abc"));
    assert_eq!(session.session_context.cwd.as_deref(), Some("/repo"));
    assert_eq!(session.session_context.launch_cwd.as_deref(), Some("/repo"));
}

#[test]
fn launch_cwd_is_first_cwd_seen_for_the_session_id() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));
    session.apply_event(&session_event(
        CLIAgentEventType::PromptSubmit,
        Some("abc"),
        Some("/repo/sub dir"),
        CLIAgentEventPayload {
            query: Some("fix the build".to_owned()),
            ..Default::default()
        },
    ));

    assert_eq!(
        session.session_context.cwd.as_deref(),
        Some("/repo/sub dir")
    );
    assert_eq!(session.session_context.launch_cwd.as_deref(), Some("/repo"));
}

#[test]
fn launch_cwd_waits_for_the_first_reported_cwd() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("abc"),
        None,
        CLIAgentEventPayload::default(),
    ));
    assert_eq!(session.session_context.launch_cwd, None);

    session.apply_event(&session_event(
        CLIAgentEventType::PromptSubmit,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));
    assert_eq!(session.session_context.launch_cwd.as_deref(), Some("/repo"));
}

#[test]
fn transcript_path_is_captured_from_events() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&session_event(
        CLIAgentEventType::Stop,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload {
            transcript_path: Some("/home/me/.claude/projects/repo/abc.jsonl".to_owned()),
            ..Default::default()
        },
    ));
    assert_eq!(
        session.session_context.transcript_path.as_deref(),
        Some("/home/me/.claude/projects/repo/abc.jsonl")
    );

    // Events without a transcript path keep the known one.
    session.apply_event(&session_event(
        CLIAgentEventType::PromptSubmit,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));
    assert_eq!(
        session.session_context.transcript_path.as_deref(),
        Some("/home/me/.claude/projects/repo/abc.jsonl")
    );
}

#[test]
fn new_session_id_resets_launch_cwd_and_transcript_path() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&session_event(
        CLIAgentEventType::Stop,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload {
            transcript_path: Some("/t/abc.jsonl".to_owned()),
            ..Default::default()
        },
    ));
    session.apply_event(&session_event(
        CLIAgentEventType::PromptSubmit,
        Some("abc"),
        Some("/repo/sub"),
        CLIAgentEventPayload::default(),
    ));
    session.apply_event(&session_event(
        CLIAgentEventType::Stop,
        Some("abc"),
        Some("/repo/sub"),
        CLIAgentEventPayload::default(),
    ));

    // e.g. `/resume` of another session from a subdirectory, between turns.
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("def"),
        Some("/repo/sub"),
        CLIAgentEventPayload::default(),
    ));

    assert_eq!(session.session_context.session_id.as_deref(), Some("def"));
    assert_eq!(
        session.session_context.resume_session_id.as_deref(),
        Some("def")
    );
    assert_eq!(
        session.session_context.launch_cwd.as_deref(),
        Some("/repo/sub")
    );
    assert_eq!(session.session_context.transcript_path, None);
}

fn event_from(agent: CLIAgent, event: CLIAgentEventType, session_id: &str) -> CLIAgentEvent {
    CLIAgentEvent {
        agent,
        ..session_event(
            event,
            Some(session_id),
            Some("/elsewhere"),
            CLIAgentEventPayload {
                query: Some(format!("{agent:?} prompt")),
                transcript_path: Some(format!("/t/{session_id}.jsonl")),
                ..Default::default()
            },
        )
    }
}

/// A Claude session "main" in `/repo` that is in the middle of a turn.
fn session_mid_turn() -> CLIAgentSession {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("main"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));
    session.apply_event(&session_event(
        CLIAgentEventType::PromptSubmit,
        Some("main"),
        Some("/repo"),
        CLIAgentEventPayload {
            query: Some("fix the build".to_owned()),
            ..Default::default()
        },
    ));
    session
}

#[test]
fn nested_session_start_during_a_turn_keeps_the_resume_identity() {
    let mut session = session_mid_turn();
    assert!(session.session_context.resume_turn_in_progress);

    // The agent's shell tool runs `claude -p …` (or `claude -p --resume other …`), whose
    // plugin reports its own session to the same terminal.
    for event in [
        CLIAgentEventType::SessionStart,
        CLIAgentEventType::PromptSubmit,
        CLIAgentEventType::Stop,
    ] {
        session.apply_event(&event_from(CLIAgent::Claude, event, "nested"));
    }
    let context = &session.session_context;
    assert_eq!(context.resume_session_id.as_deref(), Some("main"));
    assert_eq!(context.launch_cwd.as_deref(), Some("/repo"));
    assert_eq!(context.transcript_path, None);
    assert!(context.query_from_other_session);
    assert!(
        context.resume_turn_in_progress,
        "the nested run's stop doesn't end the main turn"
    );

    // Another nested run in the same turn is ignored too.
    session.apply_event(&event_from(
        CLIAgent::Claude,
        CLIAgentEventType::SessionStart,
        "nested2",
    ));
    assert_eq!(
        session.session_context.resume_session_id.as_deref(),
        Some("main")
    );

    // Once the main session stops, an in-session `/resume` switches sessions.
    session.apply_event(&session_event(
        CLIAgentEventType::Stop,
        Some("main"),
        Some("/repo"),
        CLIAgentEventPayload {
            query: Some("fix the build".to_owned()),
            ..Default::default()
        },
    ));
    assert!(!session.session_context.query_from_other_session);
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("cleared"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));
    assert_eq!(
        session.session_context.resume_session_id.as_deref(),
        Some("cleared")
    );
    assert!(!session.session_context.resume_turn_in_progress);
}

#[test]
fn stop_with_background_work_keeps_the_turn_open() {
    let mut session = session_mid_turn();
    session.apply_event(&session_event(
        CLIAgentEventType::Stop,
        Some("main"),
        Some("/repo"),
        CLIAgentEventPayload {
            pending_background_work_count: Some(1),
            ..Default::default()
        },
    ));
    // e.g. a background shell task runs `claude -p`.
    session.apply_event(&event_from(
        CLIAgent::Claude,
        CLIAgentEventType::SessionStart,
        "nested",
    ));
    assert_eq!(
        session.session_context.resume_session_id.as_deref(),
        Some("main")
    );
}

#[test]
fn idle_prompt_ends_the_turn() {
    let mut session = session_mid_turn();
    session.apply_event(&session_event(
        CLIAgentEventType::IdlePrompt,
        Some("main"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));
    assert!(!session.session_context.resume_turn_in_progress);
}

#[test]
fn other_agents_events_never_change_the_resume_identity() {
    // `codex exec` run by Claude's shell tool reports Codex events (`task_started` is a
    // `SessionStart`) through Claude's listener, even between Claude's turns.
    let mut session = session_mid_turn();
    session.apply_event(&session_event(
        CLIAgentEventType::Stop,
        Some("main"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));
    for event in [
        CLIAgentEventType::SessionStart,
        CLIAgentEventType::PromptSubmit,
        CLIAgentEventType::Stop,
    ] {
        session.apply_event(&event_from(CLIAgent::Codex, event, "codex-thread"));
    }
    let context = &session.session_context;
    assert_eq!(context.resume_session_id.as_deref(), Some("main"));
    assert_eq!(context.launch_cwd.as_deref(), Some("/repo"));
    assert_eq!(context.transcript_path, None);
    assert!(context.query_from_other_session);
    assert!(!context.resume_turn_in_progress);
    // The live id still follows events, as before.
    assert_eq!(context.session_id.as_deref(), Some("codex-thread"));
}

#[test]
fn ctrl_c_cancel_ends_the_turn() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(view_id, session_mid_turn(), ctx);
            // What the Ctrl-C grace window does when the agent doesn't report back.
            m.force_cancel(view_id, ctx);
        });
        model.read(&app, |m, _| {
            let session = m.session(view_id).expect("session is registered");
            assert_eq!(session.status, CLIAgentSessionStatus::Cancelled);
            assert!(!session.session_context.resume_turn_in_progress);
        });
    });
}

#[test]
fn sessions_open_in_other_panes_are_found_by_resume_id() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let pane = EntityId::new();
        let other_pane = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(other_pane, session_mid_turn(), ctx);
        });
        model.read(&app, |m, _| {
            assert!(m.is_session_open_elsewhere(pane, CLIAgent::Claude, "main"));
            assert!(!m.is_session_open_elsewhere(other_pane, CLIAgent::Claude, "main"));
            assert!(!m.is_session_open_elsewhere(pane, CLIAgent::Codex, "main"));
            assert!(!m.is_session_open_elsewhere(pane, CLIAgent::Claude, "other"));
        });
    });
}

#[test]
fn nested_agent_ids_do_not_change_the_resume_identity() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload {
            transcript_path: Some("/t/abc.jsonl".to_owned()),
            ..Default::default()
        },
    ));
    session.apply_event(&session_event(
        CLIAgentEventType::PromptSubmit,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload {
            query: Some("fix the build".to_owned()),
            ..Default::default()
        },
    ));

    // e.g. a `claude -p` run by the agent's own shell tool announces and reports its own
    // session.
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("nested"),
        Some("/tmp/scratch"),
        CLIAgentEventPayload::default(),
    ));
    session.apply_event(&session_event(
        CLIAgentEventType::Stop,
        Some("nested"),
        Some("/tmp/scratch"),
        CLIAgentEventPayload {
            transcript_path: Some("/t/nested.jsonl".to_owned()),
            ..Default::default()
        },
    ));

    let context = &session.session_context;
    // The live id keeps following events, as before.
    assert_eq!(context.session_id.as_deref(), Some("nested"));
    assert_eq!(context.resume_session_id.as_deref(), Some("abc"));
    assert_eq!(context.launch_cwd.as_deref(), Some("/repo"));
    assert_eq!(context.transcript_path.as_deref(), Some("/t/abc.jsonl"));
    // Prompt fields keep following events, as before; the persisted record ignores them while
    // the live id is a nested one (see `restore::tests::nested_agent_prompt_keeps_the_title`).
}

#[test]
fn repeated_session_start_updates_session_id() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::Success, true);
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));
    let status = session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("def"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));

    assert_eq!(status, None);
    assert_eq!(session.status, CLIAgentSessionStatus::Success);
    assert_eq!(session.session_context.session_id.as_deref(), Some("def"));
    assert_eq!(
        session.session_context.resume_session_id.as_deref(),
        Some("def")
    );
}

#[test]
fn repeated_session_start_without_plugin_version_keeps_version() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload {
            plugin_version: Some("1.5.0".to_owned()),
            ..Default::default()
        },
    ));
    session.apply_event(&session_event(
        CLIAgentEventType::SessionStart,
        Some("def"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));

    assert_eq!(session.plugin_version.as_deref(), Some("1.5.0"));
}

#[test]
fn new_session_id_resets_prompt_and_tool_state() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&session_event(
        CLIAgentEventType::PermissionRequest,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload {
            summary: Some("Wants to run Bash".to_owned()),
            tool_name: Some("Bash".to_owned()),
            ..Default::default()
        },
    ));
    session.session_context.query = Some("old prompt".to_owned());

    // `/clear` starts a new session, first reported by its prompt: the old prompt no longer
    // describes it.
    session.apply_event(&session_event(
        CLIAgentEventType::PromptSubmit,
        Some("def"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));
    assert_eq!(
        session.session_context.resume_session_id.as_deref(),
        Some("def")
    );
    assert_eq!(session.session_context.query, None);
    assert_eq!(session.session_context.summary, None);
    assert_eq!(session.session_context.tool_name, None);
}

#[test]
fn first_reported_session_id_keeps_prompt_state() {
    // A session detected from its command gets its id from the first plugin event.
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, false);
    session.session_context.query = Some("Agent turn complete".to_owned());
    session.apply_event(&session_event(
        CLIAgentEventType::IdlePrompt,
        Some("abc"),
        Some("/repo"),
        CLIAgentEventPayload::default(),
    ));
    assert_eq!(session.session_context.session_id.as_deref(), Some("abc"));
    assert_eq!(
        session.session_context.query.as_deref(),
        Some("Agent turn complete")
    );
}

#[test]
fn parse_trims_session_id() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","session_id":"  abc \n"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();
    assert_eq!(notif.session_id.as_deref(), Some("abc"));
}

#[test]
fn identity_change_on_any_event_emits_session_updated() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::Success, true),
                ctx,
            );
        });
        let updates = std::rc::Rc::new(std::cell::Cell::new(0_usize));
        let counter = updates.clone();
        app.update(|ctx| {
            ctx.subscribe_to_model(&model, move |_, event, _| {
                if matches!(event, CLIAgentSessionsModelEvent::SessionUpdated { .. }) {
                    counter.set(counter.get().saturating_add(1));
                }
            });
        });

        // `IdlePrompt` changes neither status nor (normally) anything else, but here it is the
        // first event to report the session id.
        let idle = |session_id: &str| {
            session_event(
                CLIAgentEventType::IdlePrompt,
                Some(session_id),
                Some("/repo"),
                CLIAgentEventPayload::default(),
            )
        };
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &idle("abc"), ctx);
        });
        assert_eq!(updates.get(), 1);

        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &idle("abc"), ctx);
        });
        assert_eq!(updates.get(), 1, "an unchanged identity emits nothing");

        // Another id on an event that doesn't otherwise announce anything (e.g. the first stop
        // after `/clear`, which reports no `SessionStart`) changes it.
        let stop = session_event(
            CLIAgentEventType::Stop,
            Some("cleared"),
            Some("/repo"),
            CLIAgentEventPayload::default(),
        );
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &stop, ctx);
        });
        assert_eq!(updates.get(), 2);

        // So does a `SessionStart` with another id between turns (an in-session `/resume`).
        let session_start = session_event(
            CLIAgentEventType::SessionStart,
            Some("def"),
            Some("/repo"),
            CLIAgentEventPayload::default(),
        );
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &session_start, ctx);
        });
        assert_eq!(updates.get(), 3);
    });
}

/// A notification as the claude-code-warp plugin (2.4.0) sends it: the common fields its
/// `build_payload` adds, plus the event's own.
fn plugin_notification(event: &str, session_id: &str, fields: serde_json::Value) -> CLIAgentEvent {
    let mut body = serde_json::json!({
        "v": 1,
        "agent": "claude",
        "event": event,
        "session_id": session_id,
        "cwd": "/repo",
        "project": "repo",
    });
    if let (Some(body), Some(fields)) = (body.as_object_mut(), fields.as_object()) {
        body.extend(fields.clone());
    }
    parse_event(Some("warp://cli-agent"), &body.to_string()).expect("plugin notifications parse")
}

/// `on-session-start.sh`. Its hook only matches `startup|resume`: `/clear` and compaction send
/// nothing.
fn session_start(session_id: &str) -> CLIAgentEvent {
    plugin_notification(
        "session_start",
        session_id,
        serde_json::json!({ "plugin_version": "2.4.0" }),
    )
}

fn prompt_submit(session_id: &str, prompt: &str) -> CLIAgentEvent {
    plugin_notification(
        "prompt_submit",
        session_id,
        serde_json::json!({ "query": prompt }),
    )
}

fn tool_complete(session_id: &str) -> CLIAgentEvent {
    plugin_notification(
        "tool_complete",
        session_id,
        serde_json::json!({ "tool_name": "Bash" }),
    )
}

/// `on-stop.sh`; `background_tasks` counts e.g. a dev server the agent left running.
fn stop(session_id: &str, prompt: &str, background_tasks: u32) -> CLIAgentEvent {
    plugin_notification(
        "stop",
        session_id,
        serde_json::json!({
            "query": prompt,
            "response": "Done.",
            "transcript_path": format!("/home/me/.claude/projects/repo/{session_id}.jsonl"),
            "background_task_count": background_tasks,
            "session_cron_count": 0,
        }),
    )
}

fn idle_prompt(session_id: &str) -> CLIAgentEvent {
    plugin_notification(
        "idle_prompt",
        session_id,
        serde_json::json!({ "summary": "Claude is waiting for your input" }),
    )
}

fn permission_request(session_id: &str) -> CLIAgentEvent {
    plugin_notification(
        "permission_request",
        session_id,
        serde_json::json!({
            "summary": "Wants to run Bash: npm test",
            "tool_name": "Bash",
            "tool_input": { "command": "npm test" },
        }),
    )
}

fn apply_all(session: &mut CLIAgentSession, events: &[CLIAgentEvent]) {
    for event in events {
        session.apply_event(event);
    }
}

/// An interactive Claude session `old` that finished a turn and waits for the next prompt.
fn idle_claude_session() -> CLIAgentSession {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    apply_all(
        &mut session,
        &[
            session_start("old"),
            prompt_submit("old", "fix the build"),
            tool_complete("old"),
            stop("old", "fix the build", 0),
        ],
    );
    assert_eq!(
        session.session_context.resume_session_id.as_deref(),
        Some("old")
    );
    assert!(!session.session_context.resume_turn_in_progress);
    session
}

/// An interactive Claude session `old` in the middle of a turn.
fn claude_session_mid_turn() -> CLIAgentSession {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    apply_all(
        &mut session,
        &[session_start("old"), prompt_submit("old", "fix the build")],
    );
    assert!(session.session_context.resume_turn_in_progress);
    session
}

fn assert_resumes(session: &CLIAgentSession, session_id: &str) {
    assert_eq!(
        session.session_context.resume_session_id.as_deref(),
        Some(session_id)
    );
}

#[test]
fn clear_while_idle_switches_to_the_new_session() {
    let mut session = idle_claude_session();
    // `/clear`: the new session first shows up in its own prompt.
    apply_all(
        &mut session,
        &[
            prompt_submit("cleared", "start over"),
            tool_complete("cleared"),
        ],
    );
    assert_resumes(&session, "cleared");
    assert!(session.session_context.resume_turn_in_progress);
    assert_eq!(session.session_context.query.as_deref(), Some("start over"));
    assert!(!session.session_context.query_from_other_session);

    session.apply_event(&stop("cleared", "start over", 0));
    let context = &session.session_context;
    assert_resumes(&session, "cleared");
    assert_eq!(context.launch_cwd.as_deref(), Some("/repo"));
    assert_eq!(
        context.transcript_path.as_deref(),
        Some("/home/me/.claude/projects/repo/cleared.jsonl")
    );
    assert!(!context.resume_turn_in_progress);
}

#[test]
fn clear_after_escape_switches_to_the_new_session() {
    // Escape interrupted the turn, which reports no stop, so the turn still looks open.
    let mut session = claude_session_mid_turn();
    session.apply_event(&prompt_submit("cleared", "start over"));
    assert_resumes(&session, "cleared");
}

#[test]
fn clear_with_a_background_task_running_switches_to_the_new_session() {
    let mut session = claude_session_mid_turn();
    // A dev server the agent started keeps running, so every stop has background work.
    session.apply_event(&stop("old", "fix the build", 1));
    assert!(session.session_context.resume_turn_in_progress);

    session.apply_event(&prompt_submit("cleared", "start over"));
    assert_resumes(&session, "cleared");
    session.apply_event(&stop("cleared", "start over", 1));
    assert_resumes(&session, "cleared");
}

#[test]
fn in_session_resume_while_idle_switches_to_the_picked_session() {
    let mut session = idle_claude_session();
    apply_all(
        &mut session,
        &[session_start("picked"), prompt_submit("picked", "continue")],
    );
    assert_resumes(&session, "picked");
    assert_eq!(session.session_context.query.as_deref(), Some("continue"));
}

#[test]
fn nested_headless_run_during_a_turn_never_switches() {
    let mut session = claude_session_mid_turn();
    // The agent's shell tool runs `claude -p "summarize"`.
    apply_all(
        &mut session,
        &[
            session_start("nested"),
            prompt_submit("nested", "summarize"),
            tool_complete("nested"),
            stop("nested", "summarize", 0),
        ],
    );
    assert_resumes(&session, "old");
    assert!(
        session.session_context.resume_turn_in_progress,
        "the nested run's stop doesn't end the turn"
    );
    assert!(session.session_context.query_from_other_session);

    apply_all(
        &mut session,
        &[tool_complete("old"), stop("old", "fix the build", 0)],
    );
    let context = &session.session_context;
    assert_resumes(&session, "old");
    assert!(!context.resume_turn_in_progress);
    assert!(!context.query_from_other_session);
    assert_eq!(
        context.transcript_path.as_deref(),
        Some("/home/me/.claude/projects/repo/old.jsonl")
    );
    assert!(!context.showed_interactive_prompt);

    // A late event from the nested run, after the turn, is still recognized as nested.
    session.apply_event(&stop("nested", "summarize", 0));
    assert_resumes(&session, "old");
}

#[test]
fn nested_id_is_adopted_once_it_shows_an_idle_prompt() {
    // Escape interrupted the turn (no stop), then `/resume` picked another session: its
    // `SessionStart` arrives while the turn still looks open, like a nested run's would.
    let mut session = claude_session_mid_turn();
    apply_all(
        &mut session,
        &[
            session_start("picked"),
            prompt_submit("picked", "continue"),
            stop("picked", "continue", 0),
        ],
    );
    assert_resumes(&session, "old");

    // Headless runs never sit at an idle prompt.
    session.apply_event(&idle_prompt("picked"));
    let context = &session.session_context;
    assert_resumes(&session, "picked");
    assert!(!context.resume_turn_in_progress);
    assert!(context.showed_interactive_prompt);
    assert!(context.nested_session_ids.is_empty());
}

#[test]
fn nested_id_is_adopted_once_it_asks_for_permission() {
    let mut session = claude_session_mid_turn();
    apply_all(
        &mut session,
        &[session_start("picked"), prompt_submit("picked", "continue")],
    );
    assert_resumes(&session, "old");

    // Headless runs never show permission prompts.
    session.apply_event(&permission_request("picked"));
    assert_resumes(&session, "picked");
    assert!(session.session_context.resume_turn_in_progress);
    assert!(session.session_context.showed_interactive_prompt);
}

#[test]
fn idle_prompt_of_the_session_ends_a_turn_left_open() {
    let mut session = claude_session_mid_turn();
    session.apply_event(&idle_prompt("old"));
    assert!(!session.session_context.resume_turn_in_progress);
    assert!(session.session_context.showed_interactive_prompt);
    // So an in-session `/resume` switches sessions again.
    session.apply_event(&session_start("picked"));
    assert_resumes(&session, "picked");
}

#[test]
fn nested_ids_are_bounded() {
    let mut session = claude_session_mid_turn();
    for index in 0..20 {
        session.apply_event(&session_start(&format!("nested-{index}")));
    }
    let nested = &session.session_context.nested_session_ids;
    assert_eq!(nested.len(), 16);
    assert_eq!(nested.front().map(String::as_str), Some("nested-4"));
    assert_resumes(&session, "old");
}

#[test]
fn unnamed_agents_events_never_switch_sessions() {
    let mut session = idle_claude_session();
    let mut event = prompt_submit("other", "hi");
    event.agent = CLIAgent::Unknown;
    session.apply_event(&event);
    assert_resumes(&session, "old");
}

#[test]
fn unanswered_escape_ends_the_turn() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(view_id, claude_session_mid_turn(), ctx);
            m.observe_escape_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;

        model.read(&app, |m, _| {
            let session = m.session(view_id).expect("session is registered");
            assert!(!session.session_context.resume_turn_in_progress);
            assert_eq!(
                session.status,
                CLIAgentSessionStatus::InProgress,
                "Escape may only have closed a menu, so the status is left alone"
            );
        });
        // An in-session `/resume` now switches sessions.
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &session_start("picked"), ctx);
        });
        model.read(&app, |m, _| {
            let session = m.session(view_id).expect("session is registered");
            assert_resumes(session, "picked");
        });
    });
}

#[test]
fn agent_activity_after_escape_keeps_the_turn_open() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(view_id, claude_session_mid_turn(), ctx);
            m.observe_escape_write_with_window(view_id, TEST_WINDOW, ctx);
            // e.g. Escape closed an autocomplete menu while a tool kept running.
            m.update_from_event(view_id, &tool_complete("old"), ctx);
        });
        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;

        model.read(&app, |m, _| {
            let session = m.session(view_id).expect("session is registered");
            assert!(session.session_context.resume_turn_in_progress);
        });
    });
}

#[test]
fn escape_between_turns_arms_nothing() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(view_id, idle_claude_session(), ctx);
            m.observe_escape_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        model.read(&app, |m, _| {
            assert!(
                m.ctrl_c_cancel_state
                    .get(&view_id)
                    .is_none_or(|state| state.pending_turn_end.is_none())
            );
        });
    });
}
