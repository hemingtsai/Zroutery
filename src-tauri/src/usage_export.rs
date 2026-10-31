//! Portable accounting archives, written to a user-selected file in Rust.

use std::io::{BufWriter, Write};
use std::path::Path;

use serde::Serialize;
use zroutery_core::stats::{RequestRecord, Stats};

#[derive(Serialize)]
pub(crate) struct Archive {
    schema_version: u32,
    records: Vec<RequestRecord>,
}

pub(crate) fn archive(stats: &Stats) -> Result<Archive, String> {
    let mut records = stats.export_history()?;
    // The memory-only path can contain live diagnostics. Portable archives
    // have the same redaction contract whether SQLite is attached or not.
    for record in &mut records {
        record.error = None;
        record.routing_decision = None;
    }
    Ok(Archive {
        schema_version: 1,
        records,
    })
}

pub(crate) fn write_archive(stats: &Stats, path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or("the selected export path has no parent directory")?;
    let temporary = parent.join(format!(
        ".zroutery-usage-{}.tmp",
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
        stats.write_history_archive(&mut writer)?;
        writer
            .flush()
            .map_err(|error| format!("cannot flush usage archive: {error}"))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("cannot sync usage archive: {error}"))?;
        drop(writer);
        publish(&temporary, path)
            .map_err(|error| format!("cannot publish usage archive: {error}"))?;
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
    // ReplaceFile merges the original file's DACL/attributes; plain rename of
    // a temp would inherit the directory's ACL and could widen a private export.
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

#[cfg(test)]
mod tests {
    use super::*;
    use zroutery_core::stats::RecordBuilder;

    #[test]
    fn file_export_round_trips_metadata_without_live_diagnostics() {
        let directory =
            std::env::temp_dir().join(format!("zroutery-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("用量.json");
        let stats = Stats::new(1);
        let mut record =
            RecordBuilder::new(zroutery_core::ir::Dialect::OpenAI, "standard-class", false)
                .finish(1);
        record.error = Some("private diagnostic".into());
        stats.record(record);
        write_archive(&stats, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["records"].as_array().unwrap().len(), 1);
        assert!(!text.contains("private diagnostic"));
        let records =
            serde_json::from_value::<Vec<RequestRecord>>(value["records"].clone()).unwrap();
        assert_eq!(records[0].requested_model, "standard-class");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        // A second export replaces the selected file rather than failing on an
        // existing destination or leaving the old archive partially truncated.
        stats.clear_history(None).unwrap();
        write_archive(&stats, &path).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(value["records"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_publication_preserves_destination_and_removes_temporary_export() {
        let directory =
            std::env::temp_dir().join(format!("zroutery-export-fail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let destination = directory.join("kept.json");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("kept.txt"), b"user file").unwrap();
        assert!(write_archive(&Stats::new(1), &destination).is_err());
        assert_eq!(
            std::fs::read(destination.join("kept.txt")).unwrap(),
            b"user file"
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
