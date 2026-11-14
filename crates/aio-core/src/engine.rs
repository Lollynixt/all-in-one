use crate::provider::{ChatEvent, ChatMessage, ChatRequest, Provider, Role};
use crate::nodes;
use crate::notes::NoteStore;
use crate::workflow::{FileFormat, NodeId, NodeKind, WriteMode, Workflow, WorkflowError};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Root for file nodes. Nodes cannot escape it.
    pub workspace: PathBuf,
    /// Model used by AI nodes whose model field is empty.
    pub default_model: String,
    /// Workflow-level switch: may any AI node use a cloud provider?
    pub cloud_allowed: bool,
    /// Root of the notebook, for Note in / Note out.
    pub notes_root: PathBuf,
    /// Workflow-level switch: may API nodes reach the network?
    pub allow_network: bool,
}

#[derive(Debug, Clone)]
pub enum RunEvent {
    NodeStarted(NodeId),
    NodeToken(NodeId, String),
    NodeFinished(NodeId, String),
    NodeSkipped(NodeId),
    NodeFailed(NodeId, String),
    Finished { ok: bool },
}

/// Resolves a workspace-relative path, rejecting absolute paths and `..`.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if rel.trim().is_empty() {
        return Err("path is empty".into());
    }
    if p.is_absolute() {
        return Err(format!("absolute paths are not allowed: {rel}"));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir)) {
        return Err(format!("path escapes the workspace: {rel}"));
    }
    Ok(root.join(p))
}

const NETWORK_BLOCKED: &str = "blocked: API nodes are off. Turn on 'Allow API nodes to use the network' in Models, Privacy to run this node";

fn page_name(page: &str) -> String {
    let p = page.trim();
    if p.ends_with(".md") { p.to_string() } else { format!("{p}.md") }
}

async fn append_file(p: &Path, text: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let needs_gap = tokio::fs::read(p).await.map(|b| !b.is_empty() && !b.ends_with(b"\n")).unwrap_or(false);
    let mut f = tokio::fs::OpenOptions::new().create(true).append(true).open(p).await?;
    if needs_gap {
        f.write_all(b"\n").await?;
    }
    f.write_all(text.as_bytes()).await?;
    if !text.ends_with('\n') {
        f.write_all(b"\n").await?;
    }
    Ok(())
}

