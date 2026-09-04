#[test]
fn modern_openai_requests_use_current_cache_options() {
    for model in [
        "gpt-6-astra",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ] {
        for legacy_retention in [None, Some("24h"), Some("in_memory")] {
            let request = build_test_response_request(
                model,
                false,
                Some(32_768),
                Some("high"),
                None,
                Some("stable-session-key"),
                legacy_retention,
                None,
            );
            assert_eq!(
                request["prompt_cache_options"],
                serde_json::json!({"ttl": "30m"}),
                "{model}"
            );
            assert!(request.get("prompt_cache_retention").is_none());
            assert_eq!(request["prompt_cache_key"], "stable-session-key");
            assert_eq!(request["parallel_tool_calls"], true);
            assert_eq!(request["reasoning"]["effort"], "high");
            assert_eq!(request["store"], false);
            assert_eq!(
                request["include"],
                serde_json::json!(["reasoning.encrypted_content"])
            );
        }
        let oauth_request = build_test_response_request(
            model,
            true,
            Some(32_768),
            Some("high"),
            None,
            Some("api-only-key"),
            Some("24h"),
            None,
        );
        assert!(oauth_request.get("prompt_cache_options").is_none());
        assert!(oauth_request.get("prompt_cache_retention").is_none());
        assert!(oauth_request.get("prompt_cache_key").is_none());
        assert!(oauth_request.get("max_output_tokens").is_none());
        assert_eq!(oauth_request["parallel_tool_calls"], true);
    }
}

#[test]
fn astra_context_window_uses_endpoint_metadata() {
    let _guard = jcode_base::storage::lock_test_env();
    let model = "gpt-6-astra-context-fixture";
    let provider = OpenAIProvider::new_browser_only();
    *provider.model.try_write().expect("model lock") = model.to_string();
    jcode_base::provider::populate_context_limits(HashMap::from([(model.to_string(), 272_000)]));
    assert_eq!(provider.context_window(), 272_000);
}

#[test]
fn astra_reasoning_migration_preserves_supported_efforts() {
    let _guard = jcode_base::storage::lock_test_env();
    let provider = OpenAIProvider::new_browser_only();
    *provider.model.try_write().expect("model lock") = "gpt-6-astra".to_string();
    provider
        .model_reasoning_efforts
        .write()
        .expect("catalog lock")
        .clear();

    assert_eq!(
        provider.available_efforts(),
        vec![
            "low",
            "medium",
            "high",
            "xhigh",
            "max",
            "swarm",
            "swarm-deep"
        ]
    );
    for old_effort in ["none", "minimal"] {
        provider
            .set_reasoning_effort(old_effort)
            .expect("legacy session effort migrates to low");
        assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
        *provider.reasoning_effort.write().expect("effort lock") = Some(old_effort.to_string());
        provider.revalidate_reasoning_effort();
        assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
    }
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        provider
            .set_reasoning_effort(effort)
            .expect("supported effort");
        provider.revalidate_reasoning_effort();
        assert_eq!(provider.reasoning_effort().as_deref(), Some(effort));
    }

    // Account metadata may narrow the supported ladder, including swarm's ceiling.
    provider
        .model_reasoning_efforts
        .write()
        .expect("catalog lock")
        .insert(
            "gpt-6-astra".to_string(),
            vec!["low".to_string(), "high".to_string()],
        );
    assert_eq!(
        provider.available_efforts(),
        vec!["low", "high", "swarm", "swarm-deep"]
    );
    assert!(provider.set_reasoning_effort("max").is_err());
    assert_eq!(
        provider.api_reasoning_effort(Some("swarm")).as_deref(),
        Some("high")
    );
}

#[tokio::test]
async fn modern_websocket_continuation_preserves_request_settings_on_the_wire() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept");
            let mut ws = tokio_tungstenite::accept_async(socket)
                .await
                .expect("handshake");
            let frame = ws
                .next()
                .await
                .expect("request frame")
                .expect("valid frame");
            let request: Value =
                serde_json::from_str(frame.to_text().expect("text frame")).expect("JSON request");
            ws.send(WsMessage::Text(
                serde_json::json!({
                    "type": "response.created",
                    "response": {"id": "resp_next", "status": "in_progress"}
                })
                .to_string(),
            ))
            .await
            .expect("response creation");
            ws.send(WsMessage::Text(
                serde_json::json!({
                    "type": "response.completed",
                    "response": {"id": "resp_next", "status": "completed"}
                })
                .to_string(),
            ))
            .await
            .expect("completion");
            request
        });
        let (ws_stream, _) = connect_async(format!("ws://{addr}"))
            .await
            .expect("connect");
        let persistent_ws = Arc::new(Mutex::new(Some(PersistentWsState {
            ws_stream,
            last_response_id: "resp_previous".to_string(),
            connected_at: Instant::now(),
            last_activity_at: Instant::now(),
            last_response_completed_at: Instant::now(),
            message_count: 1,
            last_input_item_count: 1,
        })));
        let input = vec![
            serde_json::json!({"role": "user", "content": "first"}),
            serde_json::json!({"type": "reasoning", "id": "rs_previous", "summary": []}),
            serde_json::json!({"type": "function_call_output", "call_id": "call_first", "output": "first result"}),
            serde_json::json!({"type": "function_call_output", "call_id": "call_second", "output": "second result"}),
        ];
        let request = OpenAIProvider::build_response_request(
            "gpt-6-astra",
            "stable instructions".to_string(),
            &input,
            &[serde_json::json!({"type": "function", "name": "read"})],
            false,
            Some(32_768),
            Some("high"),
            None,
            Some("session-key"),
            None,
            None,
        );
        let (tx, _rx) = mpsc::channel(8);
        let result =
            try_persistent_ws_continuation(&persistent_ws, &request, &input, input.len(), &tx)
                .await;
        assert!(matches!(result, PersistentWsResult::Success));
        let sent = server.await.expect("server");
        assert_eq!(sent["previous_response_id"], "resp_previous");
        assert_eq!(sent["input"], serde_json::json!(&input[2..]));
        assert_eq!(
            sent["prompt_cache_options"],
            serde_json::json!({"ttl": "30m"})
        );
        assert!(sent.get("prompt_cache_retention").is_none());
        assert_eq!(sent["parallel_tool_calls"], true);
        for key in [
            "model",
            "tools",
            "instructions",
            "reasoning",
            "include",
            "max_output_tokens",
            "prompt_cache_key",
        ] {
            assert_eq!(sent[key], request[key], "lost {key} during continuation");
        }
        assert!(sent.get("stream").is_none());
        assert!(sent.get("background").is_none());
        assert_eq!(
            persistent_ws
                .lock()
                .await
                .as_ref()
                .expect("reusable socket")
                .last_response_id,
            "resp_next"
        );
    })
    .await
    .expect("bounded WebSocket test");
}
