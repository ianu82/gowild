use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Tabs},
    Frame,
};

use super::widgets::{
    action_button_row_rects, action_button_width, centered_popup_rect, modal_stack_areas,
    panel_contrast_fg, render_action_button, render_modal_choice_list, render_panel_shell,
    ActionButtonSpec,
};
use crate::{
    app::{
        state::{
            CustomGatewayFormMode, GatewayCredentialStatus, GatewayDetailField, GatewayFormField,
            GatewayNoticeKind, GatewaySettingsView, GuidedSetupStep, Palette,
        },
        AppState,
    },
    config::{StatusIndicatorStyle, ToastDelivery},
    gateway::{AuthenticationMode, ConnectionStatus, CredentialRemoval, Gateway, GatewayProtocol},
};

pub(crate) const SETTINGS_POPUP_WIDTH: u16 = 76;
pub(crate) const SETTINGS_POPUP_BASE_HEIGHT: u16 = 22;

pub(crate) fn settings_popup_height(app: &AppState) -> u16 {
    if app.settings.section != crate::app::state::SettingsSection::Integrations {
        return SETTINGS_POPUP_BASE_HEIGHT;
    }
    let list_rows = app.integration_recommendations.len().max(1) as u16;
    let footer_rows = integrations_footer_height(app, SETTINGS_POPUP_WIDTH - 2);
    // borders 2 + header 3 + stack gaps 2 + modal footer 2
    // + section title 1 + description 2 + spacers 2
    (14 + list_rows + footer_rows).max(SETTINGS_POPUP_BASE_HEIGHT)
}

pub(crate) fn settings_is_compact(area: Rect) -> bool {
    area.width < 80 || area.height < 22
}

pub(crate) fn settings_popup_rect(area: Rect, app: &AppState) -> Option<Rect> {
    if settings_is_compact(area) {
        return (area.width >= 4 && area.height >= 4).then_some(area);
    }
    if area.width >= 140 && area.height >= 35 {
        return centered_popup_rect(
            area,
            (area.width * 3 / 4).clamp(104, 156),
            (area.height * 3 / 4).clamp(settings_popup_height(app), 48),
        );
    }
    centered_popup_rect(area, SETTINGS_POPUP_WIDTH, settings_popup_height(app))
}

pub(super) fn render_settings_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::SettingsSection;

    let p = &app.palette;
    let Some(popup) = settings_popup_rect(area, app) else {
        return;
    };

    super::dim_background(frame, area);

    let Some(inner) = render_panel_shell(frame, popup, p.accent, p.panel_bg) else {
        return;
    };
    if inner.height < 4 || inner.width < 10 {
        return;
    }

    let stack = modal_stack_areas(inner, 3, 2, 0, 1);
    let header_rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas::<3>(stack.header);

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            if app.settings.guided_setup {
                " GoWild setup"
            } else {
                " settings"
            },
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )])),
        header_rows[0],
    );

    let tab_labels = SettingsSection::ALL.iter().map(|section| {
        if app.settings_section_has_badge(*section) {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw(section.tab_label(inner.width)),
            ])
        } else {
            Line::from(section.tab_label(inner.width))
        }
    });
    let tabs = Tabs::new(tab_labels)
        .select(
            SettingsSection::ALL
                .iter()
                .position(|section| *section == app.settings.section)
                .unwrap_or(0),
        )
        .style(Style::default().fg(p.overlay1))
        .highlight_style(
            Style::default()
                .fg(panel_contrast_fg(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD),
        )
        .divider(" ")
        .padding(" ", " ");
    if app.settings.guided_setup {
        render_guided_setup_progress(app, frame, header_rows[1]);
    } else if settings_is_compact(area) {
        let index = SettingsSection::ALL
            .iter()
            .position(|section| *section == app.settings.section)
            .unwrap_or_default();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ‹ ", Style::default().fg(p.accent)),
                Span::styled(
                    format!("{:^14}", app.settings.section.label()),
                    Style::default().fg(p.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" › ", Style::default().fg(p.accent)),
                Span::styled(
                    format!(" section {} of {}", index + 1, SettingsSection::ALL.len()),
                    Style::default().fg(p.overlay1),
                ),
            ])),
            header_rows[1],
        );
    } else {
        frame.render_widget(tabs, header_rows[1]);
    }

    let sep = "─".repeat(inner.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep, Style::default().fg(p.surface0))),
        header_rows[2],
    );

    let content_area = stack.content;

    if app.settings.guided_setup {
        render_guided_setup(app, frame, content_area);
    } else {
        match app.settings.section {
            SettingsSection::Gateways => {
                render_settings_gateways(app, frame, content_area);
            }
            SettingsSection::Theme => {
                render_settings_theme(app, frame, content_area);
            }
            SettingsSection::Indicators => {
                render_modal_choice_list(
                    frame,
                    content_area,
                    "agent status indicators",
                    "choose color dots or distinct symbols for each state",
                    &[
                        ("color dots  ● ● ● ○ ·", StatusIndicatorStyle::Dots),
                        ("distinct symbols  × ◐ ✓ ○ ·", StatusIndicatorStyle::Symbols),
                    ],
                    app.status_indicators,
                    app.settings.list.selected,
                    p,
                    1,
                );
            }
            SettingsSection::Sound => {
                render_settings_toggle(
                    frame,
                    content_area,
                    p,
                    "sound alerts",
                    "play sounds when agents change state in background",
                    app.sound_enabled(),
                    app.settings.list.selected,
                );
            }
            SettingsSection::Toast => {
                render_modal_choice_list(
                    frame,
                    content_area,
                    "notification popups",
                    "choose where background popup notifications should appear",
                    &[
                        ("off", ToastDelivery::Off),
                        ("inside gowild", ToastDelivery::GoWild),
                        ("via terminal", ToastDelivery::Terminal),
                        ("via system", ToastDelivery::System),
                    ],
                    app.toast_delivery(),
                    app.settings.list.selected,
                    p,
                    2,
                );
            }
            SettingsSection::PaneLabels => {
                render_settings_toggle(
                    frame,
                    content_area,
                    p,
                    "agent border labels",
                    "show detected agent names in split pane borders",
                    app.agent_border_labels_enabled(),
                    app.settings.list.selected,
                );
            }
            SettingsSection::Integrations => {
                render_settings_integrations(app, frame, content_area);
            }
        }
    }

    if let Some(footer_area) = stack.footer {
        let footer_rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
            .areas::<2>(footer_area);
        let primary_label = settings_primary_button_label(app);
        let primary_hint = settings_primary_button_hint(app);
        let close_label = settings_close_button_label(app);
        let show_primary = settings_show_primary_action(app);
        let (apply_rect, close_rect) = settings_button_rects(inner, app, show_primary);
        if let Some(apply_rect) = apply_rect {
            let destructive = app.settings.section == SettingsSection::Gateways
                && app.settings.gateways.view == GatewaySettingsView::DeleteConfirm;
            render_action_button(
                frame,
                apply_rect,
                Some(primary_hint),
                primary_label,
                Style::default()
                    .fg(panel_contrast_fg(p))
                    .bg(if destructive { p.red } else { p.accent })
                    .add_modifier(Modifier::BOLD),
            );
        }
        render_action_button(
            frame,
            close_rect,
            Some("esc"),
            close_label,
            Style::default()
                .fg(p.text)
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD),
        );

        let hint = if app.settings.guided_setup && app.settings.gateways.editing_credential {
            " input hidden locally  ^u clear  esc cancel"
        } else if app.settings.guided_setup {
            " a custom gateway  q skip setup  esc finish later"
        } else if app.settings.section == SettingsSection::Gateways {
            match app.settings.gateways.view {
                GatewaySettingsView::List => {
                    " ↑↓ select  ↵ configure  t test  space default  tab section"
                }
                GatewaySettingsView::Detail if app.settings.gateways.editing_credential => {
                    " input hidden locally  ^u clear  esc cancel"
                }
                GatewaySettingsView::Detail => {
                    let custom = app
                        .settings
                        .gateways
                        .detail_gateway_id
                        .as_ref()
                        .and_then(|id| app.gateway_catalog.gateways.get(id))
                        .is_some_and(|gateway| gateway.preset.is_none());
                    let credential =
                        app.settings.gateways.detail_field == GatewayDetailField::Credential;
                    if custom && credential {
                        " ↑↓ field  ↵ edit key  t test  e edit  d duplicate  x delete"
                    } else if custom {
                        " ↑↓ field  ↵ choose model  t test  e edit  d duplicate  x delete"
                    } else if credential {
                        " ↑↓ field  ↵ edit key  t test  d duplicate  space default"
                    } else {
                        " ↑↓ field  ↵ choose model  t test  d duplicate  space default"
                    }
                }
                GatewaySettingsView::Form => {
                    " ↑↓ field  type/paste  ^u clear  ←→ auth  ↵ save  esc cancel"
                }
                GatewaySettingsView::DeleteConfirm => {
                    " ←→ choose key handling  ↵ delete gateway  esc cancel"
                }
            }
        } else {
            " ↑↓ select  tab section"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(p.overlay1))),
            footer_rows[0],
        );
    }
}

