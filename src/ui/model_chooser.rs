use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::{
    text::truncate_end,
    widgets::{
        action_button_row_rects, centered_popup_rect, panel_contrast_fg, render_action_button,
        render_panel_shell, ActionButtonSpec,
    },
};
use crate::app::{
    model_chooser::{filtered_models, selected_model},
    state::{AppState, GatewayModelTarget, ModelChooserContext},
};

#[derive(Debug, Clone)]
pub(crate) struct ModelChooserGeometry {
    pub(crate) popup: Rect,
    pub(crate) rows: Vec<Rect>,
    pub(crate) first_visible: usize,
    pub(crate) choose: Rect,
    pub(crate) cancel: Rect,
    search: Rect,
    count: Rect,
    details: Rect,
}

pub(crate) fn model_chooser_geometry(area: Rect, app: &AppState) -> Option<ModelChooserGeometry> {
    let chooser = app.model_chooser.as_ref()?;
    let compact = area.width < 80 || area.height < 22;
    let popup = if compact {
        area
    } else {
        centered_popup_rect(
            area,
            if area.width >= 140 {
                (area.width * 3 / 4).clamp(96, 140)
            } else {
                90
            },
            if area.height >= 38 {
                (area.height * 3 / 4).clamp(24, 34)
            } else {
                24
            },
        )?
    };
    if popup.width < 4 || popup.height < 10 {
        return None;
    }
    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    let footer_y = inner.bottom().saturating_sub(1);
    let detail_height = 5.min(inner.height.saturating_sub(8));
    let details_y = footer_y.saturating_sub(detail_height);
    let list_y = inner.y.saturating_add(5);
    let list_height = details_y.saturating_sub(list_y).saturating_sub(1);
    let row_height = if compact { 1 } else { 2 };
    let capacity = (list_height / row_height).max(1) as usize;
    let count = filtered_models(app).len();
    let selected = chooser.selected.min(count.saturating_sub(1));
    let first_visible = selected
        .saturating_add(1)
        .saturating_sub(capacity)
        .min(count.saturating_sub(capacity));
    let rows = (0..capacity.min(count.saturating_sub(first_visible)))
        .map(|index| {
            Rect::new(
                inner.x,
                list_y + index as u16 * row_height,
                inner.width,
                row_height,
            )
        })
        .collect();
    let buttons = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: if selected_model(app).is_some_and(|model| model.enabled) {
                    "choose"
                } else {
                    "unavailable"
                },
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "cancel",
            },
        ],
        2,
        footer_y.saturating_sub(inner.y),
    );
    Some(ModelChooserGeometry {
        popup,
        rows,
        first_visible,
        choose: buttons[0],
        cancel: buttons[1],
        search: Rect::new(inner.x, inner.y + 2, inner.width, 1),
        count: Rect::new(inner.x, inner.y + 3, inner.width, 1),
        details: Rect::new(inner.x, details_y, inner.width, detail_height),
    })
}

