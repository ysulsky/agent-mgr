//! Where the web UI and the prompt templates are read from at runtime, so editing them
//! needs no rebuild: `$AM_ASSETS` if set, else the `assets/` directory of the tree this
//! binary was built from.

use std::path::PathBuf;

pub fn assets_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("AM_ASSETS").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"))
}

pub fn webui_dir() -> PathBuf {
    assets_dir().join("webui")
}

pub fn prompts_dir() -> PathBuf {
    assets_dir().join("prompts")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_built_tree_has_the_ui_and_prompts() {
        assert!(super::webui_dir().join("index.html").exists());
        assert!(super::prompts_dir().join("README.md").exists());
    }
}
