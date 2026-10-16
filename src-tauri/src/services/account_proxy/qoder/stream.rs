use super::{
    failure,
    protocol::{ChatDecoder, WireProtocol},
    stream_response, ProxyError,
};
use axum::{
    body::Body,
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use tokio_stream::wrappers::ReceiverStream;

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
    index: usize,
}
struct Decoder {
    chat: ChatDecoder,
    calls: BTreeMap<String, Call>,
    indices: BTreeMap<u64, String>,
    allowed: Vec<String>,
    text: String,
    in_call: bool,
    usage: Option<Value>,
    reason: String,
    started: bool,
}
impl Decoder {
    fn new(model: &str, body: &Value) -> Self {
        Self {
            chat: ChatDecoder::new(WireProtocol::Chat, model),
            calls: BTreeMap::new(),
            indices: BTreeMap::new(),
            allowed: if body["tool_choice"] == "none" {
                vec![]
            } else {
                body["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                    .collect()
            },
            text: String::new(),
            in_call: false,
            usage: None,
            reason: "stop".into(),
            started: false,
        }
    }
    fn decode(&mut self, data: &str, headers: &HeaderMap) -> Result<Vec<Bytes>, ProxyError> {
        let envelope: Value = serde_json::from_str(data)
            .map_err(|_| ProxyError::upstream("Invalid Qoder stream envelope"))?;
        let inner = envelope["body"]
            .as_str()
            .ok_or_else(|| ProxyError::upstream("Qoder stream envelope has no body"))?;
        if let Some(status) = envelope["statusCodeValue"].as_u64().filter(|n| *n != 200) {
            return Err(failure(
                StatusCode::from_u16(status as u16).unwrap_or(StatusCode::BAD_GATEWAY),
                headers,
                &serde_json::from_str::<Value>(inner).unwrap_or(Value::Null),
            ));
        }
        if inner.trim() == "[DONE]" {
            return self.complete();
        }
        let mut value: Value = serde_json::from_str(inner)
            .map_err(|_| ProxyError::upstream("Invalid Qoder stream data"))?;
        if value.get("error").is_some_and(|e| !e.is_null()) {
            return Err(failure(StatusCode::BAD_GATEWAY, headers, &value["error"]));
        }
        if let Some(usage) = value.get("usage").filter(|v| v.is_object()) {
            self.usage = Some(usage.clone());
        }
        let Some(choice) = value
            .get_mut("choices")
            .and_then(Value::as_array_mut)
            .and_then(|choices| choices.first_mut())
        else {
            return Ok(vec![]);
        };
        if let Some(reason) = choice["finish_reason"].as_str() {
            if !matches!(reason, "stop" | "length" | "tool_calls") {
                return Err(ProxyError::upstream(
                    "Qoder returned an unsupported finish reason",
                ));
            }
            self.reason = reason.into();
        }
        let delta = &choice["delta"];
        let mut out = vec![];
        if let Some(reasoning) = delta["reasoning_content"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            out.extend(self.emit(json!({"reasoning_content":reasoning}))?);
        }
        if let Some(text) = delta["content"].as_str() {
            out.extend(self.feed_text(text)?);
        }
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            out.extend(self.native_call(call)?);
        }
        Ok(out)
    }
    fn emit(&mut self, delta: Value) -> Result<Vec<Bytes>, ProxyError> {
        self.started = true;
        self.chat
            .decode(
                "",
                &json!({"choices":[{"delta":delta,"finish_reason":null}]}).to_string(),
            )
            .map_err(ProxyError::upstream)
    }
    fn native_call(&mut self, call: &Value) -> Result<Vec<Bytes>, ProxyError> {
        let id = call["id"].as_str().filter(|s| !s.is_empty());
        let native = call["index"].as_u64();
        let key = if let Some(index) = native {
            if let Some(key) = self.indices.get(&index) {
                key.clone()
            } else {
                let key = id
                    .map(|id| format!("id:{id}"))
                    .unwrap_or_else(|| format!("index:{index}"));
                self.indices.insert(index, key.clone());
                key
            }
        } else if let Some(id) = id {
            format!("id:{id}")
        } else if self.calls.len() == 1 {
            self.calls.keys().next().expect("one call").clone()
        } else {
            return Err(ProxyError::upstream(
                "Qoder tool fragment has no unambiguous identity",
            ));
        };
        let next = self.calls.len();
        let state = self.calls.entry(key).or_insert_with(|| Call {
            id: id
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple())),
            index: next,
            ..Call::default()
        });
        if id.is_some_and(|id| id != state.id) {
            return Err(ProxyError::upstream("Qoder changed a tool call identity"));
        }
        if let Some(kind) = call.get("type") {
            if kind != "function" {
                return Err(ProxyError::upstream(
                    "Qoder returned an unsupported tool type",
                ));
            }
        }
        if let Some(name) = call["function"]["name"].as_str().filter(|s| !s.is_empty()) {
            if !self.allowed.iter().any(|allowed| allowed == name)
                || (!state.name.is_empty() && state.name != name)
            {
                return Err(ProxyError::upstream(
                    "Qoder returned an undeclared or changed tool",
                ));
            }
            state.name = name.into();
        }
        let arguments = call["function"]["arguments"].as_str().unwrap_or("");
        state.arguments.push_str(arguments);
        let normalized = json!({"index":state.index,"id":state.id,"type":"function","function":{"name":state.name,"arguments":arguments}});
        if state.name.is_empty() && arguments.is_empty() {
            return Ok(vec![]);
        }
        self.emit(json!({"tool_calls":[normalized]}))
    }
    fn feed_text(&mut self, text: &str) -> Result<Vec<Bytes>, ProxyError> {
        const OPEN: &str = "<tool_call>";
        const CLOSE: &str = "</tool_call>";
        self.text.push_str(text);
        let mut out = vec![];
        loop {
            if self.in_call {
                let Some(end) = self.text.find(CLOSE) else {
                    break;
                };
                let call = self.text[..end].to_string();
                self.text.drain(..end + CLOSE.len());
                self.in_call = false;
                let (name, args) = parse_call(&call)?;
                let index = self.calls.len();
                out.extend(self.native_call(&json!({"id":format!("call_{}",uuid::Uuid::new_v4().simple()),"index":(u64::MAX-index as u64),"type":"function","function":{"name":name,"arguments":args.to_string()}}))?);
            } else if let Some(start) = self.text.find(OPEN) {
                let text = self.text[..start].to_string();
                self.text.drain(..start + OPEN.len());
                self.in_call = true;
                if !text.is_empty() {
                    out.extend(self.emit(json!({"content":text}))?);
                }
            } else {
                let keep = (1..OPEN.len())
                    .rev()
                    .find(|n| self.text.ends_with(&OPEN[..*n]))
                    .unwrap_or(0);
                let text = self.text[..self.text.len() - keep].to_string();
                self.text.drain(..self.text.len() - keep);
                if !text.is_empty() {
                    out.extend(self.emit(json!({"content":text}))?);
                }
                break;
            }
        }
        Ok(out)
    }
    fn complete(&mut self) -> Result<Vec<Bytes>, ProxyError> {
        if self.in_call {
            return Err(ProxyError::upstream(
                "Qoder ended with an incomplete tool call",
            ));
        }
        for call in self.calls.values() {
            if call.name.is_empty()
                || !serde_json::from_str::<Value>(&call.arguments).is_ok_and(|v| v.is_object())
            {
                return Err(ProxyError::upstream(
                    "Qoder ended with incomplete tool arguments",
                ));
            }
        }
        if self.reason == "tool_calls" && self.calls.is_empty() {
            return Err(ProxyError::upstream("Qoder ended with missing tool calls"));
        }
        let mut out = vec![];
        if !self.text.is_empty() {
            let text = std::mem::take(&mut self.text);
            out.extend(self.emit(json!({"content":text}))?);
        }
        if let Some(usage) = &self.usage {
            out.extend(
                self.chat
                    .decode("", &json!({"choices":[],"usage":usage}).to_string())
                    .map_err(ProxyError::upstream)?,
            );
        }
        if !self.started {
            out.extend(self.emit(json!({"content":""}))?);
        }
        let reason = if self.calls.is_empty() {
            self.reason.as_str()
        } else {
            "tool_calls"
        };
        self.chat
            .decode(
                "",
                &json!({"choices":[{"delta":{},"finish_reason":reason}]}).to_string(),
            )
            .map_err(ProxyError::upstream)?;
        out.extend(
            self.chat
                .decode("", "[DONE]")
                .map_err(ProxyError::upstream)?,
        );
        Ok(out)
    }
}
fn parse_call(text: &str) -> Result<(String, Value), ProxyError> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        if let (Some(name), Some(args)) = (
            v["name"].as_str().filter(|s| !s.is_empty()),
            v.get("arguments").filter(|a| a.is_object()),
        ) {
            return Ok((name.into(), args.clone()));
        }
    }
    let text = text.trim();
    let after = text
        .strip_prefix("<function=")
        .ok_or_else(|| ProxyError::upstream("Invalid Qoder XML tool call"))?;
    let (name, body) = after
        .split_once('>')
        .ok_or_else(|| ProxyError::upstream("Invalid Qoder tool name"))?;
    let mut body = body
        .strip_suffix("</function>")
        .ok_or_else(|| ProxyError::upstream("Incomplete Qoder XML function"))?
        .trim();
    let mut args = Map::new();
    while !body.is_empty() {
        let rest = body
            .strip_prefix("<parameter=")
            .ok_or_else(|| ProxyError::upstream("Invalid Qoder XML parameter"))?;
        let (key, rest) = rest
            .split_once('>')
            .ok_or_else(|| ProxyError::upstream("Invalid Qoder parameter name"))?;
        let (value, rest) = rest
            .split_once("</parameter>")
            .ok_or_else(|| ProxyError::upstream("Incomplete Qoder parameter"))?;
        if key.trim().is_empty() || args.contains_key(key.trim()) {
            return Err(ProxyError::upstream("Invalid duplicate Qoder parameter"));
        }
        args.insert(
            key.trim().into(),
            serde_json::from_str(value.trim()).unwrap_or_else(|_| json!(value.trim())),
        );
        body = rest.trim();
    }
    if name.trim().is_empty() {
        return Err(ProxyError::upstream("Empty Qoder tool name"));
    }
    Ok((name.trim().into(), Value::Object(args)))
}
pub(super) async fn convert(
    response: reqwest::Response,
    model: &str,
    body: &Value,
) -> Result<Response, ProxyError> {
    let streaming = body["stream"] == true;
    let mut decoder = Decoder::new(model, body);
    let headers = response.headers().clone();
    let mut upstream = response.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        let mut parser = super::super::SseParser::default();
        loop {
            let next = tokio::select! {_ = tx.closed()=>return,next=upstream.next()=>next};
            let eof = next.is_none();
            let frames = match next {
                Some(Ok(bytes)) => parser.push(&bytes),
                Some(Err(_)) => {
                    let _ = tx
                        .send(Err(std::io::Error::other(ProxyError::upstream(
                            "Qoder stream transport failed",
                        ))))
                        .await;
                    return;
                }
                None => parser.finish(),
            };
            for (_, data) in frames {
                match decoder.decode(&data, &headers) {
                    Ok(chunks) => {
                        if streaming {
                            for chunk in chunks {
                                if tx.send(Ok(chunk)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(std::io::Error::other(error))).await;
                        return;
                    }
                }
                if decoder.chat.finished {
                    if !streaming {
                        let _ = tx
                            .send(Ok(Bytes::from(decoder.chat.collected().to_string())))
                            .await;
                    }
                    return;
                }
            }
            if eof {
                let _ = tx
                    .send(Err(std::io::Error::other(ProxyError::upstream(
                        "Qoder stream ended before completion",
                    ))))
                    .await;
                return;
            }
        }
    });
    let body = Body::from_stream(ReceiverStream::new(rx));
    Ok(if streaming {
        stream_response(body)
    } else {
        Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .expect("fixed response headers")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(v: Value) -> String {
        json!({"statusCodeValue":200,"body":v.to_string()}).to_string()
    }
    fn tools() -> Value {
        json!({"tools":[{"type":"function","function":{"name":"read"}}]})
    }
    #[test]
    fn native_parallel_fragments_keep_ids_and_usage_after_finish() {
        let mut d = Decoder::new("model", &tools());
        let headers = HeaderMap::new();
        assert!(d
            .decode(
                &frame(json!({"choices":[],"usage":{"prompt_tokens":4}})),
                &headers
            )
            .unwrap()
            .is_empty());
        for calls in [
            json!([{"index":0,"id":"a","function":{"name":"read","arguments":"{\"a\":"}},{"index":1,"id":"b","function":{"name":"read","arguments":"{\"b\":"}}]),
            json!([{"index":1,"function":{"arguments":"2}"}},{"index":0,"function":{"arguments":"1}"}}]),
        ] {
            d.decode(
                &frame(json!({"choices":[{"delta":{"tool_calls":calls}}]})),
                &headers,
            )
            .unwrap();
        }
        d.decode(
            &frame(json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})),
            &headers,
        )
        .unwrap();
        assert!(!d.chat.finished);
        d.decode(&frame(json!({"choices":[],"usage":{"prompt_tokens":4,"completion_tokens":3,"total_tokens":7,"prompt_tokens_details":{"cached_tokens":2}}})),&headers).unwrap();
        d.decode(
            &json!({"statusCodeValue":200,"body":"[DONE]"}).to_string(),
            &headers,
        )
        .unwrap();
        let result = d.chat.collected();
        let calls = &result["choices"][0]["message"]["tool_calls"];
        assert_eq!(calls[0]["id"], "a");
        assert_eq!(calls[0]["function"]["arguments"], "{\"a\":1}");
        assert_eq!(calls[1]["id"], "b");
        assert_eq!(calls[1]["function"]["arguments"], "{\"b\":2}");
        assert_eq!(result["usage"]["total_tokens"], 7);
        assert_eq!(result["usage"]["prompt_tokens_details"]["cached_tokens"], 2);
    }
    #[test]
    fn xml_calls_handle_split_text_and_do_not_turn_truncated_calls_into_success() {
        let mut d = Decoder::new("model", &tools());
        for text in [
            "模型<tool_",
            "call><function=read><parameter=path>\"x\"</parameter></function></tool_call>结束",
        ] {
            d.feed_text(text).unwrap();
        }
        d.complete().unwrap();
        let result = d.chat.collected();
        assert_eq!(result["choices"][0]["message"]["content"], "模型结束");
        assert_eq!(
            result["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"x\"}"
        );
        let mut d = Decoder::new("model", &tools());
        d.feed_text("<tool_call>{\"name\":\"read\"").unwrap();
        assert!(d.complete().is_err());
        assert!(!d.chat.finished);
        let mut d = Decoder::new("model", &json!({"tool_choice":"none"}));
        assert!(d
            .feed_text("<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>")
            .is_err());
    }
    #[test]
    fn first_envelope_errors_preserve_auth_queue_and_model_scope_without_role() {
        let mut d = Decoder::new("model", &tools());
        let err=d.decode(&json!({"statusCodeValue":403,"body":json!({"code":10605,"queue":{"isQueued":true,"retryAfterSeconds":1.5}}).to_string()}).to_string(),&HeaderMap::new()).unwrap_err();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(err.model_scope);
        assert_eq!(err.retry_after.unwrap(), "2");
        assert!(!d.started);
        let err = d
            .decode(
                &json!({"statusCodeValue":401,"body":"{}"}).to_string(),
                &HeaderMap::new(),
            )
            .unwrap_err();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert!(!d.started);
    }
    #[test]
    fn identity_changes_and_ambiguous_parallel_fragments_are_rejected() {
        let mut d = Decoder::new("model", &tools());
        d.native_call(&json!({"index":0,"id":"a","function":{"name":"read","arguments":"{}"}}))
            .unwrap();
        assert!(d
            .native_call(&json!({"index":0,"id":"b","function":{"name":"read","arguments":"{}"}}))
            .is_err());
        d.native_call(&json!({"index":1,"id":"b","function":{"name":"read","arguments":"{}"}}))
            .unwrap();
        assert!(d
            .native_call(&json!({"function":{"arguments":"{}"}}))
            .is_err());
    }
}
