use petgraph::algo::toposort;
use petgraph::graph::DiGraph;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type NodeId = u32;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum WorkflowError {
    #[error("workflow contains a cycle")]
    Cycle,
    #[error("edge references unknown node {0}")]
    UnknownNode(NodeId),
    #[error("duplicate node id {0}")]
    DuplicateNode(NodeId),
    #[error("serialisation: {0}")]
    Serde(String),
}

/// Declares a small enum used for dropdowns: first variant is the default.
macro_rules! choice {
    ($(#[$m:meta])* $name:ident { $first:ident => $fl:literal $(, $var:ident => $label:literal)* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            #[default]
            $first,
            $($var,)*
        }
        impl $name {
            pub const ALL: &'static [$name] = &[$name::$first $(, $name::$var)*];
            pub fn label(self) -> &'static str {
                match self {
                    $name::$first => $fl,
                    $($name::$var => $label,)*
                }
            }
        }
    };
}

choice!(
    /// How file output is shaped.
    FileFormat { Text => "Plain text", Markdown => "Markdown", Csv => "CSV", Json => "JSON" }
);
impl FileFormat {
    pub fn ext(self) -> &'static str {
        match self {
            FileFormat::Text => "txt",
            FileFormat::Markdown => "md",
            FileFormat::Csv => "csv",
            FileFormat::Json => "json",
        }
    }
}
choice!(WriteMode { Replace => "Replace", Append => "Append" });
choice!(SqlMode { Query => "Query (returns rows)", Execute => "Run statements" });
choice!(Table { Csv => "CSV", Json => "JSON" });
choice!(LineOp { SortAsc => "Sort A to Z", SortDesc => "Sort Z to A", Unique => "Remove duplicates", Reverse => "Reverse order", Trim => "Trim whitespace", DropEmpty => "Drop empty lines", Number => "Number the lines" });
choice!(CountWhat { Lines => "Lines", Words => "Words", Chars => "Characters" });
choice!(ConvertOp { CsvToJson => "CSV to JSON", JsonToCsv => "JSON to CSV" });
choice!(HttpMethod { Post => "POST", Put => "PUT", Patch => "PATCH" });
choice!(MatchMode { Keep => "Keep matching lines", Drop => "Drop matching lines" });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    // ---- inputs
    /// Emits constant text.
    Text { text: String },
    /// Reads a UTF-8 file (path relative to the workspace).
    ReadFile { path: String },
    /// Reads a page from the notebook.
    ReadNote { page: String },
    /// GET a URL and emit the response body. `{{input}}` in the URL is replaced by upstream text.
    HttpGet { url: String, #[serde(default)] headers: String },
    /// Runs SQL against a SQLite file in the workspace (or a scratch database holding upstream CSV/JSON as table `input`).
    Sql { #[serde(default)] db: String, query: String, #[serde(default)] mode: SqlMode, #[serde(default)] format: Table },
    // ---- scripts
    /// Runs Python: upstream text on stdin, stdout becomes the output.
    Python { code: String },
    // ---- tools
    /// Replaces `{{input}}` in the template with the upstream text.
    Template { template: String },
    /// Chat completion. `model` empty means "use the recommended model".
    Ai { model: String, system: String, allow_cloud: bool },
    /// Routes to the `true` or `false` port depending on whether input contains `needle`.
    Contains { needle: String },
    Replace { find: String, replacement: String, #[serde(default)] regex: bool },
    Filter { pattern: String, #[serde(default)] mode: MatchMode, #[serde(default)] regex: bool },
    Lines { #[serde(default)] op: LineOp },
    JsonPath { path: String },
    Count { #[serde(default)] what: CountWhat },
    Convert { #[serde(default)] op: ConvertOp },
    // ---- outputs
    /// Writes input to a file in the workspace (optionally converting to CSV/JSON) and passes it through.
    WriteFile { path: String, #[serde(default)] format: FileFormat, #[serde(default)] mode: WriteMode },
    /// Writes input to a notebook page.
    WriteNote { page: String, #[serde(default)] mode: WriteMode },
    /// Sends the input as the request body and emits the response.
    HttpSend { url: String, #[serde(default)] method: HttpMethod, #[serde(default)] headers: String },
    /// Inserts CSV or JSON rows from upstream into a table of a SQLite file.
    DbInsert { db: String, table: String },
    /// Terminal node that shows its input in run output.
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Input,
    Tools,
    Script,
    Ai,
    Output,
}

impl Category {
    pub const ALL: [Category; 5] = [Category::Input, Category::Tools, Category::Script, Category::Ai, Category::Output];
    pub fn label(self) -> &'static str {
        match self {
            Category::Input => "Input",
            Category::Tools => "Tools",
            Category::Script => "Scripts",
            Category::Ai => "AI",
            Category::Output => "Output",
        }
    }
}

impl NodeKind {
    pub fn label(&self) -> &'static str {
        match self {
            NodeKind::Text { .. } => "Text",
            NodeKind::ReadFile { .. } => "File in",
            NodeKind::ReadNote { .. } => "Note in",
            NodeKind::HttpGet { .. } => "API get",
            NodeKind::Sql { .. } => "SQL",
            NodeKind::Python { .. } => "Python",
            NodeKind::Template { .. } => "Template",
            NodeKind::Ai { .. } => "AI chat",
            NodeKind::Contains { .. } => "Contains",
            NodeKind::Replace { .. } => "Replace",
            NodeKind::Filter { .. } => "Filter lines",
            NodeKind::Lines { .. } => "Lines",
            NodeKind::JsonPath { .. } => "JSON path",
            NodeKind::Count { .. } => "Count",
            NodeKind::Convert { .. } => "Convert",
            NodeKind::WriteFile { .. } => "File out",
            NodeKind::WriteNote { .. } => "Note out",
            NodeKind::HttpSend { .. } => "API send",
            NodeKind::DbInsert { .. } => "Database out",
            NodeKind::Output => "Output",
        }
    }
    pub fn category(&self) -> Category {
        match self {
            NodeKind::Text { .. } | NodeKind::ReadFile { .. } | NodeKind::ReadNote { .. } | NodeKind::HttpGet { .. } => Category::Input,
            NodeKind::Sql { .. } | NodeKind::Python { .. } => Category::Script,
            NodeKind::Template { .. } | NodeKind::Contains { .. } | NodeKind::Replace { .. } | NodeKind::Filter { .. } | NodeKind::Lines { .. } | NodeKind::JsonPath { .. } | NodeKind::Count { .. } | NodeKind::Convert { .. } => Category::Tools,
            NodeKind::Ai { .. } => Category::Ai,
            NodeKind::WriteFile { .. } | NodeKind::WriteNote { .. } | NodeKind::HttpSend { .. } | NodeKind::DbInsert { .. } | NodeKind::Output => Category::Output,
        }
    }
    /// True when the node talks to the network.
    pub fn uses_network(&self) -> bool {
        matches!(self, NodeKind::HttpGet { .. } | NodeKind::HttpSend { .. })
    }
    pub fn output_ports(&self) -> &'static [&'static str] {
        match self {
            NodeKind::Contains { .. } => &["true", "false"],
            NodeKind::Output => &[],
            _ => &["out"],
        }
    }
    /// Pure source nodes have no input pin. SQL and API get accept an optional input (`:input` / `{{input}}`).
    pub fn has_input(&self) -> bool {
        !matches!(self, NodeKind::Text { .. } | NodeKind::ReadFile { .. } | NodeKind::ReadNote { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub title: String,
    pub kind: NodeKind,
    #[serde(default)]
    pub pos: (f32, f32),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub from: NodeId,
    #[serde(default = "default_port")]
    pub from_port: String,
    pub to: NodeId,
}

fn default_port() -> String {
    "out".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Workflow {
    pub name: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Workflow {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Default::default() }
    }

    pub fn next_id(&self) -> NodeId {
        self.nodes.iter().map(|n| n.id).max().map_or(1, |m| m + 1)
    }

    pub fn add_node(&mut self, title: impl Into<String>, kind: NodeKind, pos: (f32, f32)) -> NodeId {
        let id = self.next_id();
        self.nodes.push(Node { id, title: title.into(), kind, pos });
        id
    }

    pub fn connect(&mut self, from: NodeId, from_port: &str, to: NodeId) {
        let e = Edge { from, from_port: from_port.into(), to };
        if !self.edges.contains(&e) {
            self.edges.push(e);
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Validates references and returns node ids in execution order.
    pub fn execution_order(&self) -> Result<Vec<NodeId>, WorkflowError> {
        let mut g = DiGraph::<NodeId, ()>::new();
        let mut idx = HashMap::new();
        for n in &self.nodes {
            if idx.insert(n.id, g.add_node(n.id)).is_some() {
                return Err(WorkflowError::DuplicateNode(n.id));
            }
        }
        for e in &self.edges {
            let a = *idx.get(&e.from).ok_or(WorkflowError::UnknownNode(e.from))?;
            let b = *idx.get(&e.to).ok_or(WorkflowError::UnknownNode(e.to))?;
            g.add_edge(a, b, ());
        }
        let order = toposort(&g, None).map_err(|_| WorkflowError::Cycle)?;
        Ok(order.into_iter().map(|i| g[i]).collect())
    }

    pub fn to_json(&self) -> Result<String, WorkflowError> {
        serde_json::to_string_pretty(self).map_err(|e| WorkflowError::Serde(e.to_string()))
    }

    pub fn from_json(s: &str) -> Result<Self, WorkflowError> {
        let w: Workflow = serde_json::from_str(s).map_err(|e| WorkflowError::Serde(e.to_string()))?;
        w.execution_order()?;
        Ok(w)
    }

    /// Small demo workflow used on first launch.
    pub fn demo() -> Self {
        let mut w = Workflow::new("Daily invoice summary");
        let a = w.add_node("Invoice text", NodeKind::Text { text: "Harbour & Co £1,240.00 due 28 Nov\nRidgeway Supplies £860.50 paid\nBrightside Print £312.00 due 28 Nov".into() }, (0.0, 0.0));
        let b = w.add_node("Prompt", NodeKind::Template { template: "Summarise these invoices by supplier and flag anything unusual:\n\n{{input}}".into() }, (290.0, 0.0));
        let c = w.add_node("Summarise", NodeKind::Ai { model: String::new(), system: "You are a concise finance assistant.".into(), allow_cloud: false }, (580.0, 0.0));
        let d = w.add_node("Mentions unusual?", NodeKind::Contains { needle: "unusual".into() }, (580.0, 260.0));
        let e = w.add_node("Show summary", NodeKind::Output, (870.0, 220.0));
        let f = w.add_node("Save for review", NodeKind::WriteFile { path: "review/flagged.md".into(), format: FileFormat::Markdown, mode: WriteMode::Replace }, (870.0, 440.0));
        w.connect(a, "out", b);
        w.connect(b, "out", c);
        w.connect(c, "out", d);
        w.connect(d, "false", e);
        w.connect(d, "true", f);
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_respects_edges() {
        let w = Workflow::demo();
        let order = w.execution_order().unwrap();
        let pos = |id| order.iter().position(|x| *x == id).unwrap();
        for e in &w.edges {
            assert!(pos(e.from) < pos(e.to));
        }
    }

    #[test]
    fn cycle_is_rejected() {
        let mut w = Workflow::new("c");
        let a = w.add_node("a", NodeKind::Output, (0.0, 0.0));
        let b = w.add_node("b", NodeKind::Output, (0.0, 0.0));
        w.connect(a, "out", b);
        w.connect(b, "out", a);
        assert_eq!(w.execution_order(), Err(WorkflowError::Cycle));
    }

    #[test]
    fn unknown_node_is_rejected() {
        let mut w = Workflow::new("u");
        let a = w.add_node("a", NodeKind::Output, (0.0, 0.0));
        w.connect(a, "out", 99);
        assert_eq!(w.execution_order(), Err(WorkflowError::UnknownNode(99)));
    }

    #[test]
    fn json_round_trip() {
        let w = Workflow::demo();
        let back = Workflow::from_json(&w.to_json().unwrap()).unwrap();
        assert_eq!(w, back);
    }
}
