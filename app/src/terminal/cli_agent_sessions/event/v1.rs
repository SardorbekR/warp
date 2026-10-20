use warp_core::cli_agent_protocol::CLIAgentNotification;

use super::{CLIAgentEvent, CLIAgentEventPayload, CLIAgentEventSource, CLIAgentEventType};
use crate::terminal::CLIAgent;

/// Resolves a CLI agent from the `"agent"` string in a CLI agent event.
/// Returns `None` if the string doesn't match any known agent.
fn resolve_agent(agent: &str) -> Option<CLIAgent> {
    enum_iterator::all::<CLIAgent>().find(|candidate| candidate.command_prefixes().contains(&agent))
}

/// Plugins send `""` for fields they don't know, which must not overwrite known values.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

/// Session ids are compared to tell sessions apart, so surrounding whitespace is dropped too.
fn normalized_session_id(value: Option<String>) -> Option<String> {
    non_empty(value).map(|value| value.trim().to_owned())
}

pub(super) fn parse(body: &str) -> Option<CLIAgentEvent> {
    let raw: CLIAgentNotification = serde_json::from_str(body).ok()?;

    let event = match raw.event.as_str() {
        "session_start" | "task_started" => CLIAgentEventType::SessionStart,
        "prompt_submit" => CLIAgentEventType::PromptSubmit,
        "tool_complete" => CLIAgentEventType::ToolComplete,
        "stop" | "task_complete" => CLIAgentEventType::Stop,
        "stop_failure" => CLIAgentEventType::StopFailure,
        "permission_request" => CLIAgentEventType::PermissionRequest,
        "permission_replied" => CLIAgentEventType::PermissionReplied,
        "question_asked" => CLIAgentEventType::QuestionAsked,
        "agent_needs_input" | "prompt_required" => CLIAgentEventType::NeedsInput,
        "idle_prompt" => CLIAgentEventType::IdlePrompt,
        other => CLIAgentEventType::Unknown(other.to_string()),
    };

    let tool_input_preview = raw.tool_input.as_ref().and_then(|val| {
        val.get("command")
            .or_else(|| val.get("file_path"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    let agent = raw
        .agent
        .as_deref()
        .and_then(resolve_agent)
        .unwrap_or(CLIAgent::Unknown);

    let pending_background_work_count = match (raw.background_task_count, raw.session_cron_count) {
        (None, None) => None,
        (tasks, crons) => Some(tasks.unwrap_or(0).saturating_add(crons.unwrap_or(0))),
    };

    Some(CLIAgentEvent {
        v: raw.v.unwrap_or(1),
        agent,
        event,
        session_id: normalized_session_id(raw.session_id),
        cwd: non_empty(raw.cwd),
        project: non_empty(raw.project),
        payload: CLIAgentEventPayload {
            query: raw.query,
            response: raw.response,
            transcript_path: non_empty(raw.transcript_path),
            summary: raw.summary,
            tool_name: raw.tool_name,
            tool_input_preview,
            plugin_version: raw.plugin_version,
            error_type: raw.error_type,
            pending_background_work_count,
        },
        source: CLIAgentEventSource::RichPlugin,
    })
}
