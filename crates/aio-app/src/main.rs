//! All In One: desktop shell prototype (egui). Core logic lives in `aio-core` and `aio-llm`.
mod fmt;
mod fonts;
mod files;
mod graph;
mod md;
mod release;
mod settings;
mod theme;
mod update;
mod web;

use aio_core::hardware::{self, HardwareProfile};
use aio_core::notes::NoteStore;
use aio_core::provider::EchoProvider;
use aio_core::{ChatEvent, ChatMessage, ChatRequest, ModelInfo, Node, Provider, Role, RunEvent, RunOptions, Workflow};
use aio_llm::launcher::{self, StartOutcome};
use aio_llm::cloud::{self, Anthropic, Router};
use aio_llm::{Ollama, OpenAiCompat, PullEvent};
use settings::Settings;
use egui::{Frame, Margin, RichText};
use egui_snarl::ui::SnarlWidget;
use egui_snarl::{NodeId as SnarlId, Snarl};
use graph::Status;
use std::collections::{BTreeMap, HashMap};
use std::process::Child;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use theme::*;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

/// Local provider (per the chosen kind) wrapped in a router so AI nodes can also pick cloud models.
fn build_provider(kind: ProviderKind, ollama_url: &str, openai_url: &str, settings: &Settings) -> Arc<dyn Provider> {
    let local: Arc<dyn Provider> = match kind {
        ProviderKind::Ollama => Arc::new(Ollama::new(ollama_url.to_string())),
        ProviderKind::OpenAiCompat => Arc::new(OpenAiCompat::new(openai_url.to_string(), None, false)),
        ProviderKind::Demo => Arc::new(EchoProvider),
    };
    let anthropic = settings.anthropic().map(|k| Arc::new(Anthropic::new(k)) as Arc<dyn Provider>);
    let openai = settings.openai().map(|k| Arc::new(cloud::openai(k)) as Arc<dyn Provider>);
    Arc::new(Router::new(local, anthropic, openai))
}

