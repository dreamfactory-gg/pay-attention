use crate::{detail, text};
use serde_json::Value;
use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

pub(crate) fn tail(path: &Path) -> Vec<Value> {
    let read = || -> std::io::Result<Vec<u8>> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Ok(Vec::new());
        }
        let len = metadata.len();
        let offset = len.saturating_sub(256 * 1024);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(256 * 1024).read_to_end(&mut bytes)?;
        if offset > 0 {
            let start = bytes
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |i| i + 1);
            bytes.drain(..start);
        }
        Ok(bytes)
    };
    read()
        .unwrap_or_default()
        .split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

pub(crate) fn questions(input: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(questions) = input["questions"].as_array() {
        for q in questions {
            parts.push(text(&q["question"]));
            if let Some(options) = q["options"].as_array() {
                parts.extend(options.iter().map(|o| text(&o["label"])));
            }
        }
    }
    parts.join(" ")
}

pub(crate) fn command(input: &Value, tool: &str) -> String {
    match &input["command"] {
        Value::String(s) => s.clone(),
        Value::Array(args) => args
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => tool.into(),
    }
}

pub(crate) fn claude_tool(input: &Value, tool: &str) -> String {
    match tool {
        "Bash" => text(&input["command"]),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => text(&input["file_path"]),
        _ => tool,
    }
    .into()
}

pub(crate) fn claude_detail(path: &Path) -> Option<String> {
    tail(path)
        .iter()
        .rev()
        .filter(|l| l["type"] == "assistant")
        .find_map(|l| {
            let content = &l["message"]["content"];
            if let Some(s) = content.as_str() {
                return detail(s);
            }
            let texts: Vec<_> = content
                .as_array()?
                .iter()
                .filter(|c| c["type"] == "text")
                .map(|c| text(&c["text"]))
                .collect();
            detail(&texts.join(" "))
        })
}

pub(crate) fn codex_error(path: &Path) -> Option<String> {
    tail(path)
        .iter()
        .rev()
        .filter(|l| l["type"] == "event_msg")
        .take_while(|l| l["payload"]["type"] != "task_started")
        .find(|l| l["payload"]["type"] == "error")
        .map(|l| text(&l["payload"]["message"]).to_owned())
}
