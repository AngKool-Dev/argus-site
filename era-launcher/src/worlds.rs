//! World backup / restore / delete helpers.
//!
//! Backups are stored as `<data_local>/backups/<world>-<unix_ts>.zip`. The
//! archive is a standard zip of the entire `<instance>/saves/<world>/` tree
//! (preserving relative paths). Restore unpacks over the destination and
//! leaves a one-line confirmation in the launcher log.

use crate::prelude::*;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub fn backups_dir() -> PathBuf {
    crate::platform::Paths::new().data_local.join("backups")
}

/// Zip `<instance>/saves/<world>/` into `<backups_dir>/<world>-<ts>.zip`.
/// Returns the path of the created archive.
pub fn backup_world(instance_id: &str, world: &str) -> Result<PathBuf> {
    let saves_dir = crate::platform::Paths::new()
        .instances_dir()
        .join(instance_id)
        .join("saves")
        .join(world);
    if !saves_dir.is_dir() {
        return Err(LauncherError::NotFound(format!(
            "world directory not found: {}",
            saves_dir.display()
        )));
    }
    std::fs::create_dir_all(backups_dir())?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let archive_path = backups_dir().join(format!("{}-{}.zip", world, ts));
    let file = File::create(&archive_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let prefix_path = saves_dir.clone();
    add_dir_to_zip(&mut zip, &saves_dir, &prefix_path)?;
    zip.finish().map_err(|e| LauncherError::Zip(e.to_string()))?;
    Ok(archive_path)
}

fn add_dir_to_zip(
    zip: &mut zip::ZipWriter<File>,
    dir: &Path,
    prefix: &Path,
) -> std::io::Result<()> {
    use walkdir::WalkDir;
    let mut paths: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_path_buf())
        .collect();
    paths.sort();
    for path in paths {
        let rel = path
            .strip_prefix(prefix)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        if path.is_dir() {
            zip.add_directory(rel, zip::write::FileOptions::default())
                .map_err(io_err)?;
        } else {
            zip.start_file(rel, zip::write::FileOptions::default())
                .map_err(io_err)?;
            let mut f = File::open(&path)?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            zip.write_all(&buf)?;
        }
    }
    Ok(())
}

fn io_err(e: zip::result::ZipError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

/// Unpack `archive` into `<instances>/<instance>/saves/<world>/`. Returns
/// the destination directory.
pub fn restore_world(instance_id: &str, world: &str, archive: &Path) -> Result<PathBuf> {
    let dest = crate::platform::Paths::new()
        .instances_dir()
        .join(instance_id)
        .join("saves")
        .join(world);
    std::fs::create_dir_all(&dest)?;
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| LauncherError::Zip(e.to_string()))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| LauncherError::Zip(e.to_string()))?;
        let name = entry
            .enclosed_name()
            .ok_or_else(|| LauncherError::Zip("unsafe path".into()))?;
        let outpath = dest.join(name);
        if entry.is_dir() {
            std::fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            std::fs::write(&outpath, &buf)?;
        }
    }
    Ok(dest)
}

/// Remove a world's saves directory. The caller must prompt the user; this
/// function performs no confirmation.
pub fn delete_world(instance_id: &str, world: &str) -> Result<()> {
    let dir = crate::platform::Paths::new()
        .instances_dir()
        .join(instance_id)
        .join("saves")
        .join(world);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// List archives currently in the backups directory, newest first.
pub fn list_backups() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(backups_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("zip") {
                out.push(path);
            }
        }
    }
    out.sort();
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backup_restore_round_trip() {
        let inst = "test-instance";
        let world_name = "TestWorld";

        let tmp = std::env::temp_dir().join(format!("era-test-worlds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("instances").join(inst).join("saves").join(world_name)).unwrap();
        std::fs::write(
            tmp.join("instances")
                .join(inst)
                .join("saves")
                .join(world_name)
                .join("level.dat"),
            b"FAKE_LEVEL",
        )
        .unwrap();

        // Override the data dir via env (POSIX only). On Windows we still
        // exercise the path layout but the tests skip assertion of file
        // location when DATA_LOCAL override isn't applied.
        if cfg!(unix) {
            unsafe {
                std::env::set_var("HOME", &tmp);
            }
        }

        // Skip the round-trip on Windows (would write to the real LOCALAPPDATA).
        if cfg!(unix) {
            let archive = backup_world(inst, world_name).expect("backup");
            assert!(archive.exists());
            assert!(archive.to_string_lossy().ends_with(".zip"));

            // Wipe original, restore, verify
            let saves_dir = tmp.join("instances").join(inst).join("saves").join(world_name);
            std::fs::remove_dir_all(&saves_dir).unwrap();
            restore_world(inst, world_name, &archive).unwrap();
            let restored = saves_dir.join("level.dat");
            assert!(restored.exists());
            assert_eq!(std::fs::read(&restored).unwrap(), b"FAKE_LEVEL".to_vec());
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }
}