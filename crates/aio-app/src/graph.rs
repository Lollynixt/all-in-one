//! Node canvas: converts between `aio_core::Workflow` and an egui-snarl graph and draws it.
use crate::files;
use crate::theme::*;
use aio_core::{Category, ConvertOp, CountWhat, FileFormat, HttpMethod, LineOp, MatchMode, Node, NodeKind, SqlMode, Table, Workflow, WriteMode};
use egui::{Color32, RichText, Ui};
use egui_snarl::ui::{PinInfo, SnarlViewer};
use egui_snarl::{InPin, InPinId, NodeId, OutPin, OutPinId, Snarl};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Running,
    Done,
    Skipped,
    Failed,
}

/// Workflow node ids are snarl ids + 1 so that 0 is never used.
pub fn wf_id(id: NodeId) -> u32 {
    id.0 as u32 + 1
}

pub fn build_snarl(wf: &Workflow) -> Snarl<Node> {
    let mut snarl = Snarl::new();
    let mut map: HashMap<u32, NodeId> = HashMap::new();
    for n in &wf.nodes {
        let id = snarl.insert_node(egui::pos2(n.pos.0, n.pos.1), n.clone());
        map.insert(n.id, id);
    }
    for e in &wf.edges {
        let (Some(&from), Some(&to)) = (map.get(&e.from), map.get(&e.to)) else { continue };
        let Some(n) = wf.node(e.from) else { continue };
        let Some(port) = n.kind.output_ports().iter().position(|p| *p == e.from_port) else { continue };
        snarl.connect(OutPinId { node: from, output: port }, InPinId { node: to, input: 0 });
    }
    snarl
}

pub fn to_workflow(snarl: &Snarl<Node>, name: &str) -> Workflow {
    let mut wf = Workflow::new(name);
    for (id, pos, node) in snarl.nodes_pos_ids() {
        let mut n = node.clone();
        n.id = wf_id(id);
        n.pos = (pos.x, pos.y);
        wf.nodes.push(n);
    }
    for (out, inp) in snarl.wires() {
        let Some(src) = snarl.get_node(out.node) else { continue };
        let Some(port) = src.kind.output_ports().get(out.output) else { continue };
        wf.connect(wf_id(out.node), port, wf_id(inp.node));
    }
    wf.nodes.sort_by_key(|n| n.id);
    wf
}

pub fn kind_color(k: &NodeKind) -> Color32 {
    match k.category() {
        Category::Input => Color32::from_rgb(0x7D, 0xB4, 0xFF),
        Category::Tools => GOOD,
        Category::Script => Color32::from_rgb(0xC9, 0xA7, 0xFF),
        Category::Ai => ACCENT,
        Category::Output => Color32::from_rgb(0x9A, 0xA3, 0xB5),
    }
}

const PYTHON_SAMPLE: &str = "import sys\n\ntext = sys.stdin.read()   # the incoming text\n# Whatever you print becomes this node's output\nprint(text.upper())";

/// Every node the user can add, grouped by `NodeKind::category`.
pub fn new_kinds() -> Vec<(&'static str, NodeKind)> {
    let kinds = vec![
        NodeKind::Text { text: String::new() },
        NodeKind::ReadFile { path: "input.txt".into() },
        NodeKind::ReadNote { page: String::new() },
        NodeKind::HttpGet { url: "https://".into(), headers: String::new() },
        NodeKind::Template { template: "{{input}}".into() },
        NodeKind::Contains { needle: String::new() },
        NodeKind::Replace { find: String::new(), replacement: String::new(), regex: false },
        NodeKind::Filter { pattern: String::new(), mode: MatchMode::Keep, regex: false },
        NodeKind::Lines { op: LineOp::SortAsc },
        NodeKind::JsonPath { path: String::new() },
        NodeKind::Count { what: CountWhat::Lines },
        NodeKind::Convert { op: ConvertOp::CsvToJson },
        NodeKind::Python { code: PYTHON_SAMPLE.into() },
        NodeKind::Sql { db: String::new(), query: "SELECT * FROM input LIMIT 10".into(), mode: SqlMode::Query, format: Table::Csv },
        NodeKind::Ai { model: String::new(), system: String::new(), allow_cloud: false },
        NodeKind::Output,
        NodeKind::WriteFile { path: "output.md".into(), format: FileFormat::Markdown, mode: WriteMode::Replace },
        NodeKind::WriteNote { page: "Inbox/Workflow output".into(), mode: WriteMode::Append },
        NodeKind::HttpSend { url: "https://".into(), method: HttpMethod::Post, headers: String::new() },
        NodeKind::DbInsert { db: "data.db".into(), table: "results".into() },
    ];
    kinds.into_iter().map(|k| (k.label(), k)).collect()
}

