use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::{
    app::{
        state::{
            AppState, GatewayDetailField, GatewayModelTarget, GatewaySettingsView, SettingsSection,
            THEME_NAMES,
        },
        App, Mode,
    },
    config::{StatusIndicatorStyle, ToastDelivery},
};

#[derive(Debug, Clone, PartialEq, Eq)]
// The shared `Save` verb is semantic: these actions persist settings.
#[allow(clippy::enum_variant_names)]
pub(super) enum SettingsAction {
    SaveTheme(String),
    SaveStatusIndicators(StatusIndicatorStyle),
    SaveSound(bool),
    SaveToastDelivery(ToastDelivery),
    SaveAgentBorderLabels(bool),
    InstallRecommendedIntegrations,
    SaveDefaultGateway(String),
    SaveGatewayCredential(String),
    TestGateway(String),
    CycleGatewayModel {
        gateway_id: String,
        target: GatewayModelTarget,
        direction: i8,
    },
}

impl App {
    pub(crate) fn handle_settings_key(&mut self, key: KeyEvent) {
        let previous_section = self.state.settings.section;
        if let Some(action) = update_settings_state(&mut self.state, key) {
            match action {
                SettingsAction::SaveTheme(name) => self.save_theme(&name),
                SettingsAction::SaveStatusIndicators(style) => self.save_status_indicators(style),
                SettingsAction::SaveSound(enabled) => self.save_sound(enabled),
                SettingsAction::SaveToastDelivery(delivery) => self.save_toast_delivery(delivery),
                SettingsAction::SaveAgentBorderLabels(enabled) => {
                    self.save_agent_border_labels(enabled)
                }
                SettingsAction::InstallRecommendedIntegrations => {
                    self.install_recommended_integrations()
                }
                SettingsAction::SaveDefaultGateway(gateway_id) => {
                    self.save_default_gateway(&gateway_id)
                }
                SettingsAction::SaveGatewayCredential(gateway_id) => {
                    self.save_gateway_credential(&gateway_id)
                }
                SettingsAction::TestGateway(gateway_id) => self.start_gateway_test(&gateway_id),
                SettingsAction::CycleGatewayModel {
                    gateway_id,
                    target,
                    direction,
                } => self.cycle_gateway_model(&gateway_id, target, direction),
            }
        }
        if previous_section != SettingsSection::Integrations
            && self.state.settings.section == SettingsSection::Integrations
        {
            self.refresh_integration_recommendations();
        }
    }
}

fn normalize_theme_name(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-")
}

fn current_theme_index(theme_name: &str) -> usize {
    let normalized = normalize_theme_name(theme_name);
    THEME_NAMES
        .iter()
        .position(|name| normalize_theme_name(name) == normalized)
        .unwrap_or(0)
}

fn status_indicator_index(style: StatusIndicatorStyle) -> usize {
    match style {
        StatusIndicatorStyle::Dots => 0,
        StatusIndicatorStyle::Symbols => 1,
    }
}

fn status_indicator_for_index(idx: usize) -> StatusIndicatorStyle {
    if idx == 0 {
        StatusIndicatorStyle::Dots
    } else {
        StatusIndicatorStyle::Symbols
    }
}

fn toast_delivery_index(delivery: ToastDelivery) -> usize {
    match delivery {
        ToastDelivery::Off => 0,
        ToastDelivery::GoWild => 1,
        ToastDelivery::Terminal => 2,
        ToastDelivery::System => 3,
    }
}

fn toast_delivery_for_index(idx: usize) -> ToastDelivery {
    match idx {
        0 => ToastDelivery::Off,
        1 => ToastDelivery::GoWild,
        2 => ToastDelivery::Terminal,
        _ => ToastDelivery::System,
    }
}

fn preview_selected_theme(state: &mut AppState) {
    use crate::app::state::Palette;

    let name = THEME_NAMES[state.settings.list.selected];
    if let Some(mut palette) = Palette::from_name(name) {
        if let Some(custom) = &state.theme_runtime.custom {
            palette = palette.with_overrides(custom);
        }
        if let Some(accent) = &state.theme_runtime.legacy_accent {
            palette.accent = crate::config::parse_color(accent);
        }
        state.palette = palette;
        state.theme_name = name.to_string();
    }
}

