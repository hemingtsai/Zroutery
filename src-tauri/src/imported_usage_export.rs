//! Portable imported-history archives, written to a user-selected file.

use std::io::{BufWriter, Write};
use std::path::Path;

use serde::Serialize;
use zroutery_core::history_import::{ImportedFact, ImportedHistoryQuery};
use zroutery_core::runtime_store::RuntimeStore;

#[derive(Serialize)]
pub(crate) struct Archive {
    schema_version: u32,
    /// The imported-history revision the export was read from.
    revision: i64,
    records: Vec<ImportedFact>,
}

pub(crate) fn archive(
    store: &RuntimeStore,
    query: &ImportedHistoryQuery,
) -> Result<Archive, String> {
    let (revision, mut records) = store.export_imported_facts(query)?;
    for record in &mut records {
        // Source error strings can contain upstream diagnostics; portable
        // archives carry accounting facts, not live diagnostics.
        record.error = None;
    }
    Ok(Archive {
        schema_version: 1,
        revision,
        records,
    })
}

pub(crate) fn write_archive(
    store: &RuntimeStore,
    query: &ImportedHistoryQuery,
    path: &Path,
) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or("the selected export path has no parent directory")?;
    let temporary = parent.join(format!(
        ".zroutery-imported-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let permissions = match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.permissions().readonly() => {
            Some(metadata.permissions())
        }
        Ok(_) => return Err("the export destination is not a writable regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot inspect export destination: {error}")),
    };
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&temporary)
            .map_err(|error| format!("cannot create export: {error}"))?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)
                .map_err(|error| format!("cannot preserve export permissions: {error}"))?;
        }
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, &archive(store, query)?)
            .map_err(|error| format!("cannot write imported archive: {error}"))?;
        writer
            .flush()
            .map_err(|error| format!("cannot flush imported archive: {error}"))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("cannot sync imported archive: {error}"))?;
        drop(writer);
        publish(&temporary, path)
            .map_err(|error| format!("cannot publish imported archive: {error}"))?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("cannot sync export directory: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn publish(temporary: &Path, path: &Path) -> std::io::Result<()> {
    std::fs::rename(temporary, path)
}

#[cfg(windows)]
fn publish(temporary: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "Kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }
    if !path.try_exists()? {
        return std::fs::rename(temporary, path);
    }
    let replaced: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let replacement: Vec<_> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
