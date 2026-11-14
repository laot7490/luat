use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use serde_json::{Value, json};

#[test]
fn stdio_editor_assistance_providers_and_markdown() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_luat-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = BufReader::new(stdout);
        while let Ok(message) = read_message(&mut output) {
            if sender.send(message).is_err() {
                break;
            }
        }
    });
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{},"initializationOptions":{"inlayHints":{"parameterNames":{"enabled":true},"variableTypes":{"enabled":true}}}}}),
    );
    let initialize = receive_response(&receiver, 1);
    let capabilities = &initialize["result"]["capabilities"];
    assert_eq!(capabilities["documentSymbolProvider"], true);
    assert_eq!(capabilities["workspaceSymbolProvider"], true);
    assert_eq!(
        capabilities["signatureHelpProvider"]["triggerCharacters"],
        json!(["(", ","])
    );
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    );
    let uri = format!(
        "file:///{}",
        std::env::current_dir()
            .unwrap()
            .join("target/tests/editor-assistance.luat")
            .to_string_lossy()
            .replace('\\', "/")
    );
    let source = "---[Native Documentation](https://docs.fivem.net/natives/?_0xE7490533)\n---@param value Açıklama\n---@deprecated Use replacement.\nlocal function old(value: number, unused: string) return value end\nlocal count = 1\nold(2, 'x')\nold(2, 'x')";
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"luat","version":1,"text":source}}}),
    );
    for (id, method, params) in [
        (
            2,
            "textDocument/documentSymbol",
            json!({"textDocument":{"uri":uri}}),
        ),
        (3, "workspace/symbol", json!({"query":"old"})),
        (
            4,
            "textDocument/inlayHint",
            json!({"textDocument":{"uri":uri},"range":{"start":{"line":0,"character":0},"end":{"line":7,"character":0}}}),
        ),
        (
            5,
            "textDocument/completion",
            json!({"textDocument":{"uri":uri},"position":{"line":6,"character":3}}),
        ),
        (
            6,
            "textDocument/codeAction",
            json!({"textDocument":{"uri":uri},"range":{"start":{"line":3,"character":34},"end":{"line":3,"character":40}},"context":{"diagnostics":[]}}),
        ),
    ] {
        send_message(
            &mut input,
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        );
        let response = receive_response(&receiver, id);
        assert!(response.get("error").is_none(), "{response}");
        assert!(
            !response["result"].as_array().unwrap().is_empty(),
            "{method}: {response}"
        );
        if id == 5 {
            let old = response["result"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["label"] == "old")
                .unwrap();
            assert_eq!(old["documentation"]["kind"], "markdown");
            assert!(
                old["documentation"]["value"]
                    .as_str()
                    .unwrap()
                    .contains("[Native Documentation](https://")
            );
            assert_eq!(old["tags"], json!([1]));
        }
    }
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"workspace/didChangeConfiguration","params":{"settings":{"luat":{"diagnostics":{"unusedParameters":false,"unusedLocals":false},"inlayHints":{"parameterNames":{"enabled":false},"variableTypes":{"enabled":false}}}}}}),
    );
    loop {
        let message = receive(&receiver);
        if message["method"] == "textDocument/publishDiagnostics" && message["params"]["uri"] == uri
        {
            let diagnostics = message["params"]["diagnostics"].as_array().unwrap();
            if !diagnostics.iter().any(|diagnostic| {
                ["LTH1001", "LTH1002", "LTH1003"]
                    .contains(&diagnostic["code"].as_str().unwrap_or_default())
            }) {
                assert!(
                    diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic["code"] == "LTH1004"
                            && diagnostic["tags"] == json!([2]))
                );
                break;
            }
        }
    }
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":8,"method":"textDocument/inlayHint","params":{"textDocument":{"uri":uri},"range":{"start":{"line":0,"character":0},"end":{"line":7,"character":0}}}}),
    );
    assert_eq!(receive_response(&receiver, 8)["result"], json!([]));
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":7,"method":"shutdown","params":null}),
    );
    receive_response(&receiver, 7);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    );
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn stdio_initializes_publishes_diagnostics_completes_and_shuts_down() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target/tests")
        .join(format!("luat-lsp-stdio-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let document = root.join("main.luat");
    let uri = format!("file:///{}", document.to_string_lossy().replace('\\', "/"));

    let mut child = Command::new(env!("CARGO_BIN_EXE_luat-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = BufReader::new(stdout);
        while let Ok(message) = read_message(&mut output) {
            if sender.send(message).is_err() {
                return;
            }
        }
    });

    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}),
    );
    let initialize = receive_response(&receiver, 1);
    assert_eq!(initialize["id"], 1);
    assert_eq!(initialize["result"]["capabilities"]["hoverProvider"], true);

    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    );
    send_message(
        &mut input,
        json!({
            "jsonrpc":"2.0","method":"textDocument/didOpen",
            "params":{"textDocument":{"uri":uri,"languageId":"luat","version":1,"text":"local value: number = 'bad'\nprint(value)"}}
        }),
    );
    send_message(
        &mut input,
        json!({
            "jsonrpc":"2.0","method":"textDocument/didChange",
            "params":{"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":"local value: number = 'still bad'\nprint(value)"}]}
        }),
    );
    let diagnostics = loop {
        let message = receive(&receiver);
        if message["method"] == "textDocument/publishDiagnostics" {
            break message;
        }
    };
    assert_eq!(diagnostics["params"]["uri"], uri);
    assert_eq!(diagnostics["params"]["version"], 2);
    assert!(
        diagnostics["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "LT2001"),
        "{diagnostics}"
    );

    send_message(
        &mut input,
        json!({
            "jsonrpc":"2.0","id":2,"method":"textDocument/completion",
            "params":{"textDocument":{"uri":uri},"position":{"line":1,"character":11}}
        }),
    );
    let completion = receive_response(&receiver, 2);
    assert_eq!(completion["id"], 2);
    assert!(
        completion["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["label"] == "value")
    );

    assert_eq!(
        initialize["result"]["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"][0],
        "variable"
    );
    assert_eq!(
        initialize["result"]["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"][5],
        "operator"
    );
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":20,"method":"textDocument/semanticTokens/full","params":{"textDocument":{"uri":uri}}}),
    );
    let semantic = receive_response(&receiver, 20);
    assert_eq!(semantic["id"], 20);
    assert!(!semantic["result"]["data"].as_array().unwrap().is_empty());

    let signature_path = root.join("signature.luat");
    let signature_uri = format!(
        "file:///{}",
        signature_path.to_string_lossy().replace('\\', "/")
    );
    send_message(
        &mut input,
        json!({
            "jsonrpc":"2.0","method":"textDocument/didOpen",
            "params":{"textDocument":{"uri":signature_uri,"languageId":"luat","version":1,"text":"---@param value The value to display.\nfunction log_value(value: string) end\nlog_value('ok')"}}
        }),
    );
    send_message(
        &mut input,
        json!({
            "jsonrpc":"2.0","id":3,"method":"textDocument/signatureHelp",
            "params":{"textDocument":{"uri":signature_uri},"position":{"line":2,"character":11}}
        }),
    );
    let signature = receive_response(&receiver, 3);
    assert_eq!(signature["id"], 3);
    assert_eq!(
        signature["result"]["signatures"][0]["parameters"][0]["documentation"]["value"],
        "The value to display."
    );

    let project_root = root.join("project");
    std::fs::create_dir_all(project_root.join("src/client")).unwrap();
    std::fs::create_dir_all(project_root.join("src/shared")).unwrap();
    let project_main = project_root.join("src/client/main.luat");
    let project_api = project_root.join("src/shared/api.luat");
    let project_main_uri = format!(
        "file:///{}",
        project_main.to_string_lossy().replace('\\', "/")
    );
    let project_api_uri = format!(
        "file:///{}",
        project_api.to_string_lossy().replace('\\', "/")
    );
    let project_main_text =
        "local api = import('@/shared/api')\nlocal value: string = api.get()\nprint(value)";
    std::fs::write(&project_main, project_main_text).unwrap();
    std::fs::write(
        &project_api,
        "return { get = function(): string return 'ready' end }",
    )
    .unwrap();
    std::fs::write(
        project_root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
    )
    .unwrap();
    send_message(
        &mut input,
        json!({
            "jsonrpc":"2.0","method":"textDocument/didOpen",
            "params":{"textDocument":{"uri":project_main_uri,"languageId":"luat","version":1,"text":project_main_text}}
        }),
    );
    send_message(
        &mut input,
        json!({
            "jsonrpc":"2.0","method":"textDocument/didOpen",
            "params":{"textDocument":{"uri":project_api_uri,"languageId":"luat","version":1,"text":"return { get = function(): number return 1 end }"}}
        }),
    );
    loop {
        let message = receive(&receiver);
        if message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == project_main_uri
            && message["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == "LT2001")
        {
            break;
        }
    }
    send_message(
        &mut input,
        json!({
            "jsonrpc":"2.0","method":"textDocument/didClose",
            "params":{"textDocument":{"uri":project_api_uri}}
        }),
    );
    loop {
        let message = receive(&receiver);
        if message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == project_main_uri
            && message["params"]["diagnostics"] == json!([])
        {
            break;
        }
    }

    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":null}),
    );
    let shutdown = loop {
        let message = receive(&receiver);
        if message["id"] == 4 {
            break message;
        }
    };
    assert_eq!(shutdown["id"], 4);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    );
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn stdio_cancels_pending_analysis_requests() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target/tests")
        .join(format!("luat-lsp-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let document = root.join("main.luat");
    let uri = format!("file:///{}", document.to_string_lossy().replace('\\', "/"));

    let mut child = Command::new(env!("CARGO_BIN_EXE_luat-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = BufReader::new(stdout);
        while let Ok(message) = read_message(&mut output) {
            if sender.send(message).is_err() {
                return;
            }
        }
    });

    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}),
    );
    assert_eq!(receive(&receiver)["id"], 1);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    );
    send_message(
        &mut input,
        json!({
            "jsonrpc":"2.0","method":"textDocument/didOpen",
            "params":{"textDocument":{"uri":uri,"languageId":"luat","version":1,"text":("local value = 1\n").repeat(10_000)}}
        }),
    );
    send_message(
        &mut input,
        json!({
            "jsonrpc":"2.0","id":2,"method":"textDocument/hover",
            "params":{"textDocument":{"uri":uri},"position":{"line":0,"character":8}}
        }),
    );
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":2}}),
    );
    let canceled = loop {
        let message = receive(&receiver);
        if message["id"] == 2 {
            break message;
        }
    };
    assert_eq!(canceled["error"]["code"], -32800, "{canceled}");

    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":null}),
    );
    let shutdown = loop {
        let message = receive(&receiver);
        if message["id"] == 3 {
            break message;
        }
    };
    assert_eq!(shutdown["id"], 3);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    );
    drop(input);
    assert!(child.wait().unwrap().success());
}

