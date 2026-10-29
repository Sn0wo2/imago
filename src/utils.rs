use std::path::Path;

pub fn file_url(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    let absolute_path = std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    let s = absolute_path.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    format!("file:///{s}")
}