fn cancel_settings(state: &mut AppState) {
    state.settings.gateways.secret_input.clear();
    state.settings.gateways.editing_credential = false;
    if let Some(palette) = state.settings.original_palette.take() {
        state.palette = palette;
    }
    if let Some(theme_name) = state.settings.original_theme.take() {
        state.theme_name = theme_name;
    }
    super::modal::leave_modal(state);
}

fn integrations_need_install(state: &AppState) -> bool {
    state
        .integration_recommendations
        .iter()
        .any(crate::integration::IntegrationRecommendation::needs_install)
}

fn apply_settings(state: &mut AppState) -> Option<SettingsAction> {
    match state.settings.section {
        SettingsSection::Theme => {
            let theme_name = state.theme_name.clone();
            state.settings.original_palette = None;
            state.settings.original_theme = None;
            super::modal::leave_modal(state);
            Some(SettingsAction::SaveTheme(theme_name))
        }
        SettingsSection::Integrations if integrations_need_install(state) => {
            Some(SettingsAction::InstallRecommendedIntegrations)
        }
        SettingsSection::Integrations => None,
        _ => {
            super::modal::leave_modal(state);
            None
        }
    }
}

fn selected_gateway_id(state: &AppState) -> Option<String> {
    state
        .gateway_catalog
        .gateways
        .keys()
        .nth(state.settings.gateways.selected_gateway)
        .cloned()
}

fn update_gateway_settings(state: &mut AppState, key: KeyEvent) -> Option<SettingsAction> {
    if state.settings.gateways.editing_credential {
        match key.code {
            KeyCode::Enter => {
                let gateway_id = state.settings.gateways.detail_gateway_id.clone()?;
                return Some(SettingsAction::SaveGatewayCredential(gateway_id));
            }
            KeyCode::Esc => {
                state.settings.gateways.secret_input.clear();
                state.settings.gateways.editing_credential = false;
            }
            KeyCode::Backspace => state.settings.gateways.secret_input.backspace(),
            KeyCode::Char(character)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                let mut encoded = [0; 4];
                state
                    .settings
                    .gateways
                    .secret_input
                    .insert(character.encode_utf8(&mut encoded));
            }
            _ => {}
        }
        return None;
    }

    match state.settings.gateways.view {
        GatewaySettingsView::List => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                state.settings.gateways.selected_gateway =
                    state.settings.gateways.selected_gateway.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = state.gateway_catalog.gateways.len();
                if count > 0 {
                    state.settings.gateways.selected_gateway =
                        (state.settings.gateways.selected_gateway + 1).min(count - 1);
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(gateway_id) = selected_gateway_id(state) {
                    state.settings.gateways.detail_gateway_id = Some(gateway_id);
                    state.settings.gateways.detail_field = GatewayDetailField::Credential;
                    state.settings.gateways.view = GatewaySettingsView::Detail;
                    state.settings.gateways.notice = None;
                }
            }
            KeyCode::Char(' ') => {
                return selected_gateway_id(state).map(SettingsAction::SaveDefaultGateway);
            }
            KeyCode::Char('t') | KeyCode::Char('r') => {
                return selected_gateway_id(state).map(SettingsAction::TestGateway);
            }
            KeyCode::Tab => {
                state.settings.section = SettingsSection::Theme;
                state.settings.list.selected = current_theme_index(&state.theme_name);
            }
            KeyCode::BackTab => {
                state.settings.section = SettingsSection::Integrations;
                state.settings.list.selected = 0;
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        GatewaySettingsView::Detail => match key.code {
            KeyCode::Esc | KeyCode::Char('h') => {
                state.settings.gateways.secret_input.clear();
                state.settings.gateways.view = GatewaySettingsView::List;
                state.settings.gateways.notice = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let current = GatewayDetailField::ALL
                    .iter()
                    .position(|field| *field == state.settings.gateways.detail_field)
                    .unwrap_or(0);
                state.settings.gateways.detail_field =
                    GatewayDetailField::ALL[current.saturating_sub(1)];
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let current = GatewayDetailField::ALL
                    .iter()
                    .position(|field| *field == state.settings.gateways.detail_field)
                    .unwrap_or(0);
                state.settings.gateways.detail_field =
                    GatewayDetailField::ALL[(current + 1).min(GatewayDetailField::ALL.len() - 1)];
            }
            KeyCode::Enter
                if state.settings.gateways.detail_field == GatewayDetailField::Credential =>
            {
                state.settings.gateways.secret_input.clear();
                state.settings.gateways.editing_credential = true;
                state.settings.gateways.notice = None;
            }
            KeyCode::Left | KeyCode::Right
                if state.settings.gateways.detail_field != GatewayDetailField::Credential =>
            {
                let gateway_id = state.settings.gateways.detail_gateway_id.clone()?;
                let target =
                    if state.settings.gateways.detail_field == GatewayDetailField::CodexModel {
                        GatewayModelTarget::Codex
                    } else {
                        GatewayModelTarget::Claude
                    };
                let direction = if key.code == KeyCode::Left { -1 } else { 1 };
                return Some(SettingsAction::CycleGatewayModel {
                    gateway_id,
                    target,
                    direction,
                });
            }
            KeyCode::Char('t') | KeyCode::Char('r') => {
                return state
                    .settings
                    .gateways
                    .detail_gateway_id
                    .clone()
                    .map(SettingsAction::TestGateway);
            }
            KeyCode::Char(' ') => {
                return state
                    .settings
                    .gateways
                    .detail_gateway_id
                    .clone()
                    .map(SettingsAction::SaveDefaultGateway);
            }
            KeyCode::Tab => {
                state.settings.gateways.secret_input.clear();
                state.settings.gateways.editing_credential = false;
                state.settings.gateways.view = GatewaySettingsView::List;
                state.settings.section = SettingsSection::Theme;
                state.settings.list.selected = current_theme_index(&state.theme_name);
            }
            KeyCode::BackTab => {
                state.settings.gateways.secret_input.clear();
                state.settings.gateways.editing_credential = false;
                state.settings.gateways.view = GatewaySettingsView::List;
                state.settings.section = SettingsSection::Integrations;
                state.settings.list.selected = 0;
            }
            _ => {}
        },
    }
    None
}

