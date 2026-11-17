//! Integration tests that a terminal's scroll position survives switching to another tab and back.
use std::time::Duration;

use settings::Setting as _;
use warp::integration_testing::terminal::util::ExpectedExitStatus;
use warp::integration_testing::terminal::{
    assert_single_terminal_in_tab_bootstrapped, execute_command_for_single_terminal_in_tab,
    execute_long_running_command, wait_until_bootstrapped_single_pane_for_tab,
};
use warp::integration_testing::view_getters::{single_terminal_view_for_tab, workspace_view};
use warp::settings::InputModeSettings;
use warp::terminal::CLIAgent;
use warp::terminal::block_list_viewport::ScrollPosition;
use warp::terminal::view::{TerminalAction, TerminalView};
use warp::workspace::WorkspaceAction;
use warpui_core::integration::{AssertionOutcome, StepDataMap, TestStep};
use warpui_core::units::{IntoLines, Lines};
use warpui_core::{App, EntityId, SingletonEntity, TypedActionView, ViewHandle, WindowId};

use super::new_builder;
use crate::Builder;

const SCROLLED_TERMINAL_KEY: &str = "scrolled terminal";
const SAVED_SCROLL_KEY: &str = "scroll before switching tabs";
const SCROLL_UP_LINES: f64 = 40.;
const STEP_TIMEOUT: Duration = Duration::from_secs(15);

/// Prints many lines, then behaves like a CLI agent that redraws its prompt area whenever the
/// terminal reports focus changes (as Claude Code's classic renderer does). Every byte it reads is
/// appended to `$SCROLL_TEST_INPUT_LOG` so a failure shows what the terminal sent.
const FOCUS_REDRAWING_APP: &str = r#"
import os, sys, termios, tty
fd = sys.stdin.fileno()
tty.setraw(fd)
out = sys.stdout.buffer
log = open(os.environ["SCROLL_TEST_INPUT_LOG"], "ab", buffering=0)
out.write(b"".join(b"agent output line %03d\r\n" % i for i in range(1, 301)))
out.write(b"\x1b[?2004h\x1b[?1004h> prompt\r\n  status line\r\n\x1b[2A\r")
out.flush()
while True:
    data = os.read(fd, 1024)
    if not data or b"\x03" in data:
        break
    log.write(data)
    if b"\x1b[I" in data or b"\x1b[O" in data:
        out.write(b"\x1b[?2026h\x1b[K> prompt\r\n\x1b[K  status line\r\n\x1b[2A\r\x1b[?2026l")
        out.flush()
"#;

fn scroll_state(
    app: &mut App,
    terminal_view: &ViewHandle<TerminalView>,
) -> (ScrollPosition, Lines) {
    terminal_view.read(app, |view, ctx| {
        let input_mode = *InputModeSettings::as_ref(ctx).input_mode.value();
        let model = view.model.lock();
        let scroll_top = view
            .viewport_state(model.block_list(), input_mode, ctx)
            .scroll_top_in_lines();
        (view.scroll_position(), scroll_top)
    })
}

fn saved_terminal(data: &StepDataMap) -> ViewHandle<TerminalView> {
    data.get::<_, ViewHandle<TerminalView>>(SCROLLED_TERMINAL_KEY)
        .expect("the scrolled terminal is saved by the scroll step")
        .clone()
}

/// Index of the tab whose pane group holds `terminal_view_id`.
fn tab_index_of(app: &mut App, window_id: WindowId, terminal_view_id: EntityId) -> Option<usize> {
    let workspace = workspace_view(app, window_id);
    workspace.read(app, |workspace, ctx| {
        (0..workspace.tab_count()).find(|&index| {
            workspace
                .get_pane_group_view(index)
                .is_some_and(|pane_group| {
                    pane_group
                        .as_ref(ctx)
                        .find_pane_id_for_terminal_view(terminal_view_id, ctx)
                        .is_some()
                })
        })
    })
}

fn scroll_up_in_first_tab() -> TestStep {
    TestStep::new("Scroll up in the first tab")
        .set_timeout(STEP_TIMEOUT)
        .with_action(|app, window_id, data| {
            let terminal_view = single_terminal_view_for_tab(app, window_id, 0);
            terminal_view.update(app, |view, ctx| {
                view.handle_action(
                    &TerminalAction::Scroll {
                        delta: SCROLL_UP_LINES.into_lines(),
                    },
                    ctx,
                );
            });
            data.insert(SCROLLED_TERMINAL_KEY, terminal_view);
        })
        .add_named_assertion_with_data_from_prior_step(
            "terminal is scrolled away from the bottom",
            |app, _, data| {
                let terminal_view = saved_terminal(data);
                let (position, scroll_top) = scroll_state(app, &terminal_view);
                if matches!(position, ScrollPosition::FollowsBottomOfMostRecentBlock) {
                    return AssertionOutcome::failure(format!(
                        "expected a fixed scroll position after scrolling up, got {position:?}"
                    ));
                }
                data.insert(SAVED_SCROLL_KEY, (position, scroll_top));
                AssertionOutcome::Success
            },
        )
}