pub async fn run_workflow(
    wf: &Workflow,
    provider: Arc<dyn Provider>,
    opts: &RunOptions,
    events: UnboundedSender<RunEvent>,
) -> Result<bool, WorkflowError> {
    let order = wf.execution_order()?;
    let mut outputs: HashMap<(NodeId, String), String> = HashMap::new();
    let mut all_ok = true;

    for id in order {
        let node = wf.node(id).expect("validated");
        // Gather inputs from active upstream ports.
        let incoming: Vec<_> = wf.edges.iter().filter(|e| e.to == id).collect();
        let inputs: Vec<&String> = incoming.iter().filter_map(|e| outputs.get(&(e.from, e.from_port.clone()))).collect();
        if node.kind.has_input() && !incoming.is_empty() && inputs.is_empty() {
            let _ = events.send(RunEvent::NodeSkipped(id));
            continue;
        }
        let input = inputs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n\n");
        let _ = events.send(RunEvent::NodeStarted(id));

        let result: Result<(String, &'static str), String> = match &node.kind {
            NodeKind::Text { text } => Ok((text.clone(), "out")),
            NodeKind::ReadFile { path } => match safe_join(&opts.workspace, path) {
                Ok(p) => tokio::fs::read_to_string(&p).await.map(|s| (s, "out")).map_err(|e| format!("{}: {e}", p.display())),
                Err(e) => Err(e),
            },
            NodeKind::Template { template } => Ok((template.replace("{{input}}", &input), "out")),
            NodeKind::Contains { needle } => {
                let hit = input.to_lowercase().contains(&needle.to_lowercase());
                Ok((input.clone(), if hit { "true" } else { "false" }))
            }
            NodeKind::ReadNote { page } => match NoteStore::new(&opts.notes_root) {
                Ok(store) => store.read(&page_name(page)).map(|s| (s, "out")).map_err(|e| format!("note '{page}': {e}")),
                Err(e) => Err(e.to_string()),
            },
            NodeKind::HttpGet { url, headers } => {
                if !opts.allow_network {
                    Err(NETWORK_BLOCKED.into())
                } else {
                    nodes::http_get(&url.replace("{{input}}", input.trim()), headers).await.map(|s| (s, "out"))
                }
            }
            NodeKind::HttpSend { url, method, headers } => {
                if !opts.allow_network {
                    Err(NETWORK_BLOCKED.into())
                } else {
                    nodes::http_send(*method, &url.replace("{{input}}", input.trim()), headers, &input).await.map(|s| (s, "out"))
                }
            }
            NodeKind::Python { code } => nodes::run_python(code, &input, &opts.workspace).await.map(|s| (s, "out")),
            NodeKind::Sql { db, query, mode, format } => {
                let db_path = if db.trim().is_empty() { Ok(None) } else { safe_join(&opts.workspace, db).map(Some) };
                match db_path {
                    Ok(dbp) => {
                        let (query, mode, format, input) = (query.clone(), *mode, *format, input.clone());
                        tokio::task::spawn_blocking(move || nodes::run_sql(dbp.as_deref(), &query, mode, format, &input))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|r| r)
                            .map(|s| (s, "out"))
                    }
                    Err(e) => Err(e),
                }
            }
            NodeKind::DbInsert { db, table } => match safe_join(&opts.workspace, db) {
                Ok(dbp) => {
                    let (table, text) = (table.clone(), input.clone());
                    tokio::task::spawn_blocking(move || nodes::db_insert(&dbp, &table, &text))
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r)
                        .map(|s| (s, "out"))
                }
                Err(e) => Err(e),
            },
            NodeKind::Replace { find, replacement, regex } => nodes::replace(&input, find, replacement, *regex).map(|s| (s, "out")),
            NodeKind::Filter { pattern, mode, regex } => nodes::filter_lines(&input, pattern, *mode, *regex).map(|s| (s, "out")),
            NodeKind::Lines { op } => Ok((nodes::lines_op(&input, *op), "out")),
            NodeKind::JsonPath { path } => nodes::json_path(&input, path).map(|s| (s, "out")),
            NodeKind::Count { what } => Ok((nodes::count(&input, *what), "out")),
            NodeKind::Convert { op } => nodes::convert(&input, *op).map(|s| (s, "out")),
            NodeKind::WriteFile { path, format, mode } => match safe_join(&opts.workspace, &nodes::with_default_ext(path, *format)) {
                Ok(p) => {
                    if let Some(parent) = p.parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                    let body = nodes::shape_for_file(&input, *format);
                    let res = match (mode, format) {
                        (WriteMode::Append, f) if !matches!(f, FileFormat::Json) => append_file(&p, &body).await,
                        (WriteMode::Append, _) => Err(std::io::Error::other("JSON files cannot be appended to; use Replace")),
                        _ => tokio::fs::write(&p, &body).await,
                    };
                    res.map(|_| (input.clone(), "out")).map_err(|e| format!("{}: {e}", p.display()))
                }
                Err(e) => Err(e),
            },
            NodeKind::WriteNote { page, mode } => match NoteStore::new(&opts.notes_root) {
                Ok(store) => {
                    let name = page_name(page);
                    let text = match mode {
                        WriteMode::Replace => input.clone(),
                        WriteMode::Append => match store.read(&name) {
                            Ok(old) if !old.trim().is_empty() => format!("{}\n\n{}", old.trim_end(), input),
                            _ => input.clone(),
                        },
                    };
                    store.write(&name, &text).map(|_| (input.clone(), "out")).map_err(|e| format!("note '{page}': {e}"))
                }
                Err(e) => Err(e.to_string()),
            },
            NodeKind::Output => Ok((input.clone(), "out")),
            NodeKind::Ai { model, system, allow_cloud } => {
                let model = if model.trim().is_empty() { opts.default_model.clone() } else { model.trim().to_string() };
                if model.is_empty() {
                    Err("no model selected and no recommended model available".into())
                } else if provider.is_cloud_model(&model) && !(*allow_cloud && opts.cloud_allowed) {
                    Err(format!("blocked: {model} is a cloud model and cloud models are off. Allow them in Models, Privacy, and tick 'May use cloud models' on this node").into())
                } else {
                    let mut messages = Vec::new();
                    if !system.trim().is_empty() {
                        messages.push(ChatMessage::new(Role::System, system.clone()));
                    }
                    messages.push(ChatMessage::new(Role::User, input.clone()));
                    stream_chat(provider.as_ref(), ChatRequest { model, messages }, id, &events).await.map(|s| (s, "out"))
                }
            }
        };

        match result {
            Ok((text, port)) => {
                let _ = events.send(RunEvent::NodeFinished(id, text.clone()));
                outputs.insert((id, port.to_string()), text);
            }
            Err(e) => {
                all_ok = false;
                let _ = events.send(RunEvent::NodeFailed(id, e));
                break; // fail fast for the prototype
            }
        }
    }
    let _ = events.send(RunEvent::Finished { ok: all_ok });
    Ok(all_ok)
}

