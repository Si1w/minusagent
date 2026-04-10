use std::sync::Arc;

use anyhow::Result;
use futures_util::StreamExt;
use serde::Deserialize;

use super::{LLMResponse, ResponseToolCall, Usage};
use crate::frontend::Channel;

#[derive(Debug, Deserialize)]
struct StreamUsage {
    #[serde(rename = "prompt_tokens")]
    prompt: usize,
    #[serde(rename = "completion_tokens")]
    completion: usize,
    #[serde(rename = "total_tokens")]
    total: usize,
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
    usage: Option<StreamUsage>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCall {
    index: usize,
    id: Option<String>,
    function: Option<StreamFunction>,
}

#[derive(Debug, Deserialize)]
struct StreamFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Default)]
struct StreamAccumulator {
    buffer: String,
    content: String,
    tool_calls: Vec<ResponseToolCall>,
    usage: Option<Usage>,
}

pub(super) async fn collect_response(
    response: reqwest::Response,
    channel: &Arc<dyn Channel>,
) -> Result<LLMResponse> {
    let mut accumulator = StreamAccumulator::default();
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        accumulator.push_chunk(&chunk?, channel).await;
    }

    Ok(accumulator.finish())
}

impl StreamAccumulator {
    async fn push_chunk(&mut self, chunk: &[u8], channel: &Arc<dyn Channel>) {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));

        while let Some(line_end) = self.buffer.find('\n') {
            let line = self.buffer[..line_end].trim().to_string();
            self.buffer.drain(..=line_end);

            let Some(data) = parse_stream_data(&line) else {
                continue;
            };
            let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) else {
                continue;
            };

            self.apply_chunk(chunk, channel).await;
        }
    }

    async fn apply_chunk(&mut self, chunk: StreamChunk, channel: &Arc<dyn Channel>) {
        self.update_usage(chunk.usage);

        let Some(choice) = chunk.choices.into_iter().next() else {
            return;
        };

        self.append_content(choice.delta.content, channel).await;
        self.merge_tool_calls(choice.delta.tool_calls);
    }

    fn update_usage(&mut self, usage: Option<StreamUsage>) {
        self.usage = usage.map(|usage| Usage {
            prompt_tokens: usage.prompt,
            completion_tokens: usage.completion,
            total_tokens: usage.total,
        });
    }

    async fn append_content(&mut self, content: Option<String>, channel: &Arc<dyn Channel>) {
        let Some(content) = content.filter(|value| !value.is_empty()) else {
            return;
        };

        channel.on_stream_chunk(&content).await;
        self.content.push_str(&content);
    }

    fn merge_tool_calls(&mut self, tool_calls: Option<Vec<StreamToolCall>>) {
        let Some(tool_calls) = tool_calls else {
            return;
        };

        for tool_call in tool_calls {
            self.ensure_tool_call_slot(&tool_call);
            self.update_tool_call(tool_call);
        }
    }

    fn ensure_tool_call_slot(&mut self, tool_call: &StreamToolCall) {
        while tool_call.index >= self.tool_calls.len() {
            self.tool_calls.push(ResponseToolCall {
                id: tool_call.id.clone().unwrap_or_default(),
                name: tool_call
                    .function
                    .as_ref()
                    .and_then(|function| function.name.clone())
                    .unwrap_or_default(),
                arguments: String::new(),
            });
        }
    }

    fn update_tool_call(&mut self, tool_call: StreamToolCall) {
        if let Some(id) = tool_call.id {
            self.tool_calls[tool_call.index].id = id;
        }

        if let Some(function) = tool_call.function {
            if let Some(name) = function.name {
                self.tool_calls[tool_call.index].name = name;
            }
            if let Some(arguments) = function.arguments {
                self.tool_calls[tool_call.index]
                    .arguments
                    .push_str(&arguments);
            }
        }
    }

    fn finish(self) -> LLMResponse {
        LLMResponse {
            content: (!self.content.is_empty()).then_some(self.content),
            tool_calls: (!self.tool_calls.is_empty()).then_some(self.tool_calls),
            usage: self.usage,
        }
    }
}

fn parse_stream_data(line: &str) -> Option<&str> {
    if line.is_empty() || line == "data: [DONE]" {
        return None;
    }

    line.strip_prefix("data: ")
}