pub(super) fn update_settings_state(state: &mut AppState, key: KeyEvent) -> Option<SettingsAction> {
    match state.settings.section {
        SettingsSection::Gateways => return update_gateway_settings(state, key),
        SettingsSection::Theme => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                let previous = state.settings.list.selected;
                state.settings.list.move_prev();
                if state.settings.list.selected != previous {
                    preview_selected_theme(state);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let previous = state.settings.list.selected;
                state.settings.list.move_next(THEME_NAMES.len());
                if state.settings.list.selected != previous {
                    preview_selected_theme(state);
                }
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Indicators;
                state.settings.list.selected = status_indicator_index(state.status_indicators);
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Gateways;
                state.settings.list.selected = 0;
            }
            _ => match super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS) {
                Some(super::modal::ModalAction::Apply) => return apply_settings(state),
                Some(super::modal::ModalAction::Close) => cancel_settings(state),
                _ => {}
            },
        },
        SettingsSection::Indicators => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.selected = 1 - state.settings.list.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let style = status_indicator_for_index(state.settings.list.selected);
                return Some(SettingsAction::SaveStatusIndicators(style));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Theme;
                state.settings.list.selected = current_theme_index(&state.theme_name);
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Sound;
                state.settings.list.selected = usize::from(!state.sound_enabled());
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Sound => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.selected = 1 - state.settings.list.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let enabled = state.settings.list.selected == 0;
                return Some(SettingsAction::SaveSound(enabled));
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Toast;
                state.settings.list.selected = toast_delivery_index(state.toast_delivery());
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Indicators;
                state.settings.list.selected = status_indicator_index(state.status_indicators);
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Toast => match key.code {
            KeyCode::Up | KeyCode::Char('k') => state.settings.list.move_prev(),
            KeyCode::Down | KeyCode::Char('j') => state.settings.list.move_next(4),
            KeyCode::Enter | KeyCode::Char(' ') => {
                let delivery = toast_delivery_for_index(state.settings.list.selected);
                return Some(SettingsAction::SaveToastDelivery(delivery));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Sound;
                state.settings.list.selected = usize::from(!state.sound_enabled());
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::PaneLabels;
                state.settings.list.selected = usize::from(!state.agent_border_labels_enabled());
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::PaneLabels => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.selected = 1 - state.settings.list.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let enabled = state.settings.list.selected == 0;
                return Some(SettingsAction::SaveAgentBorderLabels(enabled));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Toast;
                state.settings.list.selected = toast_delivery_index(state.toast_delivery());
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Integrations;
                state.settings.list.selected = 0;
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Integrations => match key.code {
            KeyCode::Enter | KeyCode::Char(' ') if integrations_need_install(state) => {
                return Some(SettingsAction::InstallRecommendedIntegrations);
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::PaneLabels;
                state.settings.list.selected = usize::from(!state.agent_border_labels_enabled());
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Gateways;
                state.settings.list.selected = 0;
            }
            _ => match super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS) {
                Some(super::modal::ModalAction::Apply) => return apply_settings(state),
                Some(super::modal::ModalAction::Close) => cancel_settings(state),
                _ => {}
            },
        },
    }

    None
}

