use crate::app::state::{
    GatewayCredentialStatus, GatewayModelTarget, GatewayNotice, GatewayNoticeKind,
};
use crate::events::AppEvent;
use crate::gateway::{
    AuthenticationMode, ConnectionStatus, Credential, CredentialBackend, CredentialRemoval,
    Gateway, GatewayCatalog, GatewayInspection, GatewayTester,
};

use super::App;

impl App {
    pub(crate) fn add_custom_gateway(&mut self, gateway: Gateway) -> bool {
        if self
            .state
            .gateway_catalog
            .gateways
            .contains_key(&gateway.id)
        {
            self.set_gateway_notice(
                GatewayNoticeKind::Error,
                "A gateway with that ID already exists.",
            );
            return false;
        }
        self.persist_custom_gateway(gateway, None)
    }

    pub(crate) fn update_custom_gateway(&mut self, gateway_id: &str, gateway: Gateway) -> bool {
        if gateway.id != gateway_id {
            self.set_gateway_notice(
                GatewayNoticeKind::Error,
                "A gateway ID cannot be changed after creation.",
            );
            return false;
        }
        let Some(existing) = self.state.gateway_catalog.gateways.get(gateway_id).cloned() else {
            self.set_gateway_notice(GatewayNoticeKind::Error, "That gateway no longer exists.");
            return false;
        };
        if existing.preset.is_some() {
            self.set_gateway_notice(
                GatewayNoticeKind::Error,
                "Built-in gateway presets cannot be edited.",
            );
            return false;
        }
        self.persist_custom_gateway(gateway, Some(existing))
    }

