use bytes::Bytes;
use futures::{StreamExt, stream};
use jcode_message_types::StreamEvent;
use jcode_provider_openai::stream::OpenAIResponsesStream;
use serde_json::json;

#[test]
fn interleaved_function_calls_keep_their_arguments_and_emit_once() {
    let first = json!({
        "type": "function_call", "id": "fc_first", "call_id": "call_first",
        "name": "read", "arguments": "{\"file_path\":\"first.rs\"}"
    });
    let second = json!({
        "type": "function_call", "id": "fc_second", "call_id": "call_second",
        "name": "read", "arguments": "{\"file_path\":\"second.rs\"}"
    });
    let events = [
        json!({"type": "response.output_item.added", "item": {
            "type": "function_call", "id": "fc_first", "call_id": "call_first",
            "name": "read", "arguments": ""
        }}),
        json!({"type": "response.output_item.added", "item": {
            "type": "function_call", "id": "fc_second", "call_id": "call_second",
            "name": "read", "arguments": ""
        }}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_first", "delta": "{\"file_path\":"}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_second", "delta": "{\"file_path\":\"second.rs\"}"}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_first", "delta": "\"first.rs\"}"}),
        json!({"type": "response.function_call_arguments.done", "item_id": "fc_first"}),
        json!({"type": "response.function_call_arguments.done", "item_id": "fc_second"}),
        json!({"type": "response.output_item.done", "item": first}),
        json!({"type": "response.output_item.done", "item": second}),
        json!({"type": "response.completed", "response": {"id": "resp_tools", "status": "completed"}}),
    ];
    let wire: String = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    let chunks: Vec<_> = wire
        .as_bytes()
        .chunks(17)
        .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
        .collect();
    let output = futures::executor::block_on(
        OpenAIResponsesStream::new(stream::iter(chunks)).collect::<Vec<_>>(),
    );
    let output: Vec<_> = output
        .into_iter()
        .map(|event| event.expect("valid stream"))
        .collect();

    assert_eq!(
        output.len(),
        7,
        "two complete calls and one response completion"
    );
    for (events, id, path) in [
        (&output[..3], "call_first", "first.rs"),
        (&output[3..6], "call_second", "second.rs"),
    ] {
        assert!(
            matches!(&events[0], StreamEvent::ToolUseStart { id: actual, name } if actual == id && name == "read")
        );
        let StreamEvent::ToolInputDelta(arguments) = &events[1] else {
            panic!("expected complete arguments");
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(arguments).expect("JSON arguments"),
            json!({"file_path": path})
        );
        assert!(matches!(&events[2], StreamEvent::ToolUseEnd));
    }
    assert!(matches!(
        output.last(),
        Some(StreamEvent::MessageEnd { stop_reason: None })
    ));
}
