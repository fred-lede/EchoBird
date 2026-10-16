use std::collections::BTreeMap;

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::ProxyError;

pub(crate) const RESPONSES_REQUEST: &str = "_echobird_responses";

pub(crate) fn alias(namespace: &str, name: &str) -> String {
    if namespace.is_empty() || namespace == "functions" {
        name.into()
    } else {
        format!("{namespace}__{name}")
    }
}

/// Retain each original declaration so grammar and namespace survive a native hop.
pub(crate) fn catalog(request: &Value) -> Result<BTreeMap<String, Value>, ProxyError> {
    let mut catalog = BTreeMap::new();
    for tool in request["tools"].as_array().into_iter().flatten().chain(
        request["input"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["type"] == "additional_tools")
            .flat_map(|item| item["tools"].as_array().into_iter().flatten()),
    ) {
        collect(tool, "", &mut catalog)?;
    }
    Ok(catalog)
}

fn collect(
    tool: &Value,
    namespace: &str,
    catalog: &mut BTreeMap<String, Value>,
) -> Result<(), ProxyError> {
    if tool["type"] == "namespace" {
        if !namespace.is_empty() {
            return Err(ProxyError::new(
                StatusCode::BAD_REQUEST,
                "Nested tool namespaces are unsupported",
            ));
        }
        let namespace = tool["name"].as_str().unwrap_or_default();
        for child in tool["tools"].as_array().into_iter().flatten() {
            collect(child, namespace, catalog)?;
        }
        return Ok(());
    }
    if tool["type"] != "function" && tool["type"] != "custom" {
        return Err(ProxyError::new(StatusCode::BAD_REQUEST, "The router accepts client function and custom tools only; hosted tools require their existing client setting to be disabled"));
    }
    let name = tool["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ProxyError::new(StatusCode::BAD_REQUEST, "Client tool name is required"))?;
    let mut original = tool.clone();
    if !namespace.is_empty() {
        original["namespace"] = json!(namespace);
    }
    let name = alias(namespace, name);
    if let Some(previous) = catalog.insert(name, original.clone()) {
        if previous != original {
            return Err(ProxyError::new(
                StatusCode::BAD_REQUEST,
                "Client tool names collide after namespace conversion",
            ));
        }
    }
    Ok(())
}

pub(crate) fn chat_tools(catalog: &BTreeMap<String, Value>) -> Vec<Value> {
    catalog.iter().map(|(name, tool)| {
        let mut function = tool.clone();
        function.as_object_mut().unwrap().remove("type");
        function.as_object_mut().unwrap().remove("namespace");
        function["name"] = json!(name);
        if tool["type"] == "custom" {
            function.as_object_mut().unwrap().remove("format");
            let mut description = tool["description"].as_str().unwrap_or_default().to_string();
            if tool["format"]["type"] == "grammar" {
                description.push_str(&format!("\nInput must follow this {} grammar:\n{}", tool["format"]["syntax"].as_str().unwrap_or("provided"), tool["format"]["definition"].as_str().unwrap_or_default()));
            }
            function["description"] = json!(description);
            function["parameters"] = json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false});
        }
        json!({"type":"function","function":function})
    }).collect()
}

pub(crate) fn custom_input(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| value["input"].as_str().map(str::to_owned))
        .unwrap_or_else(|| arguments.into())
}
