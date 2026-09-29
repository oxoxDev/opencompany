//! Tests for the host-side tool loop: tool calls are executed in-process
//! and their results fed back, the cap ends the loop, usage is summed.

use super::*;
use async_trait::async_trait;
use std::sync::Mutex;
use tinyinference::model::ModelProfile;
use tinyinference::tool::ToolCall;
use tinyinference::usage::Usage;
use tinytools::ToolResult;

struct Recorder(Mutex<Vec<serde_json::Value>>);

#[async_trait]
impl Tool for Recorder {
    fn name(&self) -> &str {
        "record"
    }
    fn description(&self) -> &str {
        "records"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.0.lock().unwrap().push(args);
        Ok(ToolResult::success("recorded"))
    }
}

/// Calls `record` on every iteration until told to stop, so a cap of 2
/// ends on `HitCap` and a script that stops on the second call replies.
struct Model {
    stop_after: u32,
    calls: Mutex<u32>,
}

#[async_trait]
impl ChatModel<()> for Model {
    fn profile(&self) -> Option<&ModelProfile> {
        None
    }
    async fn invoke(&self, _: &(), request: ModelRequest) -> tinyinference::Result<ModelResponse> {
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        assert_eq!(request.tools.len(), 1);
        let mut response = if *calls > self.stop_after {
            let last = request.messages.last().expect("message");
            assert!(matches!(last, Message::Tool(_)));
            ModelResponse::assistant("finished")
        } else {
            let mut response = ModelResponse::assistant("calling");
            response.message.tool_calls.push(ToolCall {
                id: format!("c{}", *calls),
                name: "record".to_string(),
                arguments: serde_json::json!({"n": *calls}),
                invalid: None,
            });
            response
        };
        response = response.with_usage(Usage {
            input_tokens: 3,
            output_tokens: 1,
            total_tokens: 4,
            ..Usage::default()
        });
        Ok(response)
    }
}

#[tokio::test]
async fn tool_calls_are_executed_and_the_reply_ends_the_loop() {
    let model: Arc<dyn ChatModel<()>> = Arc::new(Model {
        stop_after: 1,
        calls: Mutex::new(0),
    });
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(Recorder(Mutex::new(Vec::new())))];
    let outcome = run(&model, "m", "sys", "go", &tools, 5)
        .await
        .expect("loop");
    assert_eq!(outcome.end, LoopEnd::Replied("finished".to_string()));
    assert_eq!(outcome.usage.input_tokens, 6, "two calls summed");
}

#[tokio::test]
async fn the_cap_ends_a_loop_still_calling_tools() {
    let model: Arc<dyn ChatModel<()>> = Arc::new(Model {
        stop_after: 10,
        calls: Mutex::new(0),
    });
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(Recorder(Mutex::new(Vec::new())))];
    let outcome = run(&model, "m", "sys", "go", &tools, 2)
        .await
        .expect("loop");
    assert_eq!(outcome.end, LoopEnd::HitCap("calling".to_string()));
    assert_eq!(outcome.usage.output_tokens, 2);
}
