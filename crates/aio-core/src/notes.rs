//! Notes are plain Markdown files under a root folder. Folders are notebooks/sections.
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotePage {
    /// Path relative to the notes root, using `/`, e.g. `Finance/Invoices/14 Nov.md`.
    pub rel: String,
}

impl NotePage {
    pub fn title(&self) -> &str {
        let name = self.rel.rsplit('/').next().unwrap_or(&self.rel);
        name.strip_suffix(".md").unwrap_or(name)
    }
}

#[derive(Debug, Clone)]
pub struct NoteStore {
    root: PathBuf,
}

impl NoteStore {
    pub fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, rel: &str) -> std::io::Result<PathBuf> {
        crate::engine::safe_join(&self.root, rel).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
    }

    pub fn list(&self) -> Vec<NotePage> {
        let mut out = vec![];
        fn walk(dir: &Path, root: &Path, out: &mut Vec<NotePage>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else if p.extension().is_some_and(|x| x == "md") {
                    if let Ok(rel) = p.strip_prefix(root) {
                        out.push(NotePage { rel: rel.to_string_lossy().replace('\\', "/") });
                    }
                }
            }
        }
        walk(&self.root, &self.root, &mut out);
        out.sort_by(|a, b| a.rel.to_lowercase().cmp(&b.rel.to_lowercase()));
        out
    }

    pub fn read(&self, rel: &str) -> std::io::Result<String> {
        std::fs::read_to_string(self.resolve(rel)?)
    }

    pub fn write(&self, rel: &str, text: &str) -> std::io::Result<()> {
        let p = self.resolve(rel)?;
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write to a temp file then rename so a crash never leaves a half-written page.
        let tmp = p.with_extension("md.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, p)
    }

    /// Case-insensitive substring search over titles and bodies.
    pub fn search(&self, query: &str) -> Vec<NotePage> {
        let q = query.to_lowercase();
        if q.is_empty() {
            return self.list();
        }
        self.list()
            .into_iter()
            .filter(|p| p.rel.to_lowercase().contains(&q) || self.read(&p.rel).is_ok_and(|t| t.to_lowercase().contains(&q)))
            .collect()
    }

    /// Pages that contain a `[[title]]` link to `title`.
    pub fn backlinks(&self, title: &str) -> Vec<NotePage> {
        let needle = format!("[[{}]]", title);
        self.list().into_iter().filter(|p| p.title() != title && self.read(&p.rel).is_ok_and(|t| t.contains(&needle))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> NoteStore {
        NoteStore::new(std::env::temp_dir().join(format!("aio-notes-{}", uuid::Uuid::new_v4()))).unwrap()
    }

    #[test]
    fn write_list_read_search_backlinks() {
        let s = store();
        s.write("Finance/Invoices/14 Nov.md", "Totals look fine. See [[Supplier notes]].").unwrap();
        s.write("Finance/Supplier notes.md", "Harbour & Co pays late").unwrap();
        assert_eq!(s.list().len(), 2);
        assert_eq!(s.read("Finance/Supplier notes.md").unwrap(), "Harbour & Co pays late");
        assert_eq!(s.search("pays late").len(), 1);
        assert_eq!(s.backlinks("Supplier notes")[0].title(), "14 Nov");
        let _ = std::fs::remove_dir_all(s.root());
    }

    #[test]
    fn rejects_escape() {
        let s = store();
        assert!(s.write("../x.md", "no").is_err());
        assert!(s.read("/etc/passwd").is_err());
        let _ = std::fs::remove_dir_all(s.root());
    }
}