fn send_message(input: &mut impl Write, value: Value) {
    let body = serde_json::to_vec(&value).unwrap();
    write!(input, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    input.write_all(&body).unwrap();
    input.flush().unwrap();
}

#[test]
fn runtime_completion_and_markdown_hover_use_qualified_signatures() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target/tests/runtime-hover-protocol");
    std::fs::create_dir_all(&root).unwrap();
    let uri = format!(
        "file:///{}",
        root.join("main.luat").to_string_lossy().replace('\\', "/")
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_luat-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = BufReader::new(stdout);
        while let Ok(message) = read_message(&mut output) {
            if sender.send(message).is_err() {
                break;
            }
        }
    });
    let response = |id: i64| receive_response(&receiver, id);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}),
    );
    let initialize = response(1);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    );
    let source = "local title = 'ş😀'; json.encode({})\njson.en\nlocal state: json_encode_st";
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"luat","version":1,"text":source}}}),
    );
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":uri},"position":{"line":1,"character":7}}}),
    );
    let completion = response(2);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":26}}}),
    );
    let hover = response(3);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":4,"method":"textDocument/signatureHelp","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":33}}}),
    );
    let signature = response(4);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":5,"method":"textDocument/completion","params":{"textDocument":{"uri":uri},"position":{"line":2,"character":32}}}),
    );
    let types = response(5);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","id":6,"method":"shutdown","params":null}),
    );
    response(6);
    send_message(
        &mut input,
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    );
    drop(input);
    assert!(child.wait().unwrap().success());
    assert_eq!(
        initialize["result"]["capabilities"]["completionProvider"]["triggerCharacters"],
        json!([".", "@", "-", " "])
    );
    let encode = completion["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["label"] == "encode")
        .unwrap();
    assert_eq!(encode["kind"], 3);
    assert_eq!(hover["result"]["contents"]["kind"], "markdown");
    let contents = hover["result"]["contents"]["value"].as_str().unwrap();
    assert!(
        contents.starts_with(
            "```luat\nfunction json.encode(object: any, state: json_encode_state?): string\n```"
        ),
        "{hover}"
    );
    assert!(!contents.contains("**Documentation**"));
    assert_eq!(hover["result"]["range"]["start"]["character"], 26);
    assert_eq!(hover["result"]["range"]["end"]["character"], 32);
    assert_eq!(
        signature["result"]["signatures"][0]["label"],
        "function json.encode(object: any, state: json_encode_state?): string"
    );
    assert!(
        types["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["label"] == "json_encode_state" && item["kind"] == 25)
    );
}

fn receive(receiver: &mpsc::Receiver<Value>) -> Value {
    receiver.recv_timeout(Duration::from_secs(10)).unwrap()
}

fn receive_response(receiver: &mpsc::Receiver<Value>, id: i64) -> Value {
    loop {
        let message = receive(receiver);
        if message["id"] == id {
            return message;
        }
    }
}

fn read_message(input: &mut impl BufRead) -> std::io::Result<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; content_length.unwrap()];
    input.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).unwrap())
}
