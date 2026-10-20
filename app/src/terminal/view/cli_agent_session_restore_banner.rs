//! Inline banner shown in a restored terminal pane that was running a CLI agent session (Claude
//! Code, Codex) when Warp last saved its state, e.g. right before a crash. It offers to resume the
//! session, copy the resume command, or dismiss the offer.

use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    ConstrainedBox, Container, CrossAxisAlignment, Flex, Hoverable, MainAxisAlignment,
    MainAxisSize, MouseStateHandle, ParentElement, Shrinkable, Text,
};
use warpui::platform::Cursor;
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::UiComponent as _;
use warpui::{AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::Appearance;
use crate::terminal::cli_agent_sessions::restore::PersistedCLIAgentSession;
use crate::ui_components::icons::Icon;

/// Longest title (in characters) shown before it's elided.
const MAX_DISPLAYED_TITLE_CHARS: usize = 80;

#[derive(Clone, Debug)]
pub enum CLIAgentSessionRestoreBannerAction {
    Resume,
    CopyCommand,
    Dismiss,
}

#[derive(Clone, Debug)]
pub enum CLIAgentSessionRestoreBannerEvent {
    Resume,
    CopyCommand,
    Dismiss,
}

/// How the last attempt to resume the session from this pane ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviousResumeAttempt {
    #[default]
    NotAttempted,
    /// The resume commands failed or never ran.
    Failed,
    /// The agent exited normally without reporting the session, so it's unknown whether the
    /// session was resumed.
    Unconfirmed,
}

pub struct CLIAgentSessionRestoreBanner {
    record: PersistedCLIAgentSession,
    transcript_missing: bool,
    launch_directory_missing: bool,
    previous_resume: PreviousResumeAttempt,
    resume_mouse_state: MouseStateHandle,
    copy_mouse_state: MouseStateHandle,
    dismiss_mouse_state: MouseStateHandle,
    close_mouse_state: MouseStateHandle,
}

impl CLIAgentSessionRestoreBanner {
    pub fn new(
        record: PersistedCLIAgentSession,
        previous_resume: PreviousResumeAttempt,
        launch_directory_missing: bool,
    ) -> Self {
        Self {
            record,
            transcript_missing: false,
            launch_directory_missing,
            previous_resume,
            resume_mouse_state: MouseStateHandle::default(),
            copy_mouse_state: MouseStateHandle::default(),
            dismiss_mouse_state: MouseStateHandle::default(),
            close_mouse_state: MouseStateHandle::default(),
        }
    }

    pub fn set_transcript_missing(
        &mut self,
        transcript_missing: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.transcript_missing != transcript_missing {
            self.transcript_missing = transcript_missing;
            ctx.notify();
        }
    }

    /// Marks the session's launch directory as missing on this machine, which means it ran
    /// elsewhere, so it can only be copied.
    pub fn set_launch_directory_missing(&mut self, missing: bool, ctx: &mut ViewContext<Self>) {
        if self.launch_directory_missing != missing {
            self.launch_directory_missing = missing;
            ctx.notify();
        }
    }

    fn can_resume_here(&self) -> bool {
        self.record.can_resume_here() && !self.launch_directory_missing
    }

    fn title_text(&self) -> String {
        let agent_name = self.record.agent.display_name();
        match self.record.display_title() {
            Some(title) => {
                let mut displayed: String = title.chars().take(MAX_DISPLAYED_TITLE_CHARS).collect();
                if title.chars().count() > MAX_DISPLAYED_TITLE_CHARS {
                    displayed.push('…');
                }
                format!("{agent_name} session “{displayed}” was running here")
            }
            None => format!("{agent_name} session was running here"),
        }
    }

    fn details_text(&self) -> Option<String> {
        let mut details = Vec::new();
        if let Some(launch_cwd) = self.record.launch_cwd.as_deref() {
            details.push(launch_cwd.to_owned());
        }
        if let Some(remote_host) = self.record.remote_host.as_deref() {
            details.push(format!(
                "Ran on {remote_host}; copy the command to resume it there"
            ));
        } else if !self.record.can_resume_here() {
            details.push(
                "Didn't start from this pane's shell (e.g. over SSH or in tmux); copy the command \
                 to resume it where it ran"
                    .to_owned(),
            );
        } else if self.launch_directory_missing {
            details.push(
                "That directory doesn't exist here; copy the command to resume it where it ran"
                    .to_owned(),
            );
        }
        if self.transcript_missing {
            details.push("Transcript not found; resuming may fail".to_owned());
        }
        match self.previous_resume {
            PreviousResumeAttempt::NotAttempted => {}
            PreviousResumeAttempt::Failed => {
                details.push("The last resume attempt failed".to_owned());
            }
            PreviousResumeAttempt::Unconfirmed => {
                details.push("Warp couldn't confirm that the last resume worked".to_owned());
            }
        }
        (!details.is_empty()).then(|| details.join(" · "))
    }
}

