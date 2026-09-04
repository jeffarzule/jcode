//! Offline integration of the native OpenAI provider, session persistence, and
//! the agent's compaction budget. No model requests or user credentials are used.

use jcode::agent::Agent;
use jcode::auth::codex::CodexCredentials;
use jcode::auth::test_sandbox::AuthTestSandbox;
use jcode::auth::{AuthState, AuthStatus};
use jcode::provider::{MultiProvider, Provider, external};
use jcode::session::Session;
use jcode::tool::Registry;
use jcode_provider_core::{RouteSelection, RuntimeKey};
use jcode_provider_openai_runtime::OpenAIProvider;
use std::collections::HashMap;
use std::sync::Arc;

fn assert_effort(agent: &Agent, expected: &str) {
    assert_eq!(agent.provider_reasoning_effort().as_deref(), Some(expected));
    assert_eq!(
        Session::load(agent.session_id())
            .expect("persisted session")
            .reasoning_effort
            .as_deref(),
        Some(expected)
    );
    assert_eq!(
        jcode::session_effort::session_effort(agent.session_id()).as_deref(),
        Some(expected)
    );
}

#[tokio::test]
async fn astra_restore_and_model_switch_keep_effective_settings_in_sync() {
    let sandbox = AuthTestSandbox::new().expect("isolated auth and session storage");
    jcode::env::set_var("JCODE_RUNTIME_PROVIDER", "openai-api");
    jcode::env::set_var("OPENAI_API_KEY", "offline-test-key");
    jcode::provider::populate_context_limits(HashMap::from([("gpt-6-astra".to_string(), 272_000)]));
    jcode::provider::populate_account_models(vec![
        "gpt-5.6-sol".to_string(),
        "gpt-6-astra".to_string(),
    ]);
    external::register_external_provider(external::OPENAI_RUNTIME, || {
        Arc::new(OpenAIProvider::new(CodexCredentials {
            access_token: "offline-test-key".to_string(),
            refresh_token: String::new(),
            id_token: None,
            account_id: None,
            expires_at: None,
        }))
    });

    for (saved, expected) in [("none", "low"), ("minimal", "low"), ("high", "high")] {
        let provider = Arc::new(MultiProvider::from_auth_status(AuthStatus {
            openai: AuthState::Available,
            openai_has_api_key: true,
            ..AuthStatus::default()
        }));
        provider
            .set_model("openai-api:gpt-5.6-sol")
            .expect("initial model");
        provider
            .set_reasoning_effort("high")
            .expect("initial effort");
        let registry = Registry::new(provider.clone()).await;
        let compaction = registry.compaction();
        let mut session = Session::create(None, Some(format!("Astra migration: {saved}")));
        session.working_dir = Some(sandbox.root().display().to_string());
        session.model = Some("gpt-6-astra".to_string());
        session.provider_key = Some("openai-api".to_string());
        session.route_api_method = Some("openai-api".to_string());
        session.reasoning_effort = Some(saved.to_string());
        session.save().expect("save old session");
        let session = Session::load(&session.id).expect("reload saved session");

        let mut agent = Agent::new_with_session(provider, registry, session, None);
        assert_eq!(agent.provider_model(), "gpt-6-astra");
        assert_effort(&agent, expected);
        assert_eq!(compaction.read().await.token_budget(), 272_000);

        agent.set_model("gpt-5.6-sol").expect("select Sol");
        agent
            .set_reasoning_effort("none")
            .expect("Sol supports none");
        agent.set_model("gpt-6-astra").expect("switch to Astra");
        assert_effort(&agent, "low");
        assert_eq!(compaction.read().await.token_budget(), 272_000);

        agent.set_reasoning_effort("high").expect("explicit effort");
        agent.set_model("gpt-5.6-sol").expect("select Sol");
        agent.set_model("gpt-6-astra").expect("switch to Astra");
        assert_effort(&agent, "high");

        agent.set_model("gpt-5.6-sol").expect("select Sol");
        agent
            .set_reasoning_effort("none")
            .expect("Sol supports none");
        agent
            .set_route_selection(&RouteSelection {
                model: "gpt-6-astra".to_string(),
                runtime_key: RuntimeKey::OpenAIApiKey,
                api_method: "openai-api".to_string(),
                provider_label: "OpenAI".to_string(),
                detail: String::new(),
            })
            .expect("select Astra API route");
        assert_eq!(agent.provider_model(), "gpt-6-astra");
        assert_effort(&agent, "low");
        assert_eq!(compaction.read().await.token_budget(), 272_000);
    }
}