    fn persist_custom_gateway(&mut self, mut gateway: Gateway, existing: Option<Gateway>) -> bool {
        if gateway.preset.is_some() {
            self.set_gateway_notice(
                GatewayNoticeKind::Error,
                "Custom gateways cannot claim a built-in preset.",
            );
            return false;
        }
        normalize_custom_gateway_auth(&mut gateway, existing.as_ref());
        let connection_settings_changed = existing
            .as_ref()
            .is_none_or(|existing| gateway_connection_settings_changed(existing, &gateway));
        if connection_settings_changed {
            clear_gateway_runtime_state(&mut gateway);
        } else if let Some(existing) = existing.as_ref() {
            gateway.model_discovery.cached_models = existing.model_discovery.cached_models.clone();
            gateway.model_discovery.refreshed_at = existing.model_discovery.refreshed_at.clone();
            gateway.default_models = existing.default_models.clone();
            gateway.connection_test = existing.connection_test.clone();
        }

        let gateway_id = gateway.id.clone();
        let credential_status = if gateway.auth.mode == AuthenticationMode::None {
            None
        } else {
            Some(
                self.gateway_credentials
                    .get(
                        gateway
                            .auth
                            .credential_ref
                            .as_deref()
                            .expect("authenticated custom gateway credential reference"),
                    )
                    .map(|credential| {
                        if credential.is_some() {
                            GatewayCredentialStatus::Stored
                        } else {
                            GatewayCredentialStatus::Missing
                        }
                    })
                    .unwrap_or(GatewayCredentialStatus::Unknown),
            )
        };
        let mut candidate = self.state.gateway_catalog.clone();
        candidate.gateways.insert(gateway_id.clone(), gateway);
        let message = if existing.is_some() {
            "Custom gateway updated."
        } else {
            "Custom gateway added."
        };
        if !self.persist_gateway_catalog(candidate, GatewayNoticeKind::Success, message) {
            return false;
        }
        if connection_settings_changed
            && self
                .state
                .settings
                .gateways
                .test_in_flight
                .as_ref()
                .is_some_and(|(_, active_gateway_id)| active_gateway_id == &gateway_id)
        {
            self.state.settings.gateways.test_in_flight = None;
        }
        match credential_status {
            Some(status) => {
                self.state
                    .settings
                    .gateways
                    .credential_status
                    .insert(gateway_id, status);
            }
            None => {
                self.state
                    .settings
                    .gateways
                    .credential_status
                    .remove(&gateway_id);
            }
        }
        true
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "wired into custom gateway forms next")
    )]
    pub(crate) fn duplicate_gateway(
        &mut self,
        source_gateway_id: &str,
        new_gateway_id: &str,
        display_name: &str,
    ) -> bool {
        if self
            .state
            .gateway_catalog
            .gateways
            .contains_key(new_gateway_id)
        {
            self.set_gateway_notice(
                GatewayNoticeKind::Error,
                "A gateway with that ID already exists.",
            );
            return false;
        }
        let Some(mut duplicate) = self
            .state
            .gateway_catalog
            .gateways
            .get(source_gateway_id)
            .cloned()
        else {
            self.set_gateway_notice(GatewayNoticeKind::Error, "That gateway no longer exists.");
            return false;
        };
        duplicate.id = new_gateway_id.to_string();
        duplicate.display_name = display_name.to_string();
        duplicate.preset = None;
        clear_gateway_runtime_state(&mut duplicate);
        self.add_custom_gateway(duplicate)
    }

    pub(crate) fn delete_custom_gateway(
        &mut self,
        gateway_id: &str,
        credential_removal: CredentialRemoval,
    ) -> bool {
        let Some(gateway) = self.state.gateway_catalog.gateways.get(gateway_id) else {
            self.set_gateway_notice(GatewayNoticeKind::Error, "That gateway no longer exists.");
            return false;
        };
        if gateway.preset.is_some() {
            self.set_gateway_notice(
                GatewayNoticeKind::Error,
                "Built-in gateway presets cannot be deleted.",
            );
            return false;
        }
        let credential_ref = gateway
            .auth
            .credential_ref
            .clone()
            .or_else(|| Some(custom_gateway_credential_ref(gateway_id)));
        let mut candidate = self.state.gateway_catalog.clone();
        candidate.gateways.remove(gateway_id);
        if candidate.default_gateway_id.as_deref() == Some(gateway_id) {
            candidate.default_gateway_id = None;
        }
        let credential_is_shared = credential_ref.as_deref().is_some_and(|credential_ref| {
            candidate
                .gateways
                .values()
                .any(|gateway| gateway.auth.credential_ref.as_deref() == Some(credential_ref))
        });
        if !self.persist_gateway_catalog(
            candidate,
            GatewayNoticeKind::Success,
            "Custom gateway deleted.",
        ) {
            return false;
        }

        self.state
            .settings
            .gateways
            .credential_status
            .remove(gateway_id);
        let gateway_count = self.state.gateway_catalog.gateways.len();
        self.state.settings.gateways.selected_gateway = self
            .state
            .settings
            .gateways
            .selected_gateway
            .min(gateway_count.saturating_sub(1));
        if self.state.settings.gateways.detail_gateway_id.as_deref() == Some(gateway_id) {
            self.state.settings.gateways.secret_input.clear();
            self.state.settings.gateways.editing_credential = false;
            self.state.settings.gateways.detail_gateway_id = None;
            self.state.settings.gateways.view = crate::app::state::GatewaySettingsView::List;
        }
        if self
            .state
            .settings
            .gateways
            .test_in_flight
            .as_ref()
            .is_some_and(|(_, active_gateway_id)| active_gateway_id == gateway_id)
        {
            self.state.settings.gateways.test_in_flight = None;
        }

        if credential_removal == CredentialRemoval::Delete {
            if credential_is_shared {
                self.set_gateway_notice(
                    GatewayNoticeKind::Warning,
                    "Gateway deleted. Its credential was kept because another gateway uses it.",
                );
            } else if let Some(credential_ref) = credential_ref {
                match self.gateway_credentials.delete(&credential_ref) {
                    Ok(()) => self.set_gateway_notice(
                        GatewayNoticeKind::Success,
                        "Gateway and its stored credential deleted.",
                    ),
                    Err(error) => self.set_gateway_notice(
                        GatewayNoticeKind::Warning,
                        format!(
                            "Gateway deleted, but its stored credential could not be removed: {error}"
                        ),
                    ),
                }
            }
        }
        true
    }

    pub(crate) fn save_default_gateway(&mut self, gateway_id: &str) {
        if !self.state.gateway_catalog.gateways.contains_key(gateway_id) {
            self.set_gateway_notice(GatewayNoticeKind::Error, "That gateway no longer exists.");
            return;
        }
        let mut candidate = self.state.gateway_catalog.clone();
        candidate.default_gateway_id = Some(gateway_id.to_string());
        self.persist_gateway_catalog(
            candidate,
            GatewayNoticeKind::Success,
            "Default gateway updated.",
        );
    }

    pub(crate) fn save_gateway_credential(&mut self, gateway_id: &str) {
        if self.state.settings.gateways.secret_input.is_empty() {
            self.set_gateway_notice(GatewayNoticeKind::Error, "Enter an API key first.");
            return;
        }
        let credential = match Credential::new(
            self.state
                .settings
                .gateways
                .secret_input
                .expose()
                .to_owned(),
        ) {
            Ok(credential) => credential,
            Err(_) => {
                self.set_gateway_notice(
                    GatewayNoticeKind::Error,
                    "The API key is empty or contains unsupported characters.",
                );
                return;
            }
        };
        let Some(credential_ref) = self
            .state
            .gateway_catalog
            .gateways
            .get(gateway_id)
            .and_then(|gateway| gateway.auth.credential_ref.as_deref())
        else {
            self.set_gateway_notice(
                GatewayNoticeKind::Error,
                "This gateway does not have a credential reference.",
            );
            return;
        };

        match self.gateway_credentials.set(credential_ref, &credential) {
            Ok(backend) => {
                self.state.settings.gateways.secret_input.clear();
                self.state.settings.gateways.editing_credential = false;
                self.state
                    .settings
                    .gateways
                    .credential_status
                    .insert(gateway_id.to_string(), GatewayCredentialStatus::Stored);
                let message = match backend {
                    CredentialBackend::System => {
                        "API key stored in the operating system credential store."
                    }
                    CredentialBackend::RestrictedFile => {
                        "API key stored in GoWild's owner-only credential file."
                    }
                };
                self.set_gateway_notice(GatewayNoticeKind::Success, message);
            }
            Err(error) => {
                self.set_gateway_notice(
                    GatewayNoticeKind::Error,
                    format!("Could not store the API key securely: {error}"),
                );
            }
        }
    }

    pub(crate) fn save_gateway_model(
        &mut self,
        gateway_id: &str,
        target: GatewayModelTarget,
        model_id: &str,
    ) {
        let Some(gateway) = self.state.gateway_catalog.gateways.get(gateway_id) else {
            self.set_gateway_notice(GatewayNoticeKind::Error, "That gateway no longer exists.");
            return;
        };
        if !gateway.supports(target.protocol()) {
            self.set_gateway_notice(
                GatewayNoticeKind::Error,
                "This gateway does not support the protocol required by that CLI.",
            );
            return;
        }
        if !gateway
            .model_discovery
            .cached_models
            .iter()
            .any(|model| model.enabled && !model.embedding && model.id == model_id)
        {
            self.set_gateway_notice(
                GatewayNoticeKind::Warning,
                "That model is no longer selectable. Test the gateway to refresh the catalog.",
            );
            return;
        }
        let mut candidate = self.state.gateway_catalog.clone();
        candidate
            .gateways
            .get_mut(gateway_id)
            .expect("validated gateway exists in cloned catalog")
            .default_models
            .insert(target.config_key().to_string(), model_id.to_string());
        let cli = match target {
            GatewayModelTarget::Codex => "Codex",
            GatewayModelTarget::Claude => "Claude",
        };
        self.persist_gateway_catalog(
            candidate,
            GatewayNoticeKind::Success,
            format!("{cli} default model set to {model_id}."),
        );
    }

    pub(crate) fn start_gateway_test(&mut self, gateway_id: &str) {
        if self.state.settings.gateways.test_in_flight.is_some() {
            self.set_gateway_notice(
                GatewayNoticeKind::Info,
                "A gateway test is already running.",
            );
            return;
        }
        let Some(gateway) = self.state.gateway_catalog.gateways.get(gateway_id).cloned() else {
            self.set_gateway_notice(GatewayNoticeKind::Error, "That gateway no longer exists.");
            return;
        };
        let credential = match gateway.auth.mode {
            AuthenticationMode::None => None,
            _ => {
                let Some(credential_ref) = gateway.auth.credential_ref.as_deref() else {
                    self.set_gateway_notice(
                        GatewayNoticeKind::Error,
                        "This gateway does not have a credential reference.",
                    );
                    return;
                };
                match self.gateway_credentials.get(credential_ref) {
                    Ok(Some(credential)) => {
                        self.state
                            .settings
                            .gateways
                            .credential_status
                            .insert(gateway_id.to_string(), GatewayCredentialStatus::Stored);
                        Some(credential)
                    }
                    Ok(None) => {
                        self.state
                            .settings
                            .gateways
                            .credential_status
                            .insert(gateway_id.to_string(), GatewayCredentialStatus::Missing);
                        self.set_gateway_notice(
                            GatewayNoticeKind::Error,
                            "Add an API key before testing this gateway.",
                        );
                        return;
                    }
                    Err(error) => {
                        self.state
                            .settings
                            .gateways
                            .credential_status
                            .insert(gateway_id.to_string(), GatewayCredentialStatus::Unknown);
                        self.set_gateway_notice(
                            GatewayNoticeKind::Error,
                            format!("Could not read the gateway credential securely: {error}"),
                        );
                        return;
                    }
                }
            }
        };

        let generation = self.state.settings.gateways.next_test_generation;
        self.state.settings.gateways.next_test_generation = next_generation(generation);
        self.state.settings.gateways.test_in_flight = Some((generation, gateway_id.to_string()));
        self.set_gateway_notice(
            GatewayNoticeKind::Info,
            "Testing authentication, model discovery, Responses, and Messages…",
        );

        let event_tx = self.event_tx.clone();
        let gateway_id = gateway_id.to_string();
        let spawn = std::thread::Builder::new()
            .name("gowild-gateway-test".into())
            .spawn(move || {
                let result = GatewayTester::new()
                    .map(|tester| Box::new(tester.inspect(&gateway, credential.as_ref())));
                let _ = event_tx.blocking_send(AppEvent::GatewayTestFinished {
                    generation,
                    gateway_id,
                    result,
                });
            });
        if spawn.is_err() {
            self.state.settings.gateways.test_in_flight = None;
            self.set_gateway_notice(
                GatewayNoticeKind::Error,
                "Could not start the background gateway test.",
            );
        }
    }

    pub(crate) fn finish_gateway_test(
        &mut self,
        generation: u64,
        gateway_id: String,
        result: Result<Box<GatewayInspection>, crate::gateway::GatewayTesterError>,
    ) {
        if !matches!(
            self.state.settings.gateways.test_in_flight.as_ref(),
            Some((active_generation, active_gateway_id))
                if *active_generation == generation && active_gateway_id == &gateway_id
        ) {
            return;
        }
        self.state.settings.gateways.test_in_flight = None;
        let inspection = match result {
            Ok(inspection) => *inspection,
            Err(error) => {
                self.set_gateway_notice(
                    GatewayNoticeKind::Error,
                    format!("Could not initialize gateway testing: {error}"),
                );
                return;
            }
        };
        let status = inspection.connection_test.status;
        let mut candidate = self.state.gateway_catalog.clone();
        let Some(gateway) = candidate.gateways.get_mut(&gateway_id) else {
            self.set_gateway_notice(GatewayNoticeKind::Error, "That gateway no longer exists.");
            return;
        };
        inspection.apply_to(gateway);
        if self.state.settings.guided_setup
            && gateway_id == "mindshub"
            && status == ConnectionStatus::Passed
        {
            candidate.default_gateway_id = Some(gateway_id.clone());
        }
        let (kind, message) = match status {
            ConnectionStatus::Passed => (
                GatewayNoticeKind::Success,
                "Gateway connected. Both configured protocols passed.",
            ),
            ConnectionStatus::Partial => (
                GatewayNoticeKind::Warning,
                "Gateway partially connected. Review the protocol results above.",
            ),
            ConnectionStatus::Failed => (
                GatewayNoticeKind::Error,
                "Gateway test failed. Review the redacted diagnostics above.",
            ),
            ConnectionStatus::NotTested => (GatewayNoticeKind::Info, "Gateway test did not run."),
        };
        self.persist_gateway_catalog(candidate, kind, message);
    }

    fn persist_gateway_catalog(
        &mut self,
        candidate: GatewayCatalog,
        success_kind: GatewayNoticeKind,
        success_message: impl Into<String>,
    ) -> bool {
        match self.gateway_repository.save(&candidate) {
            Ok(()) => {
                self.state.gateway_catalog = candidate;
                self.set_gateway_notice(success_kind, success_message);
                true
            }
            Err(error) => {
                self.set_gateway_notice(
                    GatewayNoticeKind::Error,
                    format!("Could not save gateway settings: {error}"),
                );
                false
            }
        }
    }

    fn set_gateway_notice(&mut self, kind: GatewayNoticeKind, message: impl Into<String>) {
        self.state.settings.gateways.notice = Some(GatewayNotice {
            kind,
            message: message.into(),
        });
    }
}