pub(crate) fn settings_primary_button_label(app: &AppState) -> &'static str {
    if app.settings.guided_setup {
        if app.settings.gateways.editing_credential {
            return "store";
        }
        return match app.guided_setup_step() {
            GuidedSetupStep::CliCheck => "check again",
            GuidedSetupStep::ConnectMindshub => "connect MindsHub",
            GuidedSetupStep::VerifyMindshub => "test",
            GuidedSetupStep::ChooseCodexModel => "choose Codex model",
            GuidedSetupStep::ChooseClaudeModel => "choose Claude model",
            GuidedSetupStep::Launch
                if app.guided_cli_available(crate::api::schema::IntegrationTarget::Codex) =>
            {
                "launch Codex"
            }
            GuidedSetupStep::Launch => "launch Claude",
        };
    }
    match app.settings.section {
        crate::app::state::SettingsSection::Gateways
            if app.settings.gateways.editing_credential =>
        {
            "store"
        }
        crate::app::state::SettingsSection::Gateways
            if app.settings.gateways.view == GatewaySettingsView::Form =>
        {
            match app
                .settings
                .gateways
                .gateway_form
                .as_ref()
                .map(|form| form.mode)
            {
                Some(CustomGatewayFormMode::Edit) => "save",
                Some(CustomGatewayFormMode::Duplicate) => "duplicate",
                _ => "add",
            }
        }
        crate::app::state::SettingsSection::Gateways
            if app.settings.gateways.view == GatewaySettingsView::DeleteConfirm =>
        {
            "delete gateway"
        }
        crate::app::state::SettingsSection::Gateways
            if app.settings.gateways.view == GatewaySettingsView::List =>
        {
            "configure"
        }
        crate::app::state::SettingsSection::Gateways
            if app.settings.gateways.view == GatewaySettingsView::Detail
                && app.settings.gateways.detail_field == GatewayDetailField::Credential
                && app
                    .settings
                    .gateways
                    .detail_gateway_id
                    .as_ref()
                    .and_then(|id| app.gateway_catalog.gateways.get(id))
                    .is_some_and(|gateway| gateway.auth.mode != AuthenticationMode::None) =>
        {
            "edit API key"
        }
        crate::app::state::SettingsSection::Gateways => "test",
        crate::app::state::SettingsSection::Integrations => "install",
        _ => "apply",
    }
}

pub(crate) fn settings_primary_button_hint(app: &AppState) -> &'static str {
    if app.settings.guided_setup {
        if app.settings.gateways.editing_credential {
            return "↵";
        }
        return match app.guided_setup_step() {
            GuidedSetupStep::CliCheck => "r",
            GuidedSetupStep::ConnectMindshub => "↵",
            GuidedSetupStep::VerifyMindshub => "t",
            GuidedSetupStep::ChooseCodexModel | GuidedSetupStep::ChooseClaudeModel => "↵",
            GuidedSetupStep::Launch
                if app.guided_cli_available(crate::api::schema::IntegrationTarget::Codex) =>
            {
                "c"
            }
            GuidedSetupStep::Launch => "l",
        };
    }
    if app.settings.section == crate::app::state::SettingsSection::Gateways
        && app.settings.gateways.view == GatewaySettingsView::Detail
        && !app.settings.gateways.editing_credential
        && (app.settings.gateways.detail_field != GatewayDetailField::Credential
            || app
                .settings
                .gateways
                .detail_gateway_id
                .as_ref()
                .and_then(|id| app.gateway_catalog.gateways.get(id))
                .is_none_or(|gateway| gateway.auth.mode == AuthenticationMode::None))
    {
        "t"
    } else {
        "↵"
    }
}

pub(crate) fn settings_close_button_label(app: &AppState) -> &'static str {
    if app.settings.guided_setup {
        return if app.settings.gateways.editing_credential {
            "cancel key"
        } else {
            "finish later"
        };
    }
    if app.settings.section == crate::app::state::SettingsSection::Gateways
        && (matches!(
            app.settings.gateways.view,
            GatewaySettingsView::Form | GatewaySettingsView::DeleteConfirm
        ) || app.settings.gateways.editing_credential)
    {
        "cancel"
    } else if app.settings.section == crate::app::state::SettingsSection::Gateways
        && app.settings.gateways.view == GatewaySettingsView::Detail
    {
        "back"
    } else {
        "close"
    }
}

pub(crate) fn settings_show_primary_action(app: &AppState) -> bool {
    if app.settings.guided_setup {
        return true;
    }
    match app.settings.section {
        crate::app::state::SettingsSection::Gateways => match app.settings.gateways.view {
            GatewaySettingsView::List => app
                .gateway_catalog
                .gateways
                .keys()
                .nth(app.settings.gateways.selected_gateway)
                .is_some(),
            GatewaySettingsView::Detail => app
                .settings
                .gateways
                .detail_gateway_id
                .as_ref()
                .is_some_and(|id| app.gateway_catalog.gateways.contains_key(id)),
            GatewaySettingsView::Form => app.settings.gateways.gateway_form.is_some(),
            GatewaySettingsView::DeleteConfirm => app
                .settings
                .gateways
                .detail_gateway_id
                .as_ref()
                .and_then(|id| app.gateway_catalog.gateways.get(id))
                .is_some_and(|gateway| gateway.preset.is_none()),
        },
        crate::app::state::SettingsSection::Integrations => app
            .integration_recommendations
            .iter()
            .any(crate::integration::IntegrationRecommendation::needs_install),
        _ => true,
    }
}

pub(crate) fn settings_button_rects(
    inner: Rect,
    app: &AppState,
    show_primary: bool,
) -> (Option<Rect>, Rect) {
    let close_label = settings_close_button_label(app);
    if !show_primary {
        let rects = action_button_row_rects(
            inner,
            &[ActionButtonSpec {
                hint: Some("esc"),
                label: close_label,
            }],
            2,
            inner.height.saturating_sub(1),
        );
        return (None, rects[0]);
    }

    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some(settings_primary_button_hint(app)),
                label: settings_primary_button_label(app),
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: close_label,
            },
        ],
        2,
        inner.height.saturating_sub(1),
    );
    (Some(rects[0]), rects[1])
}

fn guided_step_order(step: GuidedSetupStep) -> usize {
    match step {
        GuidedSetupStep::CliCheck => 0,
        GuidedSetupStep::ConnectMindshub => 1,
        GuidedSetupStep::VerifyMindshub => 2,
        GuidedSetupStep::ChooseCodexModel | GuidedSetupStep::ChooseClaudeModel => 3,
        GuidedSetupStep::Launch => 4,
    }
}

