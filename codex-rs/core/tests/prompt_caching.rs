use codex_core::ConversationManager;
use codex_core::ModelProviderInfo;
use codex_core::built_in_model_providers;
use codex_core::protocol::AskForApproval;
use codex_core::protocol::EventMsg;
use codex_core::protocol::InputItem;
use codex_core::protocol::Op;
use codex_core::protocol::SandboxPolicy;
use codex_core::config_types::ReasoningEffort;
use codex_core::config_types::ReasoningSummary;
use codex_login::CodexAuth;
use core_test_support::load_default_config_for_test;
use core_test_support::load_sse_fixture_with_id;
use core_test_support::wait_for_event;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

/// Build minimal SSE stream with completed marker using the JSON fixture.
fn sse_completed(id: &str) -> String {
    load_sse_fixture_with_id("tests/fixtures/completed_template.json", id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prefixes_context_and_instructions_once_and_consistently_across_requests() {
    if std::net::TcpListener::bind("127.0.0.1:0").is_err() {
        println!("Skipping test due to sandbox network bind restrictions.");
        return;
    }
    use pretty_assertions::assert_eq;

    let server = MockServer::start().await;

    let sse = sse_completed("resp");
    let template = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(sse, "text/event-stream");

    // Expect two POSTs to /v1/responses
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(template)
        .expect(2)
        .mount(&server)
        .await;

    let model_provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };

    let cwd = TempDir::new().unwrap();
    let codex_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&codex_home);
    config.cwd = cwd.path().to_path_buf();
    config.model_provider = model_provider;
    config.user_instructions = Some("be consistent and helpful".to_string());

    let conversation_manager = ConversationManager::default();
    let codex = conversation_manager
        .new_conversation_with_auth(config.clone(), Some(CodexAuth::from_api_key("Test API Key")))
        .await
        .expect("create new conversation")
        .conversation;

    codex
        .submit(Op::UserInput {
            items: vec![InputItem::Text {
                text: "hello 1".into(),
            }],
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![InputItem::Text {
                text: "hello 2".into(),
            }],
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "expected two POST requests");

    // Validate new input formatting order and contents
    let body1 = requests[0].body_json::<serde_json::Value>().unwrap();
    let input1 = body1["input"].as_array().expect("input should be array");
    // We expect at least dev, env, ui, and user message; additional trailing
    // ephemeral status items (e.g., system status, screenshots) may be
    // appended to the request.
    assert!(
        input1.len() >= 4,
        "expected at least dev, env, ui, and user message"
    );
    // 0: developer additional instructions
    assert_eq!(input1[0]["role"], serde_json::json!("developer"));
    let dev_text = input1[0]["content"][0]["text"].as_str().unwrap_or("");
    assert!(dev_text.starts_with("In this environment, you are running as `coder`"));
    // 1: environment context (pretty-JSON inside tag)
    assert_eq!(input1[1]["role"], serde_json::json!("user"));
    let env_text = input1[1]["content"][0]["text"].as_str().unwrap_or("");
    assert!(env_text.starts_with("<environment_context>"));
    assert!(env_text.contains("<approval_policy>on-request</approval_policy>"));
    assert!(env_text.contains("<sandbox_mode>read-only</sandbox_mode>"));
    assert!(env_text.contains("<network_access>restricted</network_access>"));
    // 2: user instructions
    assert_eq!(input1[2]["role"], serde_json::json!("user"));
    let ui_text = input1[2]["content"][0]["text"].as_str().unwrap_or("");
    assert!(ui_text.starts_with("<user_instructions>"));
    assert!(ui_text.contains("be consistent and helpful"));
    // 3+: first user message should be present after the prefixes (may not be last
    // due to ephemeral items appended to the tail).
    let user_first = serde_json::json!({
        "type": "message",
        "id": serde_json::Value::Null,
        "role": "user",
        "content": [ { "type": "input_text", "text": "hello 1" } ]
    });
    assert!(
        input1.iter().skip(3).any(|v| v == &user_first),
        "first user message not found after prefixes"
    );

    // Second request should keep dev+ui the same and include the new user
    // message (ephemeral items may still appear at the end).
    let expected_user_message_2 = serde_json::json!({
        "type": "message",
        "id": serde_json::Value::Null,
        "role": "user",
        "content": [ { "type": "input_text", "text": "hello 2" } ]
    });
    let body2 = requests[1].body_json::<serde_json::Value>().unwrap();
    let input2 = body2["input"].as_array().expect("input should be array");
    assert_eq!(input2[0], input1[0], "developer instructions should match");
    assert_eq!(input2[2], input1[2], "user instructions should match");
    assert!(
        input2.iter().any(|v| v == &expected_user_message_2),
        "second request should contain the new user message"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overrides_turn_context_but_keeps_cached_prefix_and_key_constant() {
    if std::net::TcpListener::bind("127.0.0.1:0").is_err() {
        println!("Skipping test due to sandbox network bind restrictions.");
        return;
    }
    use pretty_assertions::assert_eq;

    let server = MockServer::start().await;

    let sse = sse_completed("resp");
    let template = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(sse, "text/event-stream");

    // Expect two POSTs to /v1/responses
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(template)
        .expect(2)
        .mount(&server)
        .await;

    let model_provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };

    let cwd = TempDir::new().unwrap();
    let codex_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&codex_home);
    config.cwd = cwd.path().to_path_buf();
    config.model_provider = model_provider;
    config.user_instructions = Some("be consistent and helpful".to_string());

    let conversation_manager = ConversationManager::default();
    let codex = conversation_manager
        .new_conversation_with_auth(config.clone(), Some(CodexAuth::from_api_key("Test API Key")))
        .await
        .expect("create new conversation")
        .conversation;

    // First turn
    codex
        .submit(Op::UserInput {
            items: vec![InputItem::Text {
                text: "hello 1".into(),
            }],
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;

    // Change everything about the turn context via ConfigureSession
    let new_cwd = TempDir::new().unwrap();
    let writable = TempDir::new().unwrap();
    codex
        .submit(Op::ConfigureSession {
            provider: config.model_provider.clone(),
            model: "o3".to_string(),
            model_reasoning_effort: ReasoningEffort::High,
            model_reasoning_summary: ReasoningSummary::Detailed,
            model_text_verbosity: config.model_text_verbosity,
            user_instructions: config.user_instructions.clone(),
            base_instructions: config.base_instructions.clone(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![writable.path().to_path_buf()],
                network_access: true,
                exclude_tmpdir_env_var: true,
                exclude_slash_tmp: true,
            },
            disable_response_storage: config.disable_response_storage,
            notify: config.notify.clone(),
            cwd: new_cwd.path().to_path_buf(),
            resume_path: None,
        })
        .await
        .unwrap();

    // Second turn after overrides
    codex
        .submit(Op::UserInput {
            items: vec![InputItem::Text {
                text: "hello 2".into(),
            }],
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;

    // Verify we issued exactly two requests, and the cached prefix stayed identical.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "expected two POST requests");

    let body1 = requests[0].body_json::<serde_json::Value>().unwrap();
    let body2 = requests[1].body_json::<serde_json::Value>().unwrap();

    // prompt_cache_key should remain constant across overrides
    assert_eq!(
        body1["prompt_cache_key"], body2["prompt_cache_key"],
        "prompt_cache_key should not change across overrides"
    );

    // Developer and UI instructions should match the first request; env message should change;
    // the new user message should be present (may not be last if ephemeral items are appended).
    let expected_user_message_2 = serde_json::json!({
        "type": "message",
        "id": serde_json::Value::Null,
        "role": "user",
        "content": [ { "type": "input_text", "text": "hello 2" } ]
    });
    assert_eq!(body2["input"][0], body1["input"][0]);
    assert_ne!(body2["input"][1], body1["input"][1]);
    let env2_text = body2["input"][1]["content"][0]["text"].as_str().unwrap_or("");
    assert!(env2_text.contains("<sandbox_mode>workspace-write</sandbox_mode>"));
    assert!(env2_text.contains("<network_access>enabled</network_access>"));
    assert_eq!(body2["input"][2], body1["input"][2]);
    assert!(
        body2["input"].as_array().unwrap().iter().any(|v| v == &expected_user_message_2),
        "second request should contain the new user message"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_turn_overrides_keep_cached_prefix_and_key_constant() {
    if std::net::TcpListener::bind("127.0.0.1:0").is_err() {
        println!("Skipping test due to sandbox network bind restrictions.");
        return;
    }
    use pretty_assertions::assert_eq;

    let server = MockServer::start().await;

    let sse = sse_completed("resp");
    let template = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(sse, "text/event-stream");

    // Expect two POSTs to /v1/responses
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(template)
        .expect(2)
        .mount(&server)
        .await;

    let model_provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };

    let cwd = TempDir::new().unwrap();
    let codex_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&codex_home);
    config.cwd = cwd.path().to_path_buf();
    config.model_provider = model_provider;
    config.user_instructions = Some("be consistent and helpful".to_string());

    let conversation_manager = ConversationManager::default();
    let codex = conversation_manager
        .new_conversation_with_auth(config.clone(), Some(CodexAuth::from_api_key("Test API Key")))
        .await
        .expect("create new conversation")
        .conversation;

    // First turn
    codex
        .submit(Op::UserInput {
            items: vec![InputItem::Text {
                text: "hello 1".into(),
            }],
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;

    // Second turn using ConfigureSession + UserInput
    let new_cwd = TempDir::new().unwrap();
    let writable = TempDir::new().unwrap();
    codex
        .submit(Op::ConfigureSession {
            provider: config.model_provider.clone(),
            model: "o3".to_string(),
            model_reasoning_effort: ReasoningEffort::High,
            model_reasoning_summary: ReasoningSummary::Detailed,
            model_text_verbosity: config.model_text_verbosity,
            user_instructions: config.user_instructions.clone(),
            base_instructions: config.base_instructions.clone(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![writable.path().to_path_buf()],
                network_access: true,
                exclude_tmpdir_env_var: true,
                exclude_slash_tmp: true,
            },
            disable_response_storage: config.disable_response_storage,
            notify: config.notify.clone(),
            cwd: new_cwd.path().to_path_buf(),
            resume_path: None,
        })
        .await
        .unwrap();
    codex
        .submit(Op::UserInput { items: vec![InputItem::Text { text: "hello 2".into() }] })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;

    // Verify we issued exactly two requests, and the cached prefix stayed identical.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "expected two POST requests");

    let body1 = requests[0].body_json::<serde_json::Value>().unwrap();
    let body2 = requests[1].body_json::<serde_json::Value>().unwrap();

    // prompt_cache_key should remain constant across per-turn overrides
    assert_eq!(
        body1["prompt_cache_key"], body2["prompt_cache_key"],
        "prompt_cache_key should not change across per-turn overrides"
    );

    // Developer and user-instructions should remain identical; the new user message
    // should be present (it may not be the last item if ephemeral status items are appended).
    let expected_user_message_2 = serde_json::json!({
        "type": "message",
        "id": serde_json::Value::Null,
        "role": "user",
        "content": [ { "type": "input_text", "text": "hello 2" } ]
    });
    assert_eq!(body2["input"][0], body1["input"][0]);
    assert_eq!(body2["input"][2], body1["input"][2]);
    assert!(
        body2["input"].as_array().unwrap().iter().any(|v| v == &expected_user_message_2),
        "second request should contain the new user message"
    );
}