fn normalize_custom_gateway_auth(gateway: &mut Gateway, existing: Option<&Gateway>) {
    match gateway.auth.mode {
        AuthenticationMode::None => gateway.auth.credential_ref = None,
        _ => {
            gateway.auth.credential_ref = existing
                .and_then(|gateway| gateway.auth.credential_ref.clone())
                .or_else(|| Some(custom_gateway_credential_ref(&gateway.id)));
        }
    }
}

fn custom_gateway_credential_ref(gateway_id: &str) -> String {
    format!("gateway:{gateway_id}")
}

fn gateway_connection_settings_changed(existing: &Gateway, candidate: &Gateway) -> bool {
    existing.endpoints != candidate.endpoints
        || existing.capabilities != candidate.capabilities
        || existing.auth != candidate.auth
        || existing.custom_headers != candidate.custom_headers
        || existing.model_discovery.enabled != candidate.model_discovery.enabled
        || existing.model_discovery.url != candidate.model_discovery.url
}

fn clear_gateway_runtime_state(gateway: &mut Gateway) {
    gateway.model_discovery.cached_models.clear();
    gateway.model_discovery.refreshed_at = None;
    gateway.default_models.clear();
    gateway.connection_test = Default::default();
}

fn next_generation(current: u64) -> u64 {
    match current.wrapping_add(1) {
        0 => 1,
        next => next,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::{next_generation, App};
    use crate::{
        config::Config,
        gateway::{
            AuthenticationMode, CachedModel, Credential, CredentialBackend, CredentialRemoval,
            CredentialStore, CredentialStoreError, Gateway, GatewayRepository,
        },
    };

    #[derive(Default)]
    struct MockCredentialValues {
        values: Mutex<BTreeMap<String, String>>,
    }

    struct MockCredentialStore {
        values: Arc<MockCredentialValues>,
    }

    struct FailingCredentialStore;

    impl CredentialStore for MockCredentialStore {
        fn get(&self, credential_ref: &str) -> Result<Option<Credential>, CredentialStoreError> {
            self.values
                .values
                .lock()
                .expect("credential map")
                .get(credential_ref)
                .cloned()
                .map(Credential::new)
                .transpose()
        }

        fn set(
            &self,
            credential_ref: &str,
            credential: &Credential,
        ) -> Result<CredentialBackend, CredentialStoreError> {
            self.values
                .values
                .lock()
                .expect("credential map")
                .insert(credential_ref.into(), credential.expose().into());
            Ok(CredentialBackend::System)
        }

        fn delete(&self, credential_ref: &str) -> Result<(), CredentialStoreError> {
            self.values
                .values
                .lock()
                .expect("credential map")
                .remove(credential_ref);
            Ok(())
        }
    }

    impl CredentialStore for FailingCredentialStore {
        fn get(&self, _: &str) -> Result<Option<Credential>, CredentialStoreError> {
            Err(CredentialStoreError::SystemStoreUnavailable)
        }

        fn set(&self, _: &str, _: &Credential) -> Result<CredentialBackend, CredentialStoreError> {
            Err(CredentialStoreError::SystemStoreUnavailable)
        }

        fn delete(&self, _: &str) -> Result<(), CredentialStoreError> {
            Err(CredentialStoreError::SystemStoreUnavailable)
        }
    }

    fn temp_gateway_path(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix clock")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("gowild-gateway-ui-{name}-{nonce}"))
            .join("gateways.json")
    }

    fn gateway_app(path: std::path::PathBuf, values: Arc<MockCredentialValues>) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config {
                onboarding: Some(false),
                ..Config::default()
            },
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.gateway_repository = GatewayRepository::new(path);
        app.gateway_credentials = Box::new(MockCredentialStore { values });
        app
    }

    fn custom_gateway(id: &str) -> Gateway {
        let mut gateway = Gateway::mindshub();
        gateway.id = id.into();
        gateway.display_name = format!("{id} gateway");
        gateway.preset = None;
        gateway.auth.credential_ref = Some(format!("gateway:{id}"));
        gateway
    }

    #[test]
    fn gateway_test_generations_never_use_the_idle_zero_value() {
        assert_eq!(next_generation(u64::MAX), 1);
    }

    #[test]
    fn gateway_defaults_and_cli_models_persist_to_the_catalog() {
        let path = temp_gateway_path("defaults");
        let mut app = gateway_app(path.clone(), Arc::default());
        app.state
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .expect("MindsHub preset")
            .model_discovery
            .cached_models = vec![CachedModel {
            id: "provider/model-alpha".into(),
            label: None,
            provider: Some("provider".into()),
            enabled: true,
            embedding: false,
            reasoning_efforts: Vec::new(),
        }];

        app.save_default_gateway("mindshub");
        app.save_gateway_model(
            "mindshub",
            crate::app::state::GatewayModelTarget::Codex,
            "provider/model-alpha",
        );
        app.save_gateway_model(
            "mindshub",
            crate::app::state::GatewayModelTarget::Claude,
            "provider/model-alpha",
        );

        let saved = GatewayRepository::new(path.clone())
            .load()
            .expect("saved gateway catalog");
        assert_eq!(saved.default_gateway_id.as_deref(), Some("mindshub"));
        assert_eq!(
            saved.gateways["mindshub"]
                .default_models
                .get("codex")
                .map(String::as_str),
            Some("provider/model-alpha")
        );
        assert_eq!(
            saved.gateways["mindshub"]
                .default_models
                .get("claude")
                .map(String::as_str),
            Some("provider/model-alpha")
        );
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }

    #[test]
    fn credential_save_uses_the_secret_store_and_zeroizes_the_editor() {
        let path = temp_gateway_path("credential");
        let values = Arc::new(MockCredentialValues::default());
        let mut app = gateway_app(path.clone(), values.clone());
        app.state
            .settings
            .gateways
            .secret_input
            .insert("TOP_SECRET_GATEWAY_KEY");
        app.state.settings.gateways.editing_credential = true;

        app.save_gateway_credential("mindshub");

        assert_eq!(
            values
                .values
                .lock()
                .expect("credential map")
                .get("gateway:mindshub")
                .map(String::as_str),
            Some("TOP_SECRET_GATEWAY_KEY")
        );
        assert!(app.state.settings.gateways.secret_input.is_empty());
        assert!(!app.state.settings.gateways.editing_credential);
        assert!(
            !path.exists(),
            "credentials must not create gateway metadata"
        );
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }

    #[test]
    fn gateway_test_finishes_through_the_background_event_path() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let address = listener.local_addr().expect("loopback address");
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("gateway request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).expect("read gateway request");
            stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: 24\r\nConnection: close\r\n\r\n{\"error\":\"unauthorized\"}",
                )
                .expect("write gateway response");
        });

        let path = temp_gateway_path("background-test");
        let values = Arc::new(MockCredentialValues::default());
        values
            .values
            .lock()
            .expect("credential map")
            .insert("gateway:mindshub".into(), "TOP_SECRET_GATEWAY_KEY".into());
        let mut app = gateway_app(path.clone(), values);
        let gateway = app
            .state
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .expect("MindsHub preset");
        gateway.model_discovery.url = Some(format!("http://{address}/v1/models"));
        gateway.endpoints.openai_responses = Some(format!("http://{address}/v1"));
        gateway.endpoints.anthropic_messages = Some(format!("http://{address}"));

        app.start_gateway_test("mindshub");

        assert!(app.state.settings.gateways.test_in_flight.is_some());
        let event = app.event_rx.blocking_recv().expect("gateway result event");
        app.handle_internal_event(event);
        responder.join().expect("gateway responder");

        assert!(app.state.settings.gateways.test_in_flight.is_none());
        assert_eq!(
            app.state.gateway_catalog.gateways["mindshub"]
                .connection_test
                .status,
            crate::gateway::ConnectionStatus::Failed
        );
        let saved = GatewayRepository::new(path.clone())
            .load()
            .expect("persisted gateway test");
        let serialized = std::fs::read_to_string(&path).expect("gateway metadata");
        assert_eq!(
            saved.gateways["mindshub"].connection_test.status,
            crate::gateway::ConnectionStatus::Failed
        );
        assert!(!serialized.contains("TOP_SECRET_GATEWAY_KEY"));
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }

    #[test]
    fn credential_read_failure_clears_a_stale_stored_status() {
        let path = temp_gateway_path("credential-read-failure");
        let mut app = gateway_app(path.clone(), Arc::default());
        app.gateway_credentials = Box::new(FailingCredentialStore);
        app.state.settings.gateways.credential_status.insert(
            "mindshub".into(),
            crate::app::state::GatewayCredentialStatus::Stored,
        );

        app.start_gateway_test("mindshub");

        assert_eq!(
            app.state
                .settings
                .gateways
                .credential_status
                .get("mindshub"),
            Some(&crate::app::state::GatewayCredentialStatus::Unknown)
        );
        assert!(app.state.settings.gateways.test_in_flight.is_none());
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }

    #[test]
    fn custom_gateway_add_and_edit_preserve_only_current_runtime_state() {
        let path = temp_gateway_path("custom-save");
        let mut app = gateway_app(path.clone(), Arc::default());
        let mut gateway = custom_gateway("local-proxy");
        gateway.model_discovery.cached_models = vec![CachedModel {
            id: "stale-on-create".into(),
            label: None,
            provider: None,
            enabled: true,
            embedding: false,
            reasoning_efforts: Vec::new(),
        }];

        assert!(app.add_custom_gateway(gateway));
        assert!(app.state.gateway_catalog.gateways["local-proxy"]
            .model_discovery
            .cached_models
            .is_empty());
        let mut colliding_add = custom_gateway("local-proxy");
        colliding_add.display_name = "Must not overwrite".into();
        assert!(!app.add_custom_gateway(colliding_add));
        assert_eq!(
            app.state.gateway_catalog.gateways["local-proxy"].display_name,
            "local-proxy gateway"
        );

        let saved = app
            .state
            .gateway_catalog
            .gateways
            .get_mut("local-proxy")
            .expect("custom gateway");
        saved.model_discovery.cached_models = vec![CachedModel {
            id: "tested-model".into(),
            label: None,
            provider: None,
            enabled: true,
            embedding: false,
            reasoning_efforts: Vec::new(),
        }];
        saved
            .default_models
            .insert("codex".into(), "tested-model".into());
        saved.connection_test.status = crate::gateway::ConnectionStatus::Passed;
        saved.auth.credential_ref = Some("gateway:legacy-reference".into());

        let mut display_only_edit = saved.clone();
        display_only_edit.display_name = "Local Proxy".into();
        assert!(app.update_custom_gateway("local-proxy", display_only_edit));
        let saved = &app.state.gateway_catalog.gateways["local-proxy"];
        assert_eq!(saved.model_discovery.cached_models[0].id, "tested-model");
        assert_eq!(
            saved.auth.credential_ref.as_deref(),
            Some("gateway:legacy-reference")
        );
        assert_eq!(
            saved.connection_test.status,
            crate::gateway::ConnectionStatus::Passed
        );

        let mut connection_edit = saved.clone();
        connection_edit.endpoints.openai_responses = Some("https://example.com/v2".into());
        app.state.settings.gateways.test_in_flight = Some((9, "local-proxy".into()));
        assert!(app.update_custom_gateway("local-proxy", connection_edit));
        let saved = &app.state.gateway_catalog.gateways["local-proxy"];
        assert!(saved.model_discovery.cached_models.is_empty());
        assert!(saved.default_models.is_empty());
        assert_eq!(
            saved.connection_test.status,
            crate::gateway::ConnectionStatus::NotTested
        );
        assert!(app.state.settings.gateways.test_in_flight.is_none());

        let reloaded = GatewayRepository::new(path.clone())
            .load()
            .expect("saved custom gateway catalog");
        assert_eq!(reloaded.gateways["local-proxy"], *saved);
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }

    #[test]
    fn duplicate_gets_an_independent_credential_reference_without_copying_the_secret() {
        let path = temp_gateway_path("custom-duplicate");
        let values = Arc::new(MockCredentialValues::default());
        values
            .values
            .lock()
            .expect("credential map")
            .insert("gateway:mindshub".into(), "SOURCE_SECRET".into());
        let mut app = gateway_app(path.clone(), values.clone());
        app.state
            .gateway_catalog
            .gateways
            .get_mut("mindshub")
            .expect("MindsHub preset")
            .connection_test
            .status = crate::gateway::ConnectionStatus::Passed;

        assert!(app.duplicate_gateway("mindshub", "private-hub", "Private Hub"));

        let duplicate = &app.state.gateway_catalog.gateways["private-hub"];
        assert_eq!(duplicate.preset, None);
        assert_eq!(
            duplicate.auth.credential_ref.as_deref(),
            Some("gateway:private-hub")
        );
        assert_eq!(
            duplicate.connection_test.status,
            crate::gateway::ConnectionStatus::NotTested
        );
        assert!(!values
            .values
            .lock()
            .expect("credential map")
            .contains_key("gateway:private-hub"));
        assert_eq!(
            app.state
                .settings
                .gateways
                .credential_status
                .get("private-hub"),
            Some(&crate::app::state::GatewayCredentialStatus::Missing)
        );
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }

    #[test]
    fn custom_gateway_delete_makes_credential_retention_an_explicit_choice() {
        let path = temp_gateway_path("custom-delete");
        let values = Arc::new(MockCredentialValues::default());
        let mut app = gateway_app(path.clone(), values.clone());
        assert!(app.add_custom_gateway(custom_gateway("keep-secret")));
        assert!(app.add_custom_gateway(custom_gateway("delete-secret")));
        let mut no_auth = custom_gateway("no-auth-secret");
        no_auth.auth.mode = AuthenticationMode::None;
        assert!(app.add_custom_gateway(no_auth));
        {
            let mut stored = values.values.lock().expect("credential map");
            stored.insert("gateway:keep-secret".into(), "KEEP_ME".into());
            stored.insert("gateway:delete-secret".into(), "DELETE_ME".into());
            stored.insert("gateway:no-auth-secret".into(), "ORPHANED_SECRET".into());
        }
        app.state.gateway_catalog.default_gateway_id = Some("delete-secret".into());
        app.state.settings.gateways.detail_gateway_id = Some("delete-secret".into());
        app.state.settings.gateways.view = crate::app::state::GatewaySettingsView::Detail;
        app.state.settings.gateways.editing_credential = true;
        app.state
            .settings
            .gateways
            .secret_input
            .insert("UNSAVED_SECRET");
        app.state.settings.gateways.test_in_flight = Some((7, "delete-secret".into()));

        assert!(app.delete_custom_gateway("keep-secret", CredentialRemoval::Keep));
        assert!(app.delete_custom_gateway("delete-secret", CredentialRemoval::Delete));
        assert!(app.delete_custom_gateway("no-auth-secret", CredentialRemoval::Delete));

        let stored = values.values.lock().expect("credential map");
        assert!(stored.contains_key("gateway:keep-secret"));
        assert!(!stored.contains_key("gateway:delete-secret"));
        assert!(!stored.contains_key("gateway:no-auth-secret"));
        drop(stored);
        assert_eq!(app.state.gateway_catalog.default_gateway_id, None);
        assert_eq!(app.state.settings.gateways.detail_gateway_id, None);
        assert_eq!(
            app.state.settings.gateways.view,
            crate::app::state::GatewaySettingsView::List
        );
        assert!(!app.state.settings.gateways.editing_credential);
        assert!(app.state.settings.gateways.secret_input.is_empty());
        assert!(app.state.settings.gateways.test_in_flight.is_none());
        let reloaded = GatewayRepository::new(path.clone())
            .load()
            .expect("catalog after deletion");
        assert_eq!(reloaded.default_gateway_id, None);
        assert!(!reloaded.gateways.contains_key("keep-secret"));
        assert!(!reloaded.gateways.contains_key("delete-secret"));
        assert!(!reloaded.gateways.contains_key("no-auth-secret"));
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }

    #[test]
    fn deleting_a_shared_or_builtin_credential_fails_safe() {
        let path = temp_gateway_path("custom-delete-safe");
        let values = Arc::new(MockCredentialValues::default());
        let mut app = gateway_app(path.clone(), values.clone());
        let shared_ref = "gateway:shared";
        let mut first = custom_gateway("shared-one");
        first.auth.credential_ref = Some(shared_ref.into());
        let mut second = custom_gateway("shared-two");
        second.auth.credential_ref = Some(shared_ref.into());
        app.state
            .gateway_catalog
            .gateways
            .insert(first.id.clone(), first);
        app.state
            .gateway_catalog
            .gateways
            .insert(second.id.clone(), second);
        values
            .values
            .lock()
            .expect("credential map")
            .insert(shared_ref.into(), "SHARED_SECRET".into());

        assert!(app.delete_custom_gateway("shared-one", CredentialRemoval::Delete));
        assert!(values
            .values
            .lock()
            .expect("credential map")
            .contains_key(shared_ref));
        assert!(!app.delete_custom_gateway("mindshub", CredentialRemoval::Delete));
        assert!(app.state.gateway_catalog.gateways.contains_key("mindshub"));
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }

    #[test]
    fn failed_gateway_metadata_delete_never_removes_the_credential_first() {
        let path = temp_gateway_path("custom-delete-metadata-failure");
        std::fs::create_dir_all(&path).expect("create a directory where the config file belongs");
        let values = Arc::new(MockCredentialValues::default());
        values
            .values
            .lock()
            .expect("credential map")
            .insert("gateway:durable".into(), "DURABLE_SECRET".into());
        let mut app = gateway_app(path.clone(), values.clone());
        let gateway = custom_gateway("durable");
        app.state
            .gateway_catalog
            .gateways
            .insert(gateway.id.clone(), gateway);

        assert!(!app.delete_custom_gateway("durable", CredentialRemoval::Delete));
        assert!(app.state.gateway_catalog.gateways.contains_key("durable"));
        assert!(values
            .values
            .lock()
            .expect("credential map")
            .contains_key("gateway:durable"));
        let _ = std::fs::remove_dir_all(path.parent().expect("gateway config directory"));
    }
}
