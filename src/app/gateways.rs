// The immediately stacked TUI change is the first production caller of this
// controller. Keeping the runtime seam independently reviewable is worth a
// narrow temporary dead-code allowance on this module.
#![allow(dead_code)]

use crate::app::state::{
    GatewayCredentialStatus, GatewayModelTarget, GatewayNotice, GatewayNoticeKind,
};
use crate::events::AppEvent;
use crate::gateway::{
    AuthenticationMode, ConnectionStatus, Credential, CredentialBackend, GatewayCatalog,
    GatewayInspection, GatewayTester,
};

use super::App;

impl App {
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

    pub(crate) fn cycle_gateway_model(
        &mut self,
        gateway_id: &str,
        target: GatewayModelTarget,
        direction: i8,
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
        let available = gateway
            .model_discovery
            .cached_models
            .iter()
            .filter(|model| model.enabled && !model.embedding)
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();
        if available.is_empty() {
            self.set_gateway_notice(
                GatewayNoticeKind::Warning,
                "Test the connection to discover selectable models first.",
            );
            return;
        }
        let current = gateway.default_models.get(target.config_key());
        let selected = next_model(&available, current.map(String::as_str), direction);
        let mut candidate = self.state.gateway_catalog.clone();
        if let Some(gateway) = candidate.gateways.get_mut(gateway_id) {
            gateway
                .default_models
                .insert(target.config_key().to_string(), selected.to_string());
        }
        let cli = match target {
            GatewayModelTarget::Codex => "Codex",
            GatewayModelTarget::Claude => "Claude",
        };
        self.persist_gateway_catalog(
            candidate,
            GatewayNoticeKind::Success,
            format!("{cli} default model set to {selected}."),
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
        let (kind, message) = match status {
            ConnectionStatus::Passed => (
                GatewayNoticeKind::Success,
                "Gateway connected. Both configured protocols passed.",
            ),
            ConnectionStatus::Partial => (
                GatewayNoticeKind::Warning,
                "Gateway partially connected. Review the protocol results below.",
            ),
            ConnectionStatus::Failed => (
                GatewayNoticeKind::Error,
                "Gateway test failed. Review the redacted diagnostics below.",
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

fn next_model<'a>(available: &'a [String], current: Option<&str>, direction: i8) -> &'a str {
    let current_index = current.and_then(|current| available.iter().position(|id| id == current));
    let index = match (current_index, direction.is_negative()) {
        (Some(0), true) | (None, true) => available.len() - 1,
        (Some(index), true) => index - 1,
        (Some(index), false) => (index + 1) % available.len(),
        (None, false) => 0,
    };
    &available[index]
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

    use super::{next_generation, next_model, App};
    use crate::{
        config::Config,
        gateway::{
            CachedModel, Credential, CredentialBackend, CredentialStore, CredentialStoreError,
            GatewayRepository,
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

    #[test]
    fn model_cycle_wraps_and_recovers_from_an_unlisted_default() {
        let models = vec!["alpha".to_string(), "beta".to_string()];
        assert_eq!(next_model(&models, Some("alpha"), 1), "beta");
        assert_eq!(next_model(&models, Some("beta"), 1), "alpha");
        assert_eq!(next_model(&models, Some("missing"), 1), "alpha");
        assert_eq!(next_model(&models, Some("missing"), -1), "beta");
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
        app.cycle_gateway_model("mindshub", crate::app::state::GatewayModelTarget::Codex, 1);
        app.cycle_gateway_model("mindshub", crate::app::state::GatewayModelTarget::Claude, 1);

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
}
