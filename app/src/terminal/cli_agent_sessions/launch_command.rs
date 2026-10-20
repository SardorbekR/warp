//! Tells whether the command running in a pane could have started a CLI agent somewhere other
//! than the pane's own shell, i.e. whether typing the agent's resume command into a fresh local
//! shell resumes the same session.
//!
//! Anything that isn't a known remote or container wrapper counts as local, so agents launched
//! through functions, aliases, `npx`, `env X=1 …` or `cd x && …` stay resumable.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;
use warp_completer::parsers::simple::all_parsed_commands;
use warp_util::path::EscapeChar;

/// Commands that run what follows them on another machine, in a container or VM, or in a
/// terminal multiplexer or detached session whose processes don't belong to the pane.
const REMOTE_WRAPPERS: &[&str] = &[
    "abduco",
    "autossh",
    "byobu",
    "devcontainer",
    "distrobox",
    "docker",
    "dtach",
    "et",
    "incus",
    "kubectl",
    "lxc",
    "mosh",
    "multipass",
    "podman",
    "screen",
    "ssh",
    "sshpass",
    "toolbox",
    "tmux",
    "tsh",
    "vagrant",
    "zellij",
];

/// A command that runs the rest of its command line in the same shell environment.
struct TransparentPrefix {
    name: &'static str,
    /// Options followed by a separate value, e.g. `-u` in `sudo -u me …`.
    value_options: &'static [&'static str],
    /// Arguments between the options and the command, e.g. the duration of `timeout 10 …`.
    leading_args: usize,
}

const TRANSPARENT_PREFIXES: &[TransparentPrefix] = &[
    TransparentPrefix {
        name: "builtin",
        value_options: &[],
        leading_args: 0,
    },
    TransparentPrefix {
        name: "caffeinate",
        value_options: &["-t", "-w"],
        leading_args: 0,
    },
    TransparentPrefix {
        name: "command",
        value_options: &[],
        leading_args: 0,
    },
    TransparentPrefix {
        name: "env",
        value_options: &["-u", "-C", "-S", "--unset", "--chdir", "--split-string"],
        leading_args: 0,
    },
    TransparentPrefix {
        name: "exec",
        value_options: &["-a"],
        leading_args: 0,
    },
    TransparentPrefix {
        name: "nice",
        value_options: &["-n", "--adjustment"],
        leading_args: 0,
    },
    TransparentPrefix {
        name: "nohup",
        value_options: &[],
        leading_args: 0,
    },
    TransparentPrefix {
        name: "sudo",
        value_options: &[
            "-u",
            "-g",
            "-h",
            "-p",
            "-C",
            "-D",
            "-r",
            "-t",
            "-U",
            "--user",
            "--group",
            "--host",
            "--prompt",
            "--close-from",
            "--chdir",
            "--role",
            "--type",
            "--other-user",
        ],
        leading_args: 0,
    },
    TransparentPrefix {
        name: "time",
        value_options: &["-f", "-o", "--format", "--output"],
        leading_args: 0,
    },
    TransparentPrefix {
        name: "timeout",
        value_options: &["-s", "-k", "--signal", "--kill-after"],
        leading_args: 1,
    },
];

/// How many alias expansions one check may do in total. Aliases can expand into several
/// commands each, so without a cap a few aliases referring to each other would take exponential
/// time on the UI thread.
const MAX_ALIAS_EXPANSIONS: usize = 32;

/// Whether any command in `command` (e.g. either side of `&&`) runs through a remote or container
/// wrapper, after skipping env assignments and transparent prefixes and resolving aliases.
pub(crate) fn runs_through_remote_wrapper(
    command: &str,
    escape_char: EscapeChar,
    aliases: &HashMap<SmolStr, String>,
) -> bool {
    AliasResolver {
        escape_char,
        aliases,
        remaining_expansions: MAX_ALIAS_EXPANSIONS,
        expanded: HashSet::new(),
    }
    .runs_through_remote_wrapper(command)
}

enum AliasExpansion {
    NotAnAlias,
    /// Already checked, or past the expansion budget.
    Skipped,
    Expanded(String),
}

struct AliasResolver<'a> {
    escape_char: EscapeChar,
    aliases: &'a HashMap<SmolStr, String>,
    remaining_expansions: usize,
    /// Command lines already produced by expanding an alias, which need no second check.
    expanded: HashSet<String>,
}

