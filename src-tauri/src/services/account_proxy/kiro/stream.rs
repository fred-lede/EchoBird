use super::super::{
    json_response,
    protocol::{chat_sse, ChatDecoder, WireProtocol},
    stream_response,
};
use super::{
    eventstream::{Frame, Parser},
    failure, ProxyError,
};
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    pin::Pin,
};
use tokio_stream::wrappers::ReceiverStream;

const OUTPUT_LIMIT: usize = 32 * 1024 * 1024;
const ARGUMENT_LIMIT: usize = 1024 * 1024;

#[derive(Default)]
struct Tool {
    name: String,
    arguments: String,
    stopped: bool,
}

#[derive(Default)]
struct Thinking {
    buffer: String,
    state: u8,
}

impl Thinking {
    fn feed(&mut self, text: &str) -> Vec<(String, bool)> {
        const OPEN: &str = "<thinking>";
        const CLOSE: &str = "</thinking>";
        self.buffer.push_str(text);
        let mut output = Vec::new();
        if self.state == 0 {
            let text = self.buffer.trim_start();
            if let Some(content) = text.strip_prefix(OPEN) {
                self.buffer = content.into();
                self.state = 1;
            } else if OPEN.starts_with(text) {
                return output;
            } else {
                self.state = 2;
            }
        }
        if self.state == 1 {
            if let Some(end) = self.buffer.find(CLOSE) {
                if end > 0 {
                    output.push((self.buffer[..end].into(), true));
                }
                self.buffer = self.buffer[end + CLOSE.len()..].into();
                self.state = 2;
            } else {
                let keep = (1..CLOSE.len())
                    .rev()
                    .find(|size| self.buffer.ends_with(&CLOSE[..*size]))
                    .unwrap_or(0);
                let emitted = self.buffer.len() - keep;
                if emitted > 0 {
                    output.push((self.buffer[..emitted].into(), true));
                    self.buffer.drain(..emitted);
                }
                return output;
            }
        }
        if self.state == 2 && !self.buffer.is_empty() {
            output.push((std::mem::take(&mut self.buffer), false));
        }
        output
    }
    fn end(&mut self) -> Result<Vec<(String, bool)>, ProxyError> {
        if self.state == 1 {
            return Err(ProxyError::upstream("Kiro ended inside a thinking block"));
        }
        Ok(if self.buffer.is_empty() {
            vec![]
        } else {
            vec![(std::mem::take(&mut self.buffer), false)]
        })
    }
}

struct Decoder {
    chat: ChatDecoder,
    names: BTreeMap<String, String>,
    allowed: BTreeSet<String>,
    tools: BTreeMap<String, Tool>,
    order: Vec<String>,
    thinking: Option<Thinking>,
    output_size: usize,
    answered: bool,
    usage: Option<Value>,
    reason: String,
}