pub(crate) fn open_settings(state: &mut AppState) {
    open_settings_at(state, SettingsSection::Gateways);
}

pub(crate) fn open_settings_at(state: &mut AppState, section: SettingsSection) {
    state.integration_install_messages.clear();
    state.settings.original_palette = Some(state.palette.clone());
    state.settings.original_theme = Some(state.theme_name.clone());
    state.settings.section = section;
    state.settings.gateways.secret_input.clear();
    state.settings.gateways.editing_credential = false;
    state.settings.gateways.view = GatewaySettingsView::List;
    state.settings.gateways.notice = None;
    state.settings.list.selected = match section {
        SettingsSection::Gateways => 0,
        SettingsSection::Theme => current_theme_index(&state.theme_name),
        SettingsSection::Indicators => status_indicator_index(state.status_indicators),
        SettingsSection::Sound => usize::from(!state.sound_enabled()),
        SettingsSection::Toast => toast_delivery_index(state.toast_delivery()),
        SettingsSection::PaneLabels => usize::from(!state.agent_border_labels_enabled()),
        SettingsSection::Integrations => 0,
    };
    state.mode = Mode::Settings;
}

impl AppState {
    fn settings_popup_rect(&self) -> Rect {
        crate::ui::centered_popup_rect(
            self.screen_rect(),
            crate::ui::SETTINGS_POPUP_WIDTH,
            crate::ui::settings_popup_height(self),
        )
        .unwrap_or_default()
    }

    fn settings_inner_rect(&self) -> Rect {
        let popup = self.settings_popup_rect();
        Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    }

    fn settings_tab_at(&self, col: u16, row: u16) -> Option<SettingsSection> {
        let inner = self.settings_inner_rect();
        let tab_y = inner.y + 1;
        if row != tab_y {
            return None;
        }
        let mut x = inner.x;
        for section in SettingsSection::ALL {
            let badge_width = if self.settings_section_has_badge(*section) {
                2
            } else {
                0
            };
            let width = section.tab_label(inner.width).len() as u16 + 2 + badge_width;
            if col >= x && col < x + width {
                return Some(*section);
            }
            x += width + 1;
        }
        None
    }

    pub(crate) fn settings_content_rect(&self) -> Rect {
        let inner = self.settings_inner_rect();
        crate::ui::modal_stack_areas(inner, 3, 2, 0, 1).content
    }

    fn settings_list_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.settings_content_rect();
        if row < area.y || row >= area.y + area.height || col < area.x || col >= area.x + area.width
        {
            return None;
        }

