use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::{
    app::{
        state::{AppState, GatewayModelTarget, ModelChooserContext, ModelChooserState},
        App,
    },
    gateway::CachedModel,
};

pub(crate) fn chooser_gateway(state: &AppState) -> Option<&crate::gateway::Gateway> {
    let chooser = state.model_chooser.as_ref()?;
    let gateway_id = match &chooser.context {
        ModelChooserContext::GatewayDefault { gateway_id, .. } => gateway_id.as_str(),
        ModelChooserContext::CodingAgentLaunch => {
            state.coding_agent_launch.gateway_id.as_deref()?
        }
    };
    state.gateway_catalog.gateways.get(gateway_id)
}

pub(crate) fn filtered_models(state: &AppState) -> Vec<&CachedModel> {
    let Some(chooser) = state.model_chooser.as_ref() else {
        return Vec::new();
    };
    let query = chooser.query.trim().to_lowercase();
    chooser_gateway(state)
        .into_iter()
        .flat_map(|gateway| gateway.model_discovery.cached_models.iter())
        .filter(|model| !model.embedding)
        .filter(|model| {
            query.is_empty()
                || model.id.to_lowercase().contains(&query)
                || model
                    .label
                    .as_deref()
                    .is_some_and(|label| label.to_lowercase().contains(&query))
                || model
                    .provider
                    .as_deref()
                    .is_some_and(|provider| provider.to_lowercase().contains(&query))
        })
        .collect()
}

pub(crate) fn selected_model(state: &AppState) -> Option<&CachedModel> {
    let chooser = state.model_chooser.as_ref()?;
    filtered_models(state).get(chooser.selected).copied()
}

impl App {
    pub(crate) fn open_gateway_model_chooser(
        &mut self,
        gateway_id: String,
        target: GatewayModelTarget,
    ) {
        let valid = self
            .state
            .gateway_catalog
            .gateways
            .get(&gateway_id)
            .is_some_and(|gateway| {
                gateway.supports(target.protocol())
                    && gateway
                        .model_discovery
                        .cached_models
                        .iter()
                        .any(|model| model.enabled && !model.embedding)
            });
        if !valid {
            self.state.settings.gateways.notice = Some(crate::app::state::GatewayNotice {
                kind: crate::app::state::GatewayNoticeKind::Warning,
                message: "Test the connection to discover selectable models first.".into(),
            });
            return;
        }
        let current = self
            .state
            .gateway_catalog
            .gateways
            .get(&gateway_id)
            .and_then(|gateway| gateway.default_models.get(target.config_key()))
            .cloned();
        self.state.model_chooser = Some(ModelChooserState::new(
            ModelChooserContext::GatewayDefault { gateway_id, target },
        ));
        let selected = filtered_models(&self.state)
            .iter()
            .position(|model| Some(model.id.as_str()) == current.as_deref())
            .filter(|index| filtered_models(&self.state)[*index].enabled)
            .or_else(|| first_selectable_index(&self.state))
            .unwrap_or_default();
        if let Some(chooser) = self.state.model_chooser.as_mut() {
            chooser.selected = selected;
        }
    }

    pub(crate) fn open_launch_model_chooser(&mut self) {
        if chooser_models_exist(&self.state) {
            self.state.model_chooser = Some(ModelChooserState::new(
                ModelChooserContext::CodingAgentLaunch,
            ));
            self.select_current_launch_model();
        } else {
            self.state.coding_agent_launch.error = Some(
                "No selectable models are available. Test the gateway in Settings first.".into(),
            );
        }
    }

    fn select_current_launch_model(&mut self) {
        let current = self.state.coding_agent_launch.model.as_deref();
        let selected = filtered_models(&self.state)
            .iter()
            .position(|model| Some(model.id.as_str()) == current)
            .filter(|index| filtered_models(&self.state)[*index].enabled)
            .or_else(|| first_selectable_index(&self.state))
            .unwrap_or_default();
        if let Some(chooser) = self.state.model_chooser.as_mut() {
            chooser.selected = selected;
        }
    }