fn render_guided_setup_progress(app: &AppState, frame: &mut Frame, area: Rect) {
    let current = guided_step_order(app.guided_setup_step());
    let mut spans = Vec::new();
    for (index, label) in ["CLIs", "Key", "Verify", "Models", "Launch"]
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        let symbol = if index < current {
            "✓"
        } else if index == current {
            "→"
        } else {
            "○"
        };
        let style = if index == current {
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else if index < current {
            Style::default().fg(app.palette.green)
        } else {
            Style::default().fg(app.palette.overlay1)
        };
        spans.push(Span::styled(format!("{symbol} {label}"), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(crate) fn guided_setup_aux_button_rects(area: Rect) -> (Rect, Rect) {
    let skip_width = action_button_width(Some("q"), "skip").min(area.width);
    let skip = Rect::new(
        area.x.saturating_add(area.width.saturating_sub(skip_width)),
        area.y,
        skip_width,
        u16::from(area.height > 0),
    );
    let custom_width = action_button_width(Some("a"), "custom")
        .min(skip.x.saturating_sub(area.x).saturating_sub(1));
    let custom = Rect::new(
        skip.x.saturating_sub(custom_width.saturating_add(1)),
        area.y,
        custom_width,
        skip.height,
    );
    (custom, skip)
}

pub(crate) fn guided_setup_launch_button_rects(area: Rect) -> (Rect, Rect) {
    let rects = action_button_row_rects(
        area,
        &[
            ActionButtonSpec {
                hint: Some("c"),
                label: "Codex",
            },
            ActionButtonSpec {
                hint: Some("l"),
                label: "Claude",
            },
        ],
        2,
        area.height.saturating_sub(2),
    );
    (rects[0], rects[1])
}

fn guided_connection_status(
    gateway: Option<&Gateway>,
    protocol: GatewayProtocol,
) -> ConnectionStatus {
    gateway
        .and_then(|gateway| gateway.connection_test.protocols.get(&protocol))
        .map_or(ConnectionStatus::NotTested, |test| test.status)
}

fn render_guided_setup(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let p = &app.palette;
    frame.render_widget(
        Paragraph::new(" open model cowork")
            .style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let (custom_rect, skip_rect) = guided_setup_aux_button_rects(area);
    render_action_button(
        frame,
        custom_rect,
        Some("a"),
        "custom",
        Style::default().fg(p.text).bg(p.surface0),
    );
    render_action_button(
        frame,
        skip_rect,
        Some("q"),
        "skip",
        Style::default().fg(p.text).bg(p.surface0),
    );

    let codex = app.guided_cli_available(crate::api::schema::IntegrationTarget::Codex);
    let claude = app.guided_cli_available(crate::api::schema::IntegrationTarget::Claude);
    let available_label = |available| {
        if available {
            "✓ found"
        } else {
            "× not found"
        }
    };
    if area.height > 1 {
        frame.render_widget(
            Paragraph::new(format!(
                " CLIs      Codex {}  ·  Claude {}",
                available_label(codex),
                available_label(claude)
            ))
            .style(Style::default().fg(p.subtext0)),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }

    let gateway = app.gateway_catalog.gateways.get("mindshub");
    let credential = app
        .settings
        .gateways
        .credential_status
        .get("mindshub")
        .copied()
        .unwrap_or_default();
    let credential_label = if app.settings.gateways.editing_credential {
        if app.settings.gateways.secret_input.is_empty() {
            "typing key: empty"
        } else {
            "typing key: ••••••••"
        }
    } else {
        match credential {
            GatewayCredentialStatus::Stored => "key stored",
            GatewayCredentialStatus::Missing => "key missing",
            GatewayCredentialStatus::Unknown => "key status unavailable",
        }
    };
    if area.height > 2 {
        frame.render_widget(
            Paragraph::new(format!(
                " MindsHub  {}  ·  Responses {}  ·  Messages {}",
                credential_label,
                status_symbol(guided_connection_status(
                    gateway,
                    GatewayProtocol::OpenAiResponses,
                )),
                status_symbol(guided_connection_status(
                    gateway,
                    GatewayProtocol::AnthropicMessages,
                )),
            ))
            .style(Style::default().fg(p.subtext0)),
            Rect::new(area.x, area.y + 2, area.width, 1),
        );
    }
    if area.height > 3 {
        let models = gateway
            .map(|gateway| {
                gateway
                    .model_discovery
                    .cached_models
                    .iter()
                    .filter(|model| model.enabled && !model.embedding)
                    .count()
            })
            .unwrap_or_default();
        let codex_model = gateway
            .and_then(|gateway| gateway.default_models.get("codex"))
            .map_or("not chosen", String::as_str);
        let claude_model = gateway
            .and_then(|gateway| gateway.default_models.get("claude"))
            .map_or("not chosen", String::as_str);
        let model_width = (area.width as usize).saturating_sub(37) / 2;
        frame.render_widget(
            Paragraph::new(format!(
                " Models    {models} found  ·  Codex {}  ·  Claude {}",
                compact_text(codex_model, model_width.max(8)),
                compact_text(claude_model, model_width.max(8)),
            ))
            .style(Style::default().fg(p.subtext0)),
            Rect::new(area.x, area.y + 3, area.width, 1),
        );
    }

    let (title, description) = match app.guided_setup_step() {
        GuidedSetupStep::CliCheck => (
            "Install a coding CLI",
            "Install Codex CLI or Claude Code, then check again. You can finish later or skip setup.",
        ),
        GuidedSetupStep::ConnectMindshub if app.settings.gateways.editing_credential => (
            "Enter your MindsHub API key",
            "Input is hidden and stored locally in the operating-system credential store.",
        ),
        GuidedSetupStep::ConnectMindshub => (
            "Connect MindsHub Inference",
            "Add the API key for the built-in MindsHub route. Custom gateways remain available above.",
        ),
        GuidedSetupStep::VerifyMindshub => (
            "Verify the complete route",
            "Test authentication, model discovery, OpenAI Responses, and Anthropic Messages.",
        ),
        GuidedSetupStep::ChooseCodexModel => (
            "Choose the Codex default",
            "Search models by label, provider, or full ID, then confirm the exact route.",
        ),
        GuidedSetupStep::ChooseClaudeModel => (
            "Choose the Claude default",
            "Search models by label, provider, or full ID, then confirm the exact route.",
        ),
        GuidedSetupStep::Launch => (
            "Launch your first managed agent",
            "Choose a detected CLI. GoWild keeps the exact MindsHub route visible after handoff.",
        ),
    };
    if area.height > 5 {
        frame.render_widget(
            Paragraph::new(format!(" {title}"))
                .style(Style::default().fg(p.accent).add_modifier(Modifier::BOLD)),
            Rect::new(area.x, area.y + 5, area.width, 1),
        );
    }
    if area.height > 6 {
        frame.render_widget(
            Paragraph::new(format!(" {description}"))
                .style(Style::default().fg(p.overlay1))
                .wrap(ratatui::widgets::Wrap { trim: false }),
            Rect::new(area.x, area.y + 6, area.width, 2.min(area.height - 6)),
        );
    }

    if app.guided_setup_step() == GuidedSetupStep::Launch && area.height > 1 {
        let (codex_rect, claude_rect) = guided_setup_launch_button_rects(area);
        render_action_button(
            frame,
            codex_rect,
            Some("c"),
            "Codex",
            Style::default()
                .fg(if codex {
                    panel_contrast_fg(p)
                } else {
                    p.overlay1
                })
                .bg(if codex { p.accent } else { p.surface0 })
                .add_modifier(Modifier::BOLD),
        );
        render_action_button(
            frame,
            claude_rect,
            Some("l"),
            "Claude",
            Style::default()
                .fg(if claude {
                    panel_contrast_fg(p)
                } else {
                    p.overlay1
                })
                .bg(if claude { p.accent } else { p.surface0 })
                .add_modifier(Modifier::BOLD),
        );
    }

    let notice = app
        .settings
        .guided_setup_error
        .as_deref()
        .map(|message| (message, p.red))
        .or_else(|| {
            app.settings.gateways.notice.as_ref().map(|notice| {
                let color = match notice.kind {
                    GatewayNoticeKind::Info => p.blue,
                    GatewayNoticeKind::Success => p.green,
                    GatewayNoticeKind::Warning => p.yellow,
                    GatewayNoticeKind::Error => p.red,
                };
                (notice.message.as_str(), color)
            })
        });
    if let Some((notice, color)) = notice {
        let notice_y = area.y.saturating_add(area.height.saturating_sub(1));
        frame.render_widget(
            Paragraph::new(format!(
                " {}",
                compact_text(notice, area.width.saturating_sub(1) as usize)
            ))
            .style(Style::default().fg(color)),
            Rect::new(area.x, notice_y, area.width, 1),
        );
    }
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn compact_input_text(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let tail_chars = max_chars.saturating_sub(1);
    let tail = value
        .chars()
        .skip(count.saturating_sub(tail_chars))
        .collect::<String>();
    format!("…{tail}")
}

fn connection_label(status: ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::NotTested => "○ not tested",
        ConnectionStatus::Passed => "● connected",
        ConnectionStatus::Partial => "◐ partial",
        ConnectionStatus::Failed => "× failed",
    }
}

fn protocol_badge(
    gateway: &Gateway,
    protocol: GatewayProtocol,
) -> (&'static str, ConnectionStatus) {
    let label = match protocol {
        GatewayProtocol::OpenAiResponses => "Responses",
        GatewayProtocol::AnthropicMessages => "Messages",
    };
    let status = gateway
        .connection_test
        .protocols
        .get(&protocol)
        .map_or(ConnectionStatus::NotTested, |test| test.status);
    (label, status)
}

fn status_color(status: ConnectionStatus, p: &Palette) -> ratatui::style::Color {
    match status {
        ConnectionStatus::NotTested => p.overlay1,
        ConnectionStatus::Passed => p.green,
        ConnectionStatus::Partial => p.yellow,
        ConnectionStatus::Failed => p.red,
    }
}

fn render_gateway_notice(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(notice) = &app.settings.gateways.notice else {
        return;
    };
    let color = match notice.kind {
        GatewayNoticeKind::Info => app.palette.blue,
        GatewayNoticeKind::Success => app.palette.green,
        GatewayNoticeKind::Warning => app.palette.yellow,
        GatewayNoticeKind::Error => app.palette.red,
    };
    frame.render_widget(
        Paragraph::new(format!(" {}", notice.message))
            .style(Style::default().fg(color))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
}

fn render_settings_gateways(app: &AppState, frame: &mut Frame, area: Rect) {
    match app.settings.gateways.view {
        GatewaySettingsView::List => render_gateway_list(app, frame, area),
        GatewaySettingsView::Detail => render_gateway_detail(app, frame, area),
        GatewaySettingsView::Form => render_gateway_form(app, frame, area),
        GatewaySettingsView::DeleteConfirm => render_gateway_delete_confirm(app, frame, area),
    }
}

pub(crate) fn gateway_add_button_rect(area: Rect) -> Rect {
    let width = 14.min(area.width);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width)),
        area.y,
        width,
        u16::from(area.height > 0),
    )
}

pub(crate) fn gateway_detail_button_rects(area: Rect, show_edit: bool) -> (Option<Rect>, Rect) {
    let duplicate_width = 13.min(area.width);
    let duplicate = Rect::new(
        area.x
            .saturating_add(area.width.saturating_sub(duplicate_width)),
        area.y.saturating_add(1),
        duplicate_width,
        u16::from(area.height > 1),
    );
    let edit = show_edit.then(|| {
        let gap = 1;
        let width = 8.min(area.width.saturating_sub(duplicate_width + gap));
        Rect::new(
            duplicate.x.saturating_sub(width + gap),
            duplicate.y,
            width,
            duplicate.height,
        )
    });
    (edit, duplicate)
}

pub(crate) fn gateway_detail_delete_button_rect(area: Rect, show_delete: bool) -> Option<Rect> {
    let (edit, duplicate) = gateway_detail_button_rects(area, show_delete);
    show_delete.then(|| {
        let anchor = edit.unwrap_or(duplicate);
        let gap = 1;
        let width = 10.min(anchor.x.saturating_sub(area.x + gap));
        Rect::new(
            anchor.x.saturating_sub(width + gap),
            anchor.y,
            width,
            anchor.height,
        )
    })
}

fn gateway_protocol_summary(gateway: &Gateway) -> &'static str {
    match (
        gateway.supports(GatewayProtocol::OpenAiResponses),
        gateway.supports(GatewayProtocol::AnthropicMessages),
    ) {
        (true, true) => "Responses + Messages",
        (true, false) => "Responses",
        (false, true) => "Messages",
        (false, false) => "No launch protocol",
    }
}

fn render_gateway_list(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    if area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " inference gateways",
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  CLI + model independent", Style::default().fg(p.accent)),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let add_rect = gateway_add_button_rect(area);
    render_action_button(
        frame,
        add_rect,
        Some("a"),
        "add custom",
        Style::default()
            .fg(panel_contrast_fg(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD),
    );
    if area.height > 1 {
        frame.render_widget(
            Paragraph::new(" choose where Codex and Claude send model requests")
                .style(Style::default().fg(p.overlay1)),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }

    if app.gateway_catalog.gateways.is_empty() && area.height > 3 {
        frame.render_widget(
            Paragraph::new(" No gateways configured.").style(Style::default().fg(p.overlay1)),
            Rect::new(area.x, area.y + 3, area.width, 1),
        );
    }

    for (index, (gateway_id, gateway)) in app.gateway_catalog.gateways.iter().enumerate() {
        let y = area.y.saturating_add(3 + index as u16 * 3);
        if y >= area.y.saturating_add(area.height) {
            break;
        }
        let row = Rect::new(
            area.x,
            y,
            area.width,
            2.min(area.y.saturating_add(area.height).saturating_sub(y)),
        );
        let selected = index == app.settings.gateways.selected_gateway;
        let style = if selected {
            Style::default().bg(p.surface0).fg(p.text)
        } else {
            Style::default().fg(p.subtext0)
        };
        let in_flight = app
            .settings
            .gateways
            .test_in_flight
            .as_ref()
            .is_some_and(|(_, id)| id == gateway_id);
        let status_label = connection_label(gateway.connection_test.status);
        let default = if app.gateway_catalog.default_gateway_id.as_deref() == Some(gateway_id) {
            "  default"
        } else {
            ""
        };
        let models = gateway
            .model_discovery
            .cached_models
            .iter()
            .filter(|model| model.enabled && !model.embedding)
            .count();
        let status_label = if in_flight {
            "◌ testing…"
        } else {
            status_label
        };
        let status_color = if in_flight {
            p.blue
        } else {
            status_color(gateway.connection_test.status, p)
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(if selected { " ▸ " } else { "   " }, style),
                    Span::styled(&gateway.display_name, style.add_modifier(Modifier::BOLD)),
                    Span::styled(default, Style::default().fg(p.accent)),
                    Span::raw("  "),
                    Span::styled(status_label, Style::default().fg(status_color)),
                ]),
                Line::from(vec![
                    Span::styled(format!("   {}", gateway_protocol_summary(gateway)), style),
                    Span::styled(
                        format!("  {models} models"),
                        Style::default().fg(p.overlay1),
                    ),
                ]),
            ])
            .style(style),
            row,
        );
    }

    let notice_y = area.y.saturating_add(area.height.saturating_sub(2));
    render_gateway_notice(
        app,
        frame,
        Rect::new(area.x, notice_y, area.width, area.height.min(2)),
    );
}

fn render_gateway_detail(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let Some(gateway_id) = app.settings.gateways.detail_gateway_id.as_deref() else {
        frame.render_widget(
            Paragraph::new(" Gateway is no longer available.").style(Style::default().fg(p.red)),
            area,
        );
        return;
    };
    let Some(gateway) = app.gateway_catalog.gateways.get(gateway_id) else {
        frame.render_widget(
            Paragraph::new(" Gateway is no longer available.").style(Style::default().fg(p.red)),
            area,
        );
        return;
    };
    let in_flight = app
        .settings
        .gateways
        .test_in_flight
        .as_ref()
        .is_some_and(|(_, id)| id == gateway_id);
    let connection_label = connection_label(gateway.connection_test.status);
    let connection_label = if in_flight {
        "◌ testing…"
    } else {
        connection_label
    };
    let connection_color = if in_flight {
        p.blue
    } else {
        status_color(gateway.connection_test.status, p)
    };
    let default = if app.gateway_catalog.default_gateway_id.as_deref() == Some(gateway_id) {
        "  default"
    } else {
        "  space: make default"
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {}", gateway.display_name),
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(default, Style::default().fg(p.accent)),
            Span::raw("  "),
            Span::styled(connection_label, Style::default().fg(connection_color)),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if area.height > 1 {
        frame.render_widget(
            Paragraph::new(" one gateway · two coding CLIs").style(Style::default().fg(p.overlay1)),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        let custom = gateway.preset.is_none();
        let (edit_rect, duplicate_rect) = gateway_detail_button_rects(area, custom);
        if let Some(edit_rect) = edit_rect {
            render_action_button(
                frame,
                edit_rect,
                Some("e"),
                "edit",
                Style::default().fg(p.text).bg(p.surface0),
            );
        }
        render_action_button(
            frame,
            duplicate_rect,
            Some("d"),
            "duplicate",
            Style::default().fg(p.text).bg(p.surface0),
        );
        if let Some(delete_rect) = gateway_detail_delete_button_rect(area, custom) {
            render_action_button(
                frame,
                delete_rect,
                Some("x"),
                "delete",
                Style::default().fg(p.text).bg(p.surface0),
            );
        }
    }

    let credential_status = if gateway.auth.mode == AuthenticationMode::None {
        "not required"
    } else if app.settings.gateways.editing_credential {
        if app.settings.gateways.secret_input.is_empty() {
            "empty"
        } else {
            "••••••••••••"
        }
    } else {
        match app
            .settings
            .gateways
            .credential_status
            .get(gateway_id)
            .copied()
            .unwrap_or_default()
        {
            GatewayCredentialStatus::Stored => "stored securely  ↵ replace",
            GatewayCredentialStatus::Missing => "not set  ↵ add API key",
            GatewayCredentialStatus::Unknown => "status unavailable  ↵ add or replace",
        }
    };
    let codex_model = gateway
        .default_models
        .get("codex")
        .map_or("test to discover models", String::as_str);
    let claude_model = gateway
        .default_models
        .get("claude")
        .map_or("test to discover models", String::as_str);
    let model_chars = (area.width as usize).saturating_sub(21).max(8);
    let fields = [
        (
            GatewayDetailField::Credential,
            "API key",
            credential_status.to_string(),
        ),
        (
            GatewayDetailField::CodexModel,
            "Codex model",
            compact_text(codex_model, model_chars),
        ),
        (
            GatewayDetailField::ClaudeModel,
            "Claude model",
            compact_text(claude_model, model_chars),
        ),
    ];
    for (index, (field, label, value)) in fields.into_iter().enumerate() {
        let y = area.y.saturating_add(3 + index as u16);
        if y >= area.y.saturating_add(area.height) {
            break;
        }
        let selected = field == app.settings.gateways.detail_field;
        let style = if selected {
            Style::default()
                .fg(p.text)
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        frame.render_widget(
            Paragraph::new(format!(
                " {} {:<13}  {}",
                if selected { "▸" } else { " " },
                label,
                value
            ))
            .style(style),
            Rect::new(area.x, y, area.width, 1),
        );
    }

    let protocol_y = area.y.saturating_add(7);
    if protocol_y < area.y.saturating_add(area.height) {
        let mut spans = vec![Span::styled(
            " protocols  ",
            Style::default().fg(p.overlay1),
        )];
        for protocol in [
            GatewayProtocol::OpenAiResponses,
            GatewayProtocol::AnthropicMessages,
        ] {
            if !gateway.supports(protocol) {
                continue;
            }
            if spans.len() > 1 {
                spans.push(Span::raw("    "));
            }
            let (label, status) = protocol_badge(gateway, protocol);
            spans.push(Span::styled(
                format!("{label} {}", status_symbol(status)),
                Style::default().fg(status_color(status, p)),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, protocol_y, area.width, 1),
        );
    }

    let diagnostic_y = area.y.saturating_add(8);
    if diagnostic_y < area.y.saturating_add(area.height) {
        let diagnostic = gateway.connection_test.diagnostics.first().or_else(|| {
            gateway
                .connection_test
                .protocols
                .values()
                .find_map(|test| test.diagnostics.first())
        });
        if let Some(diagnostic) = diagnostic {
            frame.render_widget(
                Paragraph::new(format!(" {}", compact_text(diagnostic.message(), 67)))
                    .style(Style::default().fg(p.overlay1)),
                Rect::new(area.x, diagnostic_y, area.width, 1),
            );
        }
    }

    let notice_y = area.y.saturating_add(area.height.saturating_sub(2));
    render_gateway_notice(
        app,
        frame,
        Rect::new(area.x, notice_y, area.width, area.height.min(2)),
    );
}

fn render_gateway_delete_confirm(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let Some(gateway_id) = app.settings.gateways.detail_gateway_id.as_deref() else {
        frame.render_widget(
            Paragraph::new(" Gateway is no longer available.").style(Style::default().fg(p.red)),
            area,
        );
        return;
    };
    let Some(gateway) = app.gateway_catalog.gateways.get(gateway_id) else {
        frame.render_widget(
            Paragraph::new(" Gateway is no longer available.").style(Style::default().fg(p.red)),
            area,
        );
        return;
    };
    if gateway.preset.is_some() {
        frame.render_widget(
            Paragraph::new(" Built-in gateway presets cannot be deleted.")
                .style(Style::default().fg(p.red)),
            area,
        );
        return;
    }

    let lines = [
        Line::from(Span::styled(
            " delete custom gateway",
            Style::default().fg(p.red).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(" remove  ", Style::default().fg(p.overlay1)),
            Span::styled(&gateway.display_name, Style::default().fg(p.text)),
        ]),
        Line::from(Span::styled(
            " This removes the gateway definition and its CLI model defaults.",
            Style::default().fg(p.subtext0),
        )),
        Line::from(Span::styled(
            " Choose what happens to its stored credential:",
            Style::default().fg(p.subtext0),
        )),
    ];
    for (index, line) in lines.into_iter().enumerate() {
        let y = area.y.saturating_add(index as u16);
        if y < area.y.saturating_add(area.height) {
            frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        }
    }

    let choices = [
        (
            CredentialRemoval::Keep,
            "keep stored credential",
            "recommended; delete only the definition",
        ),
        (
            CredentialRemoval::Delete,
            "delete stored credential too",
            "shared credentials are retained automatically",
        ),
    ];
    for (index, (choice, label, description)) in choices.into_iter().enumerate() {
        let y = area.y.saturating_add(5 + index as u16 * 2);
        if y >= area.y.saturating_add(area.height) {
            break;
        }
        let selected = app.settings.gateways.credential_removal == choice;
        let style = if selected {
            Style::default()
                .fg(p.text)
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        frame.render_widget(
            Paragraph::new(format!(" {} {}", if selected { "▸" } else { " " }, label)).style(style),
            Rect::new(area.x, y, area.width, 1),
        );
        if y + 1 < area.y.saturating_add(area.height) {
            frame.render_widget(
                Paragraph::new(format!("   {description}")).style(Style::default().fg(p.overlay1)),
                Rect::new(area.x, y + 1, area.width, 1),
            );
        }
    }

    let notice_y = area.y.saturating_add(area.height.saturating_sub(2));
    render_gateway_notice(
        app,
        frame,
        Rect::new(area.x, notice_y, area.width, area.height.min(2)),
    );
}

fn authentication_label(mode: AuthenticationMode) -> &'static str {
    match mode {
        AuthenticationMode::BearerToken => "bearer token",
        AuthenticationMode::XApiKey => "x-api-key",
        AuthenticationMode::CustomHeader => "custom secret header",
        AuthenticationMode::None => "none",
    }
}

fn gateway_form_field_label(field: GatewayFormField) -> &'static str {
    match field {
        GatewayFormField::Id => "Gateway ID",
        GatewayFormField::DisplayName => "Name",
        GatewayFormField::ResponsesUrl => "Responses URL",
        GatewayFormField::MessagesUrl => "Messages URL",
        GatewayFormField::ModelsUrl => "Models URL",
        GatewayFormField::Authentication => "Authentication",
        GatewayFormField::AuthHeader => "Secret header",
        GatewayFormField::AuthPrefix => "Value prefix",
    }
}

fn gateway_form_field_value(
    form: &crate::app::state::CustomGatewayFormState,
    field: GatewayFormField,
) -> String {
    match field {
        GatewayFormField::Id => form.id.clone(),
        GatewayFormField::DisplayName => form.display_name.clone(),
        GatewayFormField::ResponsesUrl => form.responses_url.clone(),
        GatewayFormField::MessagesUrl => form.messages_url.clone(),
        GatewayFormField::ModelsUrl => form.models_url.clone(),
        GatewayFormField::Authentication => authentication_label(form.authentication).into(),
        GatewayFormField::AuthHeader => form.auth_header.clone(),
        GatewayFormField::AuthPrefix => form.auth_prefix.clone(),
    }
}

fn render_gateway_form(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let Some(form) = app.settings.gateways.gateway_form.as_ref() else {
        frame.render_widget(
            Paragraph::new(" Gateway form is no longer available.")
                .style(Style::default().fg(p.red)),
            area,
        );
        return;
    };
    if area.height == 0 {
        return;
    }
    let title = match form.mode {
        CustomGatewayFormMode::Add => " add custom gateway",
        CustomGatewayFormMode::Edit => " edit custom gateway",
        CustomGatewayFormMode::Duplicate => " duplicate as custom gateway",
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                title,
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  API key follows after save",
                Style::default().fg(p.accent),
            ),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let first_offset = if area.height < 12 { 1 } else { 2 };
    let visible_rows = area.height.saturating_sub(first_offset + 2) as usize;
    let selected_index = GatewayFormField::ALL
        .iter()
        .position(|field| *field == form.selected_field)
        .unwrap_or_default();
    let scroll = selected_index
        .saturating_add(1)
        .saturating_sub(visible_rows);
    let value_chars = (area.width as usize).saturating_sub(23).max(8);
    for (visible_index, field) in GatewayFormField::ALL
        .iter()
        .copied()
        .skip(scroll)
        .take(visible_rows)
        .enumerate()
    {
        let selected = field == form.selected_field;
        let enabled = form.field_is_editable(field) || field == GatewayFormField::Authentication;
        let mut value = gateway_form_field_value(form, field);
        if value.is_empty() {
            value = match field {
                GatewayFormField::ResponsesUrl | GatewayFormField::MessagesUrl => {
                    "optional; configure at least one".into()
                }
                GatewayFormField::ModelsUrl => "optional model discovery".into(),
                GatewayFormField::AuthHeader | GatewayFormField::AuthPrefix if !enabled => {
                    "custom header auth only".into()
                }
                _ => "required".into(),
            };
        } else if field == GatewayFormField::Id && form.is_editing_existing() {
            value.push_str("  fixed");
        } else if selected && form.field_is_editable(field) {
            value.push('▏');
        }
        let value = if selected && form.field_is_editable(field) {
            compact_input_text(&value, value_chars)
        } else {
            compact_text(&value, value_chars)
        };
        let style = if selected {
            Style::default()
                .fg(if enabled { p.text } else { p.overlay1 })
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD)
        } else if enabled {
            Style::default().fg(p.subtext0)
        } else {
            Style::default().fg(p.overlay0)
        };
        let y = area
            .y
            .saturating_add(first_offset)
            .saturating_add(visible_index as u16);
        frame.render_widget(
            Paragraph::new(format!(
                " {} {:<15}  {}",
                if selected { "▸" } else { " " },
                gateway_form_field_label(field),
                value
            ))
            .style(style),
            Rect::new(area.x, y, area.width, 1),
        );
    }

    let notice_y = area.y.saturating_add(area.height.saturating_sub(2));
    render_gateway_notice(
        app,
        frame,
        Rect::new(area.x, notice_y, area.width, area.height.min(2)),
    );
}

fn status_symbol(status: ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::NotTested => "○",
        ConnectionStatus::Passed => "✓",
        ConnectionStatus::Partial => "◐",
        ConnectionStatus::Failed => "×",
    }
}

fn integrations_footer_paragraph(app: &AppState) -> Paragraph<'static> {
    let p = &app.palette;
    let mut footer_lines = Vec::new();
    if !app.integration_install_messages.is_empty() {
        for message in &app.integration_install_messages {
            footer_lines.push(Line::from(Span::styled(
                format!(" {message}"),
                Style::default().fg(p.overlay1),
            )));
        }
    } else {
        let found_any = app.integration_recommendations.iter().any(|item| {
            item.available || item.state != crate::integration::IntegrationStatusKind::NotInstalled
        });
        let hint = if app
            .integration_recommendations
            .iter()
            .any(crate::integration::IntegrationRecommendation::needs_install)
        {
            " press install to add available or outdated integrations"
        } else if found_any {
            " all detected integrations are installed"
        } else {
            " no supported agent CLIs found on PATH"
        };
        footer_lines.push(Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(p.overlay1),
        )));
    }
    Paragraph::new(footer_lines).wrap(ratatui::widgets::Wrap { trim: false })
}

