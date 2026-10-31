//! A scripted OpenAI-compatible model on 127.0.0.1 for agents under test
//! (OpenCode, Grok Build, Kimi Code): asked anything with `ASK` in the
//! newest typed prompt, it calls the agent's own question tool (whichever
//! of `question`, `ask_user_question` or `AskUserQuestion` the request
//! offers, with that tool's flag for several choices) with [`COLOUR`] and
//! [`FRUITS`]; after a tool's result it says `Answered.`; else `Hello.`.
//! Streamed (server-sent events, chunked) when asked to stream. Synthetic
//! data only; nothing leaves the machine.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const COLOUR: &str = "Which colour do you prefer?";
pub const FRUITS: &str = "Which fruits do you like?";

/// The question tools agents offer, by the names their requests give them.
const TOOLS: [&str; 3] = ["question", "ask_user_question", "AskUserQuestion"];

pub struct MockModel {
    /// `http://127.0.0.1:<port>/v1`.
    pub base_url: String,
    stop: Arc<AtomicBool>,
    port: u16,
}

impl MockModel {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(stream) = stream {
                    std::thread::spawn(move || {
                        let _ = serve(stream);
                    });
                }
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            stop,
            port,
        }
    }
}

impl Drop for MockModel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// One request on `stream`, answered, then the connection closed.
fn serve(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut start = String::new();
    reader.read_line(&mut start)?;
    let (mut length, mut chunked) = (0, false);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size)?;
            let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk)?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    } else {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    }
    let mut out = stream;
    if start.starts_with("GET") {
        let models = json!({"object": "list", "data": [
            {"id": "mock-model", "object": "model", "owned_by": "mock", "created": 0}]});
        return respond_json(&mut out, &models);
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (text, call) = plan(&request);
    if request["stream"] != json!(true) {
        let mut message = json!({"role": "assistant", "content": text});
        if let Some((name, args)) = &call {
            message["tool_calls"] = json!([{"id": "call_mock_1", "type": "function",
                "function": {"name": name, "arguments": args.to_string()}}]);
        }
        return respond_json(
            &mut out,
            &json!({"id": "chatcmpl-mock", "object": "chat.completion", "created": 0,
                "model": "mock-model", "choices": [{"index": 0, "message": message,
                "finish_reason": if call.is_some() { "tool_calls" } else { "stop" }}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}}),
        );
    }
    out.write_all(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\n\
          transfer-encoding: chunked\r\nconnection: close\r\n\r\n",
    )?;
    let event = |out: &mut TcpStream, data: &str| -> std::io::Result<()> {
        let data = format!("data: {data}\n\n");
        write!(out, "{:x}\r\n{data}\r\n", data.len())?;
        out.flush()
    };
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 0,
            "model": "mock-model", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        .to_string()
    };
    event(
        &mut out,
        &chunk(json!({"role": "assistant", "content": ""}), Value::Null),
    )?;
    if !text.is_empty() {
        event(&mut out, &chunk(json!({"content": text}), Value::Null))?;
    }
    if let Some((name, args)) = &call {
        event(
            &mut out,
            &chunk(
                json!({"tool_calls": [{"index": 0, "id": "call_mock_1", "type": "function",
                    "function": {"name": name, "arguments": args.to_string()}}]}),
                Value::Null,
            ),
        )?;
    }
    let finish = if call.is_some() { "tool_calls" } else { "stop" };
    event(&mut out, &chunk(json!({}), json!(finish)))?;
    event(
        &mut out,
        &json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 0,
            "model": "mock-model", "choices": [],
            "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}})
        .to_string(),
    )?;
    event(&mut out, "[DONE]")?;
    out.write_all(b"0\r\n\r\n")?;
    out.flush()
}

fn respond_json(out: &mut TcpStream, body: &Value) -> std::io::Result<()> {
    let body = body.to_string();
    write!(
        out,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        body.len()
    )?;
    out.flush()
}

/// A message's text: a string, or its parts' texts.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// What to say, and the tool call to make, for `request`.
fn plan(request: &Value) -> (String, Option<(String, Value)>) {
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    if messages.last().is_some_and(|last| last["role"] == "tool") {
        return ("Answered.".into(), None);
    }
    // The newest typed prompt, past what the agent adds (`<system-reminder>`…;
    // Grok wraps what's typed in `<user_query>`).
    let typed = messages
        .iter()
        .rev()
        .filter(|message| message["role"] == "user")
        .map(|message| text_of(&message["content"]))
        .find(|text| text.contains("<user_query>") || !text.trim_start().starts_with('<'))
        .unwrap_or_default();
    if !typed.contains("ASK") {
        return ("Hello.".into(), None);
    }
    let tools = request["tools"].as_array().cloned().unwrap_or_default();
    let Some(tool) = tools.iter().find_map(|tool| {
        let name = tool["function"]["name"].as_str()?;
        TOOLS.contains(&name).then_some(&tool["function"])
    }) else {
        return ("No question tool.".into(), None);
    };
    let fields = &tool["parameters"]["properties"]["questions"]["items"]["properties"];
    let flag = ["multiSelect", "multi_select", "multiple"]
        .into_iter()
        .find(|flag| !fields[*flag].is_null())
        .unwrap_or("multiSelect");
    let question = |text: &str, header: &str, options: &[&str], several: bool| {
        json!({"question": text, "header": header, flag: several, "options": options.iter()
            .map(|label| json!({"label": label, "description": label.to_lowercase()}))
            .collect::<Vec<_>>()})
    };
    let args = json!({"questions": [
        question(COLOUR, "Colour", &["Red", "Green", "Blue"], false),
        question(FRUITS, "Fruits", &["Apple", "Banana", "Cherry"], true),
    ]});
    (
        String::new(),
        Some((tool["name"].as_str().unwrap_or_default().to_owned(), args)),
    )
}