    pub(crate) fn insert_model_chooser_query(&mut self, text: &str) -> bool {
        let Some(chooser) = self.state.model_chooser.as_mut() else {
            return false;
        };
        chooser
            .query
            .extend(text.chars().filter(|character| !character.is_control()));
        chooser.selected = 0;
        let selected = first_selectable_index(&self.state).unwrap_or_default();
        if let Some(chooser) = self.state.model_chooser.as_mut() {
            chooser.selected = selected;
        }
        true
    }

    pub(crate) fn handle_model_chooser_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.state.model_chooser = None,
            KeyCode::Up => self.move_model_chooser_selection(-1),
            KeyCode::Down => self.move_model_chooser_selection(1),
            KeyCode::PageUp => self.move_model_chooser_selection(-5),
            KeyCode::PageDown => self.move_model_chooser_selection(5),
            KeyCode::Home => self.reset_model_chooser_selection(),
            KeyCode::End => {
                let last = filtered_models(&self.state)
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(index, model)| model.enabled.then_some(index))
                    .unwrap_or_default();
                self.set_model_chooser_selection(last);
            }
            KeyCode::Backspace => {
                if let Some(chooser) = self.state.model_chooser.as_mut() {
                    chooser.query.pop();
                }
                self.reset_model_chooser_selection();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(chooser) = self.state.model_chooser.as_mut() {
                    chooser.query.clear();
                }
                self.reset_model_chooser_selection();
            }
            KeyCode::Enter => self.accept_model_chooser_selection(),
            KeyCode::Char(character)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.insert_model_chooser_query(&character.to_string());
            }
            _ => {}
        }
    }

    pub(crate) fn handle_model_chooser_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.move_model_chooser_selection(-1);
                return;
            }
            MouseEventKind::ScrollDown => {
                self.move_model_chooser_selection(1);
                return;
            }
            MouseEventKind::Down(MouseButton::Left) => {}
            _ => return,
        }
        let Some(geometry) =
            crate::ui::model_chooser_geometry(self.state.screen_rect(), &self.state)
        else {
            return;
        };
        if let Some(index) = geometry
            .rows
            .iter()
            .position(|rect| rect_contains(*rect, mouse.column, mouse.row))
        {
            self.set_model_chooser_selection(geometry.first_visible + index);
            return;
        }
        if rect_contains(geometry.choose, mouse.column, mouse.row) {
            self.accept_model_chooser_selection();
        } else if rect_contains(geometry.cancel, mouse.column, mouse.row)
            || !rect_contains(geometry.popup, mouse.column, mouse.row)
        {
            self.state.model_chooser = None;
        }
    }

    fn move_model_chooser_selection(&mut self, delta: isize) {
        let selectable = filtered_models(&self.state)
            .iter()
            .enumerate()
            .filter_map(|(index, model)| model.enabled.then_some(index))
            .collect::<Vec<_>>();
        let Some(chooser) = self.state.model_chooser.as_mut() else {
            return;
        };
        if selectable.is_empty() {
            chooser.selected = 0;
        } else {
            let current = selectable
                .iter()
                .position(|index| *index == chooser.selected)
                .unwrap_or_default();
            let next = current
                .saturating_add_signed(delta)
                .min(selectable.len().saturating_sub(1));
            chooser.selected = selectable[next];
        }
    }

    fn set_model_chooser_selection(&mut self, selected: usize) {
        let last = filtered_models(&self.state).len().saturating_sub(1);
        if let Some(chooser) = self.state.model_chooser.as_mut() {
            chooser.selected = selected.min(last);
        }
    }

    fn reset_model_chooser_selection(&mut self) {
        let selected = first_selectable_index(&self.state).unwrap_or_default();
        if let Some(chooser) = self.state.model_chooser.as_mut() {
            chooser.selected = selected;
        }
    }

    fn accept_model_chooser_selection(&mut self) {
        let Some(model_id) = selected_model(&self.state)
            .filter(|model| model.enabled)
            .map(|model| model.id.clone())
        else {
            return;
        };
        let Some(context) = self
            .state
            .model_chooser
            .as_ref()
            .map(|chooser| chooser.context.clone())
        else {
            return;
        };
        self.state.model_chooser = None;
        match context {
            ModelChooserContext::GatewayDefault { gateway_id, target } => {
                self.save_gateway_model(&gateway_id, target, &model_id);
            }
            ModelChooserContext::CodingAgentLaunch => {
                self.state.coding_agent_launch.model = Some(model_id);
                self.state.coding_agent_launch.error = None;
            }
        }
    }
}

