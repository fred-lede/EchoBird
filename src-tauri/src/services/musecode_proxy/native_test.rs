//! Opt-in contract check against an installed CLI, with no real credentials.

use std::sync::{Arc, Mutex};

use axum::{routing::post, Json, Router};
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set ECHOBIRD_MUSE_NATIVE_EXE to an installed Muse Code executable"]
async fn native_cli_reads_a_file_and_replays_the_result_through_chat_bridge() {
    let executable =
        std::env::var_os("ECHOBIRD_MUSE_NATIVE_EXE").expect("installed CLI executable");
    let dir = std::env::temp_dir().join(format!("echobird-muse-native-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("config/muse")).unwrap();
    std::fs::write(dir.join("tool-input.txt"), "bridge-file-marker\n").unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let upstream = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            captured.lock().unwrap().push(body.clone());
            let replayed = body["messages"].as_array().unwrap().iter().any(|message| {
                message["role"] == "tool" && message["content"].as_str().is_some_and(|s| s.contains("bridge-file-marker"))
            });
            let delta = if replayed {
                json!({"content":"bridge-native-ok"})
            } else {
                json!({"tool_calls":[{"index":0,"id":"native_call","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"tool-input.txt\",\"offset\":1,\"limit\":2}"}}]})
            };
            let chunk = json!({"choices":[{"index":0,"delta":delta,"finish_reason":if replayed {"stop"} else {"tool_calls"}}]});
            let usage = json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}});
            ([ ("content-type", "text/event-stream") ], format!("data: {chunk}\n\ndata: {usage}\n\ndata: [DONE]\n\n"))
        }
    }));
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = upstream_listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(upstream_listener, upstream).await.unwrap();
    });
    let relay = dir.join("relay.json");
    std::fs::write(&relay, json!({"name":"Native fixture","model":"fixture-model","baseUrl":format!("http://127.0.0.1:{upstream_port}/v1"),"apiKey":"fake-native-fixture","protocol":"openai"}).to_string()).unwrap();
    let bridge = super::server::router(relay).unwrap();
    let bridge_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bridge_port = bridge_listener.local_addr().unwrap().port();
    let bridge_task = tokio::spawn(async move {
        axum::serve(bridge_listener, bridge).await.unwrap();
    });
    std::fs::write(dir.join("config/muse/settings.json"), json!({"schema_version":1,"provider":"meta","model":"fixture-model","endpoint_transport":{"base_url":format!("http://127.0.0.1:{bridge_port}/v1"),"auth":"none"}}).to_string()).unwrap();
    let mut command = crate::utils::process::async_command(executable);
    command.current_dir(&dir).kill_on_drop(true).args([
        "exec",
        "--json",
        "--disable-sandbox",
        "--disable-approval",
        "--disable-shell",
        "--no-session-log",
        "--disable-reminders",
        "--disable-web-tools",
        "--no-foreign-personal-context",
        "--max-model-steps",
        "3",
        "Read tool-input.txt, then reply bridge-native-ok.",
    ]);
    for key in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        command.env(key, &dir);
    }
    command.env("XDG_CONFIG_HOME", dir.join("config"));
    for key in ["XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"] {
        command.env(key, dir.join(key));
    }
    command.env("MUSE_AUTH_PATH", dir.join("config/muse/auth.json"));
    command.env("NO_PROXY", "127.0.0.1,localhost");
    for key in [
        "META_API_KEY",
        "MODEL_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
    ] {
        command.env_remove(key);
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(45), command.output()).await;
    upstream_task.abort();
    bridge_task.abort();
    let captured = requests.lock().unwrap().clone();
    std::fs::remove_dir_all(&dir).unwrap();
    let output = result
        .expect("native CLI deadline")
        .expect("native CLI process");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}\n{}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<Value> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["payload_type"] == "run.terminal.completed"
                && event["payload"]["terminal"] == "completed"
                && event["payload"]["text"] == "bridge-native-ok"),
        "{stdout}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["payload_type"] == "tool.result"
                && event["payload"]["correlation_facts"]["outcome"] == "success"
                && event["payload"]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("bridge-file-marker"))),
        "native read_file did not succeed: {stdout}"
    );
    let replay = captured
        .iter()
        .find(|request| {
            request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["role"] == "tool")
        })
        .expect("native tool result reached upstream");
    let calls: Vec<&Value> = replay["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["tool_calls"].as_array().into_iter().flatten())
        .collect();
    assert!(replay["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool"
            && message["tool_call_id"] == "native_call"
            && message["content"]
                .as_str()
                .is_some_and(|text| text.contains("bridge-file-marker"))));
    assert!(calls
        .iter()
        .any(|call| call["function"]["name"] == "read_file"));
    assert!(!calls
        .iter()
        .any(|call| call["function"]["name"] == "muse.read_file"));
}
