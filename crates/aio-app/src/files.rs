//! Native file pickers for file and database nodes. Nodes stay confined to the workflow files folder, so a file
//! picked from elsewhere is copied in, and output locations must be inside it.
use std::path::{Path, PathBuf};

/// Canonical path even when the final component does not exist yet (save dialogs).
fn canon_lenient(p: &Path) -> Option<PathBuf> {
    if let Ok(c) = p.canonicalize() {
        return Some(c);
    }
    let parent = p.parent()?.canonicalize().ok()?;
    Some(parent.join(p.file_name()?))
}

/// `picked` relative to `root` with `/` separators, or None when it is outside.
pub fn relative_in(root: &Path, picked: &Path) -> Option<String> {
    let r = root.canonicalize().ok()?;
    let p = canon_lenient(picked)?;
    let rel = p.strip_prefix(&r).ok()?;
    let s = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
    (!s.is_empty()).then_some(s)
}

/// Returns a root-relative path for `picked`, copying it into `root/imports/` when it lives elsewhere.
pub fn import_file(root: &Path, picked: &Path) -> Result<String, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    if let Some(rel) = relative_in(root, picked) {
        return Ok(rel);
    }
    let name = picked.file_name().ok_or("that is not a file")?.to_string_lossy().into_owned();
    let dir = root.join("imports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let src = std::fs::read(picked).map_err(|e| format!("{}: {e}", picked.display()))?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.clone(), String::new()),
    };
    let mut candidate = name.clone();
    let mut n = 2;
    loop {
        let dest = dir.join(&candidate);
        match std::fs::read(&dest) {
            Ok(existing) if existing == src => return Ok(format!("imports/{candidate}")), // already imported
            Ok(_) => {
                candidate = format!("{stem} ({n}){ext}");
                n += 1;
            }
            Err(_) => {
                std::fs::write(&dest, &src).map_err(|e| e.to_string())?;
                return Ok(format!("imports/{candidate}"));
            }
        }
    }
}

/// Like `relative_in` but with an error message for locations outside the folder.
pub fn inside_only(root: &Path, picked: &Path) -> Result<String, String> {
    relative_in(root, picked).ok_or_else(|| format!("Choose a location inside the workflow files folder ({}).", root.display()))
}

/// Opens Finder or File Explorer to choose an input file. Files from elsewhere are copied into the folder.
pub fn pick_input(root: &Path) -> Option<Result<String, String>> {
    let _ = std::fs::create_dir_all(root);
    let f = rfd::FileDialog::new().set_title("Choose a file").set_directory(root).pick_file()?;
    Some(import_file(root, &f))
}

/// Open dialog restricted to the workflow files folder (existing databases).
pub fn pick_existing_inside(root: &Path) -> Option<Result<String, String>> {
    let _ = std::fs::create_dir_all(root);
    let f = rfd::FileDialog::new().set_title("Choose a file").set_directory(root).pick_file()?;
    Some(inside_only(root, &f))
}

/// Save dialog for outputs, starting in the workflow files folder.
pub fn pick_output(root: &Path, current: &str) -> Option<Result<String, String>> {
    let _ = std::fs::create_dir_all(root);
    let name = Path::new(current).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "output.md".into());
    let f = rfd::FileDialog::new().set_title("Save output as").set_directory(root).set_file_name(&name).save_file()?;
    Some(inside_only(root, &f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("aio-files-{tag}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn inside_files_keep_relative_path() {
        let root = tmp("a");
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/x.csv"), "a").unwrap();
        assert_eq!(import_file(&root, &root.join("data/x.csv")).unwrap(), "data/x.csv");
        assert_eq!(inside_only(&root, &root.join("new/out.md")).ok(), None); // parent missing: cannot resolve
        assert_eq!(inside_only(&root, &root.join("data/out.md")).unwrap(), "data/out.md");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn outside_files_are_copied_once_and_renamed_on_conflict() {
        let root = tmp("root");
        let other = tmp("other");
        std::fs::write(other.join("sales.csv"), "v1").unwrap();
        assert_eq!(import_file(&root, &other.join("sales.csv")).unwrap(), "imports/sales.csv");
        assert_eq!(import_file(&root, &other.join("sales.csv")).unwrap(), "imports/sales.csv"); // same content: reuse
        std::fs::write(other.join("sales.csv"), "v2").unwrap();
        assert_eq!(import_file(&root, &other.join("sales.csv")).unwrap(), "imports/sales (2).csv");
        assert_eq!(std::fs::read_to_string(root.join("imports/sales.csv")).unwrap(), "v1");
        assert!(inside_only(&root, &other.join("sales.csv")).is_err());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(other);
    }
}
