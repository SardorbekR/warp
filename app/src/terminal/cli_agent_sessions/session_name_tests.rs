use super::*;

#[test]
fn claude_titles_lose_their_status_glyph() {
    let name = |title| session_name_from_terminal_title(CLIAgent::Claude, title);
    assert_eq!(
        name("✳ Paywall free trial A/B test").as_deref(),
        Some("Paywall free trial A/B test")
    );
    assert_eq!(
        name("◐ Page mistaken for paywall").as_deref(),
        Some("Page mistaken for paywall")
    );
    assert_eq!(
        name("◑ Page mistaken for paywall").as_deref(),
        Some("Page mistaken for paywall")
    );
    // Older versions spin through braille frames.
    assert_eq!(
        name("⠂ dynamic-analytics-charts").as_deref(),
        Some("dynamic-analytics-charts")
    );
    // Before it has named the session (e.g. a resumed session, until Claude Code titles it
    // again), so the name saved for the session is kept.
    assert_eq!(name("✳ Claude Code"), None);
    assert_eq!(name("◐ Claude Code"), None);
    assert_eq!(
        name("✳ Claude Code tips").as_deref(),
        Some("Claude Code tips")
    );
}

#[test]
fn titles_without_a_status_glyph_are_not_names() {
    let name = |title| session_name_from_terminal_title(CLIAgent::Claude, title);
    // Set by the shell, e.g. when Claude Code's own titles are turned off.
    assert_eq!(name("claude"), None);
    assert_eq!(name("~/SideProjects/Shipzen"), None);
    assert_eq!(name("★Launch"), None);
    assert_eq!(name("✳Launch"), None);
    assert_eq!(name("A/B test ✳ copy"), None);
    assert_eq!(name("  ✳   spaced   out  ").as_deref(), Some("spaced out"));
}

#[test]
fn empty_titles_have_no_name() {
    let name = |title| session_name_from_terminal_title(CLIAgent::Claude, title);
    assert_eq!(name(""), None);
    assert_eq!(name("   "), None);
    assert_eq!(name("✳ "), None);
}

#[test]
fn other_agents_titles_are_not_names() {
    assert_eq!(
        session_name_from_terminal_title(CLIAgent::Codex, "Working | Shipzen"),
        None
    );
    assert_eq!(
        session_name_from_terminal_title(CLIAgent::Gemini, "✳ Something"),
        None
    );
}

#[test]
fn prompts_are_named_after_their_first_line() {
    assert_eq!(
        session_name_from_prompt("\n\n  fix the android build \nand run the tests").as_deref(),
        Some("fix the android build")
    );
    assert_eq!(session_name_from_prompt(" \n\t\n"), None);
}

#[test]
fn names_are_cleaned_and_bounded() {
    assert_eq!(
        clean_session_name("a\u{202E}b\u{0007}c\td").as_deref(),
        Some("ab c d")
    );
    let long = "x".repeat(500);
    assert_eq!(
        clean_session_name(&long).map(|name| name.chars().count()),
        Some(MAX_SESSION_NAME_CHARS)
    );
}
