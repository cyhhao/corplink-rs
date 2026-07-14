use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct PreparedUpdate {
    target: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
}

impl PreparedUpdate {
    pub fn prepare(source: &Path, target: &Path, expected_version: &str) -> Result<Self, String> {
        let token = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let staged = sibling_path(target, "new", &token)?;
        let backup = sibling_path(target, "backup", &token)?;

        if let Err(e) = copy_file_synced(source, &staged, true) {
            return Err(format!(
                "failed to stage update next to {}: {}. rerun with sufficient permissions",
                target.display(),
                e
            ));
        }

        if let Err(e) = verify_binary_version(&staged, expected_version) {
            let _ = fs::remove_file(&staged);
            return Err(e);
        }

        if target.exists() {
            if let Err(e) = copy_file_synced(target, &backup, false) {
                let _ = fs::remove_file(&staged);
                return Err(format!("failed to back up current executable: {}", e));
            }
        }

        Ok(Self {
            target: target.to_path_buf(),
            staged,
            backup,
        })
    }

    #[cfg(windows)]
    pub fn from_paths(target: PathBuf, staged: PathBuf, backup: PathBuf) -> Self {
        Self {
            target,
            staged,
            backup,
        }
    }

    pub fn target(&self) -> &Path {
        &self.target
    }

    #[cfg(windows)]
    pub fn staged(&self) -> &Path {
        &self.staged
    }

    #[cfg(windows)]
    pub fn backup(&self) -> &Path {
        &self.backup
    }

    pub fn apply(&self) -> Result<(), String> {
        atomic_replace(&self.staged, &self.target).map_err(|e| {
            format!(
                "failed to atomically replace {}: {}",
                self.target.display(),
                e
            )
        })?;
        sync_parent(&self.target).map_err(|e| format!("failed to sync install directory: {}", e))
    }

    pub fn rollback(&self) -> Result<(), String> {
        if !self.backup.exists() {
            return Err("update backup is missing".to_string());
        }
        atomic_replace(&self.backup, &self.target)
            .map_err(|e| format!("failed to restore previous executable: {}", e))?;
        sync_parent(&self.target).map_err(|e| format!("failed to sync install directory: {}", e))
    }

    pub fn cleanup(&self) {
        let _ = fs::remove_file(&self.staged);
        let _ = fs::remove_file(&self.backup);
    }
}

pub fn verify_binary_version(binary: &Path, expected_version: &str) -> Result<(), String> {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| format!("failed to run staged binary {}: {}", binary.display(), e))?;

    if !output.status.success() {
        return Err(format!(
            "staged binary exited with {}",
            output.status.code().unwrap_or(-1)
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let actual_version = stdout
        .split_whitespace()
        .last()
        .map(normalize_version)
        .ok_or_else(|| "staged binary did not report a version".to_string())?;
    let expected_version = normalize_version(expected_version);

    if actual_version != expected_version {
        return Err(format!(
            "staged binary version mismatch: expected {}, got {}",
            expected_version, actual_version
        ));
    }

    Ok(())
}

#[cfg(windows)]
pub fn copy_executable(source: &Path, destination: &Path) -> Result<(), String> {
    copy_file_synced(source, destination, true)
        .map_err(|e| format!("failed to create update helper: {}", e))
}

#[cfg(windows)]
pub fn schedule_delete_on_reboot(path: &Path) {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_DELAY_UNTIL_REBOOT: u32 = 0x4;

    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(path.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT);
    }
}

fn normalize_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn sibling_path(target: &Path, kind: &str, token: &str) -> Result<PathBuf, String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", target.display()))?;
    let name = target
        .file_name()
        .ok_or_else(|| format!("{} has no file name", target.display()))?
        .to_string_lossy();
    Ok(parent.join(format!(".{}.{}.{}", name, kind, token)))
}

fn copy_file_synced(source: &Path, destination: &Path, executable: bool) -> io::Result<()> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;

    if let Err(e) = io::copy(&mut input, &mut output) {
        drop(output);
        let _ = fs::remove_file(destination);
        return Err(e);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable {
            0o755
        } else {
            fs::metadata(source)?.permissions().mode()
        };
        fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    }

    #[cfg(not(unix))]
    if !executable {
        fs::set_permissions(destination, fs::metadata(source)?.permissions())?;
    }

    output.sync_all()?;
    sync_parent(destination)
}

#[cfg(unix)]
fn atomic_replace(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{copy_file_synced, PreparedUpdate};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(name: &str) -> PathBuf {
        let token = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "corplink-updater-test-{}-{}-{}",
            name,
            std::process::id(),
            token
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_copy_failure_does_not_modify_destination() {
        let dir = test_dir("copy-failure");
        let destination = dir.join("corplink");
        fs::write(&destination, b"old").unwrap();

        let result = copy_file_synced(&dir.join("missing"), &dir.join("staged"), true);

        assert!(result.is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_apply_and_rollback_are_transactional() {
        let dir = test_dir("transaction");
        let target = dir.join("corplink");
        let source = dir.join("downloaded-corplink");
        let expected_version = env!("BUILD_VERSION");
        fs::write(
            &source,
            format!("#!/bin/sh\necho 'corplink {}'\n", expected_version),
        )
        .unwrap();
        fs::write(&target, b"old-binary").unwrap();

        let prepared = PreparedUpdate::prepare(&source, &target, expected_version).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"old-binary");

        prepared.apply().unwrap();
        assert_ne!(fs::read(&target).unwrap(), b"old-binary");

        prepared.rollback().unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"old-binary");
        prepared.cleanup();
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_prepare_rejects_wrong_version_before_touching_target() {
        let dir = test_dir("version-mismatch");
        let target = dir.join("corplink");
        let source = dir.join("downloaded-corplink");
        fs::write(&source, b"#!/bin/sh\necho 'corplink 0.0.0'\n").unwrap();
        fs::write(&target, b"old-binary").unwrap();

        let result = PreparedUpdate::prepare(&source, &target, "9.9.9");

        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old-binary");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        let _ = fs::remove_dir_all(dir);
    }
}
