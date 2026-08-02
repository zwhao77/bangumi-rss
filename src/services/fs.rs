//! Real file-system operations behind `FileOps` trait.

use std::path::Path;

use crate::traits::FileOps;

pub struct RealFileSystem;

impl FileOps for RealFileSystem {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn move_file(&self, from: &Path, to: &Path) -> anyhow::Result<()> {
        // Try rename first (fast, same filesystem).
        if std::fs::rename(from, to).is_ok() {
            return Ok(());
        }
        // Fallback: copy + delete (cross-filesystem, e.g. Docker volumes).
        std::fs::copy(from, to)?;
        std::fs::remove_file(from)?;
        Ok(())
    }

    fn ensure_dir(&self, path: &Path) -> anyhow::Result<()> {
        Ok(std::fs::create_dir_all(path)?)
    }

    fn read_to_string(&self, path: &Path) -> anyhow::Result<String> {
        Ok(std::fs::read_to_string(path)?)
    }

    fn write_string(&self, path: &Path, content: &str) -> anyhow::Result<()> {
        Ok(std::fs::write(path, content)?)
    }

    fn open_file(&self, path: &Path) -> anyhow::Result<crate::types::FileStream> {
        let file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        Ok(crate::types::FileStream::new(file, size))
    }
}