impl Decoder {
    fn new(
        model: &str,
        names: BTreeMap<String, String>,
        allowed: BTreeSet<String>,
        thinking: bool,
    ) -> Self {
        Self {
            chat: ChatDecoder::new(WireProtocol::Chat, model),
            names,
            allowed,
            tools: BTreeMap::new(),
            order: vec![],
            thinking: thinking.then(Thinking::default),
            output_size: 0,
            answered: false,
            usage: None,
            reason: "stop".into(),
        }
    }
    fn emit(&mut self, delta: Value) -> Result<Vec<Bytes>, ProxyError> {
        self.answered = true;
        self.chat
            .decode(
                "",
                &json!({"choices":[{"delta":delta,"finish_reason":null}]}).to_string(),
            )
            .map_err(ProxyError::upstream)
    }
    fn texts(&mut self, pieces: Vec<(String, bool)>) -> Result<Vec<Bytes>, ProxyError> {
        let mut output = Vec::new();
        for (text, reasoning) in pieces {
            if !text.is_empty() {
                output.extend(self.emit(if reasoning {
                    json!({"reasoning_content":text})
                } else {
                    json!({"content":text})
                })?);
            }
        }
        Ok(output)
    }
    fn decode(&mut self, frame: Frame, headers: &HeaderMap) -> Result<Vec<Bytes>, ProxyError> {
        let message = frame
            .headers
            .get(":message-type")
            .map(String::as_str)
            .unwrap_or_default();
        if message == "error" {
            let kind = frame
                .headers
                .get(":error-code")
                .map(String::as_str)
                .unwrap_or_default();
            let message = frame
                .headers
                .get(":error-message")
                .map(String::as_str)
                .unwrap_or("Kiro upstream refused the request");
            return Err(failure(
                StatusCode::BAD_GATEWAY,
                headers,
                &json!({"message":message}),
                kind,
            ));
        }
        let value: Value = serde_json::from_slice(&frame.payload)
            .map_err(|_| ProxyError::upstream("Invalid Kiro event JSON"))?;
        if message == "exception" || message == "error" {
            let kind = frame
                .headers
                .get(":exception-type")
                .or_else(|| frame.headers.get(":error-code"))
                .map(String::as_str)
                .unwrap_or_default();
            return Err(failure(StatusCode::BAD_GATEWAY, headers, &value, kind));
        }
        if message != "event" {
            return Err(ProxyError::upstream("Invalid Kiro event message type"));
        }
        let kind = frame
            .headers
            .get(":event-type")
            .map(String::as_str)
            .ok_or_else(|| ProxyError::upstream("Kiro event has no type"))?;
        match kind {
            "assistantResponseEvent" => {
                let text = value["content"]
                    .as_str()
                    .ok_or_else(|| ProxyError::upstream("Kiro text event has no content"))?;
                self.output_size = self.output_size.saturating_add(text.len());
                if self.output_size > OUTPUT_LIMIT {
                    return Err(ProxyError::upstream(
                        "Kiro response exceeded its output limit",
                    ));
                }
                let pieces = if let Some(parser) = &mut self.thinking {
                    parser.feed(text)
                } else {
                    vec![(text.into(), false)]
                };
                self.texts(pieces)
            }
            "reasoningContentEvent" => {
                if let Some(text) = value["text"].as_str() {
                    self.output_size = self.output_size.saturating_add(text.len());
                    if self.output_size > OUTPUT_LIMIT {
                        return Err(ProxyError::upstream(
                            "Kiro response exceeded its output limit",
                        ));
                    }
                    self.texts(vec![(text.into(), true)])
                } else if value["signature"].is_string() || value["redactedContent"].is_string() {
                    Ok(vec![])
                } else {
                    Err(ProxyError::upstream("Invalid Kiro reasoning event"))
                }
            }
            "toolUseEvent" => self.tool(&value),
            "metadataEvent" | "messageMetadataEvent" => {
                if let Some(reason) = value["stopReason"].as_str() {
                    self.reason = match reason.to_ascii_lowercase().as_str() {
                        "end_turn" | "stop" => "stop",
                        "tool_use" | "tool_calls" => "tool_calls",
                        "max_tokens" => "length",
                        "content_filtered" => "content_filter",
                        _ => return Err(ProxyError::upstream("Unsupported Kiro stop reason")),
                    }
                    .into();
                }
                if let Some(usage) = value.get("tokenUsage") {
                    self.usage = Some(usage_json(usage)?);
                }
                Ok(vec![])
            }
            "invalidStateEvent"
            | "error"
            | "throttlingError"
            | "validationError"
            | "serviceUnavailableError"
            | "internalServerException"
            | "serviceQuotaExceededError"
            | "accessDeniedError" => Err(failure(StatusCode::BAD_GATEWAY, headers, &value, kind)),
            // Citation/context/telemetry events do not carry assistant content or exact token counts.
            _ => Ok(vec![]),
        }
    }
    fn tool(&mut self, value: &Value) -> Result<Vec<Bytes>, ProxyError> {
        let id = if let Some(id) = value["toolUseId"].as_str().filter(|id| !id.is_empty()) {
            id.to_string()
        } else if self.tools.len() == 1 {
            self.order[0].clone()
        } else {
            return Err(ProxyError::upstream(
                "Kiro tool fragment has no unambiguous call ID",
            ));
        };
        if id.len() > 256 {
            return Err(ProxyError::upstream("Invalid Kiro tool call ID"));
        }
        if !self.tools.contains_key(&id) {
            if self.tools.len() >= 256 {
                return Err(ProxyError::upstream(
                    "Kiro response returned too many tool calls",
                ));
            }
            let name = value["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| ProxyError::upstream("Kiro tool call has no name"))?;
            if !self.allowed.contains(name) {
                return Err(super::request::unsupported(
                    "Kiro returned a tool that was not offered for this turn",
                ));
            }
            self.tools.insert(
                id.clone(),
                Tool {
                    name: name.into(),
                    ..Tool::default()
                },
            );
            self.order.push(id.clone());
        }
        let index = self
            .order
            .iter()
            .position(|known| known == &id)
            .expect("registered tool");
        let state = self.tools.get_mut(&id).expect("registered tool");
        if state.stopped
            || value["name"]
                .as_str()
                .is_some_and(|name| !name.is_empty() && name != state.name)
        {
            return Err(ProxyError::upstream("Kiro changed or reopened a tool call"));
        }
        let fragment = if let Some(input) = value.get("input").filter(|input| !input.is_null()) {
            if let Some(text) = input.as_str() {
                text.into()
            } else if input.is_object() {
                if input.as_object().is_some_and(|value| value.is_empty()) {
                    String::new()
                } else {
                    input.to_string()
                }
            } else {
                return Err(ProxyError::upstream("Invalid Kiro tool argument fragment"));
            }
        } else {
            String::new()
        };
        if state.arguments.len().saturating_add(fragment.len()) > ARGUMENT_LIMIT {
            return Err(ProxyError::upstream(
                "Kiro tool arguments exceeded their limit",
            ));
        }
        self.output_size = self.output_size.saturating_add(fragment.len());
        if self.output_size > OUTPUT_LIMIT {
            return Err(ProxyError::upstream(
                "Kiro response exceeded its output limit",
            ));
        }
        state.arguments.push_str(&fragment);
        if let Some(stop) = value.get("stop") {
            state.stopped = stop
                .as_bool()
                .ok_or_else(|| ProxyError::upstream("Invalid Kiro tool stop marker"))?;
        }
        let name = self
            .names
            .get(&state.name)
            .cloned()
            .ok_or_else(|| ProxyError::upstream("Kiro returned an unknown tool name"))?;
        let mut output = if let Some(parser) = &mut self.thinking {
            let pieces = parser.end()?;
            self.texts(pieces)?
        } else {
            vec![]
        };
        output.extend(self.emit(json!({"tool_calls":[{"index":index,"id":id,"type":"function","function":{"name":name,"arguments":fragment}}]}))?);
        Ok(output)
    }
    fn finish(&mut self) -> Result<Vec<Bytes>, ProxyError> {
        let mut output = if let Some(parser) = &mut self.thinking {
            let pieces = parser.end()?;
            self.texts(pieces)?
        } else {
            vec![]
        };
        for id in self.order.clone() {
            let tool = self.tools.get_mut(&id).expect("registered tool");
            if !tool.stopped {
                return Err(ProxyError::upstream(
                    "Kiro ended with an incomplete function call",
                ));
            }
            if tool.arguments.is_empty() {
                tool.arguments = "{}".into();
                let index = self
                    .order
                    .iter()
                    .position(|known| known == &id)
                    .expect("registered tool");
                output.extend(
                    self.emit(
                        json!({"tool_calls":[{"index":index,"function":{"arguments":"{}"}}]}),
                    )?,
                );
            }
            let tool = &self.tools[&id];
            if !serde_json::from_str::<Value>(&tool.arguments)
                .is_ok_and(|arguments| arguments.is_object())
            {
                return Err(ProxyError::upstream(
                    "Kiro ended with incomplete function arguments",
                ));
            }
        }
        if !self.answered {
            return Err(ProxyError::upstream(
                "Kiro stream ended without an assistant response",
            ));
        }
        if self.reason == "tool_calls" && self.tools.is_empty() {
            return Err(ProxyError::upstream(
                "Kiro ended without its announced tool calls",
            ));
        }
        if let Some(usage) = &self.usage {
            self.chat
                .decode("", &json!({"choices":[],"usage":usage}).to_string())
                .map_err(ProxyError::upstream)?;
        }
        let reason = if self.tools.is_empty() {
            self.reason.as_str()
        } else {
            "tool_calls"
        };
        output.extend(self.chat.complete(reason));
        Ok(output)
    }
}

fn usage_json(value: &Value) -> Result<Value, ProxyError> {
    let number = |key: &str| -> Result<Option<u64>, ProxyError> {
        value
            .get(key)
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_u64()
                    .ok_or_else(|| ProxyError::upstream("Invalid Kiro token usage"))
            })
            .transpose()
    };
    if !value.is_object() {
        return Err(ProxyError::upstream("Invalid Kiro token usage"));
    }
    let uncached = number("uncachedInputTokens")?;
    let input = uncached.or(number("inputTokens")?);
    let output = number("outputTokens")?;
    let cached = number("cacheReadInputTokens")?;
    let written = number("cacheWriteInputTokens")?;
    let mut usage = json!({});
    if let Some(input) = input {
        usage["prompt_tokens"] = json!(if uncached.is_some() {
            input
                .saturating_add(cached.unwrap_or(0))
                .saturating_add(written.unwrap_or(0))
        } else {
            input
        });
    }
    if let Some(output) = output {
        usage["completion_tokens"] = json!(output);
    }
    if let Some(total) = number("totalTokens")? {
        usage["total_tokens"] = json!(total);
    } else if let (Some(input), Some(output)) = (usage["prompt_tokens"].as_u64(), output) {
        usage["total_tokens"] = json!(input.saturating_add(output));
    }
    if let Some(cached) = cached {
        usage["prompt_tokens_details"] = json!({"cached_tokens":cached});
    }
    if let Some(written) = written {
        usage["prompt_tokens_details"]["cache_write_tokens"] = json!(written);
    }
    Ok(usage)
}

