//! Model persistence: multi-day TTLs (ready 7d, waiting 24h) must survive
//! restarts. Atomic write-rename; saves are debounced by the runtime.

use std::path::Path;

use pet_core::Model;
use tracing::{info, warn};

pub fn load(path: &Path) -> Option<Model> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(model) => {
            info!("restored state from {}", path.display());
            Some(model)
        }
        Err(e) => {
            warn!("discarding unreadable state {}: {e}", path.display());
            None
        }
    }
}

pub fn save(path: &Path, model: &Model) {
    let run = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(model)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    if let Err(e) = run() {
        warn!("failed to persist state to {}: {e}", path.display());
    }
}