pub fn describe(k: &NodeKind) -> &'static str {
    match k {
        NodeKind::Text { .. } => "Fixed text to feed into the flow",
        NodeKind::ReadFile { .. } => "Read a text, CSV or JSON file from the workflow files folder",
        NodeKind::ReadNote { .. } => "Read a page from your notebook",
        NodeKind::HttpGet { .. } => "Fetch a URL (API). Needs API nodes allowed in Models",
        NodeKind::Sql { .. } => "Run SQL on a SQLite file, or on incoming CSV/JSON as table 'input'",
        NodeKind::Python { .. } => "Run a Python script: incoming text on stdin, printed text out",
        NodeKind::Template { .. } => "Build a prompt around the incoming text with {{input}}",
        NodeKind::Ai { .. } => "Ask a local or cloud model",
        NodeKind::Contains { .. } => "Branch on whether the text contains something",
        NodeKind::Replace { .. } => "Find and replace, optionally with a regex",
        NodeKind::Filter { .. } => "Keep or drop lines that match",
        NodeKind::Lines { .. } => "Sort, de-duplicate, trim or number lines",
        NodeKind::JsonPath { .. } => "Pull one value out of JSON, e.g. data.items.0.name",
        NodeKind::Count { .. } => "Count lines, words or characters",
        NodeKind::Convert { .. } => "Convert between CSV and JSON",
        NodeKind::WriteFile { .. } => "Save to a file as text, Markdown, CSV or JSON",
        NodeKind::WriteNote { .. } => "Append or write to a notebook page",
        NodeKind::HttpSend { .. } => "Send the text to an API (POST, PUT or PATCH)",
        NodeKind::DbInsert { .. } => "Insert CSV or JSON rows into a SQLite table",
        NodeKind::Output => "Show the text in Run output",
    }
}

fn choice<T: Copy + PartialEq>(ui: &mut Ui, salt: (usize, &'static str), width: f32, value: &mut T, all: &[T], label: fn(T) -> &'static str) {
    egui::ComboBox::from_id_salt(salt).width(width).selected_text(label(*value)).show_ui(ui, |ui| {
        for &v in all {
            ui.selectable_value(value, v, label(v));
        }
    });
}

fn hint(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(11.0).color(MUTED)).wrap_mode(egui::TextWrapMode::Wrap));
}

fn temp_err(ui: &Ui, id: usize, tag: &'static str) -> egui::Id {
    ui.make_persistent_id((id, tag, "err"))
}