type Upstream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;
struct Source {
    upstream: Upstream,
    parser: Parser,
    decoder: Decoder,
    headers: HeaderMap,
    finished: bool,
}
impl Source {
    async fn next(&mut self) -> Result<Option<Vec<Bytes>>, ProxyError> {
        if self.finished {
            return Ok(None);
        }
        loop {
            if let Some(frame) = self.parser.next()? {
                let output = self.decoder.decode(frame, &self.headers)?;
                if !output.is_empty() {
                    return Ok(Some(output));
                }
                continue;
            }
            match self.upstream.next().await {
                Some(Ok(bytes)) => self.parser.push(&bytes)?,
                Some(Err(_)) => {
                    return Err(ProxyError::upstream("Kiro event stream transport failed"))
                }
                None => {
                    self.parser.finish()?;
                    let output = self.decoder.finish()?;
                    self.finished = true;
                    return Ok(Some(output));
                }
            }
        }
    }
}

pub(super) async fn convert(
    response: reqwest::Response,
    model: &str,
    streaming: bool,
    names: BTreeMap<String, String>,
    allowed: BTreeSet<String>,
    thinking: bool,
) -> Result<Response, ProxyError> {
    let headers = response.headers().clone();
    let mut source = Source {
        upstream: Box::pin(response.bytes_stream()),
        parser: Parser::default(),
        decoder: Decoder::new(model, names, allowed, thinking),
        headers,
        finished: false,
    };
    let first = source
        .next()
        .await?
        .ok_or_else(|| ProxyError::upstream("Kiro returned an empty event stream"))?;
    if !streaming {
        while source.next().await?.is_some() {}
        return Ok(json_response(&source.decoder.chat.collected()));
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        for chunk in first {
            if tx.send(Ok(chunk)).await.is_err() {
                return;
            }
        }
        loop {
            let next = tokio::select! { _=tx.closed()=>return, next=source.next()=>next };
            match next {
                Ok(Some(chunks)) => {
                    for chunk in chunks {
                        if tx.send(Ok(chunk)).await.is_err() {
                            return;
                        }
                    }
                }
                Ok(None) => return,
                Err(error) => {
                    let _=tx.send(Ok(chat_sse(json!({"error":{"message":error.message,"type":"upstream_error","code":error.error_code}})))).await;
                    return;
                }
            }
        }
    });
    Ok(stream_response(Body::from_stream(ReceiverStream::new(rx))))
}