/// OpenAI's model list also holds embeddings, speech and image models; keep the ones that chat.
fn chat_capable_openai(id: &str) -> bool {
    let chatty = id.starts_with("gpt-") || id.starts_with("chatgpt") || id.starts_with("o1") || id.starts_with("o3") || id.starts_with("o4") || id.starts_with("o5");
    let other = ["embedding", "audio", "tts", "whisper", "image", "realtime", "transcribe", "moderation", "search", "instruct", "dall-e", "codex"];
    chatty && !other.iter().any(|o| id.contains(o))
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 900.0]).with_min_inner_size([960.0, 600.0]).with_title("All In One"),
        ..Default::default()
    };
    eframe::run_native("All In One", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Workflows,
    Notes,
    Browser,
    Models,
    Settings,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::Workflows, Tab::Notes, Tab::Browser, Tab::Models, Tab::Settings];
    fn name(self) -> &'static str {
        match self {
            Tab::Workflows => "Workflows",
            Tab::Notes => "Notes",
            Tab::Browser => "Browser",
            Tab::Models => "Models",
            Tab::Settings => "Settings",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderKind {
    Ollama,
    OpenAiCompat,
    Demo,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NoteMode {
    Source,
    Split,
    Preview,
}

#[derive(Default)]
struct PullState {
    status: String,
    completed: u64,
    total: u64,
    error: Option<String>,
}

#[derive(Clone, Copy)]
enum NoteAction {
    Wrap(&'static str, &'static str),
    Block(fmt::Block),
    Rule,
}

const EDITOR_ID: &str = "note_editor";

struct Msg {
    role: Role,
    content: String,
}

/// Forwards a tokio channel into a std channel and wakes the UI for every item.
fn forward<T: Send + 'static>(rt: &Runtime, ctx: &egui::Context, mut rx: UnboundedReceiver<T>) -> Receiver<T> {
    let (tx, out) = channel();
    let ctx = ctx.clone();
    rt.spawn(async move {
        while let Some(v) = rx.recv().await {
            if tx.send(v).is_err() {
                break;
            }
            ctx.request_repaint();
        }
        ctx.request_repaint();
    });
    out
}

fn cap(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn sanitise(name: &str) -> String {
    let s: String = name.trim().chars().map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_') { c } else { '_' }).collect();
    let s = s.trim().to_string();
    if s.is_empty() { "workflow".into() } else { s }
}

struct App {
    rt: Runtime,
    ctx: egui::Context,
    tab: Tab,
    agent_open: bool,
    workspace: PathBuf,

    // providers and models
    provider_kind: ProviderKind,
    ollama_url: String,
    openai_url: String,
    provider: Arc<dyn Provider>,
    cloud_allowed: bool,
    allow_network: bool,
    settings: Settings,
    anthropic_input: String,
    openai_input: String,
    anthropic_new_model: String,
    openai_new_model: String,
    anthropic_note: String,
    openai_note: String,
    anthropic_rx: Option<Receiver<Result<Vec<String>, String>>>,
    openai_rx: Option<Receiver<Result<Vec<String>, String>>>,
    hotbar_cat: aio_core::Category,
    tarball: Option<update::Tarball>,
    tarball_checked: Option<Instant>,
    exe_secs: u64,
    update_log: Vec<String>,
    update_rx: Option<Receiver<update::UpdateEvent>>,
    update_result: Option<Result<(), String>>,
    update_root: Option<PathBuf>,
    release: Option<release::Release>,
    release_checked: Option<Instant>,
    release_rx: Option<Receiver<Result<Option<release::Release>, String>>>,
    release_note: String,
    exe_path: Option<PathBuf>,
    restart_exe: Option<PathBuf>,
    repo_changed: bool,
    note_names: Vec<String>,
    note_names_at: Option<Instant>,
    installed: Vec<ModelInfo>,
    installed_note: String,
    installed_rx: Option<Receiver<Result<Vec<ModelInfo>, String>>>,
    installed_ok: bool,
    ollama_status: String,
    ollama_rx: Option<Receiver<StartOutcome>>,
    ollama_child: Option<Child>,
    ollama_missing: bool,
    pulls: BTreeMap<String, PullState>,
    pull_rx: Vec<(String, Receiver<PullEvent>)>,
    pull_tasks: HashMap<String, tokio::task::JoinHandle<()>>,
    need_model: Option<String>,
    custom_model: String,
    hw: HardwareProfile,
    vram_gb: f32,
    task: Option<&'static str>,
    chosen_model: Option<String>,

    // workflows
    wf_name: String,
    snarl: Snarl<Node>,
    wf_files: Vec<String>,
    status: HashMap<u32, Status>,
    node_out: BTreeMap<u32, String>,
    canvas_xf: Option<egui::emath::TSTransform>,
    pending_add: Option<(String, aio_core::NodeKind)>,
    run_rx: Option<Receiver<RunEvent>>,
    last_run: Option<bool>,
    wf_note: String,

    // notes
    notes: NoteStore,
    note_sel: Option<String>,
    note_text: String,
    note_dirty: bool,
    note_edit_at: Instant,
    note_search: String,
    note_new: String,
    note_mode: NoteMode,
    doc_fonts: Vec<fonts::DocFont>,
    doc_font_idx: usize,
    doc_size: f32,
    note_pending: Option<NoteAction>,
    jump_target: Option<usize>,
    jump_lead: u8,
    editor_ys: Vec<f32>,
    scroll_frac: f32,
    editor_max: f32,
    preview_max: f32,

    // browser
    url: String,
    page: Option<web::Page>,
    page_rx: Option<Receiver<Result<web::Page, String>>>,
    page_note: String,
    agent_sees_page: bool,

    // agent
    chat: Vec<Msg>,
    chat_input: String,
    chat_rx: Option<Receiver<ChatEvent>>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let doc_fonts = fonts::install(&cc.egui_ctx);
        let rt = Runtime::new().expect("tokio runtime");
        let workspace = std::env::var_os("AIO_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| PathBuf::from(h).join("AllInOne")))
            .unwrap_or_else(|| PathBuf::from("AllInOne"));
        let _ = std::fs::create_dir_all(workspace.join("workflows"));
        let notes = NoteStore::new(workspace.join("notes")).expect("notes folder");
        let settings = Settings::load(&workspace);
        let hw = HardwareProfile::detect();
        let vram_gb = hw.vram_gb().unwrap_or(0.0) as f32;

        let mut app = Self {
            rt,
            ctx: cc.egui_ctx.clone(),
            tab: Tab::Workflows,
            agent_open: true,
            workspace,
            provider_kind: ProviderKind::Ollama,
            ollama_url: "http://127.0.0.1:11434".into(),
            openai_url: "http://127.0.0.1:8080/v1".into(),
            provider: build_provider(ProviderKind::Ollama, "http://127.0.0.1:11434", "", &settings),
            cloud_allowed: settings.cloud_allowed,
            allow_network: settings.allow_network,
            anthropic_input: String::new(),
            openai_input: String::new(),
            anthropic_new_model: String::new(),
            openai_new_model: String::new(),
            anthropic_note: String::new(),
            openai_note: String::new(),
            anthropic_rx: None,
            openai_rx: None,
            hotbar_cat: aio_core::Category::Input,
            tarball: None,
            tarball_checked: None,
            exe_secs: update::exe_secs(),
            update_log: vec![],
            update_rx: None,
            update_result: None,
            update_root: update::project_root(),
            release: None,
            release_checked: None,
            release_rx: None,
            release_note: String::new(),
            exe_path: std::env::current_exe().ok(),
            restart_exe: None,
            repo_changed: false,
            note_names: vec![],
            note_names_at: None,
            settings: settings.clone(),
            installed: vec![],
            installed_note: "Not checked yet".into(),
            installed_rx: None,
            installed_ok: false,
            ollama_status: String::new(),
            ollama_rx: None,
            ollama_child: None,
            ollama_missing: false,
            pulls: BTreeMap::new(),
            pull_rx: vec![],
            pull_tasks: HashMap::new(),
            need_model: None,
            custom_model: String::new(),
            hw,
            vram_gb,
            task: Some("chat"),
            chosen_model: settings.chosen_model.clone(),
            wf_name: String::new(),
            snarl: Snarl::new(),
            wf_files: vec![],
            status: HashMap::new(),
            node_out: BTreeMap::new(),
            canvas_xf: None,
            pending_add: None,
            run_rx: None,
            last_run: None,
            wf_note: String::new(),
            notes,
            note_sel: None,
            note_text: String::new(),
            note_dirty: false,
            note_edit_at: Instant::now(),
            note_search: String::new(),
            note_new: String::new(),
            note_mode: NoteMode::Split,
            doc_fonts,
            doc_font_idx: 0,
            doc_size: 15.0,
            note_pending: None,
            jump_target: None,
            jump_lead: 0,
            editor_ys: vec![],
            scroll_frac: 0.0,
            editor_max: 0.0,
            preview_max: 0.0,
            url: String::new(),
            page: None,
            page_rx: None,
            page_note: String::new(),
            agent_sees_page: true,
            chat: vec![],
            chat_input: String::new(),
            chat_rx: None,
        };
        app.seed_workspace();
        app.refresh_wf_files();
        if let Some(first) = app.wf_files.first().cloned() {
            app.load_workflow(&first);
        }
        app.start_ollama();
        if app.notes.read("Welcome.md").is_ok() {
            app.open_note("Welcome.md");
        }
        // Developer hooks for screenshots: AIO_OPEN=<note path>, AIO_JUMP=<heading index>, AIO_NOTE_MODE=source|preview
        if let Ok(rel) = std::env::var("AIO_OPEN") {
            app.open_note(&rel);
        }
        if let Some(i) = std::env::var("AIO_JUMP").ok().and_then(|v| v.parse::<usize>().ok()) {
            app.jump_target = Some(i);
            app.jump_lead = 3;
        }
        match std::env::var("AIO_NOTE_MODE").as_deref() {
            Ok("source") => app.note_mode = NoteMode::Source,
            Ok("preview") => app.note_mode = NoteMode::Preview,
            _ => {}
        }
        if std::env::var("AIO_PROVIDER").as_deref() == Ok("demo") {
            app.provider_kind = ProviderKind::Demo;
            app.rebuild_provider();
        }
        // Developer hook: AIO_AUTOUPDATE=1 starts the in-app updater right away.
        if std::env::var_os("AIO_AUTOUPDATE").is_some() {
            app.check_for_update();
            app.start_update();
        }
        // Developer hook for screenshots: AIO_LOAD=<workflow file stem>
        if let Ok(stem) = std::env::var("AIO_LOAD") {
            app.load_workflow(&stem);
        }
        if std::env::var_os("AIO_AUTOADD").is_some() {
            app.pending_add = Some(("AI chat".into(), aio_core::NodeKind::Ai { model: String::new(), system: String::new(), allow_cloud: false }));
        }
        if std::env::var_os("AIO_AUTORUN").is_some() {
            app.run_workflow();
        }
        // Developer convenience for screenshots: AIO_START_TAB=notes|browser|models
        match std::env::var("AIO_START_TAB").as_deref() {
            Ok("notes") => app.tab = Tab::Notes,
            Ok("browser") => app.tab = Tab::Browser,
            Ok("models") => app.tab = Tab::Models,
            Ok("settings") => app.tab = Tab::Settings,
            _ => {}
        }
        app
    }

    fn seed_workspace(&mut self) {
        if self.notes.list().is_empty() {
            let _ = self.notes.write(
                "Welcome.md",
                "# Welcome to All In One\n\nPages are plain Markdown files in your workspace. Use the toolbar above for **bold**, *italic*, ~~strikethrough~~, <u>underline</u> and ==highlight==.\n\n## What you can do\n\n- Link pages with [[Supplier notes]]\n- Pick a font and size from the toolbar\n- Ask the agent about this page from the panel on the right\n\n> Source and preview scroll together in Split view.\n\n### Next\n\n1. Try running the demo workflow\n2. Install a local model from the Models tab\n\n- [ ] Write your first note\n- [x] Open All In One\n\n---\n\nInline `code` works too.\n",
            );
            let _ = self.notes.write("Finance/Supplier notes.md", "# Supplier notes\n\nHarbour & Co usually pays late.\n");
            let _ = self.notes.write(
                "Finance/Invoices/14 Nov summary.md",
                "# Invoice summary, 14 Nov\n\nThree new invoices arrived overnight.\n\n## By supplier\n\n- **Harbour & Co** £1,240.00 due 28 Nov\n- **Ridgeway Supplies** £860.50 paid\n- **Brightside Print** £312.00 due 28 Nov\n\nSee [[Supplier notes]].\n",
            );
        }
        if std::fs::read_dir(self.workspace.join("workflows")).map(|mut d| d.next().is_none()).unwrap_or(true) {
            let wf = Workflow::demo();
            if let Ok(json) = wf.to_json() {
                let _ = std::fs::write(self.workspace.join("workflows").join(format!("{}.aio.json", sanitise(&wf.name))), json);
            }
        }
    }

    // ---------------------------------------------------------------- providers
    fn rebuild_provider(&mut self) {
        self.provider = build_provider(self.provider_kind, &self.ollama_url, &self.openai_url, &self.settings);
        if self.provider_kind == ProviderKind::Ollama {
            self.start_ollama();
        } else {
            self.refresh_installed();
        }
    }

    fn save_settings(&mut self) {
        self.settings.cloud_allowed = self.cloud_allowed;
        self.settings.allow_network = self.allow_network;
        self.settings.chosen_model = self.chosen_model.clone();
        if let Err(e) = self.settings.save(&self.workspace) {
            self.anthropic_note = format!("Could not save settings: {e}");
        }
    }

    /// Cloud model references offered in dropdowns, only for providers that have a key.
    fn cloud_refs(&self) -> Vec<String> {
        let mut out = vec![];
        if self.settings.anthropic().is_some() {
            out.extend(self.settings.claude_models().into_iter().map(|m| format!("{}{m}", cloud::ANTHROPIC_PREFIX)));
        }
        if self.settings.openai().is_some() {
            out.extend(self.settings.openai_models.iter().map(|m| format!("{}{m}", cloud::OPENAI_PREFIX)));
        }
        out
    }

    fn model_menu(&self) -> graph::ModelMenu {
        graph::ModelMenu { local: self.installed.iter().map(|m| m.name.clone()).collect(), cloud: self.cloud_refs(), cloud_allowed: self.cloud_allowed }
    }

    /// Re-reads the notebook page names at most every two seconds, for the note nodes.
    fn refresh_note_names(&mut self) {
        if self.note_names_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        self.note_names = self.notes.list().into_iter().map(|p| p.rel.trim_end_matches(".md").to_string()).collect();
        self.note_names_at = Some(Instant::now());
    }

    /// Asks the provider for its current model list and merges it into the settings.
    fn fetch_cloud_models(&mut self, anthropic: bool) {
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        if anthropic {
            let Some(key) = self.settings.anthropic() else { return };
            self.anthropic_note = "Fetching models…".into();
            self.rt.spawn(async move {
                let r = Anthropic::new(key).list_models().await.map(|v| v.into_iter().map(|m| m.name).collect::<Vec<_>>()).map_err(|e| e.to_string());
                let _ = tx.send(r);
                ctx.request_repaint();
            });
            self.anthropic_rx = Some(rx);
        } else {
            let Some(key) = self.settings.openai() else { return };
            self.openai_note = "Fetching models…".into();
            self.rt.spawn(async move {
                let r = cloud::openai(key).list_models().await.map(|v| v.into_iter().map(|m| m.name).filter(|n| chat_capable_openai(n)).collect::<Vec<_>>()).map_err(|e| e.to_string());
                let _ = tx.send(r);
                ctx.request_repaint();
            });
            self.openai_rx = Some(rx);
        }
    }

    fn set_cloud_key(&mut self, anthropic: bool, key: String) {
        if anthropic {
            self.settings.anthropic_key = key;
        } else {
            self.settings.openai_key = key;
        }
        self.save_settings();
        self.provider = build_provider(self.provider_kind, &self.ollama_url, &self.openai_url, &self.settings);
        let has = if anthropic { self.settings.anthropic().is_some() } else { self.settings.openai().is_some() };
        if has {
            self.fetch_cloud_models(anthropic);
        } else if anthropic {
            self.settings.anthropic_models.clear();
            self.anthropic_note.clear();
        } else {
            self.settings.openai_models.clear();
            self.openai_note.clear();
        }
        self.save_settings();
    }

    /// Starts a local Ollama in the background if it is not already running, then refreshes the model list.
    fn start_ollama(&mut self) {
        if self.provider_kind != ProviderKind::Ollama || self.ollama_rx.is_some() {
            return;
        }
        self.ollama_missing = false;
        self.ollama_status = "Starting Ollama…".into();
        let url = self.ollama_url.clone();
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let _ = tx.send(launcher::ensure_running(&url).await);
            ctx.request_repaint();
        });
        self.ollama_rx = Some(rx);
    }

    fn start_pull(&mut self, model: String) {
        let model = model.trim().to_string();
        if model.is_empty() || self.provider_kind != ProviderKind::Ollama || self.pulls.contains_key(&model) {
            return;
        }
        self.pulls.insert(model.clone(), PullState { status: "Starting download".into(), ..Default::default() });
        let (tx, rx) = unbounded_channel();
        self.pull_rx.push((model.clone(), forward(&self.rt, &self.ctx, rx)));
        let ollama = Ollama::new(self.ollama_url.clone());
        let name = model.clone();
        let handle = self.rt.spawn(async move {
            if let Err(e) = ollama.pull(&name, tx.clone()).await {
                let _ = tx.send(PullEvent::Error(e.to_string()));
            }
        });
        self.pull_tasks.insert(model, handle);
    }

    fn cancel_pull(&mut self, model: &str) {
        if let Some(h) = self.pull_tasks.remove(model) {
            h.abort();
        }
        self.pulls.remove(model);
        self.pull_rx.retain(|(n, _)| n != model);
    }

    fn is_installed(&self, model: &str) -> bool {
        self.installed.iter().any(|m| m.name == model || m.name == format!("{model}:latest"))
    }

    /// First model the workflow needs that Ollama does not have. None when everything is ready or the check does not apply.
    fn missing_model(&self, wf: &Workflow) -> Option<String> {
        if self.provider_kind != ProviderKind::Ollama || !self.installed_ok {
            return None;
        }
        let default = self.resolve_model();
        wf.nodes
            .iter()
            .filter_map(|n| match &n.kind {
                aio_core::NodeKind::Ai { model, .. } => Some(if model.trim().is_empty() { default.clone() } else { model.trim().to_string() }),
                _ => None,
            })
            .find(|m| !m.is_empty() && !cloud::is_cloud_ref(m) && !self.is_installed(m))
    }

    /// True when Ollama is the provider but not reachable; kicks off a start attempt.
    fn ollama_unavailable(&mut self) -> bool {
        if self.provider_kind == ProviderKind::Ollama && !self.installed_ok && self.installed_rx.is_none() {
            self.start_ollama();
            return true;
        }
        self.provider_kind == ProviderKind::Ollama && !self.installed_ok
    }

    fn refresh_installed(&mut self) {
        let provider = self.provider.clone();
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        self.installed_note = "Checking…".into();
        self.rt.spawn(async move {
            let _ = tx.send(provider.list_models().await.map_err(|e| e.to_string()));
            ctx.request_repaint();
        });
        self.installed_rx = Some(rx);
    }

    fn hw_eff(&self) -> HardwareProfile {
        let mut h = self.hw.clone();
        h.vram_bytes = (self.vram_gb > 0.0).then(|| (self.vram_gb as f64 * 1024.0 * 1024.0 * 1024.0) as u64);
        h
    }

    /// The model used when the user has not chosen one: the recommended model if installed, else any installed one.
    fn resolve_model(&self) -> String {
        if self.provider_kind == ProviderKind::Demo {
            return "echo".into();
        }
        if let Some(m) = &self.chosen_model {
            return m.clone();
        }
        let rec = hardware::recommended(&self.hw_eff(), Some("chat"));
        if let Some(r) = &rec {
            if self.is_installed(r.ollama_tag) {
                return r.ollama_tag.to_string();
            }
        }
        if let Some(m) = self.installed.first() {
            return m.name.clone();
        }
        rec.map(|r| r.ollama_tag.to_string()).unwrap_or_default()
    }

    // ---------------------------------------------------------------- workflows
    fn wf_dir(&self) -> PathBuf {
        self.workspace.join("workflows")
    }

    fn refresh_wf_files(&mut self) {
        let mut v: Vec<String> = std::fs::read_dir(self.wf_dir())
            .map(|d| d.flatten().filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".aio.json")).map(str::to_string)).collect())
            .unwrap_or_default();
        v.sort();
        self.wf_files = v;
    }

    fn load_workflow(&mut self, file_stem: &str) {
        let path = self.wf_dir().join(format!("{file_stem}.aio.json"));
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| Workflow::from_json(&s).map_err(|e| e.to_string())) {
            Ok(wf) => {
                self.wf_name = wf.name.clone();
                self.snarl = graph::build_snarl(&wf);
                self.status.clear();
                self.node_out.clear();
                self.last_run = None;
                self.wf_note = format!("Loaded {file_stem}");
            }
            Err(e) => self.wf_note = format!("Could not load {file_stem}: {e}"),
        }
    }

    fn save_workflow(&mut self) {
        let wf = graph::to_workflow(&self.snarl, &self.wf_name);
        let result = wf.to_json().map_err(|e| e.to_string()).and_then(|json| {
            let path = self.wf_dir().join(format!("{}.aio.json", sanitise(&self.wf_name)));
            std::fs::create_dir_all(self.wf_dir()).map_err(|e| e.to_string())?;
            std::fs::write(path, json).map_err(|e| e.to_string())
        });
        self.wf_note = match result {
            Ok(()) => "Saved".into(),
            Err(e) => format!("Save failed: {e}"),
        };
        self.refresh_wf_files();
    }

    /// True when any AI node in the workflow will use a local model.
    fn needs_local_model(&self, wf: &Workflow) -> bool {
        let default = self.resolve_model();
        wf.nodes.iter().any(|n| match &n.kind {
            aio_core::NodeKind::Ai { model, .. } => {
                let m = if model.trim().is_empty() { default.as_str() } else { model.trim() };
                !cloud::is_cloud_ref(m)
            }
            _ => false,
        })
    }

    fn run_workflow(&mut self) {
        if self.run_rx.is_some() {
            return;
        }
        let wf = graph::to_workflow(&self.snarl, &self.wf_name);
        if let Err(e) = wf.execution_order() {
            self.wf_note = format!("Cannot run: {e}");
            return;
        }
        if self.needs_local_model(&wf) && self.ollama_unavailable() {
            self.wf_note = "Starting Ollama. Run again in a moment.".into();
            return;
        }
        if let Some(model) = self.missing_model(&wf) {
            self.wf_note = format!("{model} is not installed yet");
            self.need_model = Some(model);
            return;
        }
        self.status.clear();
        self.node_out.clear();
        self.last_run = None;
        self.wf_note = "Running…".into();
        let (tx, rx) = unbounded_channel();
        self.run_rx = Some(forward(&self.rt, &self.ctx, rx));
        let provider = self.provider.clone();
        let opts = RunOptions {
            workspace: self.workspace.join("workflow-files"),
            default_model: self.resolve_model(),
            cloud_allowed: self.cloud_allowed,
            notes_root: self.workspace.join("notes"),
            allow_network: self.allow_network,
        };
        let _ = std::fs::create_dir_all(&opts.workspace);
        self.rt.spawn(async move {
            let _ = aio_core::run_workflow(&wf, provider, &opts, tx).await;
        });
    }

    fn update_available(&self) -> bool {
        self.update_result.is_none() && (self.release_newer() || self.tarball.as_ref().is_some_and(|t| update::is_available(t, self.exe_secs, self.settings.update_applied)))
    }

    fn release_newer(&self) -> bool {
        self.release.as_ref().is_some_and(|r| release::is_newer(&r.tag_name, env!("CARGO_PKG_VERSION")))
    }

    fn check_release(&mut self) {
        if self.release_rx.is_some() {
            return;
        }
        let repo = self.settings.repo();
        let token = self.settings.github();
        let beta = self.settings.include_prereleases;
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        self.release_note = "Checking…".into();
        self.release_checked = Some(Instant::now());
        self.rt.spawn(async move {
            let _ = tx.send(release::fetch_latest(&repo, token.as_deref(), beta).await);
            ctx.request_repaint();
        });
        self.release_rx = Some(rx);
    }

    fn start_release_install(&mut self) {
        let (Some(rel), Some(exe)) = (self.release.clone(), self.exe_path.clone()) else { return };
        if self.update_rx.is_some() {
            return;
        }
        self.update_log.clear();
        self.update_result = None;
        self.restart_exe = Some(exe.clone());
        let (tx, rx) = unbounded_channel();
        self.update_rx = Some(forward(&self.rt, &self.ctx, rx));
        let token = self.settings.github();
        self.rt.spawn(release::install_release(rel, token, exe, tx));
    }

    fn check_for_update(&mut self) {
        self.tarball = update::downloads_dir().and_then(|d| update::newest_tarball(&d));
        self.tarball_checked = Some(Instant::now());
    }

    fn start_update(&mut self) {
        let (Some(root), Some(t)) = (self.update_root.clone(), self.tarball.clone()) else { return };
        if self.update_rx.is_some() {
            return;
        }
        self.update_log.clear();
        self.update_result = None;
        let (tx, rx) = unbounded_channel();
        self.update_rx = Some(forward(&self.rt, &self.ctx, rx));
        self.restart_exe = Some(update::built_binary(&root));
        self.update_log.push(format!("Updating from {}", t.path.display()));
        self.rt.spawn(update::run_update(root, t.path, tx));
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if self.tarball_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) && self.update_rx.is_none() {
            self.check_for_update();
        }
        // GitHub release check: at startup and every six hours, unless switched off.
        let auto = !self.settings.disable_auto_update_check && std::env::var_os("AIO_NO_UPDATE_CHECK").is_none();
        if auto && self.release_rx.is_none() && self.update_rx.is_none() && self.release_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(6 * 3600)) {
            self.check_release();
        }
        // Developer hook: AIO_AUTOUPDATE_REL=1 installs a newer release as soon as one is found.
        if std::env::var_os("AIO_AUTOUPDATE_REL").is_some() && self.release_newer() && self.update_rx.is_none() && self.update_result.is_none() {
            self.start_release_install();
        }
        if let Some(rx) = &self.release_rx {
            match rx.try_recv() {
                Ok(res) => {
                    self.release_rx = None;
                    match res {
                        Ok(Some(rel)) => {
                            self.release_note = String::new();
                            self.release = Some(rel);
                        }
                        Ok(None) => {
                            self.release = None;
                            self.release_note = "No releases published yet.".into();
                        }
                        Err(e) => self.release_note = e,
                    }
                }
                Err(TryRecvError::Disconnected) => self.release_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.update_rx {
            let mut finished = false;
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    update::UpdateEvent::Line(l) => {
                        self.update_log.push(l);
                        if self.update_log.len() > 400 {
                            self.update_log.drain(..100);
                        }
                    }
                    update::UpdateEvent::Finished(r) => {
                        if r.is_ok() {
                            if let Some(t) = &self.tarball {
                                self.settings.update_applied = update::unix_secs(t.modified);
                            }
                        }
                        self.update_result = Some(r);
                        finished = true;
                    }
                }
            }
            if finished {
                self.update_rx = None;
                self.save_settings();
            }
        }
        for anthropic in [true, false] {
            let rx = if anthropic { &self.anthropic_rx } else { &self.openai_rx };
            let Some(rx) = rx else { continue };
            let got = match rx.try_recv() {
                Ok(v) => v,
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => Err("request was dropped".into()),
            };
            let mut list = match got {
                Ok(l) => l,
                Err(e) => {
                    let note = format!("Could not fetch models: {e}");
                    if anthropic {
                        self.anthropic_note = note;
                        self.anthropic_rx = None;
                    } else {
                        self.openai_note = note;
                        self.openai_rx = None;
                    }
                    continue;
                }
            };
            list.sort();
            list.dedup();
            let note = format!("{} models available", list.len());
            if anthropic {
                self.settings.anthropic_models = list;
                self.anthropic_note = note;
                self.anthropic_rx = None;
            } else {
                self.settings.openai_models = list;
                self.openai_note = note;
                self.openai_rx = None;
            }
            self.save_settings();
        }
        // models list
        if let Some(rx) = &self.installed_rx {
            match rx.try_recv() {
                Ok(Ok(list)) => {
                    self.installed_ok = true;
                    self.installed_note = format!("{} installed", list.len());
                    self.installed = list;
                    self.installed_rx = None;
                }
                Ok(Err(e)) => {
                    self.installed_ok = false;
                    self.installed.clear();
                    self.installed_note = e;
                    self.installed_rx = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.installed_rx = None,
            }
        }
        // Ollama start-up
        if let Some(rx) = &self.ollama_rx {
            let outcome = match rx.try_recv() {
                Ok(o) => Some(Some(o)),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(None),
            };
            if let Some(o) = outcome {
                self.ollama_rx = None;
                match o {
                    Some(StartOutcome::AlreadyRunning) => self.ollama_status = "Ollama is running".into(),
                    Some(StartOutcome::Started(child)) => {
                        self.ollama_child = child;
                        self.ollama_status = "Started Ollama in the background".into();
                    }
                    Some(StartOutcome::NotInstalled) => {
                        self.ollama_missing = true;
                        self.ollama_status = "Ollama is not installed".into();
                    }
                    Some(StartOutcome::Failed(e)) => self.ollama_status = e,
                    None => self.ollama_status = "Could not start Ollama".into(),
                }
                if !self.ollama_missing {
                    self.refresh_installed();
                    // Developer hook for demos: AIO_AUTOINSTALL=1 starts the recommended download at launch.
                    if std::env::var_os("AIO_AUTOINSTALL").is_some() {
                        if let Some(r) = hardware::recommended(&self.hw_eff(), Some("chat")) {
                            self.start_pull(r.ollama_tag.to_string());
                        }
                    }
                }
            }
        }
        // model downloads
        if !self.pull_rx.is_empty() {
            let mut refresh = false;
            let mut finished: Vec<String> = vec![];
            for (name, rx) in &self.pull_rx {
                loop {
                    match rx.try_recv() {
                        Ok(PullEvent::Status { text, completed, total }) => {
                            if let Some(p) = self.pulls.get_mut(name) {
                                p.status = text;
                                p.completed = completed;
                                p.total = total;
                            }
                        }
                        Ok(PullEvent::Done) => {
                            finished.push(name.clone());
                            refresh = true;
                            break;
                        }
                        Ok(PullEvent::Error(e)) => {
                            if let Some(p) = self.pulls.get_mut(name) {
                                p.error = Some(e);
                            }
                            finished.push(name.clone());
                            break;
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            finished.push(name.clone());
                            break;
                        }
                    }
                }
            }
            for n in &finished {
                self.pull_rx.retain(|(x, _)| x != n);
                self.pull_tasks.remove(n);
                if self.pulls.get(n).is_some_and(|p| p.error.is_none()) {
                    self.pulls.remove(n);
                }
            }
            if refresh {
                self.refresh_installed();
            }
        }
        if let Some(m) = self.need_model.clone() {
            if self.is_installed(&m) {
                self.need_model = None;
            }
        }
        // workflow run
        let mut run_done = false;
        if let Some(rx) = &self.run_rx {
            loop {
                match rx.try_recv() {
                    Ok(RunEvent::NodeStarted(id)) => {
                        self.status.insert(id, Status::Running);
                        self.node_out.insert(id, String::new());
                    }
                    Ok(RunEvent::NodeToken(id, t)) => self.node_out.entry(id).or_default().push_str(&t),
                    Ok(RunEvent::NodeFinished(id, s)) => {
                        self.status.insert(id, Status::Done);
                        self.node_out.insert(id, s);
                    }
                    Ok(RunEvent::NodeSkipped(id)) => {
                        self.status.insert(id, Status::Skipped);
                    }
                    Ok(RunEvent::NodeFailed(id, e)) => {
                        self.status.insert(id, Status::Failed);
                        self.node_out.insert(id, e);
                    }
                    Ok(RunEvent::Finished { ok }) => {
                        self.last_run = Some(ok);
                        self.wf_note = if ok { "Run finished".into() } else { "Run failed".into() };
                        run_done = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        run_done = true;
                        break;
                    }
                }
            }
        }
        if run_done {
            self.run_rx = None;
        }
        // page fetch
        if let Some(rx) = &self.page_rx {
            match rx.try_recv() {
                Ok(Ok(p)) => {
                    self.url = p.url.clone();
                    self.page_note = String::new();
                    self.page = Some(p);
                    self.page_rx = None;
                }
                Ok(Err(e)) => {
                    self.page_note = format!("Could not load page: {e}");
                    self.page_rx = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.page_rx = None,
            }
        }
        // chat stream
        let mut chat_done = false;
        if let Some(rx) = &self.chat_rx {
            loop {
                match rx.try_recv() {
                    Ok(ChatEvent::Token(t)) => {
                        if let Some(m) = self.chat.last_mut() {
                            m.content.push_str(&t);
                        }
                    }
                    Ok(ChatEvent::Done) => {
                        chat_done = true;
                        break;
                    }
                    Ok(ChatEvent::Error(e)) => {
                        if let Some(m) = self.chat.last_mut() {
                            m.content.push_str(&format!("\n\nCould not get a reply: {e}"));
                        }
                        chat_done = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        chat_done = true;
                        break;
                    }
                }
            }
        }
        if chat_done {
            self.chat_rx = None;
        }
        // note autosave
        if self.note_dirty {
            if self.note_edit_at.elapsed() > Duration::from_millis(700) {
                self.save_note();
            } else {
                ctx.request_repaint_after(Duration::from_millis(300));
            }
        }
    }

    // ---------------------------------------------------------------- notes
    /// Applies a formatting command to the editor's current selection and restores the cursor.
    fn apply_note_action(&mut self, ctx: &egui::Context, action: NoteAction) {
        use egui::text::{CCursor, CCursorRange};
        let id = egui::Id::new(EDITOR_ID);
        let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
        let len = self.note_text.chars().count();
        let sel = state
            .cursor
            .char_range()
            .map(|r| {
                let [a, b] = r.sorted_cursors();
                (a.index.0.min(len), b.index.0.min(len))
            })
            .unwrap_or((len, len));
        let edit = match action {
            NoteAction::Wrap(o, c) => fmt::toggle_wrap(&self.note_text, sel, o, c),
            NoteAction::Block(b) => fmt::set_block(&self.note_text, sel, b),
            NoteAction::Rule => fmt::insert_rule(&self.note_text, sel),
        };
        self.note_text = edit.text;
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(edit.sel.0), CCursor::new(edit.sel.1))));
        state.store(ctx, id);
        ctx.memory_mut(|mem| mem.request_focus(id));
        self.note_dirty = true;
        self.note_edit_at = Instant::now();
    }

    fn note_toolbar(&mut self, ui: &mut egui::Ui) {
        use fmt::Block;
        let mut action: Option<NoteAction> = None;
        egui::Frame::new().fill(CARD).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let btn = |ui: &mut egui::Ui, text: RichText, tip: &str| ui.add(egui::Button::new(text).min_size(egui::vec2(28.0, 26.0))).on_hover_text(tip).clicked();
                egui::ComboBox::from_id_salt("note_style").selected_text("Style").width(84.0).show_ui(ui, |ui| {
                    for (label, b) in [("Paragraph", Block::Paragraph), ("Heading 1", Block::Heading(1)), ("Heading 2", Block::Heading(2)), ("Heading 3", Block::Heading(3))] {
                        if ui.selectable_label(false, label).clicked() {
                            action = Some(NoteAction::Block(b));
                        }
                    }
                });
                ui.separator();
                if btn(ui, RichText::new("B").strong(), "Bold (Cmd/Ctrl+B)") {
                    action = Some(NoteAction::Wrap("**", "**"));
                }
                if btn(ui, RichText::new("I").italics(), "Italic (Cmd/Ctrl+I)") {
                    action = Some(NoteAction::Wrap("*", "*"));
                }
                if btn(ui, RichText::new("S").strikethrough(), "Strikethrough") {
                    action = Some(NoteAction::Wrap("~~", "~~"));
                }
                if btn(ui, RichText::new("U").underline(), "Underline") {
                    action = Some(NoteAction::Wrap("<u>", "</u>"));
                }
                if btn(ui, RichText::new("Hi").background_color(ACCENT).color(ON_ACCENT), "Highlight") {
                    action = Some(NoteAction::Wrap("==", "=="));
                }
                if btn(ui, RichText::new("</>").monospace(), "Inline code") {
                    action = Some(NoteAction::Wrap("`", "`"));
                }
                ui.separator();
                if btn(ui, RichText::new("•"), "Bulleted list") {
                    action = Some(NoteAction::Block(Block::Bullet));
                }
                if btn(ui, RichText::new("1."), "Numbered list") {
                    action = Some(NoteAction::Block(Block::Numbered));
                }
                if btn(ui, RichText::new("[ ]").monospace(), "Checklist") {
                    action = Some(NoteAction::Block(Block::Task));
                }
                if btn(ui, RichText::new("\u{201D}").strong(), "Quote") {
                    action = Some(NoteAction::Block(Block::Quote));
                }
                if btn(ui, RichText::new("—"), "Divider") {
                    action = Some(NoteAction::Rule);
                }
                if btn(ui, RichText::new("[[ ]]"), "Link to another page") {
                    action = Some(NoteAction::Wrap("[[", "]]"));
                }
                ui.separator();
                let current = self.doc_fonts.get(self.doc_font_idx).map(|f| f.name.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt("note_font").selected_text(current).width(96.0).show_ui(ui, |ui| {
                    for (i, f) in self.doc_fonts.iter().enumerate() {
                        ui.selectable_value(&mut self.doc_font_idx, i, &f.name);
                    }
                });
                ui.add(egui::DragValue::new(&mut self.doc_size).range(10.0..=36.0).speed(0.25).suffix(" pt")).on_hover_text("Font size (drag or type)");
            });
        });
        if let Some(a) = action {
            self.note_pending = Some(a);
        }
    }

    fn save_note(&mut self) {
        if let (Some(rel), true) = (self.note_sel.clone(), self.note_dirty) {
            if self.notes.write(&rel, &self.note_text).is_ok() {
                self.note_dirty = false;
            }
        }
    }

    fn open_note(&mut self, rel: &str) {
        self.save_note();
        match self.notes.read(rel) {
            Ok(t) => {
                self.note_text = t;
                self.note_sel = Some(rel.to_string());
                self.note_dirty = false;
            }
            Err(_) => self.note_sel = None,
        }
    }

    fn create_note(&mut self) {
        let name = self.note_new.trim().trim_end_matches(".md").to_string();
        if name.is_empty() {
            return;
        }
        let rel = format!("{}.md", name.replace('\\', "/").trim_matches('/'));
        let title = name.rsplit('/').next().unwrap_or(&name).to_string();
        if self.notes.read(&rel).is_err() && self.notes.write(&rel, &format!("# {title}\n\n")).is_err() {
            return;
        }
        self.note_new.clear();
        self.open_note(&rel);
    }

    // ---------------------------------------------------------------- browser
    fn go(&mut self) {
        match web::normalise_url(&self.url) {
            Ok(url) => {
                self.url = url.clone();
                self.page_note = "Loading…".into();
                let (tx, rx) = channel();
                let ctx = self.ctx.clone();
                self.rt.spawn(async move {
                    let _ = tx.send(web::fetch(&url).await);
                    ctx.request_repaint();
                });
                self.page_rx = Some(rx);
            }
            Err(e) => self.page_note = e,
        }
    }

    fn clip_page(&mut self) {
        let Some(p) = self.page.clone() else { return };
        let title: String = p.title.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' }).collect();
        let title = cap(title.split_whitespace().collect::<Vec<_>>().join(" ").as_str(), 60);
        let rel = format!("Clippings/{}.md", if title.is_empty() { "Clip" } else { &title });
        let body = format!("# {}\n\nSource: {}\n\n{}\n", p.title, p.url, cap(&p.text, 20_000));
        self.page_note = match self.notes.write(&rel, &body) {
            Ok(()) => format!("Saved to Notes as {rel}"),
            Err(e) => format!("Could not clip: {e}"),
        };
    }

    // ---------------------------------------------------------------- agent
    fn agent_context(&self) -> String {
        let mut s = String::from(
            "You are the assistant inside All In One, a desktop app with a workflow builder, notes, a browser and local models. Be concise and concrete. \
             You can only chat for now: you cannot yet run tools or edit anything yourself, so give steps or text the user can paste.\n",
        );
        match self.tab {
            Tab::Workflows => {
                let wf = graph::to_workflow(&self.snarl, &self.wf_name);
                s.push_str(&format!("The user is on the Workflows tab, editing \"{}\".\nNodes:\n", wf.name));
                for n in &wf.nodes {
                    s.push_str(&format!("- #{} {} ({})\n", n.id, n.title, n.kind.label()));
                }
                s.push_str("Wires:\n");
                for e in &wf.edges {
                    s.push_str(&format!("- #{}.{} -> #{}\n", e.from, e.from_port, e.to));
                }
                if let Some(ok) = self.last_run {
                    s.push_str(&format!("The last run {}.\n", if ok { "succeeded" } else { "failed" }));
                    for (id, out) in &self.node_out {
                        s.push_str(&format!("Output of #{id}: {}\n", cap(out, 400)));
                    }
                }
            }
            Tab::Notes => match &self.note_sel {
                Some(rel) => s.push_str(&format!("The user is on the Notes tab viewing \"{rel}\". Its content:\n---\n{}\n---\n", cap(&self.note_text, 6000))),
                None => s.push_str("The user is on the Notes tab with no page open.\n"),
            },
            Tab::Browser => match (&self.page, self.agent_sees_page) {
                (Some(p), true) => s.push_str(&format!("The user is on the Browser tab viewing \"{}\" ({}). Page text:\n---\n{}\n---\n", p.title, p.url, cap(&p.text, 6000))),
                (Some(_), false) => s.push_str("The user is on the Browser tab. They have not shared the page with you.\n"),
                (None, _) => s.push_str("The user is on the Browser tab with no page loaded.\n"),
            },
            Tab::Settings => s.push_str("The user is on the Settings tab (updates, folders).\n"),
            Tab::Models => {
                let hw = self.hw_eff();
                s.push_str(&format!("The user is on the Models tab. Machine: {} threads, {:.0} GB RAM, GPU memory {}.\n", hw.cpu_threads, hw.ram_gb(), hw.vram_gb().map_or("unknown".into(), |v| format!("{v:.0} GB"))));
            }
        }
        s
    }

    fn send_chat(&mut self, text: String) {
        if self.chat_rx.is_some() || text.trim().is_empty() {
            return;
        }
        self.chat.push(Msg { role: Role::User, content: text });
        self.chat.push(Msg { role: Role::Assistant, content: String::new() });
        let needed = self.resolve_model();
        let cloud_model = cloud::is_cloud_ref(&needed);
        if !cloud_model && self.ollama_unavailable() {
            if let Some(m) = self.chat.last_mut() {
                m.content = "Ollama is not running yet. I am starting it; try again in a few seconds.".into();
            }
            return;
        }
        if self.provider_kind == ProviderKind::Ollama && !cloud_model && !needed.is_empty() && !self.is_installed(&needed) {
            self.need_model = Some(needed.clone());
            if let Some(m) = self.chat.last_mut() {
                m.content = format!("The model {needed} is not installed yet. Use the Install button at the top of the window.");
            }
            return;
        }
        if self.provider.is_cloud_model(&needed) && !self.cloud_allowed {
            if let Some(m) = self.chat.last_mut() {
                m.content = "Blocked: this provider is off-machine and cloud models are switched off. Enable them in the Models tab if you want that.".into();
            }
            return;
        }
        let mut messages = vec![ChatMessage::new(Role::System, self.agent_context())];
        for m in &self.chat[..self.chat.len() - 1] {
            messages.push(ChatMessage::new(m.role, m.content.clone()));
        }
        let (tx, rx) = unbounded_channel();
        self.chat_rx = Some(forward(&self.rt, &self.ctx, rx));
        let provider = self.provider.clone();
        let req = ChatRequest { model: self.resolve_model(), messages };
        self.rt.spawn(async move {
            if let Err(e) = provider.chat(req, tx.clone()).await {
                let _ = tx.send(ChatEvent::Error(e.to_string()));
            }
        });
    }
}

