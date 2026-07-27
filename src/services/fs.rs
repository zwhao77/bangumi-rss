//! Real file-system operations behind `FileOps` trait.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::traits::FileOps;

pub struct RealFileSystem;

impl FileOps for RealFileSystem {
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

    fn file_size(&self, path: &Path) -> anyhow::Result<u64> {
        Ok(std::fs::metadata(path)?.len())
    }

    fn read_chunk(&self, path: &Path, offset: u64, length: usize) -> anyhow::Result<Vec<u8>> {
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; length];
        let n = file.read(&mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn open_file(&self, path: &Path) -> anyhow::Result<std::fs::File> {
        Ok(std::fs::File::open(path)?)
    }
}