async fn stream_chat(
    provider: &dyn Provider,
    req: ChatRequest,
    id: NodeId,
    events: &UnboundedSender<RunEvent>,
) -> Result<String, String> {
    let (tx, mut rx) = unbounded_channel();
    let fut = provider.chat(req, tx);
    let mut collected = String::new();
    let drain = async {
        while let Some(ev) = rx.recv().await {
            match ev {
                ChatEvent::Token(t) => {
                    collected.push_str(&t);
                    let _ = events.send(RunEvent::NodeToken(id, t));
                }
                ChatEvent::Done => return Ok(()),
                ChatEvent::Error(e) => return Err(e),
            }
        }
        Ok(())
    };
    let (call, stream) = tokio::join!(fut, drain);
    call.map_err(|e| e.to_string())?;
    stream?;
    Ok(collected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::EchoProvider;
    use crate::workflow::{CountWhat, SqlMode, Table};

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("aio-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    async fn run(wf: &Workflow, ws: PathBuf) -> (bool, Vec<RunEvent>) {
        let (tx, mut rx) = unbounded_channel();
        let opts = RunOptions { workspace: ws.clone(), default_model: "echo".into(), cloud_allowed: false, notes_root: ws.join("notes"), allow_network: false };
        let ok = run_workflow(wf, Arc::new(EchoProvider), &opts, tx).await.unwrap();
        let mut evs = vec![];
        while let Ok(e) = rx.try_recv() {
            evs.push(e);
        }
        (ok, evs)
    }

    #[tokio::test]
    async fn demo_runs_and_branches() {
        let ws = tmp();
        let (ok, evs) = run(&Workflow::demo(), ws.clone()).await;
        assert!(ok);
        // Echo output contains "unusual" (it echoes the prompt), so the true branch writes the file.
        assert!(ws.join("review/flagged.md").exists());
        assert!(evs.iter().any(|e| matches!(e, RunEvent::NodeSkipped(5)))); // Output node skipped
        let _ = std::fs::remove_dir_all(ws);
    }

    #[tokio::test]
    async fn path_escape_fails_node() {
        let mut w = Workflow::new("x");
        let a = w.add_node("t", NodeKind::Text { text: "hi".into() }, (0.0, 0.0));
        let b = w.add_node("w", NodeKind::WriteFile { path: "../evil.txt".into(), format: FileFormat::Text, mode: WriteMode::Replace }, (0.0, 0.0));
        w.connect(a, "out", b);
        let ws = tmp();
        let (ok, evs) = run(&w, ws.clone()).await;
        assert!(!ok);
        assert!(evs.iter().any(|e| matches!(e, RunEvent::NodeFailed(_, m) if m.contains("escapes"))));
        assert!(!ws.parent().unwrap().join("evil.txt").exists());
        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn safe_join_rules() {
        let root = Path::new("/ws");
        assert!(safe_join(root, "a/b.txt").is_ok());
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert!(safe_join(root, "a/../../b").is_err());
        assert!(safe_join(root, "").is_err());
    }

    #[tokio::test]
    async fn cloud_provider_blocked_by_default() {
        struct Cloud;
        #[async_trait::async_trait]
        impl Provider for Cloud {
            fn id(&self) -> &str { "cloud" }
            fn is_cloud(&self) -> bool { true }
            async fn list_models(&self) -> Result<Vec<crate::ModelInfo>, crate::ProviderError> { Ok(vec![]) }
            async fn chat(&self, _: ChatRequest, _: UnboundedSender<ChatEvent>) -> Result<(), crate::ProviderError> { panic!("must not be called") }
        }
        let mut w = Workflow::new("c");
        let a = w.add_node("t", NodeKind::Text { text: "secret".into() }, (0.0, 0.0));
        let b = w.add_node("ai", NodeKind::Ai { model: "m".into(), system: String::new(), allow_cloud: false }, (0.0, 0.0));
        w.connect(a, "out", b);
        let (tx, mut rx) = unbounded_channel();
        let ws = tmp();
        let opts = RunOptions { workspace: ws.clone(), default_model: "m".into(), cloud_allowed: true, notes_root: ws.join("notes"), allow_network: false };
        let ok = run_workflow(&w, Arc::new(Cloud), &opts, tx).await.unwrap();
        assert!(!ok);
        let mut blocked = false;
        while let Ok(e) = rx.try_recv() {
            if matches!(e, RunEvent::NodeFailed(_, m) if m.starts_with("blocked")) { blocked = true; }
        }
        assert!(blocked);
    }

    #[tokio::test]
    async fn input_to_output_pipeline_with_formats() {
        let ws = tmp();
        let mut w = Workflow::new("etl");
        let a = w.add_node("csv", NodeKind::Text { text: "team,pts\nred,3\nblue,5\nred,4".into() }, (0.0, 0.0));
        let b = w.add_node("sql", NodeKind::Sql { db: String::new(), query: "SELECT team, SUM(CAST(pts AS INTEGER)) AS total FROM input GROUP BY team ORDER BY team".into(), mode: SqlMode::Query, format: Table::Csv }, (0.0, 0.0));
        let c = w.add_node("json file", NodeKind::WriteFile { path: "out/totals".into(), format: FileFormat::Json, mode: WriteMode::Replace }, (0.0, 0.0));
        let d = w.add_node("db", NodeKind::DbInsert { db: "scores.db".into(), table: "totals".into() }, (0.0, 0.0));
        let e = w.add_node("note", NodeKind::WriteNote { page: "Reports/Totals".into(), mode: WriteMode::Append }, (0.0, 0.0));
        w.connect(a, "out", b);
        w.connect(b, "out", c);
        w.connect(b, "out", d);
        w.connect(b, "out", e);
        let (ok, _) = run(&w, ws.clone()).await;
        assert!(ok);
        let json = std::fs::read_to_string(ws.join("out/totals.json")).unwrap();
        assert!(json.contains("\"team\": \"blue\"") && json.contains("\"total\": 5"));
        assert!(ws.join("scores.db").exists());
        assert!(std::fs::read_to_string(ws.join("notes/Reports/Totals.md")).unwrap().contains("red,7"));
        // second run appends to the note instead of replacing it
        run(&w, ws.clone()).await;
        let note = std::fs::read_to_string(ws.join("notes/Reports/Totals.md")).unwrap();
        assert_eq!(note.matches("team,total").count(), 2);
        let _ = std::fs::remove_dir_all(ws);
    }

    #[tokio::test]
    async fn read_note_and_network_gate() {
        let ws = tmp();
        NoteStore::new(ws.join("notes")).unwrap().write("Inbox.md", "line one\nline two").unwrap();
        let mut w = Workflow::new("n");
        let a = w.add_node("note", NodeKind::ReadNote { page: "Inbox".into() }, (0.0, 0.0));
        let b = w.add_node("count", NodeKind::Count { what: CountWhat::Lines }, (0.0, 0.0));
        w.connect(a, "out", b);
        let (ok, evs) = run(&w, ws.clone()).await;
        assert!(ok);
        assert!(evs.iter().any(|e| matches!(e, RunEvent::NodeFinished(2, t) if t == "2")));

        let mut w = Workflow::new("net");
        w.add_node("get", NodeKind::HttpGet { url: "https://example.com".into(), headers: String::new() }, (0.0, 0.0));
        let (ok, evs) = run(&w, ws.clone()).await;
        assert!(!ok);
        assert!(evs.iter().any(|e| matches!(e, RunEvent::NodeFailed(_, m) if m.starts_with("blocked"))));
        let _ = std::fs::remove_dir_all(ws);
    }

    #[tokio::test]
    async fn old_workflow_files_still_load() {
        let old = r#"{"name":"o","nodes":[{"id":1,"title":"w","kind":{"type":"write_file","path":"a.md"},"pos":[0,0]}],"edges":[]}"#;
        let w = Workflow::from_json(old).unwrap();
        assert!(matches!(&w.nodes[0].kind, NodeKind::WriteFile { format: FileFormat::Text, mode: WriteMode::Replace, .. }));
    }
}