/// Index of a tab other than the one holding `terminal_view_id`.
fn other_tab_index(
    app: &mut App,
    window_id: WindowId,
    terminal_view_id: EntityId,
) -> Option<usize> {
    let scrolled_tab = tab_index_of(app, window_id, terminal_view_id);
    let tab_count = workspace_view(app, window_id).read(app, |workspace, _| workspace.tab_count());
    (0..tab_count).find(|&index| Some(index) != scrolled_tab)
}

fn open_another_tab() -> TestStep {
    TestStep::new("Open another tab")
        .set_timeout(STEP_TIMEOUT)
        .with_action(|app, window_id, _| {
            workspace_view(app, window_id).update(app, |workspace, ctx| {
                workspace.handle_action(
                    &WorkspaceAction::AddTerminalTab {
                        hide_homepage: true,
                    },
                    ctx,
                );
            });
        })
        .add_named_assertion_with_data_from_prior_step(
            "a new tab is active and bootstrapped",
            |app, window_id, data| {
                let terminal_view_id = saved_terminal(data).id();
                let (active, count) = workspace_view(app, window_id).read(app, |workspace, _| {
                    (workspace.active_tab_index(), workspace.tab_count())
                });
                if count != 2 || tab_index_of(app, window_id, terminal_view_id) == Some(active) {
                    return AssertionOutcome::failure(format!(
                        "expected the new tab to be active: active {active} of {count}"
                    ));
                }
                assert_single_terminal_in_tab_bootstrapped(app, window_id, active)
            },
        )
}

/// Activates the scrolled tab (`to_scrolled_tab`) or the other tab, as clicking it in the tab bar
/// does.
fn switch_tab(to_scrolled_tab: bool) -> TestStep {
    let name = if to_scrolled_tab {
        "Switch back to the scrolled tab"
    } else {
        "Switch to the other tab"
    };
    let target = move |app: &mut App, window_id: WindowId, terminal_view_id: EntityId| {
        if to_scrolled_tab {
            tab_index_of(app, window_id, terminal_view_id)
        } else {
            other_tab_index(app, window_id, terminal_view_id)
        }
    };
    TestStep::new(name)
        .set_timeout(STEP_TIMEOUT)
        .with_action(move |app, window_id, data| {
            let terminal_view_id = saved_terminal(data).id();
            let index = target(app, window_id, terminal_view_id).expect("the target tab exists");
            workspace_view(app, window_id).update(app, |workspace, ctx| {
                workspace.handle_action(&WorkspaceAction::ActivateTab(index), ctx);
            });
        })
        .add_named_assertion_with_data_from_prior_step(
            "the target tab is active",
            move |app, window_id, data| {
                let terminal_view_id = saved_terminal(data).id();
                let active = workspace_view(app, window_id)
                    .read(app, |workspace, _| workspace.active_tab_index());
                if target(app, window_id, terminal_view_id) == Some(active) {
                    AssertionOutcome::Success
                } else {
                    AssertionOutcome::failure(format!("tab {active} is active"))
                }
            },
        )
}

fn assert_scroll_unchanged() -> TestStep {
    TestStep::new("Scroll position is unchanged")
        .set_timeout(STEP_TIMEOUT)
        .set_post_step_pause(Duration::from_millis(500))
        .add_named_assertion_with_data_from_prior_step(
            "scroll position matches the one before switching tabs",
            |app, _, data| {
                let Some((saved_position, saved_top)) = data
                    .get::<_, (ScrollPosition, Lines)>(SAVED_SCROLL_KEY)
                    .copied()
                else {
                    return AssertionOutcome::immediate_failure(
                        "no saved scroll position".to_owned(),
                    );
                };
                let (position, scroll_top) = scroll_state(app, &saved_terminal(data));
                if position == saved_position && scroll_top == saved_top {
                    AssertionOutcome::Success
                } else {
                    AssertionOutcome::failure(format!(
                        "before: {saved_position:?} top {saved_top:?}; \
                         after: {position:?} top {scroll_top:?}"
                    ))
                }
            },
        )
}