fn first_selectable_index(state: &AppState) -> Option<usize> {
    filtered_models(state)
        .iter()
        .position(|model| model.enabled)
}

fn chooser_models_exist(state: &AppState) -> bool {
    let Some(gateway) = state.coding_agent_launch.gateway(&state.gateway_catalog) else {
        return false;
    };
    gateway
        .model_discovery
        .cached_models
        .iter()
        .any(|model| model.enabled && !model.embedding)
}

fn rect_contains(rect: ratatui::layout::Rect, column: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && column >= rect.x
        && column < rect.right()
        && row >= rect.y
        && row < rect.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::coding_agent_launch::CodingAgentLaunchState, cli_adapter::CodingCli,
        gateway::CachedModel,
    };

    fn dense_model_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config {
                onboarding: Some(false),
                ..crate::config::Config::default()
            },
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let gateway = app
            .state
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .unwrap();
        gateway.model_discovery.cached_models = (0..60)
            .map(|index| CachedModel {
                id: format!(
                    "minds-labs/very-long-shared-coding-prefix-2026-08-reasoning-target-{index:02}"
                ),
                label: Some(format!("Reasoning Candidate {index:02}")),
                provider: Some(
                    if index % 2 == 0 {
                        "Minds Labs"
                    } else {
                        "Partner AI"
                    }
                    .into(),
                ),
                enabled: true,
                embedding: false,
                reasoning_efforts: Vec::new(),
            })
            .collect();
        gateway.default_models.insert(
            "codex".into(),
            "minds-labs/very-long-shared-coding-prefix-2026-08-reasoning-target-00".into(),
        );
        app.state.coding_agent_launch = CodingAgentLaunchState::new(&app.state.gateway_catalog);
        app.state.coding_agent_launch.cli = CodingCli::Codex;
        app.state.coding_agent_launch.gateway_id = Some("mindshub".into());
        app
    }

    #[test]
    fn sixty_similar_models_are_searchable_and_selectable_in_three_actions() {
        let mut app = dense_model_app();

        app.open_launch_model_chooser(); // 1: open
        assert_eq!(filtered_models(&app.state).len(), 60);
        app.insert_model_chooser_query("target-47"); // 2: search
        assert_eq!(filtered_models(&app.state).len(), 1);
        app.handle_model_chooser_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)); // 3: choose

        assert!(app.state.model_chooser.is_none());
        assert_eq!(
            app.state.coding_agent_launch.model.as_deref(),
            Some("minds-labs/very-long-shared-coding-prefix-2026-08-reasoning-target-47")
        );
    }

    #[test]
    fn search_covers_label_provider_and_full_id_and_empty_recovery() {
        let mut app = dense_model_app();
        app.open_launch_model_chooser();

        for (query, expected) in [
            ("candidate 12", 1),
            ("partner ai", 30),
            ("target-59", 1),
            ("does-not-exist", 0),
        ] {
            let chooser = app.state.model_chooser.as_mut().unwrap();
            chooser.query = query.into();
            chooser.selected = 0;
            assert_eq!(filtered_models(&app.state).len(), expected, "{query}");
        }
        app.handle_model_chooser_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(filtered_models(&app.state).len(), 60);
    }

    #[test]
    fn compact_mouse_selection_uses_the_same_filtered_result_and_choose_action() {
        let mut app = dense_model_app();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 64, 20);
        app.open_launch_model_chooser();
        app.insert_model_chooser_query("target-38");
        let geometry = crate::ui::model_chooser_geometry(app.state.screen_rect(), &app.state)
            .expect("compact chooser geometry");
        let row = geometry.rows[0];
        app.handle_model_chooser_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: row.x,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_model_chooser_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: geometry.choose.x,
            row: geometry.choose.y,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(
            app.state.coding_agent_launch.model.as_deref(),
            Some("minds-labs/very-long-shared-coding-prefix-2026-08-reasoning-target-38")
        );
    }
}
