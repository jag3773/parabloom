//! Full in-place saves are staged in a private, exclusively created directory.

use std::fs::{self, DirBuilder};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mupdf::pdf::{PdfDocument, PdfWriteOptions};

use crate::expand::Error;

struct SaveDirectory(PathBuf);

impl SaveDirectory {
    fn create(parent: &Path) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..100 {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".parabloom-save-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot reserve a private save directory",
        ))
    }
}

impl Drop for SaveDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn save_replacement(
    doc: &PdfDocument,
    target: &Path,
    options: PdfWriteOptions,
) -> Result<(), Error> {
    let permissions = fs::metadata(target)?.permissions();
    let directory = SaveDirectory::create(target.parent().unwrap_or(Path::new(".")))?;
    let staged = directory.0.join("document.pdf");
    // MuPDF unlinks an existing destination before writing, so precreating a
    // 0600 file would not protect it. The 0700 directory protects the bytes
    // throughout the write, including before the final permissions are set.
    let path = staged
        .to_str()
        .ok_or_else(|| Error::NotPdf(staged.display().to_string()))?;
    doc.save_with_options(path, options)?;
    fs::set_permissions(&staged, permissions)?;
    fs::File::open(&staged)?.sync_all()?;
    fs::rename(&staged, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_directories_are_unique_and_private() {
        let parent = SaveDirectory::create(&std::env::temp_dir()).unwrap();
        let first = SaveDirectory::create(&parent.0).unwrap();
        let second = SaveDirectory::create(&parent.0).unwrap();
        assert_ne!(first.0, second.0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&first.0).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let path = first.0.clone();
        drop(first);
        assert!(!path.exists());
    }

    #[test]
    fn failed_staged_save_leaves_original_and_cleans_temporary_directory() {
        let parent = SaveDirectory::create(&std::env::temp_dir()).unwrap();
        let target = parent.0.join("input.pdf");
        fs::write(&target, b"original bytes").unwrap();
        // A new document cannot be saved incrementally; force the write to
        // fail after staging has been reserved, without changing a real PDF.
        let doc = PdfDocument::new();
        let mut options = PdfWriteOptions::default();
        options.set_incremental(true);
        assert!(save_replacement(&doc, &target, options).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original bytes");
        assert_eq!(fs::read_dir(&parent.0).unwrap().count(), 1);
    }
}