fn integrations_footer_height(app: &AppState, width: u16) -> u16 {
    (integrations_footer_paragraph(app).line_count(width) as u16).min(6)
}

fn render_settings_integrations(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;

    let footer = integrations_footer_paragraph(app);
    let footer_height = integrations_footer_height(app, area.width);

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(footer_height),
    ])
    .areas::<6>(area);

    frame.render_widget(
        Paragraph::new("agent integrations")
            .style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(
            "let agents report state directly instead of relying only on process detection",
        )
        .style(Style::default().fg(p.overlay1))
        .wrap(ratatui::widgets::Wrap { trim: false }),
        rows[1],
    );

    let mut lines = Vec::new();
    for item in &app.integration_recommendations {
        let marker = match item.state {
            crate::integration::IntegrationStatusKind::Current => "✓",
            crate::integration::IntegrationStatusKind::Outdated => "↻",
            crate::integration::IntegrationStatusKind::NotInstalled if item.available => "+",
            crate::integration::IntegrationStatusKind::NotInstalled => "–",
        };
        let marker_style = match item.state {
            crate::integration::IntegrationStatusKind::Current => Style::default().fg(p.green),
            crate::integration::IntegrationStatusKind::Outdated => Style::default().fg(p.yellow),
            crate::integration::IntegrationStatusKind::NotInstalled if item.available => {
                Style::default().fg(p.accent)
            }
            crate::integration::IntegrationStatusKind::NotInstalled => {
                Style::default().fg(p.overlay0)
            }
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), marker_style),
            Span::styled(
                format!("{:<9}", item.label),
                Style::default().fg(p.subtext0),
            ),
            Span::styled(item.status_label(), Style::default().fg(p.overlay1)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no integration targets available",
            Style::default().fg(p.overlay1),
        )));
    }

    frame.render_widget(Paragraph::new(lines), rows[3]);
    frame.render_widget(footer, rows[5]);
}