impl AliasResolver<'_> {
    fn runs_through_remote_wrapper(&mut self, command: &str) -> bool {
        for parsed in all_parsed_commands(command, self.escape_char) {
            let words: Vec<String> = parsed
                .parts
                .iter()
                .map(|part| unquote(part.item.as_str()).to_owned())
                .collect();
            let words = without_transparent_prefixes(&words);
            let Some((first, rest)) = words.split_first() else {
                continue;
            };
            match self.expand(first, rest) {
                AliasExpansion::Expanded(resolved) => {
                    if self.runs_through_remote_wrapper(&resolved) {
                        return true;
                    }
                }
                AliasExpansion::Skipped => {}
                AliasExpansion::NotAnAlias => {
                    if is_remote_wrapper(first, rest) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// The command line that `first rest` stands for when `first` is an alias. An alias wrapping
    /// the command of the same name (`alias ssh='ssh -A'`) isn't expanded again.
    fn expand(&mut self, first: &str, rest: &[String]) -> AliasExpansion {
        let Some(value) = self
            .aliases
            .get(first)
            .filter(|value| value.split_whitespace().next() != Some(first))
        else {
            return AliasExpansion::NotAnAlias;
        };
        if self.remaining_expansions == 0 {
            return AliasExpansion::Skipped;
        }
        let resolved = format!("{} {}", value.trim(), rest.join(" "));
        if !self.expanded.insert(resolved.clone()) {
            return AliasExpansion::Skipped;
        }
        self.remaining_expansions = self.remaining_expansions.saturating_sub(1);
        AliasExpansion::Expanded(resolved)
    }
}

/// Skips leading `NAME=value` assignments and prefixes such as `env -i` or `sudo -u me`, along
/// with their options and leading arguments.
fn without_transparent_prefixes(mut words: &[String]) -> &[String] {
    loop {
        while let Some((first, rest)) = words.split_first()
            && is_assignment(first)
        {
            words = rest;
        }
        let Some((first, rest)) = words.split_first() else {
            return words;
        };
        let name = executable_name(first);
        let Some(prefix) = TRANSPARENT_PREFIXES
            .iter()
            .find(|prefix| prefix.name == name)
        else {
            return words;
        };
        words = without_options(rest, prefix.value_options);
        for _ in 0..prefix.leading_args {
            if let Some((_, rest)) = words.split_first() {
                words = rest;
            }
        }
    }
}

/// Skips the options at the start of `words`, and the values of those in `value_options`.
fn without_options<'a>(mut words: &'a [String], value_options: &[&str]) -> &'a [String] {
    while let Some((first, rest)) = words.split_first() {
        if first == "--" {
            return rest;
        }
        if !first.starts_with('-') {
            return words;
        }
        words = rest;
        if option_takes_next_word(first, value_options)
            && let Some((_, rest)) = words.split_first()
        {
            words = rest;
        }
    }
    words
}

/// Whether the option word `option` is followed by a separate value: `--user` but not
/// `--user=me`; `-u` and `-Eu` but not `-ume`.
fn option_takes_next_word(option: &str, value_options: &[&str]) -> bool {
    if option.starts_with("--") {
        return value_options.contains(&option);
    }
    let is_value_flag = |flag: char| {
        value_options.iter().any(|value_option| {
            let mut chars = value_option.chars();
            chars.next() == Some('-') && chars.next() == Some(flag) && chars.next().is_none()
        })
    };
    let mut flags = option.trim_start_matches('-').chars();
    while let Some(flag) = flags.next() {
        if is_value_flag(flag) {
            // The rest of the word, if any, is the value.
            return flags.next().is_none();
        }
    }
    false
}

fn is_remote_wrapper(command: &str, args: &[String]) -> bool {
    let has_arg = |names: &[&str]| args.iter().any(|arg| names.contains(&arg.as_str()));
    match executable_name(command) {
        // `gcloud compute ssh`, `gcloud cloud-shell ssh`
        "gcloud" => has_arg(&["ssh"]),
        // `aws ssm start-session`
        "aws" => has_arg(&["ssm"]),
        // `gh codespace ssh`
        "gh" => has_arg(&["codespace", "cs"]),
        // `kitten ssh` (kitty's SSH wrapper)
        "kitten" => has_arg(&["ssh"]),
        name => REMOTE_WRAPPERS.contains(&name),
    }
}

/// `NAME=value`, where `NAME` is a shell variable name.
fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The executable's name: without its directory, and without a leading `\` (which bypasses
/// aliases).
fn executable_name(command: &str) -> &str {
    let name = command.rsplit(['/', '\\']).next().unwrap_or(command);
    if name.is_empty() {
        command.trim_start_matches('\\')
    } else {
        name
    }
}

fn unquote(word: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = word
            .strip_prefix(quote)
            .and_then(|word| word.strip_suffix(quote))
        {
            return inner;
        }
    }
    word
}

#[cfg(test)]
#[path = "launch_command_tests.rs"]
mod tests;