pub(super) fn render_model_chooser_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(chooser) = app.model_chooser.as_ref() else {
        return;
    };
    let Some(geometry) = model_chooser_geometry(area, app) else {
        return;
    };
    super::dim_background(frame, area);
    let Some(inner) = render_panel_shell(
        frame,
        geometry.popup,
        app.palette.accent,
        app.palette.panel_bg,
    ) else {
        return;
    };
    let title = match &chooser.context {
        ModelChooserContext::GatewayDefault {
            target: GatewayModelTarget::Codex,
            ..
        } => "choose Codex model",
        ModelChooserContext::GatewayDefault {
            target: GatewayModelTarget::Claude,
            ..
        } => "choose Claude model",
        ModelChooserContext::CodingAgentLaunch => "choose launch model",
    };
    frame.render_widget(
        Paragraph::new(format!(" {title}")).style(
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let query = if chooser.query.is_empty() {
        "type to search label, provider or model ID".to_string()
    } else {
        truncate_end(&chooser.query, inner.width.saturating_sub(11) as usize)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" search  ", Style::default().fg(app.palette.overlay1)),
            Span::styled(query, Style::default().fg(app.palette.text)),
        ]))
        .style(Style::default().bg(app.palette.surface0)),
        geometry.search,
    );

    let models = filtered_models(app);
    frame.render_widget(
        Paragraph::new(format!(
            " {} match{}  ·  ↑↓ browse  ·  ^u clear",
            models.len(),
            if models.len() == 1 { "" } else { "es" }
        ))
        .style(Style::default().fg(app.palette.overlay1)),
        geometry.count,
    );
    if models.is_empty() {
        let loading = chooser_gateway_id(app).is_some_and(|gateway_id| {
            app.settings
                .gateways
                .test_in_flight
                .as_ref()
                .is_some_and(|(_, active)| active == gateway_id)
        });
        let error = app
            .settings
            .gateways
            .notice
            .as_ref()
            .filter(|notice| notice.kind == crate::app::state::GatewayNoticeKind::Error)
            .map(|notice| notice.message.as_str());
        let message = if loading {
            " Discovering models… Results will appear when the gateway test completes."
        } else {
            error.unwrap_or(" No models match. Change the search or press Ctrl+U to clear it.")
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(if error.is_some() {
                    app.palette.red
                } else if loading {
                    app.palette.blue
                } else {
                    app.palette.yellow
                }))
                .wrap(Wrap { trim: false }),
            Rect::new(inner.x, inner.y + 5, inner.width, 2),
        );
    }
    for (row_index, row) in geometry.rows.iter().copied().enumerate() {
        let index = geometry.first_visible + row_index;
        let model = models[index];
        let selected = index == chooser.selected;
        let label = model.label.as_deref().unwrap_or(&model.id);
        let provider = model.provider.as_deref().unwrap_or("provider unknown");
        let style = if selected {
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.subtext0)
        };
        let line = format!(
            " {} {}  ·  {}{}",
            if selected { "▸" } else { " " },
            label,
            provider,
            if model.enabled {
                ""
            } else {
                "  × unavailable"
            }
        );
        frame.render_widget(
            Paragraph::new(truncate_end(&line, row.width as usize)).style(style),
            Rect::new(row.x, row.y, row.width, 1),
        );
        if row.height > 1 {
            frame.render_widget(
                Paragraph::new(format!("   {}", model.id)).style(
                    Style::default().fg(app.palette.overlay1).bg(if selected {
                        app.palette.surface0
                    } else {
                        app.palette.panel_bg
                    }),
                ),
                Rect::new(row.x, row.y + 1, row.width, 1),
            );
        }
    }

    if let Some(model) = selected_model(app) {
        let label = model.label.as_deref().unwrap_or("unlabelled model");
        let provider = model.provider.as_deref().unwrap_or("provider unknown");
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(" selected  ", Style::default().fg(app.palette.overlay1)),
                    Span::styled(
                        format!("{label} · {provider}"),
                        Style::default()
                            .fg(app.palette.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(
                    format!(" full ID  {}", model.id),
                    Style::default().fg(app.palette.text),
                )),
                Line::from(Span::styled(
                    if model.enabled {
                        " status   ✓ selectable"
                    } else {
                        " status   × unavailable"
                    },
                    Style::default().fg(if model.enabled {
                        app.palette.green
                    } else {
                        app.palette.overlay1
                    }),
                )),
            ])
            .wrap(Wrap { trim: false }),
            geometry.details,
        );
    }

    let selectable = selected_model(app).is_some_and(|model| model.enabled);
    render_action_button(
        frame,
        geometry.choose,
        Some("↵"),
        if selectable { "choose" } else { "unavailable" },
        Style::default()
            .fg(if selectable {
                panel_contrast_fg(&app.palette)
            } else {
                app.palette.overlay1
            })
            .bg(if selectable {
                app.palette.accent
            } else {
                app.palette.surface0
            })
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        geometry.cancel,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0),
    );
}