        match self.settings.section {
            SettingsSection::Gateways => match self.settings.gateways.view {
                GatewaySettingsView::List => {
                    let first_row = area.y + 3;
                    if row < first_row {
                        return None;
                    }
                    let relative = row - first_row;
                    if relative % 3 >= 2 {
                        return None;
                    }
                    let idx = (relative / 3) as usize;
                    (idx < self.gateway_catalog.gateways.len()).then_some(idx)
                }
                GatewaySettingsView::Detail => {
                    let first_row = area.y + 3;
                    if row >= first_row && row < first_row + GatewayDetailField::ALL.len() as u16 {
                        Some((row - first_row) as usize)
                    } else {
                        None
                    }
                }
            },
            SettingsSection::Theme => {
                let max_visible = area.height as usize;
                let scroll = if self.settings.list.selected >= max_visible {
                    self.settings.list.selected - max_visible + 1
                } else {
                    0
                };
                let idx = scroll + (row - area.y) as usize;
                (idx < THEME_NAMES.len()).then_some(idx)
            }
            SettingsSection::Indicators | SettingsSection::Sound => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 2 {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Toast => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 8 {
                    Some(((row - list_y) / 2) as usize)
                } else {
                    None
                }
            }
            SettingsSection::PaneLabels => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 2 {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Integrations => None,
        }
    }

