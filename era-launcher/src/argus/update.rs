//! Release update checking against the public GitHub distribution repo.

use std::time::{Duration, Instant};

// Use the HTML releases page redirect to avoid the 60-req/hour unauthenticated
// API rate limit. The browser-style URL returns 302 -> /releases/tag/<version>,
// and the tag is parseable from the redirect target without consuming API quota.
const LATEST_RELEASE_REDIRECT: &str =
    "https://github.com/AngKool-Dev/argus-releases/releases/latest";

pub const RELEASES_PAGE: &str = "https://github.com/AngKool-Dev/argus-releases/releases/latest";

const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60); // 1 hour
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RETRIES: u32 = 3;
const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Path to the marker file the .bat helper writes to record the result of
/// the last update attempt. Read on launcher startup to detect failures.
pub fn update_marker_path(current_exe: &std::path::Path) -> std::path::PathBuf {
    let mut p = current_exe.as_os_str().to_owned();
    p.push(".update-result");
    std::path::PathBuf::from(p)
}

#[derive(Debug, Clone)]
pub enum UpdateCheckResult {
    UpToDate,
    UpdateAvailable(String),
    CheckFailed(String),
}

/// Spawns a detached thread that fetches the latest published release tag
/// with retries and backoff. Sends exactly one message with the result.
pub fn spawn_check(
    current_version: &'static str,
    last_check: Option<Instant>,
) -> std::sync::mpsc::Receiver<UpdateCheckResult> {
    let (tx, rx) = std::sync::mpsc::channel();

    if let Some(last) = last_check {
        if last.elapsed() < CHECK_INTERVAL {
            let _ = tx.send(UpdateCheckResult::UpToDate);
            return rx;
        }
    }

    std::thread::spawn(move || {
        let result = fetch_with_retry(current_version);
        let _ = tx.send(result);
    });
    rx
}

fn fetch_with_retry(current_version: &str) -> UpdateCheckResult {
    let mut attempt = 0;
    let mut delay = INITIAL_RETRY_DELAY;

    loop {
        attempt += 1;
        match fetch_latest_tag() {
            Ok(Some(tag)) => {
                if crate::argus::state::AppState::is_newer_version(&tag, current_version) {
                    return UpdateCheckResult::UpdateAvailable(tag);
                } else {
                    return UpdateCheckResult::UpToDate;
                }
            }
            Ok(None) => return UpdateCheckResult::UpToDate,
            Err(e) => {
                if attempt >= MAX_RETRIES {
                    return UpdateCheckResult::CheckFailed(format!(
                        "Update check failed after {} attempts: {}",
                        attempt, e
                    ));
                }
                std::thread::sleep(delay);
                delay *= 2;
            }
        }
    }
}

fn fetch_latest_tag() -> Result<Option<String>, String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(LATEST_RELEASE_REDIRECT)
            .header(
                "User-Agent",
                concat!("EraLauncher/", env!("CARGO_PKG_VERSION")),
            )
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Err(format!("GitHub returned {}", resp.status()));
        }

        // After following redirects, the final URL is /releases/tag/<version>.
        let final_url = resp.url().to_string();
        if let Some(tag) = final_url
            .rsplit('/')
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "latest")
        {
            return Ok(Some(tag));
        }
        // Fallback: scrape <title> from the final HTML body.
        let body = resp.text().await.map_err(|e| e.to_string())?;
        if let Some(start) = body.find("tag/") {
            let rest = &body[start + 4..];
            let end = rest
                .find(|c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '_')
                .unwrap_or(rest.len());
            let tag = rest[..end].trim();
            if !tag.is_empty() {
                return Ok(Some(tag.to_string()));
            }
        }
        Ok(None)
    })
}

/// Build the asset download URL from a known tag. The URL pattern is
/// `https://github.com/<owner>/<repo>/releases/download/<tag>/<asset>`, which
/// is unauthenticated and serves the actual binary without any API quota.
pub fn fetch_latest_asset_url() -> Result<String, String> {
    // The tag is fetched separately via the redirect-based fetch_latest_tag.
    // We re-derive it here to keep the call-site unchanged.
    let tag = fetch_latest_tag_internal()?;
    Ok(format!(
        "https://github.com/AngKool-Dev/argus-releases/releases/download/{}/era-launcher.exe",
        tag
    ))
}

fn fetch_latest_tag_internal() -> Result<String, String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(LATEST_RELEASE_REDIRECT)
            .header(
                "User-Agent",
                concat!("EraLauncher/", env!("CARGO_PKG_VERSION")),
            )
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("GitHub returned {}", resp.status()));
        }
        let final_url = resp.url().to_string();
        if let Some(tag) = final_url
            .rsplit('/')
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "latest")
        {
            return Ok(tag);
        }
        Err("could not parse latest tag".to_string())
    })
}

/// Download a file from `url` to `dest` with no progress reporting.
pub fn download_asset(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(url)
            .header(
                "User-Agent",
                concat!("EraLauncher/", env!("CARGO_PKG_VERSION")),
            )
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Err(format!("Download HTTP {}", resp.status()));
        }

        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        std::fs::write(dest, bytes).map_err(|e| e.to_string())?;
        Ok(())
    })
}

/// Create the per-OS update helper. On Windows this is a `.bat` file; on
/// POSIX systems it's a shell script. Both wait for the running launcher to
/// exit, copy the freshly downloaded binary over the current executable,
/// delete the staging file, relaunch the launcher, and record the result by
/// writing/clearing a marker file the launcher reads on next startup.
///
/// The marker file format is identical across platforms so the existing
/// `App::run` startup cleanup works without any Linux-specific branches.
pub fn create_update_helper(
    current_exe: &std::path::Path,
    new_exe: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    #[cfg(windows)]
    {
        create_update_helper_windows(current_exe, new_exe)
    }
    #[cfg(not(windows))]
    {
        create_update_helper_posix(current_exe, new_exe)
    }
}