fn render_settings_theme(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::THEME_NAMES;

    let p = &app.palette;
    let items: Vec<ListItem> = THEME_NAMES
        .iter()
        .map(|name| {
            let is_current = name.to_lowercase().replace([' ', '_'], "-")
                == app.theme_name.to_lowercase().replace([' ', '_'], "-");
            let marker = if is_current { " ✓" } else { "" };
            ListItem::new(Line::from(vec![
                Span::styled(*name, Style::default().fg(p.subtext0)),
                Span::styled(marker, Style::default().fg(p.green)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ▸ ")
        .style(Style::default().fg(p.subtext0));

    let mut state = ListState::default().with_selected(Some(app.settings.list.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_settings_toggle(
    frame: &mut Frame,
    area: Rect,
    p: &Palette,
    title: &str,
    description: &str,
    current_value: bool,
    selected_idx: usize,
) {
    render_modal_choice_list(
        frame,
        area,
        title,
        description,
        &[("on", true), ("off", false)],
        current_value,
        selected_idx,
        p,
        1,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;
    use crate::{
        app::state::{
            CustomGatewayFormState, GatewayCredentialStatus, GatewaySettingsView, Mode,
            SettingsSection,
        },
        gateway::CachedModel,
    };

    fn rendered_gateway_settings(app: &AppState, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| render_settings_overlay(app, frame, frame.area()))
            .expect("gateway settings render");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn gateway_settings_state() -> AppState {
        let mut app = AppState::test_new();
        app.mode = Mode::Settings;
        app.settings.section = SettingsSection::Gateways;
        let gateway = app
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .expect("built-in MindsHub gateway");
        gateway.model_discovery.cached_models = vec![CachedModel {
            id: "provider/model-alpha".into(),
            label: Some("Model Alpha".into()),
            provider: Some("provider".into()),
            enabled: true,
            embedding: false,
            reasoning_efforts: Vec::new(),
        }];
        gateway
            .default_models
            .insert("codex".into(), "provider/model-alpha".into());
        app
    }

    fn guided_settings_state(ready: bool) -> AppState {
        use crate::gateway::{ConnectionTest, ProtocolTest};

        let mut app = AppState::test_new();
        app.mode = Mode::Settings;
        app.settings.section = SettingsSection::Gateways;
        app.settings.guided_setup = true;
        app.integration_recommendations = [
            (crate::api::schema::IntegrationTarget::Codex, "codex"),
            (crate::api::schema::IntegrationTarget::Claude, "claude"),
        ]
        .into_iter()
        .map(
            |(target, label)| crate::integration::IntegrationRecommendation {
                target,
                label,
                command: label,
                available: true,
                path: std::path::PathBuf::from("/tmp/gowild-guided-render"),
                state: crate::integration::IntegrationStatusKind::NotInstalled,
            },
        )
        .collect();
        if ready {
            app.settings
                .gateways
                .credential_status
                .insert("mindshub".into(), GatewayCredentialStatus::Stored);
            let gateway = app.gateway_catalog.gateways.get_mut("mindshub").unwrap();
            gateway.connection_test = ConnectionTest {
                status: ConnectionStatus::Passed,
                checked_at: Some("fixture".into()),
                protocols: [
                    (
                        GatewayProtocol::OpenAiResponses,
                        ProtocolTest {
                            status: ConnectionStatus::Passed,
                            diagnostics: Vec::new(),
                        },
                    ),
                    (
                        GatewayProtocol::AnthropicMessages,
                        ProtocolTest {
                            status: ConnectionStatus::Passed,
                            diagnostics: Vec::new(),
                        },
                    ),
                ]
                .into_iter()
                .collect(),
                diagnostics: Vec::new(),
            };
            gateway.model_discovery.cached_models = vec![CachedModel {
                id: "provider/shared-coding-model".into(),
                label: Some("Shared coding model".into()),
                provider: Some("provider".into()),
                enabled: true,
                embedding: false,
                reasoning_efforts: Vec::new(),
            }];
            gateway
                .default_models
                .insert("codex".into(), "provider/shared-coding-model".into());
            gateway
                .default_models
                .insert("claude".into(), "provider/shared-coding-model".into());
        }
        app
    }

    #[test]
    fn guided_setup_keeps_the_next_action_and_escape_hatches_visible_at_compact_sizes() {
        for (width, height) in [(64, 20), (80, 24)] {
            let rendered = rendered_gateway_settings(&guided_settings_state(false), width, height);
            assert!(
                rendered.contains("GoWild setup"),
                "{width}x{height}: {rendered}"
            );
            assert!(
                rendered.contains("Connect MindsHub"),
                "{width}x{height}: {rendered}"
            );
            assert!(rendered.contains("custom"), "{width}x{height}: {rendered}");
            assert!(rendered.contains("skip"), "{width}x{height}: {rendered}");
            assert!(
                rendered.contains("finish later"),
                "{width}x{height}: {rendered}"
            );
            assert!(!rendered.contains("prefix+a"));
        }
    }

    #[test]
    fn guided_setup_completion_exposes_direct_codex_and_claude_launches() {
        let rendered = rendered_gateway_settings(&guided_settings_state(true), 80, 24);

        assert!(rendered.contains("Launch your first managed agent"));
        assert!(rendered.contains("c Codex"));
        assert!(rendered.contains("l Claude"));
        assert!(rendered.contains("launch Codex"));
    }

    #[test]
    fn gateway_list_remains_legible_at_supported_and_constrained_sizes() {
        let app = gateway_settings_state();
        for (width, height) in [(80, 24), (100, 30), (64, 20)] {
            let rendered = rendered_gateway_settings(&app, width, height);
            assert!(rendered.contains("inference gateways"), "{width}×{height}");
            assert!(rendered.contains("MindsHub Inference"), "{width}×{height}");
            if width == 64 {
                assert!(rendered.contains("section 1 of 7"), "{width}×{height}");
            }
        }
    }

    #[test]
    fn settings_recompose_at_compact_standard_and_wide_sizes() {
        let app = gateway_settings_state();
        let compact = settings_popup_rect(Rect::new(0, 0, 64, 20), &app).unwrap();
        let standard = settings_popup_rect(Rect::new(0, 0, 100, 30), &app).unwrap();
        let wide_160 = settings_popup_rect(Rect::new(0, 0, 160, 45), &app).unwrap();
        let wide = settings_popup_rect(Rect::new(0, 0, 207, 62), &app).unwrap();

        assert_eq!(compact, Rect::new(0, 0, 64, 20));
        assert_eq!(standard.width, SETTINGS_POPUP_WIDTH);
        assert_eq!(wide_160.width, 120);
        assert_eq!(wide_160.height, 33);
        assert!(wide.width >= 150, "{wide:?}");
        assert!(wide.height >= 45, "{wide:?}");
    }

    #[test]
    fn wide_gateway_detail_uses_extra_width_for_the_complete_model_id() {
        let mut app = gateway_settings_state();
        let full_id = "provider/long-family/version-2026-08-22/reasoning-coding-context-extended";
        let gateway = app.gateway_catalog.gateways.get_mut("mindshub").unwrap();
        gateway
            .default_models
            .insert("codex".into(), full_id.into());
        gateway.model_discovery.cached_models[0].id = full_id.into();
        app.settings.gateways.view = GatewaySettingsView::Detail;
        app.settings.gateways.detail_gateway_id = Some("mindshub".into());

        let rendered = rendered_gateway_settings(&app, 207, 62);

        assert!(rendered.contains(full_id));
    }

    #[test]
    fn gateway_detail_never_renders_credential_contents() {
        let mut app = gateway_settings_state();
        app.settings.gateways.view = GatewaySettingsView::Detail;
        app.settings.gateways.detail_gateway_id = Some("mindshub".into());
        app.settings.gateways.editing_credential = true;
        app.settings
            .gateways
            .credential_status
            .insert("mindshub".into(), GatewayCredentialStatus::Stored);
        app.settings
            .gateways
            .secret_input
            .insert("TOP_SECRET_GATEWAY_KEY");

        for (width, height) in [(80, 24), (64, 20)] {
            let rendered = rendered_gateway_settings(&app, width, height);
            assert!(rendered.contains("••••••••••••"), "{width}×{height}");
            assert!(
                !rendered.contains("TOP_SECRET_GATEWAY_KEY"),
                "{width}×{height}"
            );
        }
        assert!(
            !format!("{:?}", app.settings.gateways.secret_input).contains("TOP_SECRET_GATEWAY_KEY")
        );
    }

    #[test]
    fn empty_credential_editor_has_one_concise_instruction_and_no_false_mask() {
        let mut app = gateway_settings_state();
        app.settings.gateways.view = GatewaySettingsView::Detail;
        app.settings.gateways.detail_gateway_id = Some("mindshub".into());
        app.settings.gateways.editing_credential = true;

        let rendered = rendered_gateway_settings(&app, 80, 24);

        assert!(rendered.contains("empty"));
        assert_eq!(rendered.matches("input hidden locally").count(), 1);
        assert!(!rendered.contains("••••••••••••"));
        assert!(rendered.contains("store"));
        assert!(rendered.contains("cancel"));
    }

    #[test]
    fn gateway_shortcut_labels_match_each_view() {
        let mut app = gateway_settings_state();

        for (width, height) in [(100, 30), (80, 24), (64, 20)] {
            let list = rendered_gateway_settings(&app, width, height);
            assert!(list.contains("↵ configure"), "{width}×{height}");
            assert!(list.contains("t test"), "{width}×{height}");

            app.settings.gateways.view = GatewaySettingsView::Detail;
            app.settings.gateways.detail_gateway_id = Some("mindshub".into());
            app.settings.gateways.detail_field = GatewayDetailField::Credential;
            let detail = rendered_gateway_settings(&app, width, height);
            assert!(detail.contains("↵ edit API key"), "{width}×{height}");
            assert!(detail.contains("t test"), "{width}×{height}");

            app.settings.gateways.editing_credential = true;
            let credential = rendered_gateway_settings(&app, width, height);
            assert!(credential.contains("↵ store"), "{width}×{height}");
            assert_eq!(settings_primary_button_hint(&app), "↵");
            assert_eq!(settings_primary_button_label(&app), "store");

            app.settings.gateways.editing_credential = false;
            app.settings.gateways.view = GatewaySettingsView::List;
        }
    }

    #[test]
    fn empty_gateway_catalog_has_an_explicit_state() {
        let mut app = gateway_settings_state();
        app.gateway_catalog.gateways.clear();

        let rendered = rendered_gateway_settings(&app, 80, 24);

        assert!(rendered.contains("No gateways configured."));
    }

    #[test]
    fn custom_gateway_form_scrolls_and_stays_legible_at_supported_sizes() {
        let mut app = gateway_settings_state();
        let mut form = CustomGatewayFormState::add();
        form.id = "private-hub".into();
        form.display_name = "Private Hub".into();
        form.responses_url = "https://gateway.example/v1".into();
        app.settings.gateways.view = GatewaySettingsView::Form;
        app.settings.gateways.gateway_form = Some(form);

        for (width, height) in [(80, 24), (100, 30), (64, 20)] {
            let rendered = rendered_gateway_settings(&app, width, height);
            assert!(rendered.contains("add custom gateway"), "{width}×{height}");
            assert!(rendered.contains("Gateway ID"), "{width}×{height}");
            assert!(rendered.contains("private-hub"), "{width}×{height}");
            assert!(rendered.contains("add"), "{width}×{height}");
            assert!(rendered.contains("cancel"), "{width}×{height}");
        }

        app.settings
            .gateways
            .gateway_form
            .as_mut()
            .expect("custom form")
            .selected_field = GatewayFormField::AuthPrefix;
        let constrained = rendered_gateway_settings(&app, 64, 20);
        assert!(constrained.contains("Value prefix"));

        let form = app
            .settings
            .gateways
            .gateway_form
            .as_mut()
            .expect("custom form");
        form.selected_field = GatewayFormField::ResponsesUrl;
        form.responses_url = format!("https://gateway.example/{}VISIBLE-END", "x".repeat(100));
        let constrained = rendered_gateway_settings(&app, 64, 20);
        assert!(constrained.contains("VISIBLE-END▏"));
    }

    #[test]
    fn custom_gateway_rows_show_actual_protocols_and_unauthenticated_state() {
        let mut app = gateway_settings_state();
        let mut custom = Gateway::mindshub();
        custom.id = "responses-only".into();
        custom.display_name = "Responses Only".into();
        custom.preset = None;
        custom.endpoints.anthropic_messages = None;
        custom
            .capabilities
            .protocols
            .remove(&GatewayProtocol::AnthropicMessages);
        custom.auth.mode = AuthenticationMode::None;
        custom.auth.credential_ref = None;
        app.gateway_catalog.gateways.clear();
        app.gateway_catalog
            .gateways
            .insert(custom.id.clone(), custom);

        let list = rendered_gateway_settings(&app, 100, 30);
        assert!(list.contains("Responses Only"));
        assert!(list.contains("Responses"));
        assert!(!list.contains("Messages"));

        app.settings.gateways.view = GatewaySettingsView::Detail;
        app.settings.gateways.detail_gateway_id = Some("responses-only".into());
        let detail = rendered_gateway_settings(&app, 80, 24);
        assert!(detail.contains("not required"));
        assert!(detail.contains("edit"));
        assert!(detail.contains("duplicate"));
    }

    #[test]
    fn custom_gateway_delete_confirmation_is_explicit_and_presets_have_no_delete_action() {
        let mut app = gateway_settings_state();
        app.settings.gateways.view = GatewaySettingsView::Detail;
        app.settings.gateways.detail_gateway_id = Some("mindshub".into());
        let preset = rendered_gateway_settings(&app, 80, 24);
        assert!(!preset.contains("delete"));

        let mut custom = Gateway::mindshub();
        custom.id = "private-hub".into();
        custom.display_name = "Private Hub".into();
        custom.preset = None;
        app.gateway_catalog
            .gateways
            .insert(custom.id.clone(), custom);
        app.settings.gateways.detail_gateway_id = Some("private-hub".into());
        let detail = rendered_gateway_settings(&app, 80, 24);
        assert!(detail.contains("delete"));

        app.settings.gateways.view = GatewaySettingsView::DeleteConfirm;
        app.settings.gateways.credential_removal = CredentialRemoval::Keep;
        for (width, height) in [(80, 24), (100, 30), (64, 20)] {
            let rendered = rendered_gateway_settings(&app, width, height);
            assert!(
                rendered.contains("delete custom gateway"),
                "{width}×{height}"
            );
            assert!(rendered.contains("Private Hub"), "{width}×{height}");
            assert!(
                rendered.contains("keep stored credential"),
                "{width}×{height}"
            );
            assert!(
                rendered.contains("delete stored credential too"),
                "{width}×{height}"
            );
            assert!(rendered.contains("delete gateway"), "{width}×{height}");
            assert!(rendered.contains("cancel"), "{width}×{height}");
        }

        app.settings.gateways.notice = Some(crate::app::state::GatewayNotice {
            kind: GatewayNoticeKind::Error,
            message: "Could not save gateway settings".into(),
        });
        let failed = rendered_gateway_settings(&app, 80, 24);
        assert!(failed.contains("Could not save gateway settings"));
    }
}
