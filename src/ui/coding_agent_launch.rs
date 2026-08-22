use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::{
    text::truncate_end,
    widgets::{centered_popup_rect, panel_contrast_fg, render_modal_header, render_modal_shell},
};
use crate::app::{state::CodingAgentLaunchField, AppState};

pub(crate) fn coding_agent_launch_inner_rect(area: Rect) -> Option<Rect> {
    let popup = centered_popup_rect(area, 74, 19)?;
    Some(Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    ))
}

pub(crate) fn coding_agent_launch_field_rect(inner: Rect, index: usize) -> Rect {
    let row_offset = match index {
        0 => 4,
        1 => 6,
        _ => 10,
    };
    Rect::new(inner.x, inner.y + row_offset, inner.width, 1)
}

pub(crate) fn coding_agent_launch_action_rects(inner: Rect) -> (Rect, Rect, Rect) {
    let y = inner.y + inner.height.saturating_sub(1);
    let launch = Rect::new(inner.x, y, 15.min(inner.width), 1);
    let settings_x = launch.right().saturating_add(1);
    let settings = Rect::new(
        settings_x,
        y,
        21.min(inner.right().saturating_sub(settings_x)),
        1,
    );
    let cancel_x = settings.right().saturating_add(1);
    let cancel = Rect::new(
        cancel_x,
        y,
        12.min(inner.right().saturating_sub(cancel_x)),
        1,
    );
    (launch, settings, cancel)
}

pub(super) fn render_coding_agent_launch_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);
    let Some(inner) = render_modal_shell(frame, area, 74, 19, &app.palette) else {
        return;
    };
    if inner.width < 36 || inner.height < 14 {
        return;
    }

    render_modal_header(
        frame,
        Rect::new(inner.x, inner.y, inner.width, 1),
        "launch coding agent",
        &app.palette,
    );
    frame.render_widget(
        Paragraph::new(" Choose the complete route before GoWild starts the CLI.")
            .style(Style::default().fg(app.palette.overlay1)),
        Rect::new(inner.x, inner.y + 2, inner.width, 1),
    );

    let selection = &app.coding_agent_launch;
    let gateway_name = selection
        .gateway(&app.gateway_catalog)
        .map(|gateway| gateway.display_name.as_str())
        .unwrap_or("no compatible gateway");
    let model = selection.model.as_deref().unwrap_or("no model selected");
    let fields = [
        (
            CodingAgentLaunchField::Cli,
            "CLI",
            selection.cli_label(),
            true,
        ),
        (
            CodingAgentLaunchField::Gateway,
            "Gateway",
            gateway_name,
            true,
        ),
        (CodingAgentLaunchField::Model, "Model", model, true),
    ];
    for (index, (field, label, value, editable)) in fields.into_iter().enumerate() {
        let row = coding_agent_launch_field_rect(inner, index);
        render_route_field(
            app,
            frame,
            row,
            label,
            value,
            selection.selected_field == field,
            editable,
        );
    }

    let protocol_y = inner.y + 8;
    render_route_field(
        app,
        frame,
        Rect::new(inner.x, protocol_y, inner.width, 1),
        "Protocol",
        selection.protocol().display_name(),
        false,
        false,
    );

    let can_launch = selection.can_launch(&app.gateway_catalog);
    let route = format!(
        " {} → {} → {} → {}",
        selection.cli_label(),
        gateway_name,
        selection.protocol().display_name(),
        model
    );
    let route = truncate_end(&route, inner.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            route,
            Style::default()
                .fg(if can_launch {
                    panel_contrast_fg(&app.palette)
                } else {
                    app.palette.text
                })
                .bg(if can_launch {
                    app.palette.accent
                } else {
                    app.palette.surface0
                })
                .add_modifier(Modifier::BOLD),
        ))),
        Rect::new(inner.x, inner.y + 12, inner.width, 1),
    );

    let validation_error = selection.validation_error(&app.gateway_catalog);
    let status = selection
        .error
        .as_deref()
        .or(validation_error.as_deref())
        .unwrap_or("GoWild will fail closed if any selected route value cannot be applied.");
    let footer_y = inner.y + inner.height.saturating_sub(1);
    let status_y = (inner.y + 14).min(footer_y.saturating_sub(2));
    let status_height = footer_y.saturating_sub(status_y).max(1);
    frame.render_widget(
        Paragraph::new(format!(" {status}"))
            .style(Style::default().fg(
                if selection.error.is_some() || validation_error.is_some() {
                    app.palette.red
                } else {
                    app.palette.overlay1
                },
            ))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        Rect::new(inner.x, status_y, inner.width, status_height),
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            if can_launch {
                Span::styled(
                    " enter launch ",
                    Style::default()
                        .fg(panel_contrast_fg(&app.palette))
                        .bg(app.palette.accent)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    " launch unavailable ",
                    Style::default()
                        .fg(app.palette.overlay1)
                        .bg(app.palette.surface0),
                )
            },
            Span::styled(
                if can_launch {
                    "  s gateway settings"
                } else {
                    " s fix route "
                },
                if can_launch {
                    Style::default().fg(app.palette.overlay1)
                } else {
                    Style::default()
                        .fg(panel_contrast_fg(&app.palette))
                        .bg(app.palette.accent)
                        .add_modifier(Modifier::BOLD)
                },
            ),
            Span::styled("  esc cancel", Style::default().fg(app.palette.overlay1)),
        ])),
        Rect::new(inner.x, footer_y, inner.width, 1),
    );
}

fn render_route_field(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    selected: bool,
    editable: bool,
) {
    let marker = if selected { "›" } else { " " };
    let value_width = area.width.saturating_sub(15) as usize;
    let value = truncate_end(value, value_width);
    let value = if editable {
        format!("‹ {value} ›")
    } else {
        format!("  {value}")
    };
    let style = if selected {
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.palette.text)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {marker} {label:<9}"), style),
            Span::styled(value, style),
        ])),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(app: &AppState, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_coding_agent_launch_overlay(app, frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn route_is_visible_and_truthful_at_supported_sizes() {
        let mut app = AppState::test_new();
        app.gateway_catalog.default_gateway_id = Some("mindshub".into());
        app.gateway_catalog
            .gateways
            .get_mut("mindshub")
            .unwrap()
            .default_models
            .insert("codex".into(), "routing-model".into());
        app.coding_agent_launch =
            crate::app::state::CodingAgentLaunchState::new(&app.gateway_catalog);

        for (width, height) in [(80, 24), (64, 20)] {
            let output = rendered(&app, width, height);
            assert!(output.contains("launch coding agent"), "{width}×{height}");
            assert!(output.contains("MindsHub Inference"), "{width}×{height}");
            assert!(output.contains("OpenAI Responses"), "{width}×{height}");
            assert!(output.contains("routing-model"), "{width}×{height}");
        }
    }

    #[test]
    fn missing_model_is_explicit() {
        let app = AppState::test_new();
        let output = rendered(&app, 64, 20);
        assert!(output.contains("no model selected"));
        assert!(output.contains("launch unavailable"));
        assert!(output.contains("s fix route"));
        assert!(output.contains("t test"), "{output}");
        assert!(!output.contains("enter launch"));
    }
}
