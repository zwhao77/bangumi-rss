//! State persistence — save/load `AppState` to/from `state.json` on disk.
//!
//! Uses the `FileOps` trait so all file I/O can be swapped for testing.

use std::path::PathBuf;

use crate::core::state::AppState;
use crate::traits::FileOps;

/// Load `AppState` from `{data_dir}/state.json`.
pub fn load_state(fs: &dyn FileOps, data_dir: &str) -> Option<AppState> {
    let path = data_path(data_dir);
    let json = fs.read_to_string(&path).ok()?;
    AppState::from_json(&json)
}

/// Save `AppState` to `{data_dir}/state.json` (atomic: write to .tmp then rename).
pub fn save_state(fs: &dyn FileOps, state: &AppState, data_dir: &str) -> anyhow::Result<()> {
    let path = data_path(data_dir);
    let tmp = path.with_extension("tmp");
    let json = state.to_json_pretty()?;
    fs.write_string(&tmp, &json)?;
    fs.move_file(&tmp, &path)
}

fn data_path(data_dir: &str) -> PathBuf {
    PathBuf::from(data_dir).join("state.json")
}