    pub(super) fn handle_settings_mouse(&mut self, mouse: MouseEvent) -> Option<SettingsAction> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(section) = self.settings_tab_at(mouse.column, mouse.row) {
                    if section != SettingsSection::Gateways {
                        self.settings.gateways.secret_input.clear();
                        self.settings.gateways.editing_credential = false;
                        self.settings.gateways.view = GatewaySettingsView::List;
                    }
                    self.settings.section = section;
                    self.settings.list.select(match section {
                        SettingsSection::Gateways => 0,
                        SettingsSection::Theme => current_theme_index(&self.theme_name),
                        SettingsSection::Indicators => {
                            status_indicator_index(self.status_indicators)
                        }
                        SettingsSection::Sound => usize::from(!self.sound_enabled()),
                        SettingsSection::Toast => toast_delivery_index(self.toast_delivery()),
                        SettingsSection::PaneLabels => {
                            usize::from(!self.agent_border_labels_enabled())
                        }
                        SettingsSection::Integrations => 0,
                    });
                    return None;
                }
                if let Some(idx) = self.settings_list_index_at(mouse.column, mouse.row) {
                    if self.settings.section == SettingsSection::Gateways {
                        match self.settings.gateways.view {
                            GatewaySettingsView::List => {
                                self.settings.gateways.selected_gateway = idx;
                                if let Some(gateway_id) = selected_gateway_id(self) {
                                    self.settings.gateways.detail_gateway_id = Some(gateway_id);
                                    self.settings.gateways.detail_field =
                                        GatewayDetailField::Credential;
                                    self.settings.gateways.view = GatewaySettingsView::Detail;
                                    self.settings.gateways.notice = None;
                                }
                            }
                            GatewaySettingsView::Detail => {
                                self.settings.gateways.detail_field = GatewayDetailField::ALL[idx];
                                match self.settings.gateways.detail_field {
                                    GatewayDetailField::Credential => {
                                        self.settings.gateways.secret_input.clear();
                                        self.settings.gateways.editing_credential = true;
                                        self.settings.gateways.notice = None;
                                    }
                                    GatewayDetailField::CodexModel
                                    | GatewayDetailField::ClaudeModel => {
                                        let target = if self.settings.gateways.detail_field
                                            == GatewayDetailField::CodexModel
                                        {
                                            GatewayModelTarget::Codex
                                        } else {
                                            GatewayModelTarget::Claude
                                        };
                                        if let Some(gateway_id) =
                                            self.settings.gateways.detail_gateway_id.clone()
                                        {
                                            return Some(SettingsAction::CycleGatewayModel {
                                                gateway_id,
                                                target,
                                                direction: 1,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        return None;
                    }
                    self.settings.list.select(idx);
                    return match self.settings.section {
                        SettingsSection::Gateways => None,
                        SettingsSection::Theme => {
                            preview_selected_theme(self);
                            None
                        }
                        SettingsSection::Indicators => Some(SettingsAction::SaveStatusIndicators(
                            status_indicator_for_index(idx),
                        )),
                        SettingsSection::Sound => {
                            let enabled = idx == 0;
                            Some(SettingsAction::SaveSound(enabled))
                        }
                        SettingsSection::Toast => {
                            let delivery = toast_delivery_for_index(idx);
                            Some(SettingsAction::SaveToastDelivery(delivery))
                        }
                        SettingsSection::PaneLabels => {
                            let enabled = idx == 0;
                            Some(SettingsAction::SaveAgentBorderLabels(enabled))
                        }
                        SettingsSection::Integrations => None,
                    };
                }

                let inner = self.settings_inner_rect();
                let show_primary = crate::ui::settings_show_primary_action(self);
                let (apply, close) = crate::ui::settings_button_rects(inner, self, show_primary);
                let mut buttons = vec![(close, super::modal::ModalAction::Close)];
                if let Some(apply) = apply {
                    buttons.insert(0, (apply, super::modal::ModalAction::Apply));
                }
                match super::modal::modal_action_from_buttons(mouse.column, mouse.row, &buttons) {
                    Some(super::modal::ModalAction::Apply)
                        if self.settings.section == SettingsSection::Gateways =>
                    {
                        if self.settings.gateways.editing_credential {
                            return self
                                .settings
                                .gateways
                                .detail_gateway_id
                                .clone()
                                .map(SettingsAction::SaveGatewayCredential);
                        }
                        let gateway_id = match self.settings.gateways.view {
                            GatewaySettingsView::List => selected_gateway_id(self),
                            GatewaySettingsView::Detail => {
                                self.settings.gateways.detail_gateway_id.clone()
                            }
                        };
                        gateway_id.map(SettingsAction::TestGateway)
                    }
                    Some(super::modal::ModalAction::Apply) => apply_settings(self),
                    Some(super::modal::ModalAction::Close) => {
                        if self.settings.section == SettingsSection::Gateways
                            && self.settings.gateways.editing_credential
                        {
                            self.settings.gateways.secret_input.clear();
                            self.settings.gateways.editing_credential = false;
                        } else if self.settings.section == SettingsSection::Gateways
                            && self.settings.gateways.view == GatewaySettingsView::Detail
                        {
                            self.settings.gateways.secret_input.clear();
                            self.settings.gateways.editing_credential = false;
                            self.settings.gateways.view = GatewaySettingsView::List;
                            self.settings.gateways.notice = None;
                        } else {
                            cancel_settings(self);
                        }
                        None
                    }
                    _ => {
                        cancel_settings(self);
                        None
                    }
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};

    use super::super::{app_for_mouse_test, mouse, state_with_workspaces};
    use super::*;

    #[test]
    fn settings_cancel_restores_previewed_theme_from_other_sections() {
        let mut state = state_with_workspaces(&["test"]);
        let original_palette = state.palette.clone();
        let original_theme = state.theme_name.clone();

        open_settings_at(&mut state, SettingsSection::Theme);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_ne!(state.theme_name, original_theme);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(
            state.settings.section,
            crate::app::state::SettingsSection::Indicators
        );

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
        assert_eq!(state.theme_name, original_theme);
        assert_eq!(state.palette.accent, original_palette.accent);
        assert_eq!(state.palette.panel_bg, original_palette.panel_bg);
    }

    #[test]
    fn settings_indicator_choice_returns_save_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Indicators);
        state.settings.list.selected = 1;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveStatusIndicators(
                StatusIndicatorStyle::Symbols
            ))
        );
        assert_eq!(state.status_indicators, StatusIndicatorStyle::Dots);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_sound_toggle_returns_save_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        state.settings.section = crate::app::state::SettingsSection::Sound;
        state.settings.list.selected = 0;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(action, Some(SettingsAction::SaveSound(true)));
        assert!(!state.sound.enabled);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_tab_cycle_wraps_after_integrations() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::PaneLabels);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Integrations);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Gateways);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Theme);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Gateways);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Integrations);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::PaneLabels);
    }

    #[test]
    fn integrations_enter_does_nothing_when_nothing_needs_install() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Integrations);

        let enter_action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(enter_action, None);

        let space_action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty()),
        );
        assert_eq!(space_action, None);
    }

    #[test]
    fn settings_hover_does_not_change_selection() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);
        app.state.settings.list.select(0);

        let area = app.state.settings_content_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, area.x + 2, area.y + 2));

        assert_eq!(app.state.settings.list.selected, 0);
    }

    #[test]
    fn gateway_credential_paste_is_redacted_and_cleared_on_cancel() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);
        app.state.settings.gateways.view = GatewaySettingsView::Detail;
        app.state.settings.gateways.detail_gateway_id = Some("mindshub".into());
        app.state.settings.gateways.editing_credential = true;

        assert!(app.paste_into_active_text_input("TOP_SECRET_GATEWAY_KEY\n"));
        assert_eq!(
            app.state.settings.gateways.secret_input.expose(),
            "TOP_SECRET_GATEWAY_KEY"
        );
        assert!(!format!("{:?}", app.state.settings.gateways.secret_input)
            .contains("TOP_SECRET_GATEWAY_KEY"));

        update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );
        assert!(app.state.settings.gateways.secret_input.is_empty());
        assert!(!app.state.settings.gateways.editing_credential);
    }

    #[test]
    fn gateway_detail_left_cycles_a_model_without_leaving_detail() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        state.settings.gateways.view = GatewaySettingsView::Detail;
        state.settings.gateways.detail_gateway_id = Some("mindshub".into());
        state.settings.gateways.detail_field = GatewayDetailField::CodexModel;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Left, KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::CycleGatewayModel {
                gateway_id: "mindshub".into(),
                target: GatewayModelTarget::Codex,
                direction: -1,
            })
        );
        assert_eq!(state.settings.gateways.view, GatewaySettingsView::Detail);
    }

    #[test]
    fn gateway_rows_support_mouse_configuration_and_model_selection() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);
        let area = app.state.settings_content_rect();

        let action = app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 2,
            area.y + 3,
        ));
        assert_eq!(action, None);
        assert_eq!(
            app.state.settings.gateways.view,
            GatewaySettingsView::Detail
        );
        assert_eq!(
            app.state.settings.gateways.detail_gateway_id.as_deref(),
            Some("mindshub")
        );

        let action = app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 2,
            area.y + 4,
        ));
        assert_eq!(
            action,
            Some(SettingsAction::CycleGatewayModel {
                gateway_id: "mindshub".into(),
                target: GatewayModelTarget::Codex,
                direction: 1,
            })
        );
    }

    #[test]
    fn integration_update_badge_only_tracks_outdated_recommendations() {
        let mut state = state_with_workspaces(&["test"]);
        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::NotInstalled,
            true,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::NotInstalled,
            false,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Current,
            true,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Outdated,
            true,
        )];
        assert!(state.integration_updates_available());
    }

    #[test]
    fn settings_tab_hit_area_includes_integration_update_badge() {
        let mut state = state_with_workspaces(&["test"]);
        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Outdated,
            true,
        )];
        open_settings(&mut state);

        let inner = state.settings_inner_rect();
        let tab_y = inner.y + 1;
        let integrations_idx = SettingsSection::ALL
            .iter()
            .position(|section| *section == SettingsSection::Integrations)
            .expect("integrations section should be present");
        let integrations_x = inner.x
            + SettingsSection::ALL[..integrations_idx]
                .iter()
                .map(|section| {
                    let badge_width = if state.settings_section_has_badge(*section) {
                        2
                    } else {
                        0
                    };
                    section.tab_label(inner.width).len() as u16 + 3 + badge_width
                })
                .sum::<u16>();
        let dotted_width = SettingsSection::Integrations.tab_label(inner.width).len() as u16 + 4;

        assert_eq!(
            state.settings_tab_at(integrations_x + dotted_width - 1, tab_y),
            Some(SettingsSection::Integrations)
        );
    }

    fn integration_recommendation(
        state: crate::integration::IntegrationStatusKind,
        available: bool,
    ) -> crate::integration::IntegrationRecommendation {
        crate::integration::IntegrationRecommendation {
            target: crate::api::schema::IntegrationTarget::Claude,
            label: "claude",
            command: "claude",
            available,
            path: std::path::PathBuf::from("/tmp/gowild-test-integration"),
            state,
        }
    }
}