fn chooser_gateway_id(app: &AppState) -> Option<&str> {
    match &app.model_chooser.as_ref()?.context {
        ModelChooserContext::GatewayDefault { gateway_id, .. } => Some(gateway_id),
        ModelChooserContext::CodingAgentLaunch => app.coding_agent_launch.gateway_id.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::state::{ModelChooserContext, ModelChooserState},
        gateway::CachedModel,
    };

    fn chooser_state() -> AppState {
        let mut app = AppState::test_new();
        let gateway = app.gateway_catalog.gateways.get_mut("mindshub").unwrap();
        gateway.model_discovery.cached_models = (0..55)
            .map(|index| CachedModel {
                id: format!(
                    "minds-labs/shared-prefix-for-coding-models/reasoning-edition-target-{index:02}"
                ),
                label: Some(format!("Reasoning {index:02}")),
                provider: Some("Minds Labs".into()),
                enabled: true,
                embedding: false,
                reasoning_efforts: Vec::new(),
            })
            .collect();
        app.model_chooser = Some(ModelChooserState::new(
            ModelChooserContext::GatewayDefault {
                gateway_id: "mindshub".into(),
                target: GatewayModelTarget::Codex,
            },
        ));
        app
    }

    fn rendered(app: &AppState, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .expect("test terminal");
        terminal
            .draw(|frame| render_model_chooser_overlay(app, frame, frame.area()))
            .expect("render model chooser");
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|column| {
                        terminal.backend().buffer()[(column, row)]
                            .symbol()
                            .to_string()
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn chooser_has_distinct_compact_standard_and_wide_geometry() {
        let app = chooser_state();
        let compact = model_chooser_geometry(Rect::new(0, 0, 64, 20), &app).unwrap();
        let standard = model_chooser_geometry(Rect::new(0, 0, 100, 30), &app).unwrap();
        let wide = model_chooser_geometry(Rect::new(0, 0, 207, 62), &app).unwrap();

        assert_eq!(compact.popup, Rect::new(0, 0, 64, 20));
        assert!(standard.popup.width > compact.popup.width);
        assert!(wide.popup.width > standard.popup.width);
        assert!(wide.popup.height > standard.popup.height);
    }

    #[test]
    fn compact_dense_and_empty_states_keep_identity_and_recovery_visible() {
        let mut app = chooser_state();
        app.model_chooser.as_mut().unwrap().query = "target-42".into();
        let selected = rendered(&app, 64, 20);
        assert!(selected.contains("choose Codex model"));
        assert!(selected.contains("Reasoning 42"));
        assert!(selected.contains("Minds Labs"));
        assert!(selected.contains("full ID"));
        assert!(selected.contains("minds-labs/shared-prefix"));
        assert!(selected.contains("reasoning-edition-t"), "{selected}");
        assert!(selected.contains("arget-42"), "{selected}");
        assert!(selected.contains("target-42"));
        assert!(selected.contains("choose"));
        assert!(selected.contains("cancel"));

        app.model_chooser.as_mut().unwrap().query = "missing-model".into();
        let empty = rendered(&app, 64, 20);
        assert!(empty.contains("No models match"));
        assert!(empty.contains("Ctrl+U"));
    }

    #[test]
    fn sparse_loading_error_and_disabled_states_are_explicit() {
        let mut sparse = chooser_state();
        sparse
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .unwrap()
            .model_discovery
            .cached_models
            .truncate(1);
        let sparse_render = rendered(&sparse, 80, 24);
        assert!(sparse_render.contains("1 match"));
        assert!(sparse_render.contains("✓ selectable"));

        let mut disabled = chooser_state();
        disabled
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .unwrap()
            .model_discovery
            .cached_models[0]
            .enabled = false;
        disabled.model_chooser.as_mut().unwrap().query = "target-00".into();
        let disabled_render = rendered(&disabled, 80, 24);
        assert!(disabled_render.contains("× unavailable"));

        let mut loading = chooser_state();
        loading
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .unwrap()
            .model_discovery
            .cached_models
            .clear();
        loading.settings.gateways.test_in_flight = Some((7, "mindshub".into()));
        let loading_render = rendered(&loading, 80, 24);
        assert!(loading_render.contains("Discovering models"));

        loading.settings.gateways.test_in_flight = None;
        loading.settings.gateways.notice = Some(crate::app::state::GatewayNotice {
            kind: crate::app::state::GatewayNoticeKind::Error,
            message: "Model discovery failed; check the gateway route.".into(),
        });
        let error_render = rendered(&loading, 80, 24);
        assert!(error_render.contains("Model discovery failed"));
    }
}