// ==================================================================== UI
impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        egui::Panel::top("titlebar").exact_size(44.0).frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(16, 0))).show(ui, |ui| self.title_bar(ui));
        self.status_strip(ui);
        self.agent_panel(ui);
        match self.tab {
            Tab::Workflows => self.workflows_view(ui),
            Tab::Notes => self.notes_view(ui),
            Tab::Browser => self.browser_view(ui),
            Tab::Models => self.models_view(ui),
            Tab::Settings => self.settings_view(ui),
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Only stops an Ollama this app started itself.
        if let Some(mut c) = self.ollama_child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// A small filled circle drawn directly (the glyph is missing from some fonts).
fn paint_dot(ui: &mut egui::Ui, color: egui::Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(radius * 2.0 + 3.0, radius * 2.0 + 3.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1e9
}

fn size_of_tag(tag: &str) -> Option<f32> {
    hardware::catalogue().into_iter().find(|m| m.ollama_tag == tag).map(|m| m.size_gb)
}

impl App {
    /// One progress row for an active or failed download.
    fn pull_row(&mut self, ui: &mut egui::Ui, name: &str) {
        let Some(p) = self.pulls.get(name) else { return };
        let (status, completed, total, error) = (p.status.clone(), p.completed, p.total, p.error.clone());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Installing {name}")).strong());
            match error {
                Some(e) => {
                    ui.label(RichText::new(format!("failed: {e}")).color(BAD));
                    if ui.small_button("Retry").clicked() {
                        self.pulls.remove(name);
                        self.start_pull(name.to_string());
                    }
                    if ui.small_button("Dismiss").clicked() {
                        self.pulls.remove(name);
                    }
                }
                None => {
                    let frac = if total > 0 { completed as f32 / total as f32 } else { 0.0 };
                    let text = if total > 0 { format!("{:.0}%  {:.1} of {:.1} GB", frac * 100.0, gb(completed), gb(total)) } else { status };
                    ui.add(egui::ProgressBar::new(frac).desired_width(280.0).text(text).fill(ACCENT));
                    if ui.small_button("Cancel").clicked() {
                        self.cancel_pull(name);
                    }
                }
            }
        });
    }

    /// Banner under the title bar for Ollama start-up, missing models and downloads.
    fn status_strip(&mut self, ui: &mut egui::Ui) {
        let starting = self.ollama_rx.is_some();
        let ollama_down = self.provider_kind == ProviderKind::Ollama && !self.installed_ok && !starting && self.installed_rx.is_none();
        if !(starting || ollama_down || self.need_model.is_some() || !self.pulls.is_empty()) {
            return;
        }
        egui::Panel::top("status_strip").frame(Frame::new().fill(CARD).inner_margin(Margin::symmetric(16, 8))).show(ui, |ui| {
            if starting {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Starting Ollama…");
                });
            } else if ollama_down && self.ollama_missing {
                ui.horizontal(|ui| {
                    ui.label("Ollama is not installed. It runs local models on this machine.");
                    if theme::primary(ui, "Download Ollama").clicked() {
                        let _ = web::open_external("https://ollama.com/download");
                    }
                    if ui.button("I have installed it").clicked() {
                        self.start_ollama();
                    }
                });
            } else if ollama_down {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if self.ollama_status.is_empty() { "Ollama is not running".to_string() } else { self.ollama_status.clone() }).color(ACCENT));
                    if ui.button("Start Ollama").clicked() {
                        self.start_ollama();
                    }
                });
            }
            if let Some(model) = self.need_model.clone() {
                if !self.pulls.contains_key(&model) {
                    ui.horizontal(|ui| {
                        ui.label(format!("The model {model} is not installed yet."));
                        let label = size_of_tag(&model).map_or("Install".to_string(), |s| format!("Install ({s:.1} GB)"));
                        if theme::primary(ui, &label).clicked() {
                            self.start_pull(model.clone());
                        }
                        if ui.button("Dismiss").clicked() {
                            self.need_model = None;
                        }
                    });
                }
            }
            let names: Vec<String> = self.pulls.keys().cloned().collect();
            for n in names {
                self.pull_row(ui, &n);
            }
        });
    }

    fn title_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            for _ in 0..3 {
                paint_dot(ui, ACCENT, 4.0);
            }
            ui.label(RichText::new("All In One").strong().size(14.0));
            ui.add_space(24.0);
            for t in Tab::ALL {
                let active = self.tab == t;
                let badge = t == Tab::Settings && self.update_available();
                let label = t.name().to_string();
                let r = ui.add(egui::Button::new(RichText::new(label).color(if active { TEXT } else if badge { ACCENT } else { MUTED }).strong()).frame(false).min_size(egui::vec2(0.0, 44.0)));
                if active {
                    ui.painter().hline(r.rect.x_range(), r.rect.bottom() - 1.0, egui::Stroke::new(2.0, ACCENT));
                }
                if badge {
                    ui.painter().circle_filled(egui::pos2(r.rect.right() + 5.0, r.rect.center().y - 5.0), 3.5, ACCENT);
                    ui.add_space(10.0);
                }
                if r.clicked() {
                    self.save_note();
                    self.tab = t;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.tab == Tab::Workflows && !self.run_rx.is_some() && theme::primary(ui, "Run workflow").clicked() {
                    self.run_workflow();
                }
                let model = self.resolve_model();
                let (kind, dot) = match self.provider_kind {
                    _ if self.provider.is_cloud_model(&model) => ("Cloud", ACCENT),
                    ProviderKind::Demo => ("Demo", MUTED),
                    _ => ("Local", if self.installed.is_empty() { BAD } else { GOOD }),
                };
                theme::pill(ui, &format!("{kind} · {}", if model.is_empty() { "no model".to_string() } else { cloud::display_ref(&model) }), TEXT);
                paint_dot(ui, dot, 4.5);
            });
        });
    }

    fn agent_panel(&mut self, ui: &mut egui::Ui) {
        if !self.agent_open {
            egui::Panel::right("agent_rail").exact_size(48.0).frame(Frame::new().fill(PANEL).inner_margin(Margin::same(8))).show(ui, |ui| {
                if ui.add(egui::Button::new("◀").min_size(egui::vec2(32.0, 32.0))).on_hover_text("Expand agent panel").clicked() {
                    self.agent_open = true;
                }
                ui.add_space(8.0);
                ui.label(RichText::new("✦").color(ACCENT).size(20.0));
            });
            return;
        }
        egui::Panel::right("agent").default_size(360.0).size_range(300.0..=620.0).resizable(true).frame(Frame::new().fill(PANEL).inner_margin(Margin::same(12))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Agent").strong().size(14.0));
                theme::pill(ui, self.tab.name(), MUTED);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("▶").on_hover_text("Collapse agent panel").clicked() {
                        self.agent_open = false;
                    }
                    if !self.chat.is_empty() && ui.small_button("Clear").clicked() && self.chat_rx.is_none() {
                        self.chat.clear();
                    }
                });
            });
            ui.add_space(4.0);

            egui::Panel::bottom("agent_input").frame(Frame::new().fill(PANEL).inner_margin(Margin { top: 8, ..Default::default() })).show_separator_line(false).show(ui, |ui| {
                let busy = self.chat_rx.is_some();
                let resp = ui.add(egui::TextEdit::multiline(&mut self.chat_input).hint_text("Ask the agent about what you are looking at").desired_rows(3).desired_width(f32::INFINITY));
                let ctrl_enter = resp.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
                ui.horizontal(|ui| {
                    let cloud = if self.provider.is_cloud() { "Cloud provider" } else { "Runs on this machine" };
                    ui.label(RichText::new(cloud).size(12.0).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let clicked = ui.add_enabled(!busy, egui::Button::new(RichText::new("Send").color(ON_ACCENT).strong()).fill(ACCENT)).clicked();
                        if (clicked || ctrl_enter) && !busy {
                            let text = std::mem::take(&mut self.chat_input);
                            self.send_chat(text);
                        }
                    });
                });
            });

            let chips: &[&str] = match self.tab {
                Tab::Workflows => &["Explain this workflow", "Suggest a node to add"],
                Tab::Notes => &["Summarise this page", "List action items"],
                Tab::Browser => &["Summarise this page", "Extract the key facts"],
                Tab::Models => &["Which model should I use?", "How do I install it?"],
                Tab::Settings => &["How do I update?", "Where is my data stored?"],
            };
            ui.horizontal_wrapped(|ui| {
                for c in chips {
                    if ui.small_button(*c).clicked() && self.chat_rx.is_none() {
                        self.send_chat((*c).to_string());
                    }
                }
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                if self.chat.is_empty() {
                    ui.label(RichText::new("Nothing here yet. The agent sees the tab you are on and can answer questions about it. Tool use (editing workflows and notes) comes next.").color(MUTED));
                }
                for m in &self.chat {
                    match m.role {
                        Role::User => {
                            ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                                Frame::new().fill(RAISED).corner_radius(10).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
                                    ui.set_max_width(280.0);
                                    ui.label(&m.content);
                                });
                            });
                        }
                        _ => {
                            if m.content.is_empty() {
                                ui.spinner();
                            } else {
                                ui.label(&m.content);
                            }
                        }
                    }
                    ui.add_space(8.0);
                }
            });
        });
    }

    // ------------------------------------------------------------ workflows view
    fn workflows_view(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("wf_side").default_size(232.0).resizable(false).frame(Frame::new().fill(PANEL).inner_margin(Margin::same(12))).show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section_label(ui, "WORKFLOWS");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("+").on_hover_text("New workflow").clicked() {
                        self.snarl = Snarl::new();
                        self.wf_name = "Untitled".into();
                        self.status.clear();
                        self.node_out.clear();
                    }
                });
            });
            let mut to_load = None;
            for f in &self.wf_files {
                if ui.selectable_label(sanitise(&self.wf_name) == *f, f).clicked() {
                    to_load = Some(f.clone());
                }
            }
            if let Some(f) = to_load {
                self.load_workflow(&f);
            }
            ui.add_space(16.0);
            theme::section_label(ui, "INSTALLED MODELS");
            if self.installed.is_empty() {
                ui.label(RichText::new(&self.installed_note).size(12.0).color(MUTED));
            }
            for m in &self.installed {
                ui.horizontal(|ui| {
                    ui.label(&m.name);
                    if m.size_bytes > 0 {
                        ui.label(RichText::new(format!("{:.1} GB", m.size_bytes as f64 / 1e9)).size(12.0).color(MUTED));
                    }
                });
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                Frame::new().fill(CARD).stroke(egui::Stroke::new(1.0, LINE)).corner_radius(12).inner_margin(Margin::same(12)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let hw = self.hw_eff();
                    if ui.small_button("See recommended models").clicked() {
                        self.tab = Tab::Models;
                    }
                    if let Some(r) = hardware::recommended(&hw, Some("chat")) {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Best fit").size(12.0).color(MUTED));
                            theme::badge(ui, &format!("{} {}B", r.family, r.params_b));
                        });
                    }
                    ui.label(RichText::new(format!("Memory   {:.0} GB RAM", hw.ram_gb())).size(12.0).color(MUTED));
                    ui.label(RichText::new(format!("GPU memory   {}", hw.vram_gb().map_or("not detected".into(), |v| format!("{v:.0} GB")))).size(12.0).color(MUTED));
                    ui.label(RichText::new("This machine").strong());
                });
            });
        });

        egui::Panel::bottom("wf_dock").default_size(210.0).size_range(100.0..=500.0).resizable(true).frame(Frame::new().fill(PANEL).inner_margin(Margin::same(12))).show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::section_label(ui, "RUN OUTPUT");
                match self.last_run {
                    Some(true) => ui.label(RichText::new("finished").size(12.0).color(GOOD)),
                    Some(false) => ui.label(RichText::new("failed").size(12.0).color(BAD)),
                    None if self.run_rx.is_some() => ui.label(RichText::new("running").size(12.0).color(ACCENT)),
                    None => ui.label(RichText::new("not run yet").size(12.0).color(MUTED)),
                };
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let ids: Vec<u32> = self.node_out.keys().copied().collect();
                for id in ids {
                    let title = self.snarl.get_node(SnarlId((id - 1) as usize)).map(|n| n.title.clone()).unwrap_or_else(|| format!("#{id}"));
                    let st = self.status.get(&id).copied();
                    let (txt, col) = match st {
                        Some(Status::Done) => ("done", GOOD),
                        Some(Status::Failed) => ("failed", BAD),
                        Some(Status::Running) => ("running", ACCENT),
                        _ => ("", MUTED),
                    };
                    egui::CollapsingHeader::new(RichText::new(format!("{title}  {txt}")).color(col)).id_salt(("out", id)).default_open(true).show(ui, |ui| {
                        let out = self.node_out.get(&id).map(String::as_str).unwrap_or("");
                        ui.label(RichText::new(if out.is_empty() { "(no output)" } else { out }).monospace().size(12.5));
                    });
                }
                if self.node_out.is_empty() {
                    ui.label(RichText::new("Press Run workflow. AI nodes stream their output here as it arrives.").color(MUTED));
                }
            });
        });

        egui::CentralPanel::default().frame(Frame::new().fill(BG)).show(ui, |ui| {
            Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Workflows /").color(MUTED));
                    ui.add(egui::TextEdit::singleline(&mut self.wf_name).desired_width(200.0));
                    if ui.button("Save").clicked() {
                        self.save_workflow();
                    }
                    ui.label(RichText::new(&self.wf_note).size(12.0).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("Right-click the canvas to add nodes").size(12.0).color(MUTED));
                    });
                });
            });
            // Node hotbar: always visible. Pick a group, then click a node to drop it in the middle of the canvas.
            Frame::new().fill(CARD).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    theme::section_label(ui, "ADD NODE");
                    ui.add_space(6.0);
                    for cat in aio_core::Category::ALL {
                        if ui.selectable_label(self.hotbar_cat == cat, cat.label()).clicked() {
                            self.hotbar_cat = cat;
                        }
                    }
                    ui.separator();
                    egui::ScrollArea::horizontal().id_salt("hotbar_scroll").auto_shrink([false, true]).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (label, kind) in graph::new_kinds().into_iter().filter(|(_, k)| k.category() == self.hotbar_cat) {
                                let color = graph::kind_color(&kind);
                                let resp = ui.add(egui::Button::new(RichText::new(format!("    {label}"))).min_size(egui::vec2(0.0, 26.0))).on_hover_text(graph::describe(&kind));
                                let dot = egui::Rect::from_min_size(resp.rect.left_top() + egui::vec2(4.0, resp.rect.height() / 2.0 - 3.0), egui::vec2(6.0, 6.0));
                                ui.painter().rect_filled(dot, 2.0, color);
                                if resp.clicked() {
                                    self.pending_add = Some((label.to_string(), kind));
                                }
                            }
                        });
                    });
                });
            });
            self.refresh_note_names();
            let menu = self.model_menu();
            let files_root = self.workspace.join("workflow-files");
            let model = self.resolve_model();
            let mut viewer = graph::Viewer { xf: &mut self.canvas_xf, status: &self.status, recommended_model: &model, models: &menu, notes: &self.note_names, files_root: &files_root };
            let canvas_area = ui.available_rect_before_wrap();
            SnarlWidget::new().id_salt("workflow_canvas").show(&mut self.snarl, &mut viewer, ui);
            // Wait until the canvas reports a usable transform so the node lands inside the visible area.
            let usable = self.canvas_xf.filter(|t| t.scaling.is_finite() && t.scaling > 0.001 && t.translation.x.is_finite() && t.translation.y.is_finite());
            if self.pending_add.is_some() {
                match usable {
                    None => ui.ctx().request_repaint(),
                    Some(t) => {
                        let at = t.inverse() * canvas_area.center();
                        if at.x.is_finite() && at.y.is_finite() {
                            let (title, kind) = self.pending_add.take().expect("checked");
                            let stagger = (self.snarl.nodes_ids_data().count() % 6) as f32 * 28.0;
                            let pos = egui::pos2(at.x - 108.0 + stagger, at.y - 40.0 + stagger);
                            self.snarl.insert_node(pos, Node { id: 0, title, kind, pos: (pos.x, pos.y) });
                        } else {
                            ui.ctx().request_repaint();
                        }
                    }
                }
            }
        });
    }

    // ------------------------------------------------------------ notes view
    fn notes_view(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("notes_side").default_size(260.0).resizable(true).frame(Frame::new().fill(PANEL).inner_margin(Margin::same(12))).show(ui, |ui| {
            theme::section_label(ui, "NOTEBOOKS");
            ui.add(egui::TextEdit::singleline(&mut self.note_search).hint_text("Search notes").desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.note_new).hint_text("New page, e.g. Finance/Ideas").desired_width(170.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Create").clicked() || enter {
                    self.create_note();
                }
            });
            ui.add_space(6.0);
            let pages = self.notes.search(&self.note_search);
            let mut open = None;
            let mut jump_page = None;
            let mut jump_heading = None;
            let current_title = self.note_sel.as_deref().map(|r| r.rsplit('/').next().unwrap_or(r).trim_end_matches(".md").to_string());
            if let Some(title) = current_title {
                // Outline, backlinks and links live in a resizable block at the bottom of the sidebar.
                egui::Panel::bottom("note_meta").default_size(230.0).size_range(90.0..=520.0).resizable(true).frame(Frame::new().fill(PANEL).inner_margin(Margin { top: 6, ..Default::default() })).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("note_meta_scroll").auto_shrink([false, false]).show(ui, |ui| {
                        theme::section_label(ui, "OUTLINE");
                        let heads = md::headings(&self.note_text);
                        if heads.is_empty() {
                            ui.label(RichText::new("Add a heading to build an outline").size(12.0).color(MUTED));
                        }
                        for (i, h) in heads.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.add_space(12.0 * (h.level - 1) as f32);
                                let text = RichText::new(&h.text).size(if h.level == 1 { 13.5 } else { 12.5 });
                                if ui.link(if h.level == 1 { text.strong() } else { text }).clicked() {
                                    jump_heading = Some(i);
                                }
                            });
                        }
                        ui.add_space(8.0);
                        theme::section_label(ui, "BACKLINKS");
                        let backs = self.notes.backlinks(&title);
                        if backs.is_empty() {
                            ui.label(RichText::new("None").size(12.0).color(MUTED));
                        }
                        for b in &backs {
                            if ui.link(b.title()).clicked() {
                                jump_page = Some(b.rel.clone());
                            }
                        }
                        ui.add_space(8.0);
                        theme::section_label(ui, "LINKS");
                        let all = self.notes.list();
                        for l in md::links(&self.note_text) {
                            match all.iter().find(|p| p.title() == l) {
                                Some(p) => {
                                    if ui.link(&l).clicked() {
                                        jump_page = Some(p.rel.clone());
                                    }
                                }
                                None => {
                                    ui.label(RichText::new(format!("{l} (missing)")).size(12.0).color(MUTED));
                                }
                            }
                        }
                    });
                });
            }
            egui::ScrollArea::vertical().id_salt("note_tree").auto_shrink([false, false]).show(ui, |ui| {
                let mut last_dir = String::new();
                for p in &pages {
                    let dir = p.rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
                    if dir != last_dir {
                        if !dir.is_empty() {
                            ui.label(RichText::new(dir.replace('/', " / ")).size(12.5).color(MUTED));
                        }
                        last_dir = dir.clone();
                    }
                    ui.horizontal(|ui| {
                        if !dir.is_empty() {
                            ui.add_space(14.0 * dir.matches('/').count().max(0) as f32 + 12.0);
                        }
                        if ui.selectable_label(self.note_sel.as_deref() == Some(p.rel.as_str()), p.title()).clicked() {
                            open = Some(p.rel.clone());
                        }
                    });
                }
                if pages.is_empty() {
                    ui.label(RichText::new("No pages match").color(MUTED));
                }
            });
            if let Some(rel) = open.or(jump_page) {
                self.open_note(&rel);
            }
            if let Some(i) = jump_heading {
                self.jump_target = Some(i);
                self.jump_lead = 3;
                if self.note_mode == NoteMode::Source {
                    // Source mode has no preview: scroll the editor to the heading's row.
                    ui.ctx().request_repaint();
                }
            }
        });

        egui::CentralPanel::default().frame(Frame::new().fill(BG)).show(ui, |ui| {
            let Some(rel) = self.note_sel.clone() else {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("Select a page or create one on the left").color(MUTED)));
                return;
            };
            Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(rel.trim_end_matches(".md").replace('/', " / ")).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.selectable_value(&mut self.note_mode, NoteMode::Preview, "Preview");
                        ui.selectable_value(&mut self.note_mode, NoteMode::Split, "Split");
                        ui.selectable_value(&mut self.note_mode, NoteMode::Source, "Source");
                    });
                });
            });
            self.note_toolbar(ui);
            let words = self.note_text.split_whitespace().count();
            egui::Panel::bottom("note_status").frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 4))).show_separator_line(false).show(ui, |ui| {
                ui.label(RichText::new(format!("Markdown · {words} words · {}", if self.note_dirty { "saving…" } else { "saved" })).size(12.0).color(MUTED));
            });
            let ctx = ui.ctx().clone();
            let editor_id = egui::Id::new(EDITOR_ID);
            // Keyboard shortcuts while the editor has focus.
            if ctx.memory(|m| m.has_focus(editor_id)) {
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::B)) {
                    self.note_pending = Some(NoteAction::Wrap("**", "**"));
                }
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::I)) {
                    self.note_pending = Some(NoteAction::Wrap("*", "*"));
                }
            }
            if let Some(a) = self.note_pending.take() {
                self.apply_note_action(&ctx, a);
            }
            let mode = self.note_mode;
            let mut changed = false;
            let style = md::DocStyle { font: self.doc_fonts[self.doc_font_idx.min(self.doc_fonts.len() - 1)].clone(), size: self.doc_size };
            let pointer = ui.input(|i| i.pointer.hover_pos());
            let heads = md::headings(&self.note_text);
            // A heading click is consumed once by the pane that shows it; the preview then leads the scroll sync for a few frames.
            let target = self.jump_target.take();
            let leading = self.jump_lead > 0;
            self.jump_lead = self.jump_lead.saturating_sub(1);
            let mut ys: Vec<f32> = vec![];
            match mode {
                NoteMode::Source => {
                    let mut area = egui::ScrollArea::vertical().id_salt("note_edit").auto_shrink([false, false]);
                    if let Some(y) = target.and_then(|i| self.editor_ys.get(i)) {
                        area = area.vertical_scroll_offset((*y - 8.0).max(0.0));
                    }
                    area.show(ui, |ui| {
                        let (c, y) = note_editor(ui, &mut self.note_text, editor_id, &heads);
                        changed |= c;
                        ys = y;
                    });
                }
                NoteMode::Preview => {
                    egui::ScrollArea::vertical().id_salt("note_prev").auto_shrink([false, false]).show(ui, |ui| {
                        Frame::new().inner_margin(Margin::same(24)).show(ui, |ui| md::render(ui, &self.note_text, &style, target));
                    });
                }
                NoteMode::Split => {
                    // The pane under the pointer leads; the other follows, so source and preview scroll together.
                    ui.columns(2, |cols| {
                        let over_preview = leading || pointer.is_some_and(|p| cols[1].max_rect().contains(p));
                        let mut ed_area = egui::ScrollArea::vertical().id_salt("note_edit").auto_shrink([false, false]);
                        if over_preview {
                            ed_area = ed_area.vertical_scroll_offset(self.scroll_frac * self.editor_max);
                        }
                        let ed = ed_area.show(&mut cols[0], |ui| {
                            let (c, y) = note_editor(ui, &mut self.note_text, editor_id, &heads);
                            changed |= c;
                            ys = y;
                        });
                        self.editor_max = (ed.content_size.y - ed.inner_rect.height()).max(0.0);
                        if !over_preview && self.editor_max > 0.0 {
                            self.scroll_frac = (ed.state.offset.y / self.editor_max).clamp(0.0, 1.0);
                        }
                        let mut pv_area = egui::ScrollArea::vertical().id_salt("note_prev2").auto_shrink([false, false]);
                        if !over_preview {
                            pv_area = pv_area.vertical_scroll_offset(self.scroll_frac * self.preview_max);
                        }
                        let pv = pv_area.show(&mut cols[1], |ui| {
                            Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| md::render(ui, &self.note_text, &style, target));
                        });
                        self.preview_max = (pv.content_size.y - pv.inner_rect.height()).max(0.0);
                        if over_preview && self.preview_max > 0.0 {
                            self.scroll_frac = (pv.state.offset.y / self.preview_max).clamp(0.0, 1.0);
                        }
                    });
                }
            }
            if !ys.is_empty() {
                self.editor_ys = ys;
            }
            if self.jump_lead > 0 {
                ctx.request_repaint();
            }
            if changed {
                self.note_dirty = true;
                self.note_edit_at = Instant::now();
            }
        });
    }

    // ------------------------------------------------------------ browser view
    fn browser_view(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().frame(Frame::new().fill(BG).inner_margin(Margin::same(16))).show(ui, |ui| {
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.url).hint_text("Enter an address").desired_width((ui.available_width() - 420.0).max(200.0)));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if theme::primary(ui, "Go").clicked() || enter {
                    self.go();
                }
                ui.checkbox(&mut self.agent_sees_page, "Agent can see this page");
                if ui.add_enabled(self.page.is_some(), egui::Button::new("Clip to note")).clicked() {
                    self.clip_page();
                }
                if ui.button("Open in system browser").clicked() {
                    if let Err(e) = web::open_external(&self.url) {
                        self.page_note = e;
                    }
                }
            });
            ui.add_space(8.0);
            Frame::new().fill(CARD).stroke(egui::Stroke::new(1.0, LINE)).corner_radius(10).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Text view. The embedded browser (Servo) is decided by the Phase 0 spike. Pages here load without scripts or sign-ins, so use the system browser for those.").size(12.5).color(MUTED));
            });
            if !self.page_note.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(&self.page_note).color(ACCENT));
            }
            ui.add_space(8.0);
            Frame::new().fill(Color32Ext::paper()).corner_radius(10).inner_margin(Margin::same(20)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match &self.page {
                    Some(p) => {
                        ui.label(RichText::new(&p.title).size(22.0).strong().color(Color32Ext::ink()));
                        ui.label(RichText::new(&p.url).size(12.0).color(egui::Color32::from_rgb(0x66, 0x66, 0x66)));
                        ui.add_space(10.0);
                        ui.label(RichText::new(&p.text).color(Color32Ext::ink()));
                    }
                    None => {
                        ui.label(RichText::new("Enter an address above and press Go.").color(egui::Color32::from_rgb(0x66, 0x66, 0x66)));
                    }
                });
            });
        });
    }

    // ------------------------------------------------------------ models view
    fn models_view(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().frame(Frame::new().fill(BG).inner_margin(Margin::same(20))).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.label(RichText::new("Models").size(20.0).strong());
                ui.add_space(12.0);

                if self.provider_kind == ProviderKind::Ollama && self.installed_ok && self.installed.is_empty() && self.pulls.is_empty() {
                    if let Some(rec) = hardware::recommended(&self.hw_eff(), Some("chat")) {
                        Frame::new().fill(CARD).stroke(egui::Stroke::new(1.0, ACCENT)).corner_radius(12).inner_margin(Margin::same(14)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("Get started").strong());
                            ui.label(RichText::new("No local models are installed. One click installs the best fit for this machine.").color(MUTED));
                            ui.add_space(4.0);
                            if theme::primary(ui, &format!("Install {} {}B ({:.1} GB)", rec.family, rec.params_b, rec.size_gb)).clicked() {
                                self.start_pull(rec.ollama_tag.to_string());
                            }
                        });
                        ui.add_space(12.0);
                    }
                }

                Frame::new().fill(CARD).stroke(egui::Stroke::new(1.0, LINE)).corner_radius(12).inner_margin(Margin::same(14)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("This machine").strong());
                    ui.label(RichText::new(format!("{}  ·  {} threads  ·  {:.0} GB RAM", self.hw.cpu_name, self.hw.cpu_threads, self.hw.ram_gb())).color(MUTED));
                    ui.horizontal(|ui| {
                        ui.label("GPU memory (GB)");
                        ui.add(egui::DragValue::new(&mut self.vram_gb).range(0.0..=256.0).speed(0.5));
                        ui.label(RichText::new(if self.hw.vram_bytes.is_some() { "detected, adjust if wrong" } else { "not detected: set it if you have a GPU (0 = CPU only)" }).size(12.0).color(MUTED));
                    });
                });
                ui.add_space(12.0);

                Frame::new().fill(CARD).stroke(egui::Stroke::new(1.0, LINE)).corner_radius(12).inner_margin(Margin::same(14)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Provider").strong());
                    let before = (self.provider_kind, self.ollama_url.clone(), self.openai_url.clone());
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.provider_kind, ProviderKind::Ollama, "Ollama");
                        ui.selectable_value(&mut self.provider_kind, ProviderKind::OpenAiCompat, "OpenAI-compatible");
                        ui.selectable_value(&mut self.provider_kind, ProviderKind::Demo, "Demo (offline echo)");
                    });
                    match self.provider_kind {
                        ProviderKind::Ollama => {
                            ui.add(egui::TextEdit::singleline(&mut self.ollama_url).desired_width(320.0));
                        }
                        ProviderKind::OpenAiCompat => {
                            ui.add(egui::TextEdit::singleline(&mut self.openai_url).desired_width(320.0));
                            ui.label(RichText::new("llama.cpp server, LM Studio, vLLM and similar. Treated as local.").size(12.0).color(MUTED));
                        }
                        ProviderKind::Demo => {
                            ui.label(RichText::new("Echoes prompts back so you can try workflows without a model.").size(12.0).color(MUTED));
                        }
                    }
                    if before != (self.provider_kind, self.ollama_url.clone(), self.openai_url.clone()) {
                        self.rebuild_provider();
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Refresh installed").clicked() {
                            self.refresh_installed();
                        }
                        ui.label(RichText::new(&self.installed_note).size(12.0).color(MUTED));
                    });
                });
                ui.add_space(12.0);
                self.cloud_section(ui);
                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    ui.label(RichText::new("Model for AI nodes and the agent").strong());
                    let label = self.chosen_model.as_deref().map(cloud::display_ref).unwrap_or_else(|| "Auto (recommended)".into());
                    egui::ComboBox::from_id_salt("model_choice").selected_text(label).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.chosen_model, None, "Auto (recommended)");
                        for m in self.installed.clone() {
                            ui.selectable_value(&mut self.chosen_model, Some(m.name.clone()), m.name);
                        }
                        let refs = self.cloud_refs();
                        if !refs.is_empty() {
                            ui.separator();
                            for m in refs {
                                let name = cloud::display_ref(&m);
                                ui.selectable_value(&mut self.chosen_model, Some(m), name);
                            }
                        }
                    });
                    ui.label(RichText::new(format!("currently {}", cloud::display_ref(&self.resolve_model()))).size(12.0).color(MUTED));
                });
                ui.horizontal(|ui| {
                    ui.label("Show");
                    ui.selectable_value(&mut self.task, None, "All");
                    ui.selectable_value(&mut self.task, Some("chat"), "Chat");
                    ui.selectable_value(&mut self.task, Some("code"), "Code");
                });
                ui.add_space(6.0);
                let hw = self.hw_eff();
                let ranked = hardware::rank(&hw, self.task);
                egui::Grid::new("catalogue").striped(true).num_columns(5).spacing([20.0, 8.0]).show(ui, |ui| {
                    for h in ["Model", "Size", "Fit on this machine", "", ""] {
                        ui.label(RichText::new(h).size(12.0).color(MUTED));
                    }
                    ui.end_row();
                    for r in &ranked {
                        let installed = self.installed.iter().any(|m| m.name == r.model.ollama_tag);
                        ui.horizontal(|ui| {
                            ui.label(format!("{} {}B · {}", r.model.family, r.model.params_b, r.model.quant));
                            if r.recommended {
                                theme::badge(ui, "Recommended");
                            }
                        });
                        ui.label(format!("{:.1} GB", r.model.size_gb));
                        let col = match r.fit {
                            hardware::Fit::Comfortable => GOOD,
                            hardware::Fit::Partial => ACCENT,
                            hardware::Fit::Slow => MUTED,
                            hardware::Fit::TooLarge => BAD,
                        };
                        ui.label(RichText::new(r.fit.label()).color(col));
                        if installed {
                            if ui.button("Use").clicked() {
                                self.chosen_model = Some(r.model.ollama_tag.to_string());
                            }
                        } else if self.provider_kind != ProviderKind::Ollama {
                            ui.label("");
                        } else if let Some(p) = self.pulls.get(r.model.ollama_tag) {
                            let frac = if p.total > 0 { p.completed as f32 / p.total as f32 } else { 0.0 };
                            ui.add(egui::ProgressBar::new(frac).desired_width(140.0).text(format!("{:.0}%", frac * 100.0)).fill(ACCENT));
                        } else if ui.button("Install").clicked() {
                            self.start_pull(r.model.ollama_tag.to_string());
                        }
                        ui.label(RichText::new(if installed { "installed" } else { "" }).size(12.0).color(GOOD));
                        ui.end_row();
                    }
                });
                ui.add_space(8.0);
                if self.provider_kind == ProviderKind::Ollama {
                    ui.horizontal(|ui| {
                        ui.label("Install any other Ollama model");
                        let r = ui.add(egui::TextEdit::singleline(&mut self.custom_model).hint_text("e.g. mistral:7b").desired_width(200.0));
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (ui.button("Install").clicked() || enter) && !self.custom_model.trim().is_empty() {
                            let name = std::mem::take(&mut self.custom_model);
                            self.start_pull(name);
                        }
                        if ui.link("Browse library").clicked() {
                            let _ = web::open_external("https://ollama.com/library");
                        }
                    });
                    ui.add_space(6.0);
                }
                ui.label(RichText::new("Recommended is a suggestion based on your memory. Any model can be chosen, including ones not listed here.").size(12.0).color(MUTED));
            });
        });
        if self.settings.cloud_allowed != self.cloud_allowed || self.settings.allow_network != self.allow_network || self.settings.chosen_model != self.chosen_model {
            self.save_settings();
        }
    }

    // ------------------------------------------------------------ updates
    fn release_card(&mut self, ui: &mut egui::Ui) {
        let newer = self.release_newer();
        let running = self.update_rx.is_some();
        let checking = self.release_rx.is_some();
        let target = release::platform_target();
        Frame::new().fill(CARD).stroke(egui::Stroke::new(1.0, if newer { ACCENT } else { LINE })).corner_radius(12).inner_margin(Margin::same(14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Updates").strong());
            ui.label(RichText::new(format!("Version {}  ·  {}  ·  {}", env!("CARGO_PKG_VERSION"), target.unwrap_or("no prebuilt updates for this platform"), if self.settings.include_prereleases { "beta channel" } else { "stable channel" })).size(12.5).color(MUTED));
            ui.add_space(4.0);
            match (&self.release, newer, &self.update_result) {
                (_, _, Some(Ok(()))) => {
                    ui.label(RichText::new("Installed. Restart to use the new version.").color(GOOD));
                }
                (Some(r), true, _) => {
                    ui.label(RichText::new(format!("Version {} is available", r.tag_name.trim_start_matches('v'))).color(ACCENT));
                }
                (Some(_), false, _) => {
                    ui.label(RichText::new("You are up to date.").color(GOOD));
                }
                (None, _, _) => {
                    ui.label(RichText::new(if checking { "Checking for updates…" } else { "Not checked yet." }).color(MUTED));
                }
            }
            if !self.release_note.is_empty() && !checking {
                ui.label(RichText::new(&self.release_note).size(12.0).color(if self.release.is_some() { MUTED } else { BAD }));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let done = matches!(self.update_result, Some(Ok(())));
                if done {
                    self.restart_button(ui);
                } else {
                    let can = newer && !running && self.exe_path.is_some() && target.is_some();
                    let text = match (&self.release, running) {
                        (_, true) => "Updating…".to_string(),
                        (Some(r), _) if newer => format!("Update to {}", r.tag_name),
                        _ => "Update now".to_string(),
                    };
                    if ui.add_enabled(can, egui::Button::new(RichText::new(text).strong().color(if can { ON_ACCENT } else { MUTED })).fill(if can { ACCENT } else { RAISED }).min_size(egui::vec2(140.0, 30.0))).clicked() {
                        self.start_release_install();
                    }
                }
                if ui.add_enabled(!checking && !running, egui::Button::new("Check now")).clicked() {
                    self.check_release();
                }
                if running || checking {
                    ui.spinner();
                }
            });
            if newer && !running && self.update_result.is_none() {
                if let Some(body) = self.release.as_ref().and_then(|r| r.body.as_deref()).filter(|b| !b.trim().is_empty()) {
                    ui.add_space(6.0);
                    ui.label(RichText::new("What's new").size(12.0).color(MUTED));
                    egui::ScrollArea::vertical().id_salt("release_notes").max_height(120.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(body.chars().take(1500).collect::<String>()).size(12.0)).wrap_mode(egui::TextWrapMode::Wrap));
                    });
                }
            }
            self.update_log_view(ui);
            ui.add_space(6.0);
            egui::CollapsingHeader::new(RichText::new("Source and options").size(12.5).color(MUTED)).default_open(false).show(ui, |ui| {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("GitHub repository");
                    changed |= ui.add(egui::TextEdit::singleline(&mut self.settings.update_repo).hint_text(settings::DEFAULT_REPO).desired_width(260.0)).changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Access token");
                    changed |= ui.add(egui::TextEdit::singleline(&mut self.settings.github_token).password(true).hint_text("only needed for a private repository").desired_width(260.0)).changed();
                });
                let mut auto = !self.settings.disable_auto_update_check;
                if ui.checkbox(&mut auto, "Check for updates automatically").changed() {
                    self.settings.disable_auto_update_check = !auto;
                    changed = true;
                }
                if ui.checkbox(&mut self.settings.include_prereleases, "Include pre-releases (beta channel)").changed() {
                    changed = true;
                    self.release = None;
                }
                ui.label(RichText::new("Checking contacts api.github.com and sends nothing about you. A private repository needs a fine-grained token with read-only access to Contents. The token is stored in settings.json.").size(12.0).color(MUTED));
                if changed {
                    self.save_settings();
                    self.repo_changed = true;
                }
            });
        });
        if self.repo_changed && !ui.ctx().memory(|m| m.focused().is_some()) {
            self.repo_changed = false;
            self.release = None;
            self.check_release();
        }
    }

    fn restart_button(&mut self, ui: &mut egui::Ui) {
        if theme::primary(ui, "Restart now").clicked() {
            match self.restart_exe.as_deref().map(update::launch_exe) {
                Some(Ok(())) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                Some(Err(e)) => self.update_result = Some(Err(e)),
                None => {}
            }
        }
    }

    fn update_log_view(&mut self, ui: &mut egui::Ui) {
        if let Some(Err(e)) = &self.update_result {
            ui.add_space(4.0);
            ui.label(RichText::new(e).color(BAD));
        }
        if !self.update_log.is_empty() {
            ui.add_space(6.0);
            Frame::new().fill(BG).corner_radius(8).inner_margin(Margin::same(8)).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("update_log").max_height(200.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for l in &self.update_log {
                        ui.label(RichText::new(l).monospace().size(11.5).color(MUTED));
                    }
                });
            });
        }
    }

    /// Developer path: update from a downloaded source tarball, rebuilding locally.
    fn tarball_card(&mut self, ui: &mut egui::Ui) {
        if self.update_root.is_none() {
            return;
        }
        let running = self.update_rx.is_some();
        egui::CollapsingHeader::new(RichText::new("Developer: update from a downloaded source file").size(12.5).color(MUTED)).default_open(false).show(ui, |ui| {
            match &self.tarball {
                Some(t) => {
                    let avail = update::is_available(t, self.exe_secs, self.settings.update_applied);
                    ui.label(RichText::new(format!("{} (downloaded {}){}", t.path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()), update::ago(t.modified), if avail { ", newer than this build" } else { "" })).size(12.5).color(if avail { ACCENT } else { MUTED }));
                }
                None => {
                    ui.label(RichText::new("No all-in-one-prototype.tar.gz in Downloads.").size(12.5).color(MUTED));
                }
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!running && self.tarball.is_some(), egui::Button::new("Update from file")).clicked() {
                    self.start_update();
                }
                if ui.add_enabled(!running, egui::Button::new("Check again")).clicked() {
                    self.check_for_update();
                }
                if matches!(self.update_result, Some(Ok(()))) {
                    self.restart_button(ui);
                }
            });
            ui.label(RichText::new("Rebuilds from source with cargo. Needs the Rust toolchain.").size(12.0).color(MUTED));
        });
    }

    // ------------------------------------------------------------ settings
    fn settings_view(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().frame(Frame::new().fill(BG).inner_margin(Margin::same(20))).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.label(RichText::new("Settings").size(20.0).strong());
                ui.add_space(12.0);

                self.release_card(ui);
                ui.add_space(12.0);
                self.tarball_card(ui);
                Frame::new().fill(CARD).stroke(egui::Stroke::new(1.0, LINE)).corner_radius(12).inner_margin(Margin::same(14)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Folders").strong());
                    ui.add_space(4.0);
                    let rows: Vec<(&str, PathBuf)> = vec![
                        ("Your data (notes, workflows, settings)", self.workspace.clone()),
                        ("Workflow files (file and database nodes)", self.workspace.join("workflow-files")),
                        ("Notes", self.workspace.join("notes")),
                    ];
                    for (label, path) in rows {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(label).size(12.5).color(MUTED));
                            ui.label(RichText::new(path.display().to_string()).monospace().size(12.0));
                            if ui.small_button("Open").clicked() {
                                let _ = std::fs::create_dir_all(&path);
                                let _ = update::open_folder(&path);
                            }
                        });
                    }
                    if let Some(root) = &self.update_root {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Project (source)").size(12.5).color(MUTED));
                            ui.label(RichText::new(root.display().to_string()).monospace().size(12.0));
                        });
                    }
                });
            });
        });
    }

    // ------------------------------------------------------------ cloud providers and privacy
    fn cloud_section(&mut self, ui: &mut egui::Ui) {
        Frame::new().fill(CARD).stroke(egui::Stroke::new(1.0, LINE)).corner_radius(12).inner_margin(Margin::same(14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Cloud providers").strong());
            ui.label(RichText::new("Optional. Prompts sent to a cloud model leave this machine. Pick them per AI node, or as the default model below.").size(12.0).color(MUTED));
            ui.add_space(6.0);
            self.cloud_provider_card(ui, true);
            ui.add_space(8.0);
            self.cloud_provider_card(ui, false);
            ui.add_space(10.0);
            ui.label(RichText::new("PRIVACY").size(10.5).color(MUTED));
            ui.checkbox(&mut self.cloud_allowed, "Allow workflows and the agent to send data to cloud providers (off by default)");
            ui.checkbox(&mut self.allow_network, "Allow API nodes to use the network (off by default)");
            ui.label(RichText::new("Keys are saved in settings.json inside your All In One folder, readable only by you. You can also set ANTHROPIC_API_KEY or OPENAI_API_KEY in the environment instead.").size(12.0).color(MUTED));
        });
    }

    fn cloud_provider_card(&mut self, ui: &mut egui::Ui, anthropic: bool) {
        let (name, key_url, env, prefix) = if anthropic {
            ("Claude (Anthropic)", "https://console.anthropic.com/settings/keys", "ANTHROPIC_API_KEY", cloud::ANTHROPIC_PREFIX)
        } else {
            ("OpenAI", "https://platform.openai.com/api-keys", "OPENAI_API_KEY", cloud::OPENAI_PREFIX)
        };
        let saved = if anthropic { !self.settings.anthropic_key.trim().is_empty() } else { !self.settings.openai_key.trim().is_empty() };
        let active = if anthropic { self.settings.anthropic().is_some() } else { self.settings.openai().is_some() };
        let models: Vec<String> = if anthropic { self.settings.claude_models() } else { self.settings.openai_models.clone() };
        let note = if anthropic { self.anthropic_note.clone() } else { self.openai_note.clone() };
        let mut set_key: Option<String> = None;
        let mut refresh = false;
        let mut use_model: Option<String> = None;
        let mut remove_model: Option<String> = None;
        let mut add_model: Option<String> = None;
        Frame::new().fill(PANEL).corner_radius(10).inner_margin(Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).strong());
                let (txt, col) = match (saved, active) {
                    (true, _) => ("key saved", GOOD),
                    (false, true) => (if anthropic { "using ANTHROPIC_API_KEY" } else { "using OPENAI_API_KEY" }, GOOD),
                    _ => ("not set up", MUTED),
                };
                ui.label(RichText::new(txt).size(12.0).color(col));
                if ui.link("Get a key").clicked() {
                    let _ = web::open_external(key_url);
                }
            });
            let input = if anthropic { &mut self.anthropic_input } else { &mut self.openai_input };
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(input).password(true).hint_text(if saved { "Replace key" } else { "Paste API key" }).desired_width(300.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Save key").clicked() || enter) && !input.trim().is_empty() {
                    set_key = Some(std::mem::take(input).trim().to_string());
                }
                if saved && ui.button("Remove key").clicked() {
                    set_key = Some(String::new());
                }
            });
            if active {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Models").size(12.0).color(MUTED));
                    if ui.small_button("Refresh list").clicked() {
                        refresh = true;
                    }
                    ui.label(RichText::new(&note).size(12.0).color(MUTED));
                });
                egui::ScrollArea::vertical().id_salt(if anthropic { "claude_models" } else { "openai_models" }).max_height(150.0).show(ui, |ui| {
                    for m in &models {
                        ui.horizontal(|ui| {
                            let is_default = self.chosen_model.as_deref() == Some(format!("{prefix}{m}").as_str());
                            ui.label(m);
                            if is_default {
                                theme::badge(ui, "Default");
                            } else if ui.small_button("Use as default").clicked() {
                                use_model = Some(format!("{prefix}{m}"));
                            }
                            if ui.small_button("Remove").clicked() {
                                remove_model = Some(m.clone());
                            }
                        });
                    }
                    if models.is_empty() {
                        ui.label(RichText::new("No models listed yet. Refresh, or add a model ID below.").size(12.0).color(MUTED));
                    }
                });
                let new_model = if anthropic { &mut self.anthropic_new_model } else { &mut self.openai_new_model };
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(new_model).hint_text("Add a model ID").desired_width(220.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.button("Add").clicked() || enter) && !new_model.trim().is_empty() {
                        add_model = Some(std::mem::take(new_model).trim().to_string());
                    }
                });
            }
            let _ = env;
        });
        if let Some(k) = set_key {
            self.set_cloud_key(anthropic, k);
        }
        if refresh {
            self.fetch_cloud_models(anthropic);
        }
        if let Some(m) = use_model {
            self.chosen_model = Some(m);
        }
        if let Some(m) = remove_model {
            let list = if anthropic {
                if self.settings.anthropic_models.is_empty() {
                    self.settings.anthropic_models = self.settings.claude_models();
                }
                &mut self.settings.anthropic_models
            } else {
                &mut self.settings.openai_models
            };
            list.retain(|x| *x != m);
            self.save_settings();
        }
        if let Some(m) = add_model {
            let list = if anthropic {
                if self.settings.anthropic_models.is_empty() {
                    self.settings.anthropic_models = self.settings.claude_models();
                }
                &mut self.settings.anthropic_models
            } else {
                &mut self.settings.openai_models
            };
            if !list.contains(&m) {
                list.push(m);
            }
            self.save_settings();
        }
    }
}

/// Source editor. Returns whether the text changed and the y offset of each heading row (for outline jumps).
fn note_editor(ui: &mut egui::Ui, text: &mut String, id: egui::Id, heads: &[md::Heading]) -> (bool, Vec<f32>) {
    let out = egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(40).frame(Frame::NONE).show(ui);
    let ys = heads.iter().map(|h| out.galley.pos_from_cursor(egui::text::CCursor::new(h.char_idx)).min.y).collect();
    (out.response.changed(), ys)
}

/// Colours for the "page" surface in the browser text view.
struct Color32Ext;
impl Color32Ext {
    fn paper() -> egui::Color32 {
        egui::Color32::from_rgb(0xF5, 0xF5, 0xF2)
    }
    fn ink() -> egui::Color32 {
        egui::Color32::from_rgb(0x1B, 0x1B, 0x1B)
    }
}
