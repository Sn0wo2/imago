use std::path::Path;

pub fn file_url(path: impl AsRef<Path>) -> String {
    let s = std::fs::canonicalize(path)
        .expect("canonicalize path")
        .to_string_lossy()
        .replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    format!("file:///{s}")
}