fn show_err(ui: &mut Ui, key: egui::Id) {
    if let Some(e) = ui.data(|d| d.get_temp::<String>(key)).filter(|e| !e.is_empty()) {
        ui.add(egui::Label::new(RichText::new(e).size(11.0).color(BAD)).wrap_mode(egui::TextWrapMode::Wrap));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Browse {
    /// Existing file, must be inside the workflow files folder.
    Existing,
    /// Output location, must be inside the workflow files folder.
    Save,
}

/// A path text box with a Browse button that opens Finder / File Explorer.
fn path_field(ui: &mut Ui, id: usize, tag: &'static str, w: f32, path: &mut String, hint_text: &str, root: &std::path::Path, mode: Browse) {
    let key = temp_err(ui, id, tag);
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(path).hint_text(hint_text).desired_width(w - 76.0));
        if ui.button("Browse…").on_hover_text("Opens Finder or File Explorer").clicked() {
            let picked = match mode {
                Browse::Existing => files::pick_existing_inside(root),
                Browse::Save => files::pick_output(root, path),
            };
            match picked {
                Some(Ok(rel)) => {
                    *path = rel;
                    ui.data_mut(|d| d.remove::<String>(key));
                }
                Some(Err(e)) => {
                    ui.data_mut(|d| d.insert_temp(key, e));
                }
                None => {}
            }
        }
    });
    show_err(ui, key);
}

fn note_picker(ui: &mut Ui, salt: usize, w: f32, page: &mut String, notes: &[String]) {
    ui.add(egui::TextEdit::singleline(page).hint_text("Folder/Page").desired_width(w));
    egui::ComboBox::from_id_salt((salt, "page")).width(w).selected_text("Pick an existing page").show_ui(ui, |ui| {
        for n in notes {
            if ui.selectable_label(page.trim_end_matches(".md") == n, n).clicked() {
                *page = n.clone();
            }
        }
        if notes.is_empty() {
            ui.label(RichText::new("No notes yet").color(MUTED));
        }
    });
}

/// Models offered by the AI node dropdown.
#[derive(Default)]
pub struct ModelMenu {
    /// Installed local models.
    pub local: Vec<String>,
    /// Cloud model references such as `anthropic/claude-sonnet-5-5`.
    pub cloud: Vec<String>,
    pub cloud_allowed: bool,
}

pub struct Viewer<'a> {
    /// Last canvas transform (graph to screen), so the hotbar can place new nodes in view.
    pub xf: &'a mut Option<egui::emath::TSTransform>,
    pub status: &'a HashMap<u32, Status>,
    pub recommended_model: &'a str,
    pub models: &'a ModelMenu,
    /// Notebook pages (without `.md`), for the note nodes.
    pub notes: &'a [String],
    /// Workflow files folder, where file and database nodes live.
    pub files_root: &'a std::path::Path,
}

