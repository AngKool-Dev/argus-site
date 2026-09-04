//! World backup/restore round-trip — builds a synthetic world, zips it via
//! `worlds::backup_world`, deletes the original, restores from the archive,
//! and asserts the original files survived byte-for-byte.

use era_launcher_lib::worlds;
use std::fs;
use std::path::PathBuf;

#[allow(dead_code)]
fn setup_world(dir: &PathBuf) {
    let saves = dir.join("saves").join("MyWorld");
    fs::create_dir_all(saves.join("data")).unwrap();
    fs::write(saves.join("level.dat"), b"LEVEL").unwrap();
    fs::write(saves.join("data").join("map_0.dat"), b"MAP").unwrap();
    fs::write(saves.join("session.lock"), b"\0\0\0").unwrap();
}

#[test]
fn test_backup_then_restore_round_trip() {
    let pid = std::process::id();
    let tmp = std::env::temp_dir().join(format!("era-test-worlds-it-{pid}"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // On Linux override HOME so Paths::new() resolves to our temp dir.
    // On Windows the backup would write to the real LOCALAPPDATA — skip
    // there.
    let run = if cfg!(unix) {
        unsafe {
            std::env::set_var("HOME", &tmp);
        }
        true
    } else {
        false
    };

    if run {
        let archive = worlds::backup_world("inst-1", "MyWorld").expect("backup");
        assert!(archive.exists());

        // Wipe the source dir
        let saves = tmp.join("instances").join("inst-1").join("saves").join("MyWorld");
        fs::remove_dir_all(&saves).unwrap();

        // Restore
        let dest = worlds::restore_world("inst-1", "MyWorld", &archive).expect("restore");
        assert!(dest.join("level.dat").exists());
        assert_eq!(fs::read(dest.join("level.dat")).unwrap(), b"LEVEL".to_vec());
        assert_eq!(
            fs::read(dest.join("data").join("map_0.dat")).unwrap(),
            b"MAP".to_vec()
        );
    }

    let _ = fs::remove_dir_all(&tmp);
}