/// Scrolls up in the first tab, opens a second tab, comes back, then switches between the two
/// existing tabs once more, checking the scroll position after each return.
fn with_tab_round_trips(builder: Builder) -> Builder {
    builder
        .with_step(scroll_up_in_first_tab())
        .with_step(open_another_tab())
        .with_step(switch_tab(true))
        .with_step(assert_scroll_unchanged())
        .with_step(switch_tab(false))
        .with_step(switch_tab(true))
        .with_step(assert_scroll_unchanged())
}

/// A finished command's output stays where the user scrolled it after a round trip to another tab.
pub fn test_scroll_position_survives_tab_switch_finished_block() -> Builder {
    with_tab_round_trips(
        new_builder()
            .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
            .with_step(execute_command_for_single_terminal_in_tab(
                0,
                "seq 1 300".to_owned(),
                ExpectedExitStatus::Success,
                (),
            )),
    )
}

/// Starts the agent stand-in in the first tab with `command` and waits until it has printed
/// enough output to scroll. `setup` also writes a `claude` wrapper that runs the stand-in, so a
/// command can make Warp treat it as Claude Code.
fn with_running_agent_stand_in(command: &str, extra_steps: Vec<TestStep>) -> Builder {
    let builder = new_builder()
        .with_setup(|utils| {
            let dir = utils.test_dir();
            let script = dir.join("focus_redrawing_app.py");
            std::fs::write(&script, FOCUS_REDRAWING_APP).expect("write the agent stand-in");
            let bin_dir = dir.join("agent-bin");
            std::fs::create_dir_all(&bin_dir).expect("create the agent bin dir");
            let claude = bin_dir.join("claude");
            std::fs::write(&claude, "#!/bin/sh\nexec python3 \"$SCROLL_TEST_APP\"\n")
                .expect("write the claude wrapper");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))
                    .expect("make the claude wrapper executable");
            }
            utils.set_env("SCROLL_TEST_APP", Some(&script));
            utils.set_env("SCROLL_TEST_BIN", Some(&bin_dir));
            utils.set_env("SCROLL_TEST_INPUT_LOG", Some(dir.join("agent_input.log")));
        })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(execute_long_running_command(0, command.to_owned()));
    extra_steps
        .into_iter()
        .fold(builder, Builder::with_step)
        .with_step(
            TestStep::new("Agent stand-in printed its output")
                .set_timeout(STEP_TIMEOUT)
                // Leave time for the agent toolbar to appear before the test scrolls up.
                .set_post_step_pause(Duration::from_millis(500))
                .add_named_assertion("output is long enough to scroll", |app, window_id| {
                    let terminal_view = single_terminal_view_for_tab(app, window_id, 0);
                    let max = terminal_view.read(app, |view, ctx| {
                        let input_mode = *InputModeSettings::as_ref(ctx).input_mode.value();
                        let model = view.model.lock();
                        view.viewport_state(model.block_list(), input_mode, ctx)
                            .max_scroll_top_in_lines()
                    });
                    if max > (SCROLL_UP_LINES * 2.).into_lines() {
                        AssertionOutcome::Success
                    } else {
                        AssertionOutcome::failure(format!("max scroll top is only {max:?}"))
                    }
                }),
        )
}

/// A running command that redraws on focus reports keeps its scroll position across round trips
/// to another tab.
pub fn test_scroll_position_survives_tab_switch_running_agent() -> Builder {
    with_tab_round_trips(with_running_agent_stand_in(
        "python3 \"$SCROLL_TEST_APP\"",
        vec![],
    ))
}

/// The same, for a command Warp detects as Claude Code, which shows the CLI agent toolbar.
pub fn test_scroll_position_survives_tab_switch_claude_session() -> Builder {
    with_tab_round_trips(with_running_agent_stand_in(
        "PATH=\"$SCROLL_TEST_BIN:$PATH\" claude",
        vec![
            TestStep::new("Warp detects Claude Code")
                .set_timeout(STEP_TIMEOUT)
                .add_named_assertion("the terminal runs Claude", |app, window_id| {
                    let terminal_view = single_terminal_view_for_tab(app, window_id, 0);
                    let agent = terminal_view.read(app, |view, ctx| view.active_cli_agent(ctx));
                    if agent == Some(CLIAgent::Claude) {
                        AssertionOutcome::Success
                    } else {
                        AssertionOutcome::failure(format!("detected agent: {agent:?}"))
                    }
                }),
        ],
    ))
}
