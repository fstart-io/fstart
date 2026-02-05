//! Load and parse board.ron files.

use fstart_types::BoardConfig;
use std::path::Path;

/// Load a BoardConfig from a RON file.
pub fn load_board_config(path: &Path) -> Result<BoardConfig, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    ron::from_str(&contents).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}