impl Entity for CLIAgentSessionRestoreBanner {
    type Event = CLIAgentSessionRestoreBannerEvent;
}

impl View for CLIAgentSessionRestoreBanner {
    fn ui_name() -> &'static str {
        "CLIAgentSessionRestoreBanner"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let fg_color = theme.foreground().into_solid();
        let muted_color = internal_colors::neutral_5(theme);
        let font_size = appearance.monospace_font_size();
        let small_font_size = font_size - 2.;

        let icon_color = self.record.agent.brand_color().unwrap_or(fg_color);
        let icon = self.record.agent.icon().unwrap_or(Icon::Terminal);
        let icon = Container::new(
            ConstrainedBox::new(icon.to_warpui_icon(icon_color.into()).finish())
                .with_width(16.)
                .with_height(16.)
                .finish(),
        )
        .with_margin_right(8.)
        .finish();

        let title = Text::new(self.title_text(), appearance.ui_font_family(), font_size)
            .soft_wrap(true)
            .with_color(fg_color)
            .finish();

        let close_icon_color = muted_color;
        let close = Hoverable::new(self.close_mouse_state.clone(), move |_| {
            ConstrainedBox::new(Icon::X.to_warpui_icon(close_icon_color.into()).finish())
                .with_width(16.)
                .with_height(16.)
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(CLIAgentSessionRestoreBannerAction::Dismiss);
        })
        .finish();

        let header_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Shrinkable::new(
                    1.,
                    Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(icon)
                        .with_child(Shrinkable::new(1., title).finish())
                        .finish(),
                )
                .finish(),
            )
            .with_child(Container::new(close).with_uniform_padding(4.).finish())
            .finish();

        let mut content = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header_row);

        if let Some(details) = self.details_text() {
            let details = Text::new(details, appearance.ui_font_family(), small_font_size)
                .soft_wrap(true)
                .with_color(muted_color)
                .finish();
            content.add_child(
                Container::new(details)
                    .with_margin_top(2.)
                    .with_margin_left(24.)
                    .finish(),
            );
        }

        let button = |variant: ButtonVariant,
                      mouse_state: &MouseStateHandle,
                      label: &str,
                      action: CLIAgentSessionRestoreBannerAction| {
            Container::new(
                appearance
                    .ui_builder()
                    .button(variant, mouse_state.clone())
                    .with_text_label(label.to_owned())
                    .build()
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(action.clone());
                    })
                    .finish(),
            )
            .with_margin_right(8.)
            .finish()
        };
        let mut buttons = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
        if self.can_resume_here() {
            buttons.add_child(button(
                ButtonVariant::Accent,
                &self.resume_mouse_state,
                "Resume",
                CLIAgentSessionRestoreBannerAction::Resume,
            ));
        }
        buttons.add_child(button(
            ButtonVariant::Secondary,
            &self.copy_mouse_state,
            "Copy command",
            CLIAgentSessionRestoreBannerAction::CopyCommand,
        ));
        buttons.add_child(button(
            ButtonVariant::Text,
            &self.dismiss_mouse_state,
            "Dismiss",
            CLIAgentSessionRestoreBannerAction::Dismiss,
        ));
        content.add_child(
            Container::new(buttons.finish())
                .with_margin_top(8.)
                .with_margin_left(24.)
                .finish(),
        );

        Container::new(content.finish())
            .with_background(internal_colors::fg_overlay_1(theme))
            .with_uniform_padding(12.)
            .finish()
    }
}

impl TypedActionView for CLIAgentSessionRestoreBanner {
    type Action = CLIAgentSessionRestoreBannerAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            CLIAgentSessionRestoreBannerAction::Resume => {
                ctx.emit(CLIAgentSessionRestoreBannerEvent::Resume);
            }
            CLIAgentSessionRestoreBannerAction::CopyCommand => {
                ctx.emit(CLIAgentSessionRestoreBannerEvent::CopyCommand);
            }
            CLIAgentSessionRestoreBannerAction::Dismiss => {
                ctx.emit(CLIAgentSessionRestoreBannerEvent::Dismiss);
            }
        }
    }
}
