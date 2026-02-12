use std::path::PathBuf;

pub fn noid_dir() -> PathBuf {
    dirs_home().join(".noid")
}

pub fn db_path() -> PathBuf {
    noid_dir().join("noid.db")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root"))
}
