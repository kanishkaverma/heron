//! The agent's tools (SPEC §7.1 plus IMPL.md's bash/read/write/edit) and
//! the result cap.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use zeron_proto::ToolCall;

use crate::chat::Chat;
use crate::llm::{ToolDef, ToolUse};
use crate::prompts::{DATE_DESCRIPTION, ZOOM_DESCRIPTION};

/// Max size of one tool result, head and tail kept (SPEC §7).
pub const CAP: usize = 30_000;
const BASH_TIMEOUT: Duration = Duration::from_secs(600);

/// Constant for the life of the process: the tool list heads every cached
/// prefix (SPEC §7.2, §8).
pub fn defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "zoom",
            description: ZOOM_DESCRIPTION,
            schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "integer", "description": "First message of the line (the number before +)."},
                    "n": {"type": "integer", "description": "Messages the line covers (the number after +)."}
                },
                "required": ["id", "n"]
            }),
        },
        ToolDef {
            name: "date",
            description: DATE_DESCRIPTION,
            schema: json!({
                "type": "object",
                "properties": {"id": {"type": "integer", "description": "Message id."}},
                "required": ["id"]
            }),
        },
        ToolDef {
            name: "bash",
            description: "Run a bash command in the working directory. Returns stdout and stderr, and the exit code when it is not 0. Times out after 10 minutes.",
            schema: json!({
                "type": "object",
                "properties": {"command": {"type": "string", "description": "The command to run."}},
                "required": ["command"]
            }),
        },
        ToolDef {
            name: "read",
            description: "Read a text file. Relative paths resolve in the working directory. offset (1-based) and limit select lines.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "offset": {"type": "integer", "description": "First line to read, 1-based."},
                    "limit": {"type": "integer", "description": "Number of lines to read."}
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "write",
            description: "Create or overwrite a file with the given content, creating parent directories.",
            schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
                "required": ["path", "content"]
            }),
        },
        ToolDef {
            name: "edit",
            description: "Replace old_string with new_string in a file. old_string must match exactly and be unique unless replace_all is true.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                    "replace_all": {"type": "boolean"}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        },
    ]
}

fn str_arg(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).map(str::to_string)
}

/// How a call renders in Zeron's tool chips.
pub fn display(call: &ToolUse) -> ToolCall {
    let path = || str_arg(&call.input, "path").unwrap_or_default();
    match call.name.as_str() {
        "bash" => ToolCall::Exec {
            command: str_arg(&call.input, "command").unwrap_or_default(),
        },
        "read" => ToolCall::ReadFile { path: path() },
        "write" => ToolCall::WriteFile {
            path: path(),
            content: str_arg(&call.input, "content"),
        },
        "edit" => ToolCall::EditFile {
            path: path(),
            old_string: str_arg(&call.input, "old_string"),
            new_string: str_arg(&call.input, "new_string"),
        },
        name => ToolCall::Unknown {
            name: name.to_string(),
            input: Some(call.input.clone()),
        },
    }
}

/// Keep the head and the tail, with a note of what was cut (SPEC §7).
pub fn cap(text: String) -> String {
    let chars = text.chars().count();
    if chars <= CAP {
        return text;
    }
    let keep = CAP / 2 - 100;
    let head: String = text.chars().take(keep).collect();
    let tail: String = text.chars().skip(chars - keep).collect();
    format!(
        "{head}\n\n[... {} of {chars} characters cut from the middle ...]\n\n{tail}",
        chars - 2 * keep
    )
}

fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Run one tool. `Err` is a tool error, shown to the model as such.
pub async fn run(chat: &Chat, cwd: &Path, call: &ToolUse) -> Result<String, String> {
    let input = &call.input;
    match call.name.as_str() {
        "zoom" => {
            let raw = |k: &str| input.get(k).map_or("?".to_string(), |v| v.to_string());
            match (
                input.get("id").and_then(Value::as_u64),
                input.get("n").and_then(Value::as_u64),
            ) {
                (Some(id), Some(n)) => Ok(chat.state().mem.zoom(id, n)),
                _ => Ok(format!("No line {}+{}.", raw("id"), raw("n"))),
            }
        }
        "date" => match input.get("id").and_then(Value::as_u64) {
            Some(id) => Ok(chat.date(id)),
            None => Ok(format!("No message {}.", input.get("id").map_or("?".into(), |v| v.to_string()))),
        },
        "bash" => {
            let command = str_arg(input, "command").ok_or("bash needs a command")?;
            bash(cwd, &command).await
        }
        "read" => {
            let path = resolve(cwd, &str_arg(input, "path").ok_or("read needs a path")?);
            let text = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
            let limit = input.get("limit").and_then(Value::as_u64).map(|n| n as usize);
            if offset == 1 && limit.is_none() {
                return Ok(text);
            }
            let lines: Vec<&str> = text.lines().skip(offset - 1).take(limit.unwrap_or(usize::MAX)).collect();
            Ok(lines.join("\n"))
        }
        "write" => {
            let path = resolve(cwd, &str_arg(input, "path").ok_or("write needs a path")?);
            let content = str_arg(input, "content").ok_or("write needs content")?;
            if let Some(dir) = path.parent() {
                tokio::fs::create_dir_all(dir)
                    .await
                    .map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            tokio::fs::write(&path, &content)
                .await
                .map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(format!("Wrote {} bytes to {}", content.len(), path.display()))
        }
        "edit" => {
            let path = resolve(cwd, &str_arg(input, "path").ok_or("edit needs a path")?);
            let old = str_arg(input, "old_string").ok_or("edit needs old_string")?;
            let new = str_arg(input, "new_string").ok_or("edit needs new_string")?;
            let all = input.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
            if old.is_empty() {
                return Err("old_string is empty".into());
            }
            let text = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let count = text.matches(&old).count();
            if count == 0 {
                return Err(format!("old_string not found in {}", path.display()));
            }
            if count > 1 && !all {
                return Err(format!(
                    "old_string appears {count} times in {}; make it unique or set replace_all",
                    path.display()
                ));
            }
            let edited = if all {
                text.replace(&old, &new)
            } else {
                text.replacen(&old, &new, 1)
            };
            tokio::fs::write(&path, edited)
                .await
                .map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(format!("Edited {} ({count} replacement{})", path.display(), if count == 1 { "" } else { "s" }))
        }
        other => Err(format!("Unknown tool {other}")),
    }
}

/// Kills the command's whole process group when dropped (interrupt, timeout).
struct Group(Option<i32>);

impl Drop for Group {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: kill(2) on the private process group this tool created.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
}

async fn bash(cwd: &Path, command: &str) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("spawn bash: {e}"))?;
    let mut group = Group(child.id().map(|pid| pid as i32));
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let drain = |mut pipe: Box<dyn tokio::io::AsyncRead + Unpin + Send>| {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = buf.clone();
        let task = tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(&chunk[..n]);
            }
        });
        (task, buf)
    };
    let (out_pipe, err_pipe) = (drain(Box::new(stdout)), drain(Box::new(stderr)));
    let status = tokio::time::timeout(BASH_TIMEOUT, child.wait())
        .await
        .map_err(|_| format!("timed out after {BASH_TIMEOUT:?}"))?
        .map_err(|e| e.to_string())?;
    // A backgrounded process can hold the pipes open forever: take what is
    // there shortly after the command itself exits.
    let grab = |(mut task, buf): (tokio::task::JoinHandle<()>, std::sync::Arc<std::sync::Mutex<Vec<u8>>>)| async move {
        if tokio::time::timeout(Duration::from_secs(2), &mut task).await.is_err() {
            task.abort();
        }
        std::mem::take(&mut *buf.lock().unwrap_or_else(|e| e.into_inner()))
    };
    let (out, err) = (grab(out_pipe).await, grab(err_pipe).await);
    let mut text = String::from_utf8_lossy(&out).into_owned();
    if !err.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&err));
    }
    // Finished normally: background children it started may keep running.
    group.0 = None;
    match status.code() {
        Some(0) => Ok(text),
        Some(code) => Err(format!("{text}\n(exit code {code})")),
        None => Err(format!("{text}\n(killed by a signal)")),
    }
}