impl SnarlViewer<Node> for Viewer<'_> {
    fn title(&mut self, node: &Node) -> String {
        node.title.clone()
    }

    fn show_header(&mut self, node: NodeId, _i: &[InPin], _o: &[OutPin], ui: &mut Ui, snarl: &mut Snarl<Node>) {
        let st = self.status.get(&wf_id(node)).copied();
        let Some(n) = snarl.get_node(node) else { return };
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, kind_color(&n.kind));
            ui.label(RichText::new(n.kind.label().to_uppercase()).size(10.5).color(MUTED));
            match st {
                Some(Status::Running) => {
                    ui.spinner();
                }
                Some(Status::Done) => {
                    ui.label(RichText::new("done").size(11.0).color(GOOD));
                }
                Some(Status::Failed) => {
                    ui.label(RichText::new("failed").size(11.0).color(BAD));
                }
                Some(Status::Skipped) => {
                    ui.label(RichText::new("skipped").size(11.0).color(MUTED));
                }
                None => {}
            }
        });
    }

    fn inputs(&mut self, node: &Node) -> usize {
        usize::from(node.kind.has_input())
    }

    fn show_input(&mut self, _pin: &InPin, ui: &mut Ui, _snarl: &mut Snarl<Node>) -> impl egui_snarl::ui::SnarlPin + 'static {
        ui.label(RichText::new("in").size(11.5).color(MUTED));
        PinInfo::circle().with_fill(MUTED)
    }

    fn outputs(&mut self, node: &Node) -> usize {
        node.kind.output_ports().len()
    }

    fn show_output(&mut self, pin: &OutPin, ui: &mut Ui, snarl: &mut Snarl<Node>) -> impl egui_snarl::ui::SnarlPin + 'static {
        let name = snarl.get_node(pin.id.node).and_then(|n| n.kind.output_ports().get(pin.id.output).copied()).unwrap_or("out");
        ui.label(RichText::new(name).size(11.5).color(MUTED));
        let c = match name {
            "true" => GOOD,
            "false" => BAD,
            _ => MUTED,
        };
        PinInfo::circle().with_fill(c)
    }

    fn has_body(&mut self, _node: &Node) -> bool {
        true
    }

    fn show_body(&mut self, node: NodeId, _i: &[InPin], _o: &[OutPin], ui: &mut Ui, snarl: &mut Snarl<Node>) {
        let Some(n) = snarl.get_node_mut(node) else { return };
        let rec = self.recommended_model;
        let menu = self.models;
        let notes = self.notes;
        let files_root = self.files_root;
        let id = node.0;
        // Snarl lays the body out horizontally; stack the fields vertically.
        let w = if matches!(n.kind, NodeKind::Python { .. } | NodeKind::Sql { .. }) { 340.0 } else { 216.0 };
        ui.vertical(|ui| {
            ui.set_width(w);
            ui.add(egui::TextEdit::singleline(&mut n.title).desired_width(w));
            match &mut n.kind {
                NodeKind::Text { text } => {
                    ui.add(egui::TextEdit::multiline(text).hint_text("Text to send downstream").desired_rows(3).desired_width(w));
                }
                NodeKind::ReadFile { path } => {
                    let edit_key = ui.make_persistent_id((id, "file_edit"));
                    let err_key = temp_err(ui, id, "file");
                    let editing = ui.data(|d| d.get_temp::<bool>(edit_key)).unwrap_or(false);
                    if editing {
                        ui.add(egui::TextEdit::singleline(path).hint_text("data/report.csv").desired_width(w));
                        if ui.link("Done").clicked() {
                            ui.data_mut(|d| d.insert_temp(edit_key, false));
                        }
                    } else {
                        let label = if path.is_empty() { "Choose a file…".to_string() } else { path.clone() };
                        let r = ui.add_sized([w, 26.0], egui::Button::new(RichText::new(label).color(if path.is_empty() { MUTED } else { TEXT })).truncate()).on_hover_text("Opens Finder or File Explorer. A file from elsewhere is copied into the workflow files folder.");
                        if r.clicked() {
                            match files::pick_input(files_root) {
                                Some(Ok(rel)) => {
                                    *path = rel;
                                    ui.data_mut(|d| d.remove::<String>(err_key));
                                }
                                Some(Err(e)) => {
                                    ui.data_mut(|d| d.insert_temp(err_key, e));
                                }
                                None => {}
                            }
                        }
                        ui.horizontal(|ui| {
                            if ui.link("Type a path").clicked() {
                                ui.data_mut(|d| d.insert_temp(edit_key, true));
                            }
                            if ui.link("Show folder").clicked() {
                                let _ = std::fs::create_dir_all(files_root);
                                let _ = crate::update::open_folder(files_root);
                            }
                        });
                    }
                    show_err(ui, err_key);
                    hint(ui, "Text, Markdown, CSV and JSON all read as text.");
                }
                NodeKind::ReadNote { page } => {
                    note_picker(ui, id, w, page, notes);
                }
                NodeKind::HttpGet { url, headers } => {
                    ui.add(egui::TextEdit::singleline(url).hint_text("https://api.example.com/items").desired_width(w));
                    ui.add(egui::TextEdit::multiline(headers).hint_text("Headers, one per line\nAuthorization: Bearer …").desired_rows(2).desired_width(w));
                    hint(ui, "{{input}} in the URL is replaced by incoming text. Headers are saved in the workflow file.");
                }
                NodeKind::Sql { db, query, mode, format } => {
                    path_field(ui, id, "sqldb", w, db, "database file (empty = incoming CSV/JSON)", files_root, Browse::Existing);
                    ui.add(egui::TextEdit::multiline(query).code_editor().lock_focus(true).desired_rows(5).desired_width(w));
                    ui.horizontal(|ui| {
                        choice(ui, (id, "mode"), 140.0, mode, SqlMode::ALL, SqlMode::label);
                        if *mode == SqlMode::Query {
                            choice(ui, (id, "fmt"), 70.0, format, Table::ALL, Table::label);
                        }
                    });
                    hint(ui, "Incoming CSV/JSON is table 'input'. :input binds the whole incoming text safely. SQLite only.");
                }
                NodeKind::Python { code } => {
                    ui.add(egui::TextEdit::multiline(code).code_editor().lock_focus(true).desired_rows(8).desired_width(w));
                    hint(ui, "Incoming text arrives on stdin, print() is the output. Runs on this machine with your python3, 60 s limit, in the workflow files folder.");
                }
                NodeKind::Template { template } => {
                    ui.add(egui::TextEdit::multiline(template).desired_rows(3).desired_width(w));
                    hint(ui, "{{input}} is the upstream text");
                }
                NodeKind::Contains { needle } => {
                    ui.add(egui::TextEdit::singleline(needle).hint_text("text to look for").desired_width(w));
                }
                NodeKind::Replace { find, replacement, regex } => {
                    ui.add(egui::TextEdit::singleline(find).hint_text("find").desired_width(w));
                    ui.add(egui::TextEdit::singleline(replacement).hint_text("replace with").desired_width(w));
                    ui.checkbox(regex, "Regular expression");
                }
                NodeKind::Filter { pattern, mode, regex } => {
                    ui.add(egui::TextEdit::singleline(pattern).hint_text("text or pattern").desired_width(w));
                    choice(ui, (id, "mm"), w, mode, MatchMode::ALL, MatchMode::label);
                    ui.checkbox(regex, "Regular expression");
                }
                NodeKind::Lines { op } => choice(ui, (id, "op"), w, op, LineOp::ALL, LineOp::label),
                NodeKind::JsonPath { path } => {
                    ui.add(egui::TextEdit::singleline(path).hint_text("items.0.name").desired_width(w));
                }
                NodeKind::Count { what } => choice(ui, (id, "what"), w, what, CountWhat::ALL, CountWhat::label),
                NodeKind::Convert { op } => choice(ui, (id, "conv"), w, op, ConvertOp::ALL, ConvertOp::label),
                NodeKind::Ai { model, system, allow_cloud } => {
                    let shown = if model.is_empty() { "Auto (recommended)".to_string() } else { aio_llm::cloud::display_ref(model) };
                    let mut picked_cloud = false;
                    egui::ComboBox::from_id_salt((id, "model")).width(w).selected_text(shown).show_ui(ui, |ui| {
                        if ui.selectable_label(model.is_empty(), format!("Auto (recommended): {rec}")).clicked() {
                            model.clear();
                        }
                        ui.label(RichText::new("LOCAL").size(10.5).color(MUTED));
                        for m in &menu.local {
                            if ui.selectable_label(model == m, m).clicked() {
                                *model = m.clone();
                            }
                        }
                        if menu.local.is_empty() {
                            ui.label(RichText::new("None installed yet (Models tab)").size(11.5).color(MUTED));
                        }
                        ui.separator();
                        ui.label(RichText::new("CLOUD").size(10.5).color(MUTED));
                        for m in &menu.cloud {
                            if ui.selectable_label(model == m, aio_llm::cloud::display_ref(m)).clicked() {
                                *model = m.clone();
                                picked_cloud = true;
                            }
                        }
                        if menu.cloud.is_empty() {
                            ui.label(RichText::new("Add Claude or OpenAI in the Models tab").size(11.5).color(MUTED));
                        }
                    });
                    if picked_cloud {
                        *allow_cloud = true;
                    }
                    if model.is_empty() {
                        hint(ui, &format!("Uses {rec}"));
                    } else if aio_llm::cloud::is_cloud_ref(model) {
                        ui.horizontal(|ui| {
                            badge(ui, "Cloud");
                            if !menu.cloud_allowed {
                                ui.label(RichText::new("off in Models, Privacy").size(11.0).color(BAD));
                            }
                        });
                    }
                    ui.add(egui::TextEdit::multiline(system).hint_text("system prompt").desired_rows(2).desired_width(w));
                    ui.checkbox(allow_cloud, "May use cloud models");
                }
                NodeKind::WriteFile { path, format, mode } => {
                    path_field(ui, id, "outfile", w, path, "reports/summary.md", files_root, Browse::Save);
                    ui.horizontal(|ui| {
                        choice(ui, (id, "ff"), 100.0, format, FileFormat::ALL, FileFormat::label);
                        choice(ui, (id, "wm"), 90.0, mode, WriteMode::ALL, WriteMode::label);
                    });
                    if matches!(format, FileFormat::Csv | FileFormat::Json) {
                        hint(ui, "CSV and JSON are converted from the other form when possible.");
                    }
                }
                NodeKind::WriteNote { page, mode } => {
                    note_picker(ui, id, w, page, notes);
                    choice(ui, (id, "nm"), w, mode, WriteMode::ALL, WriteMode::label);
                }
                NodeKind::HttpSend { url, method, headers } => {
                    ui.add(egui::TextEdit::singleline(url).hint_text("https://api.example.com/items").desired_width(w));
                    choice(ui, (id, "hm"), w, method, HttpMethod::ALL, HttpMethod::label);
                    ui.add(egui::TextEdit::multiline(headers).hint_text("Headers, one per line").desired_rows(2).desired_width(w));
                    hint(ui, "The incoming text is the request body. The response becomes the output.");
                }
                NodeKind::DbInsert { db, table } => {
                    path_field(ui, id, "outdb", w, db, "data.db", files_root, Browse::Existing);
                    ui.add(egui::TextEdit::singleline(table).hint_text("table name").desired_width(w));
                    hint(ui, "Incoming CSV or a JSON array of objects. The table is created if missing.");
                }
                NodeKind::Output => {
                    hint(ui, "Shows input in Run output");
                }
            }
        });
    }

    fn current_transform(&mut self, to_global: &mut egui::emath::TSTransform, _snarl: &mut Snarl<Node>) {
        *self.xf = Some(*to_global);
    }

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<Node>) -> bool {
        true
    }

    fn show_graph_menu(&mut self, pos: egui::Pos2, ui: &mut Ui, snarl: &mut Snarl<Node>) {
        egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
            for cat in Category::ALL {
                ui.label(RichText::new(cat.label().to_uppercase()).size(10.5).color(MUTED));
                for (label, kind) in new_kinds().into_iter().filter(|(_, k)| k.category() == cat) {
                    if ui.button(label).on_hover_text(describe(&kind)).clicked() {
                        snarl.insert_node(pos, Node { id: 0, title: label.to_string(), kind, pos: (pos.x, pos.y) });
                        ui.close();
                    }
                }
                ui.add_space(4.0);
            }
        });
    }

    fn has_node_menu(&mut self, _node: &Node) -> bool {
        true
    }

    fn show_node_menu(&mut self, node: NodeId, _i: &[InPin], _o: &[OutPin], ui: &mut Ui, snarl: &mut Snarl<Node>) {
        if ui.button("Delete node").clicked() {
            snarl.remove_node(node);
            ui.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snarl_round_trip_keeps_structure() {
        let wf = Workflow::demo();
        let snarl = build_snarl(&wf);
        let back = to_workflow(&snarl, &wf.name);
        assert_eq!(back.nodes.len(), wf.nodes.len());
        assert_eq!(back.edges.len(), wf.edges.len());
        back.execution_order().unwrap();
        // The Contains node's `true` and `false` branches survive the round trip.
        assert!(back.edges.iter().any(|e| e.from_port == "true"));
        assert!(back.edges.iter().any(|e| e.from_port == "false"));
    }
}
