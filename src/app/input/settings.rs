use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::{
    app::{
        state::{
            AppState, CustomGatewayFormMode, CustomGatewayFormState, GatewayDetailField,
            GatewayFormField, GatewayModelTarget, GatewaySettingsView, SettingsSection,
            THEME_NAMES,
        },
        App, Mode,
    },
    config::{StatusIndicatorStyle, ToastDelivery},
    gateway::CredentialRemoval,
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
    AddCustomGateway(Box<crate::gateway::Gateway>),
    UpdateCustomGateway {
        gateway_id: String,
        gateway: Box<crate::gateway::Gateway>,
    },
    DeleteCustomGateway {
        gateway_id: String,
        credential_removal: CredentialRemoval,
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
                SettingsAction::AddCustomGateway(gateway) => {
                    let gateway_id = gateway.id.clone();
                    if self.add_custom_gateway(*gateway) {
                        finish_gateway_form(&mut self.state, &gateway_id);
                    }
                }
                SettingsAction::UpdateCustomGateway {
                    gateway_id,
                    gateway,
                } => {
                    if self.update_custom_gateway(&gateway_id, *gateway) {
                        finish_gateway_form(&mut self.state, &gateway_id);
                    }
                }
                SettingsAction::DeleteCustomGateway {
                    gateway_id,
                    credential_removal,
                } => {
                    if self.delete_custom_gateway(&gateway_id, credential_removal) {
                        self.state.settings.gateways.credential_removal = CredentialRemoval::Keep;
                    }
                }
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
    state.settings.gateways.gateway_form = None;
    state.settings.gateways.credential_removal = CredentialRemoval::Keep;
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

fn begin_gateway_add(state: &mut AppState) {
    state.settings.gateways.secret_input.clear();
    state.settings.gateways.editing_credential = false;
    state.settings.gateways.gateway_form = Some(CustomGatewayFormState::add());
    state.settings.gateways.view = GatewaySettingsView::Form;
    state.settings.gateways.notice = None;
}

fn begin_gateway_edit(state: &mut AppState) {
    let Some(gateway_id) = state.settings.gateways.detail_gateway_id.as_deref() else {
        return;
    };
    let Some(gateway) = state.gateway_catalog.gateways.get(gateway_id) else {
        return;
    };
    if gateway.preset.is_some() {
        state.settings.gateways.notice = Some(crate::app::state::GatewayNotice {
            kind: crate::app::state::GatewayNoticeKind::Warning,
            message: "Built-in presets are fixed. Duplicate this gateway to customize it.".into(),
        });
        return;
    }
    state.settings.gateways.gateway_form = Some(CustomGatewayFormState::edit(gateway));
    state.settings.gateways.view = GatewaySettingsView::Form;
    state.settings.gateways.notice = None;
}

fn begin_gateway_duplicate(state: &mut AppState) {
    let Some(gateway_id) = state.settings.gateways.detail_gateway_id.as_deref() else {
        return;
    };
    let Some(gateway) = state.gateway_catalog.gateways.get(gateway_id) else {
        return;
    };
    state.settings.gateways.gateway_form = Some(CustomGatewayFormState::duplicate(gateway));
    state.settings.gateways.view = GatewaySettingsView::Form;
    state.settings.gateways.notice = None;
}

fn begin_gateway_delete(state: &mut AppState) {
    let Some(gateway_id) = state.settings.gateways.detail_gateway_id.as_deref() else {
        return;
    };
    let Some(gateway) = state.gateway_catalog.gateways.get(gateway_id) else {
        return;
    };
    if gateway.preset.is_some() {
        state.settings.gateways.notice = Some(crate::app::state::GatewayNotice {
            kind: crate::app::state::GatewayNoticeKind::Warning,
            message: "Built-in gateway presets cannot be deleted.".into(),
        });
        return;
    }
    state.settings.gateways.secret_input.clear();
    state.settings.gateways.editing_credential = false;
    state.settings.gateways.gateway_form = None;
    state.settings.gateways.credential_removal = CredentialRemoval::Keep;
    state.settings.gateways.view = GatewaySettingsView::DeleteConfirm;
    state.settings.gateways.notice = None;
}

fn cancel_gateway_delete(state: &mut AppState) {
    state.settings.gateways.credential_removal = CredentialRemoval::Keep;
    state.settings.gateways.view = GatewaySettingsView::Detail;
    state.settings.gateways.notice = None;
}

fn gateway_delete_action(state: &AppState) -> Option<SettingsAction> {
    let gateway_id = state.settings.gateways.detail_gateway_id.as_ref()?;
    let gateway = state.gateway_catalog.gateways.get(gateway_id)?;
    (gateway.preset.is_none()).then(|| SettingsAction::DeleteCustomGateway {
        gateway_id: gateway_id.clone(),
        credential_removal: state.settings.gateways.credential_removal,
    })
}

fn cancel_gateway_form(state: &mut AppState) {
    let return_to_detail = state
        .settings
        .gateways
        .gateway_form
        .as_ref()
        .is_some_and(|form| form.mode != CustomGatewayFormMode::Add);
    state.settings.gateways.gateway_form = None;
    state.settings.gateways.credential_removal = CredentialRemoval::Keep;
    state.settings.gateways.notice = None;
    state.settings.gateways.view = if return_to_detail {
        GatewaySettingsView::Detail
    } else {
        GatewaySettingsView::List
    };
}

fn gateway_form_action(state: &AppState) -> Option<SettingsAction> {
    let form = state.settings.gateways.gateway_form.as_ref()?;
    let gateway = form.gateway();
    match form.original_gateway_id() {
        Some(gateway_id) => Some(SettingsAction::UpdateCustomGateway {
            gateway_id: gateway_id.to_string(),
            gateway: Box::new(gateway),
        }),
        None => Some(SettingsAction::AddCustomGateway(Box::new(gateway))),
    }
}

pub(super) fn finish_gateway_form(state: &mut AppState, gateway_id: &str) {
    state.settings.gateways.gateway_form = None;
    state.settings.gateways.detail_gateway_id = Some(gateway_id.to_string());
    state.settings.gateways.detail_field = GatewayDetailField::Credential;
    state.settings.gateways.view = GatewaySettingsView::Detail;
    state.settings.gateways.selected_gateway = state
        .gateway_catalog
        .gateways
        .keys()
        .position(|id| id == gateway_id)
        .unwrap_or_default();
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
            KeyCode::Char('a') => begin_gateway_add(state),
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
                if state.settings.gateways.detail_field == GatewayDetailField::Credential
                    && state
                        .settings
                        .gateways
                        .detail_gateway_id
                        .as_ref()
                        .and_then(|id| state.gateway_catalog.gateways.get(id))
                        .is_some_and(|gateway| {
                            gateway.auth.mode != crate::gateway::AuthenticationMode::None
                        }) =>
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
            KeyCode::Char('e') => begin_gateway_edit(state),
            KeyCode::Char('d') => begin_gateway_duplicate(state),
            KeyCode::Char('x') => begin_gateway_delete(state),
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
        GatewaySettingsView::Form => match key.code {
            KeyCode::Esc => cancel_gateway_form(state),
            KeyCode::Up | KeyCode::BackTab => {
                if let Some(form) = state.settings.gateways.gateway_form.as_mut() {
                    form.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Tab => {
                if let Some(form) = state.settings.gateways.gateway_form.as_mut() {
                    form.move_selection(1);
                }
            }
            KeyCode::Left | KeyCode::Right => {
                if let Some(form) = state.settings.gateways.gateway_form.as_mut() {
                    if form.selected_field == GatewayFormField::Authentication {
                        form.cycle_authentication(if key.code == KeyCode::Left { -1 } else { 1 });
                    }
                }
            }
            KeyCode::Enter => return gateway_form_action(state),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(form) = state.settings.gateways.gateway_form.as_mut() {
                    form.clear_selected();
                }
            }
            KeyCode::Backspace => {
                if let Some(form) = state.settings.gateways.gateway_form.as_mut() {
                    form.backspace();
                }
            }
            KeyCode::Char(character)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                if let Some(form) = state.settings.gateways.gateway_form.as_mut() {
                    let mut encoded = [0; 4];
                    form.insert(character.encode_utf8(&mut encoded));
                }
            }
            _ => {}
        },
        GatewaySettingsView::DeleteConfirm => match key.code {
            KeyCode::Esc | KeyCode::Char('h') => cancel_gateway_delete(state),
            KeyCode::Left | KeyCode::Char('k') => {
                state.settings.gateways.credential_removal = CredentialRemoval::Keep;
            }
            KeyCode::Right | KeyCode::Char('d') => {
                state.settings.gateways.credential_removal = CredentialRemoval::Delete;
            }
            KeyCode::Enter => return gateway_delete_action(state),
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
    state.settings.gateways.gateway_form = None;
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

fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
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
                GatewaySettingsView::Form => {
                    let first_offset = if area.height < 12 { 1 } else { 2 };
                    let visible_rows = area.height.saturating_sub(first_offset + 2) as usize;
                    let selected_index = self
                        .settings
                        .gateways
                        .gateway_form
                        .as_ref()
                        .and_then(|form| {
                            GatewayFormField::ALL
                                .iter()
                                .position(|field| *field == form.selected_field)
                        })
                        .unwrap_or_default();
                    let scroll = selected_index
                        .saturating_add(1)
                        .saturating_sub(visible_rows);
                    let first_row = area.y + first_offset;
                    if row < first_row || row >= first_row + visible_rows as u16 {
                        return None;
                    }
                    let idx = scroll + (row - first_row) as usize;
                    (idx < GatewayFormField::ALL.len()).then_some(idx)
                }
                GatewaySettingsView::DeleteConfirm => None,
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
                        self.settings.gateways.gateway_form = None;
                        self.settings.gateways.credential_removal = CredentialRemoval::Keep;
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
                let gateway_area = self.settings_content_rect();
                if self.settings.section == SettingsSection::Gateways
                    && self.settings.gateways.view == GatewaySettingsView::List
                    && rect_contains(
                        crate::ui::gateway_add_button_rect(gateway_area),
                        mouse.column,
                        mouse.row,
                    )
                {
                    begin_gateway_add(self);
                    return None;
                }
                if self.settings.section == SettingsSection::Gateways
                    && self.settings.gateways.view == GatewaySettingsView::Detail
                {
                    let custom = self
                        .settings
                        .gateways
                        .detail_gateway_id
                        .as_ref()
                        .and_then(|id| self.gateway_catalog.gateways.get(id))
                        .is_some_and(|gateway| gateway.preset.is_none());
                    let (edit, duplicate) =
                        crate::ui::gateway_detail_button_rects(gateway_area, custom);
                    if edit.is_some_and(|rect| rect_contains(rect, mouse.column, mouse.row)) {
                        begin_gateway_edit(self);
                        return None;
                    }
                    if rect_contains(duplicate, mouse.column, mouse.row) {
                        begin_gateway_duplicate(self);
                        return None;
                    }
                    if crate::ui::gateway_detail_delete_button_rect(gateway_area, custom)
                        .is_some_and(|rect| rect_contains(rect, mouse.column, mouse.row))
                    {
                        begin_gateway_delete(self);
                        return None;
                    }
                }
                if self.settings.section == SettingsSection::Gateways
                    && self.settings.gateways.view == GatewaySettingsView::DeleteConfirm
                {
                    let keep_row = gateway_area.y.saturating_add(5);
                    let delete_row = gateway_area.y.saturating_add(7);
                    if mouse.row == keep_row
                        && mouse.column >= gateway_area.x
                        && mouse.column < gateway_area.x.saturating_add(gateway_area.width)
                    {
                        self.settings.gateways.credential_removal = CredentialRemoval::Keep;
                        return None;
                    }
                    if mouse.row == delete_row
                        && mouse.column >= gateway_area.x
                        && mouse.column < gateway_area.x.saturating_add(gateway_area.width)
                    {
                        self.settings.gateways.credential_removal = CredentialRemoval::Delete;
                        return None;
                    }
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
                                        let needs_credential = self
                                            .settings
                                            .gateways
                                            .detail_gateway_id
                                            .as_ref()
                                            .and_then(|id| self.gateway_catalog.gateways.get(id))
                                            .is_some_and(|gateway| {
                                                gateway.auth.mode
                                                    != crate::gateway::AuthenticationMode::None
                                            });
                                        if needs_credential {
                                            self.settings.gateways.secret_input.clear();
                                            self.settings.gateways.editing_credential = true;
                                            self.settings.gateways.notice = None;
                                        }
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
                            GatewaySettingsView::Form => {
                                if let Some(form) = self.settings.gateways.gateway_form.as_mut() {
                                    form.selected_field = GatewayFormField::ALL[idx];
                                    if form.selected_field == GatewayFormField::Authentication {
                                        form.cycle_authentication(1);
                                    }
                                }
                            }
                            GatewaySettingsView::DeleteConfirm => {}
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
                        if self.settings.gateways.view == GatewaySettingsView::Form {
                            return gateway_form_action(self);
                        }
                        if self.settings.gateways.view == GatewaySettingsView::DeleteConfirm {
                            return gateway_delete_action(self);
                        }
                        let gateway_id = match self.settings.gateways.view {
                            GatewaySettingsView::List => selected_gateway_id(self),
                            GatewaySettingsView::Detail => {
                                self.settings.gateways.detail_gateway_id.clone()
                            }
                            GatewaySettingsView::Form => None,
                            GatewaySettingsView::DeleteConfirm => None,
                        };
                        gateway_id.map(SettingsAction::TestGateway)
                    }
                    Some(super::modal::ModalAction::Apply) => apply_settings(self),
                    Some(super::modal::ModalAction::Close) => {
                        if self.settings.section == SettingsSection::Gateways
                            && self.settings.gateways.view == GatewaySettingsView::Form
                        {
                            cancel_gateway_form(self);
                        } else if self.settings.section == SettingsSection::Gateways
                            && self.settings.gateways.view == GatewaySettingsView::DeleteConfirm
                        {
                            cancel_gateway_delete(self);
                        } else if self.settings.section == SettingsSection::Gateways
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
    fn gateway_form_keyboard_and_paste_paths_return_explicit_create_and_update_actions() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);

        update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()),
        );
        assert_eq!(app.state.settings.gateways.view, GatewaySettingsView::Form);
        assert!(app.paste_into_active_text_input("private-hub\n"));
        update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert_eq!(
            app.state
                .settings
                .gateways
                .gateway_form
                .as_ref()
                .expect("add form")
                .id,
            ""
        );
        assert!(app.paste_into_active_text_input("private-hub\n"));
        let form = app
            .state
            .settings
            .gateways
            .gateway_form
            .as_mut()
            .expect("add form");
        assert_eq!(form.id, "private-hub");
        form.display_name = "Private Hub".into();
        form.responses_url = "https://gateway.example/v1".into();

        let action = update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert!(matches!(
            action,
            Some(SettingsAction::AddCustomGateway(gateway))
                if gateway.id == "private-hub"
                    && gateway.display_name == "Private Hub"
        ));

        let mut custom = crate::gateway::Gateway::mindshub();
        custom.id = "existing".into();
        custom.display_name = "Existing".into();
        custom.preset = None;
        custom.auth.credential_ref = Some("gateway:existing".into());
        app.state
            .gateway_catalog
            .gateways
            .insert(custom.id.clone(), custom);
        app.state.settings.gateways.detail_gateway_id = Some("existing".into());
        app.state.settings.gateways.view = GatewaySettingsView::Detail;
        update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()),
        );
        app.state
            .settings
            .gateways
            .gateway_form
            .as_mut()
            .expect("edit form")
            .display_name = "Edited".into();
        let action = update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert!(matches!(
            action,
            Some(SettingsAction::UpdateCustomGateway { gateway_id, gateway })
                if gateway_id == "existing" && gateway.display_name == "Edited"
        ));
    }

    #[test]
    fn gateway_form_cancel_and_unauthenticated_detail_fail_safe() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()),
        );
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.gateways.view, GatewaySettingsView::List);
        assert!(state.settings.gateways.gateway_form.is_none());

        let mut custom = crate::gateway::Gateway::mindshub();
        custom.id = "no-auth".into();
        custom.display_name = "No Auth".into();
        custom.preset = None;
        custom.auth.mode = crate::gateway::AuthenticationMode::None;
        custom.auth.credential_ref = None;
        state
            .gateway_catalog
            .gateways
            .insert(custom.id.clone(), custom);
        state.settings.gateways.detail_gateway_id = Some("no-auth".into());
        state.settings.gateways.view = GatewaySettingsView::Detail;
        state.settings.gateways.detail_field = GatewayDetailField::Credential;
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert!(!state.settings.gateways.editing_credential);
    }

    #[test]
    fn gateway_form_mouse_paths_cover_add_edit_duplicate_save_and_cancel_controls() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);
        let area = app.state.settings_content_rect();
        let add = crate::ui::gateway_add_button_rect(area);
        app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            add.x + 1,
            add.y,
        ));
        assert_eq!(app.state.settings.gateways.view, GatewaySettingsView::Form);

        let auth_row = area.y + 1 + GatewayFormField::Authentication as u16;
        app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 2,
            auth_row,
        ));
        let form = app
            .state
            .settings
            .gateways
            .gateway_form
            .as_ref()
            .expect("form after mouse add");
        assert_eq!(form.selected_field, GatewayFormField::Authentication);
        assert_eq!(
            form.authentication,
            crate::gateway::AuthenticationMode::XApiKey
        );

        {
            let form = app
                .state
                .settings
                .gateways
                .gateway_form
                .as_mut()
                .expect("form before mouse save");
            form.id = "mouse-hub".into();
            form.display_name = "Mouse Hub".into();
            form.responses_url = "https://gateway.example/v1".into();
        }
        let inner = app.state.settings_inner_rect();
        let (save, close) = crate::ui::settings_button_rects(inner, &app.state, true);
        let save = save.expect("gateway form save button");
        let action = app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            save.x + 1,
            save.y,
        ));
        assert!(matches!(
            action,
            Some(SettingsAction::AddCustomGateway(gateway))
                if gateway.id == "mouse-hub" && gateway.display_name == "Mouse Hub"
        ));
        app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            close.x + 1,
            close.y,
        ));
        assert_eq!(app.state.settings.gateways.view, GatewaySettingsView::List);

        let mut custom = crate::gateway::Gateway::mindshub();
        custom.id = "editable".into();
        custom.display_name = "Editable".into();
        custom.preset = None;
        app.state
            .gateway_catalog
            .gateways
            .insert(custom.id.clone(), custom);
        app.state.settings.gateways.detail_gateway_id = Some("editable".into());
        app.state.settings.gateways.view = GatewaySettingsView::Detail;
        let (edit, _) = crate::ui::gateway_detail_button_rects(area, true);
        let edit = edit.expect("custom gateway edit button");
        app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            edit.x + 1,
            edit.y,
        ));
        assert_eq!(app.state.settings.gateways.view, GatewaySettingsView::Form);
        assert_eq!(
            app.state
                .settings
                .gateways
                .gateway_form
                .as_ref()
                .map(|form| form.mode),
            Some(CustomGatewayFormMode::Edit)
        );

        let (_, close) = crate::ui::settings_button_rects(inner, &app.state, true);
        app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            close.x + 1,
            close.y,
        ));
        assert_eq!(
            app.state.settings.gateways.view,
            GatewaySettingsView::Detail
        );

        app.state.settings.gateways.detail_gateway_id = Some("mindshub".into());
        app.state.settings.gateways.view = GatewaySettingsView::Detail;
        let (_, duplicate) = crate::ui::gateway_detail_button_rects(area, false);
        app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            duplicate.x + 1,
            duplicate.y,
        ));
        assert_eq!(app.state.settings.gateways.view, GatewaySettingsView::Form);
        assert_eq!(
            app.state
                .settings
                .gateways
                .gateway_form
                .as_ref()
                .map(|form| form.mode),
            Some(CustomGatewayFormMode::Duplicate)
        );
    }

    #[test]
    fn gateway_form_action_persists_and_opens_the_saved_gateway() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix clock")
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("gowild-form-save-{nonce}"))
            .join("gateways.json");
        let mut app = app_for_mouse_test();
        app.gateway_repository = crate::gateway::GatewayRepository::new(path.clone());
        open_settings(&mut app.state);
        update_settings_state(
            &mut app.state,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()),
        );
        let form = app
            .state
            .settings
            .gateways
            .gateway_form
            .as_mut()
            .expect("add form");
        form.id = "local-proxy".into();
        form.display_name = "Local Proxy".into();
        form.responses_url = "http://127.0.0.1:11434/v1".into();
        form.authentication = crate::gateway::AuthenticationMode::None;

        app.handle_settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(app
            .state
            .gateway_catalog
            .gateways
            .contains_key("local-proxy"));
        assert_eq!(
            app.state.settings.gateways.detail_gateway_id.as_deref(),
            Some("local-proxy")
        );
        assert_eq!(
            app.state.settings.gateways.view,
            GatewaySettingsView::Detail
        );
        assert!(app.state.settings.gateways.gateway_form.is_none());
        assert!(crate::gateway::GatewayRepository::new(path.clone())
            .load()
            .expect("saved form catalog")
            .gateways
            .contains_key("local-proxy"));
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }

    #[test]
    fn gateway_delete_keyboard_defaults_to_keep_and_persists_after_confirmation() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix clock")
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("gowild-delete-confirm-{nonce}"))
            .join("gateways.json");
        let mut app = app_for_mouse_test();
        app.gateway_repository = crate::gateway::GatewayRepository::new(path.clone());
        open_settings(&mut app.state);
        let mut custom = crate::gateway::Gateway::mindshub();
        custom.id = "disposable".into();
        custom.display_name = "Disposable".into();
        custom.preset = None;
        app.state
            .gateway_catalog
            .gateways
            .insert(custom.id.clone(), custom);
        app.state.settings.gateways.detail_gateway_id = Some("disposable".into());
        app.state.settings.gateways.view = GatewaySettingsView::Detail;

        app.handle_settings_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty()));
        assert_eq!(
            app.state.settings.gateways.view,
            GatewaySettingsView::DeleteConfirm
        );
        assert_eq!(
            app.state.settings.gateways.credential_removal,
            CredentialRemoval::Keep
        );
        app.handle_settings_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty()));
        assert_eq!(
            app.state.settings.gateways.credential_removal,
            CredentialRemoval::Delete
        );
        app.handle_settings_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(
            app.state.settings.gateways.view,
            GatewaySettingsView::Detail
        );

        app.handle_settings_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty()));
        app.handle_settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(!app
            .state
            .gateway_catalog
            .gateways
            .contains_key("disposable"));
        assert_eq!(app.state.settings.gateways.view, GatewaySettingsView::List);
        assert!(app.state.settings.gateways.detail_gateway_id.is_none());
        assert_eq!(
            app.state.settings.gateways.credential_removal,
            CredentialRemoval::Keep
        );
        assert!(!crate::gateway::GatewayRepository::new(path.clone())
            .load()
            .expect("saved gateway catalog")
            .gateways
            .contains_key("disposable"));

        app.state.settings.gateways.detail_gateway_id = Some("mindshub".into());
        app.state.settings.gateways.view = GatewaySettingsView::Detail;
        app.handle_settings_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty()));
        assert_eq!(
            app.state.settings.gateways.view,
            GatewaySettingsView::Detail
        );
        assert!(app
            .state
            .settings
            .gateways
            .notice
            .as_ref()
            .is_some_and(|notice| notice.message.contains("cannot be deleted")));
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }

    #[test]
    fn gateway_delete_mouse_paths_select_key_removal_confirm_and_cancel() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);
        let mut custom = crate::gateway::Gateway::mindshub();
        custom.id = "mouse-delete".into();
        custom.display_name = "Mouse Delete".into();
        custom.preset = None;
        app.state
            .gateway_catalog
            .gateways
            .insert(custom.id.clone(), custom);
        app.state.settings.gateways.detail_gateway_id = Some("mouse-delete".into());
        app.state.settings.gateways.view = GatewaySettingsView::Detail;

        let area = app.state.settings_content_rect();
        let delete = crate::ui::gateway_detail_delete_button_rect(area, true)
            .expect("custom gateway delete button");
        app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            delete.x + 1,
            delete.y,
        ));
        assert_eq!(
            app.state.settings.gateways.view,
            GatewaySettingsView::DeleteConfirm
        );

        app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 2,
            area.y + 7,
        ));
        assert_eq!(
            app.state.settings.gateways.credential_removal,
            CredentialRemoval::Delete
        );
        let inner = app.state.settings_inner_rect();
        let (confirm, cancel) = crate::ui::settings_button_rects(inner, &app.state, true);
        let confirm = confirm.expect("delete confirmation button");
        let action = app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            confirm.x + 1,
            confirm.y,
        ));
        assert!(matches!(
            action,
            Some(SettingsAction::DeleteCustomGateway {
                gateway_id,
                credential_removal: CredentialRemoval::Delete,
            }) if gateway_id == "mouse-delete"
        ));
        app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            cancel.x + 1,
            cancel.y,
        ));
        assert_eq!(
            app.state.settings.gateways.view,
            GatewaySettingsView::Detail
        );
        assert!(app
            .state
            .gateway_catalog
            .gateways
            .contains_key("mouse-delete"));
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