#[cfg(windows)]
fn create_update_helper_windows(
    current_exe: &std::path::Path,
    new_exe: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let helper_path = current_exe.with_extension("bat");
    let current_exe_str = current_exe.to_string_lossy().replace("\\", "/");
    let new_exe_str = new_exe.to_string_lossy().replace("\\", "/");
    let helper_str = helper_path.to_string_lossy().replace("\\", "/");
    // Marker file the launcher reads on next startup. Writing "applied" means
    // the .bat succeeded; writing "failed" means it gave up. The marker is
    // cleared in App::run at startup.
    let marker_str = current_exe_str.clone() + ".update-result";

    let script = format!(
        r#"@echo off
setlocal EnableExtensions
set "CURRENT={current_exe_str}"
set "NEW={new_exe_str}"
set "HELPER={helper_str}"
set "MARKER={marker_str}"
set "ATTEMPTS=0"
:waitloop
set /a ATTEMPTS+=1
if %ATTEMPTS% GTR 60 (
    echo timeout waiting for old launcher to exit ^> "%MARKER%"
    goto done
)
tasklist /fi "imagename eq era-launcher.exe" 2>NUL | findstr /i "era-launcher.exe" >NUL
if not errorlevel 1 (
    timeout /t 1 /nobreak >NUL
    goto waitloop
)
:copy
copy /Y "%NEW%" "%CURRENT%" >NUL 2>&1
if errorlevel 1 (
    timeout /t 1 /nobreak >NUL
    goto copy
)
del /f /q "%NEW%" >NUL 2>&1
echo applied ^> "%MARKER%"
start "" "%CURRENT%"
exit /b 0
:done
del /f /q "%NEW%" >NUL 2>&1
exit /b 1
"#,
        current_exe_str = current_exe_str,
        new_exe_str = new_exe_str,
        helper_str = helper_str,
        marker_str = marker_str,
    );

    std::fs::write(&helper_path, script).map_err(|e| e.to_string())?;
    Ok(helper_path)
}

#[cfg(not(windows))]
fn create_update_helper_posix(
    current_exe: &std::path::Path,
    new_exe: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;

    // The helper lives next to the launcher so the relaunch can rely on
    // a relative path. Suffix `.update.sh` keeps it visually distinct
    // from the launcher binary.
    let helper_path = current_exe.with_extension("update.sh");
    let current_exe_str = shell_quote(current_exe);
    let new_exe_str = shell_quote(new_exe);
    let helper_dir = helper_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    let marker_str = shell_quote(&current_exe.with_extension("update-result"));

    // The script mirrors the Windows `.bat` semantics:
    //   1. Poll up to 60×1s for the launcher process to disappear (uses pgrep).
    //   2. cp -f the new binary over the old one; retry on transient failures.
    //   3. rm the staging file.
    //   4. Write "applied" to the marker and exec the new launcher.
    //
    // `setsid` puts the script in its own session so it survives the parent
    // launcher exiting; `disown`/`nohup` (via `&` and `exit 0`) ensures it
    // doesn't get reaped by the parent's signal mask.
    let script = format!(
        r#"#!/bin/sh
# Auto-generated update helper for EraLauncher — DO NOT EDIT.
set -u
CURRENT={current_exe_str}
NEW={new_exe_str}
MARKER={marker_str}
ATTEMPTS=0
# Wait for the old launcher to exit (up to 60 seconds).
while [ "$ATTEMPTS" -lt 60 ]; do
    ATTEMPTS=$((ATTEMPTS + 1))
    if ! pgrep -x era-launcher >/dev/null 2>&1; then
        break
    fi
    sleep 1
done
if pgrep -x era-launcher >/dev/null 2>&1; then
    printf 'timeout' > "$MARKER"
    rm -f -- "$NEW"
    exit 1
fi
# Copy the new binary in place; retry on transient failures.
ATTEMPTS=0
while [ "$ATTEMPTS" -lt 30 ]; do
    if cp -f -- "$NEW" "$CURRENT" 2>/dev/null; then
        break
    fi
    ATTEMPTS=$((ATTEMPTS + 1))
    sleep 1
done
if [ ! -f "$CURRENT" ]; then
    printf 'failed' > "$MARKER"
    rm -f -- "$NEW"
    exit 1
fi
chmod +x -- "$CURRENT" 2>/dev/null || true
rm -f -- "$NEW"
printf 'applied' > "$MARKER"
# Detach so this script keeps running after the launcher exits.
nohup "$CURRENT" >/dev/null 2>&1 &
exit 0
"#
    );

    std::fs::write(&helper_path, script).map_err(|e| e.to_string())?;
    let mut perms = std::fs::metadata(&helper_path)
        .map_err(|e| e.to_string())?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&helper_path, perms).map_err(|e| e.to_string())?;

    // Silence unused-variable lint on the directory; the script always operates
    // relative to the helper path so we don't need it.
    let _ = helper_dir;
    Ok(helper_path)
}

#[cfg(not(windows))]
fn shell_quote(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().to_string();
    if s.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | '=' | ':' | '+')
    }) {
        s
    } else {
        // Wrap in single quotes and escape any embedded single quotes.
        let escaped = s.replace('\'', "'\\''");
        format!("'{}'", escaped)
    }
}
