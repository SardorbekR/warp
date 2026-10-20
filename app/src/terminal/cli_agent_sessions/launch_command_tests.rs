use super::*;

fn aliases(pairs: &[(&str, &str)]) -> HashMap<SmolStr, String> {
    pairs
        .iter()
        .map(|(alias, value)| (SmolStr::new(alias), (*value).to_owned()))
        .collect()
}

fn remote(command: &str) -> bool {
    runs_through_remote_wrapper(command, EscapeChar::Backslash, &HashMap::new())
}

fn remote_with(command: &str, alias_pairs: &[(&str, &str)]) -> bool {
    runs_through_remote_wrapper(command, EscapeChar::Backslash, &aliases(alias_pairs))
}

#[test]
fn agents_started_locally_are_not_wrapped() {
    for command in [
        "claude",
        "claude --model opus",
        "/opt/homebrew/bin/claude --resume abc",
        "codex",
        "npx @anthropic-ai/claude-code",
        "bunx @openai/codex",
        "command claude",
        "env X=1 claude",
        "env -i HOME=/tmp claude",
        "X=1 Y=2 claude",
        "caffeinate -i claude",
        "nohup claude",
        "cd x && claude",
        "cd 'my repo' && claude",
        "my_claude_function --fast",
        "claude -p \"do not ssh anywhere; just tmux the logs\"",
    ] {
        assert!(!remote(command), "{command}");
    }
}

#[test]
fn remote_and_container_wrappers_are_detected() {
    for command in [
        "ssh devbox",
        "ssh -t devbox claude",
        "/usr/bin/ssh devbox",
        "\\ssh devbox",
        "mosh devbox",
        "et devbox",
        "autossh -M 0 devbox",
        "tmux",
        "tmux new -s work claude",
        "screen -S work",
        "zellij attach",
        "docker exec -it app claude",
        "podman run -it img",
        "kubectl exec -it pod -- claude",
        "gcloud compute ssh vm",
        "gcloud cloud-shell ssh",
        "aws ssm start-session --target i-123",
        "vagrant ssh",
        "multipass shell vm",
        "lxc exec c -- bash",
        "incus shell c",
        "distrobox enter box",
        "toolbox enter",
        "devcontainer exec --workspace-folder . claude",
        "gh codespace ssh",
        "gh cs ssh",
        "env TERM=xterm ssh devbox",
        "AWS_PROFILE=dev aws ssm start-session --target i-1",
        "caffeinate -i ssh devbox",
        "sudo -E docker exec -it app claude",
        "cd infra && ssh bastion",
        "cat key | ssh devbox",
    ] {
        assert!(remote(command), "{command}");
    }
}

#[test]
fn other_subcommands_of_multi_purpose_clis_are_local() {
    for command in ["gcloud auth login", "aws s3 ls", "gh pr view 1"] {
        assert!(!remote(command), "{command}");
    }
}

#[test]
fn aliases_are_resolved() {
    assert!(!remote_with("cc", &[("cc", "claude --model opus")]));
    assert!(!remote_with(
        "cc",
        &[("cc", "npx @anthropic-ai/claude-code")]
    ));
    assert!(!remote_with("cc", &[("cc", "env X=1 command claude")]));
    assert!(remote_with("dev", &[("dev", "ssh -t devbox claude")]));
    assert!(remote_with("dev", &[("dev", "cd ~/infra && ssh bastion")]));
    // Alias chains.
    assert!(remote_with("a", &[("a", "b --flag"), ("b", "mosh devbox")]));
    assert!(!remote_with("a", &[("a", "b --flag"), ("b", "claude")]));
    // An alias wrapping the command of the same name isn't expanded again.
    assert!(remote_with("ssh devbox", &[("ssh", "ssh -A")]));
    assert!(!remote_with("claude", &[("claude", "claude --verbose")]));
}

#[test]
fn alias_cycles_terminate() {
    assert!(!remote_with("a", &[("a", "b"), ("b", "a")]));
}

#[test]
fn detached_sessions_and_other_remote_tools_are_detected() {
    for command in [
        "byobu",
        "byobu new-session claude",
        "dtach -A /tmp/work claude",
        "abduco -A work claude",
        "sshpass -p secret ssh devbox",
        "tsh ssh me@node",
        "kitten ssh devbox",
    ] {
        assert!(remote(command), "{command}");
    }
    assert!(!remote("kitten icat diagram.png"));
}

#[test]
fn options_that_take_a_value_are_skipped_with_it() {
    for command in [
        "sudo -u me ssh devbox",
        "sudo -Eu me ssh devbox",
        "sudo -ume ssh devbox",
        "sudo --user me ssh devbox",
        "sudo --user=me ssh devbox",
        "sudo -- ssh devbox",
        "nice -n 10 ssh devbox",
        "nice -10 ssh devbox",
        "caffeinate -t 3600 ssh devbox",
        "env -u TERM ssh devbox",
        "env -C /tmp ssh devbox",
        "env - ssh devbox",
        "timeout 10 ssh devbox",
        "timeout -s KILL 10 ssh devbox",
        "timeout --kill-after=5 10 ssh devbox",
        "sudo -u me nice -n 5 timeout 60 ssh devbox",
    ] {
        assert!(remote(command), "{command}");
    }
    for command in [
        "sudo -u me claude",
        "nice -n 10 claude",
        "timeout 3600 claude",
        "caffeinate -w 123 codex",
    ] {
        assert!(!remote(command), "{command}");
    }
}

#[test]
fn alias_fan_out_is_bounded() {
    // Each alias expands into several commands that refer back to the other.
    assert!(!remote_with(
        "a",
        &[("a", "b; b; b; b; b"), ("b", "a; a; a; a; a")]
    ));
    // Distinct aliases fanning out, several levels deep.
    let levels: Vec<(String, String)> = (0..8)
        .map(|level| {
            let next = format!("l{}", level + 1);
            (format!("l{level}"), [next.as_str(); 5].join("; "))
        })
        .collect();
    let levels: Vec<(&str, &str)> = levels
        .iter()
        .map(|(alias, value)| (alias.as_str(), value.as_str()))
        .collect();
    assert!(!remote_with("l0", &levels));
    // A wrapper within the budget is still found.
    assert!(remote_with(
        "a",
        &[("a", "b; c"), ("b", "c; echo"), ("c", "ssh devbox")]
    ));
}
