//! Auto-update (design section 7, without signatures for now).
//!
//! Every machine follows npm's `latest` of this platform's package, `@runablehq/preview-<platform>`:
//!
//! - At start, before anything else, a binary hands off (`exec`) to the newest installed
//!   version named in `~/.runable/preview/current`, when that is newer than itself.
//! - After a command's own work, when the last check is older than 6 hours, it starts
//!   `preview __update` detached and exits without waiting. That process fetches the registry
//!   metadata, downloads the new tarball, checks its sha512 against the registry's integrity
//!   field and that the new binary prints its version, then installs it under
//!   `versions/<v>/` and rewrites `current`.
//!
//! None of this writes to stdout or blocks a command. Failures are recorded in `update.json`
//! and retried at the next check.

use crate::signals;
use crate::state;
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha512};
use std::cmp::Ordering;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const CHECK_EVERY: u64 = 6 * 3600;
/// How soon to retry when `PREVIEW_VERSION` names a version that is not installed yet.
const PINNED_RETRY: u64 = 10 * 60;
const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org";
const METADATA_LIMIT: u64 = 16 * 1024 * 1024;
const TARBALL_LIMIT: u64 = 64 * 1024 * 1024;

const PLATFORM: &str = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    "linux-x64"
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    "linux-arm64"
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    "darwin-x64"
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    "darwin-arm64"
} else {
    "unsupported"
};

/// A plain `MAJOR.MINOR.PATCH`. Prereleases (`0.0.0-stage`) do not parse and are ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.trim().trim_start_matches('v').split('.');
        let mut next = || -> Option<u64> {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let v = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

fn own() -> Version {
    Version::parse(VERSION).unwrap_or(Version(0, 0, 0))
}

fn enabled() -> bool {
    std::env::var("PREVIEW_UPDATE").map_or(true, |v| v != "0")
}

fn pinned() -> Option<Version> {
    std::env::var("PREVIEW_VERSION")
        .ok()
        .and_then(|v| Version::parse(&v))
}

fn dir() -> PathBuf {
    state::home().join(".runable").join("preview")
}

fn binary(v: Version) -> PathBuf {
    dir().join("versions").join(v.to_string()).join("preview")
}

fn installed(v: Version) -> bool {
    fs::metadata(binary(v)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn current() -> Option<Version> {
    Version::parse(&fs::read_to_string(dir().join("current")).ok()?)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The version that handed off to this process, if one did.
fn handed_off_from() -> Option<String> {
    std::env::var("PREVIEW_HANDOFF")
        .ok()
        .filter(|v| !v.is_empty())
}

/// Runs first in `main`. Replaces this process with the newer installed version, or returns.
pub fn hand_off() {
    if !enabled() || handed_off_from().is_some() {
        return;
    }
    let target = match pinned() {
        Some(p) => (p != own()).then_some(p),
        None => current().filter(|c| *c > own()),
    };
    let Some(v) = target.filter(|v| installed(*v)) else {
        return;
    };
    // exec only returns on failure; then this binary carries on as itself.
    let _ = Command::new(binary(v))
        .args(std::env::args_os().skip(1))
        .env("PREVIEW_HANDOFF", VERSION)
        .exec();
}

/// What `update.json` remembers between checks.
#[derive(Default)]
struct Record {
    checked_at: u64,
    latest: Option<String>,
    last_error: Option<String>,
}

impl Record {
    fn load() -> Record {
        let Some(v) = fs::read_to_string(dir().join("update.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            return Record::default();
        };
        Record {
            checked_at: v["checkedAt"].as_u64().unwrap_or(0),
            latest: v["latest"].as_str().map(String::from),
            last_error: v["lastError"].as_str().map(String::from),
        }
    }

    fn save(&self) {
        let mut v = json!({ "checkedAt": self.checked_at });
        if let Some(latest) = &self.latest {
            v["latest"] = json!(latest);
        }
        if let Some(err) = &self.last_error {
            v["lastError"] = json!(err);
        }
        let _ = write_atomic(&dir().join("update.json"), &format!("{v}\n"));
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let partial = path.with_extension("partial");
    fs::write(&partial, text)?;
    fs::rename(&partial, path)
}

/// Runs last in `main`: starts a background check when one is due.
pub fn after_command() {
    if !enabled() || fs::create_dir_all(dir()).is_err() {
        return;
    }
    let every = match pinned() {
        Some(p) if p != own() && !installed(p) => PINNED_RETRY,
        _ => CHECK_EVERY,
    };
    let checked = Record::load().checked_at;
    let now = now();
    if checked <= now && now - checked < every {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("__update")
        // Already the newest binary here; the child needs no hand-off of its own.
        .env(
            "PREVIEW_HANDOFF",
            handed_off_from().unwrap_or_else(|| VERSION.to_string()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let fd_limit = signals::fd_limit();
    // SAFETY: child_setup only makes async-signal-safe calls.
    unsafe {
        cmd.pre_exec(move || signals::child_setup(fd_limit));
    }
    // Not waited for: it outlives this process in its own session.
    let _ = cmd.spawn();
}

fn lock(wait: bool) -> Option<File> {
    fs::create_dir_all(dir()).ok()?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir().join("update.lock"))
        .ok()?;
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(TryLockError::WouldBlock) if wait => {
            eprintln!("Waiting for another preview update to finish…");
            file.lock().ok()?;
            Some(file)
        }
        Err(_) => None,
    }
}

/// `preview __update`: the detached background check.
pub fn background() -> u8 {
    if let Some(_lock) = lock(false) {
        let _ = check();
    }
    0
}

/// `preview upgrade`: the same check in the foreground, saying what it did.
pub fn upgrade() -> u8 {
    let Some(_lock) = lock(true) else {
        eprintln!("preview: cannot write {}.", dir().display());
        return 1;
    };
    match check() {
        Ok(Done::UpToDate(v)) => {
            println!("preview {v} is the latest version.");
            0
        }
        Ok(Done::Switched(v)) => {
            println!("Switched to preview {v}; the next command runs it.");
            0
        }
        Ok(Done::Installed(v)) => {
            println!("Installed preview {v}; the next command runs it.");
            0
        }
        Err(e) => {
            eprintln!("preview: update failed: {e}");
            1
        }
    }
}

enum Done {
    UpToDate(Version),
    /// `current` now names a version that was already on disk.
    Switched(Version),
    Installed(Version),
}

fn check() -> Result<Done, String> {
    let mut record = Record::load();
    let result = decide_and_install(&mut record);
    record.checked_at = now();
    record.last_error = result.as_ref().err().cloned();
    record.save();
    result
}

fn decide_and_install(record: &mut Record) -> Result<Done, String> {
    let meta = metadata()?;
    let latest = meta["dist-tags"]["latest"]
        .as_str()
        .and_then(Version::parse);
    record.latest = latest.map(|v| v.to_string());

    // A pinned version is installed but never becomes `current`: shells without
    // PREVIEW_VERSION keep following latest.
    if let Some(p) = pinned() {
        if p == own() || installed(p) {
            return Ok(Done::UpToDate(p));
        }
        install(&meta, p)?;
        return Ok(Done::Installed(p));
    }

    let latest = latest.ok_or("the registry names no latest version")?;
    let active = current().unwrap_or(own());
    let deprecated = |v: Version| {
        meta["versions"][v.to_string()]["deprecated"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    };
    let want = match latest.cmp(&active) {
        Ordering::Greater => latest,
        // Rollback: latest moved back because the active version was deprecated.
        Ordering::Less if deprecated(active) => latest,
        _ => return Ok(Done::UpToDate(active)),
    };

    let done = if want == own() || installed(want) {
        Done::Switched(want)
    } else {
        install(&meta, want)?;
        Done::Installed(want)
    };
    write_atomic(&dir().join("current"), &format!("{want}\n"))
        .map_err(|e| format!("cannot write {}: {e}", dir().join("current").display()))?;
    prune(&[want, active]);
    Ok(done)
}

fn registry() -> String {
    std::env::var("PREVIEW_REGISTRY")
        .ok()
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| DEFAULT_REGISTRY.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn metadata() -> Result<Value, String> {
    let url = format!("{}/@runablehq%2fpreview-{PLATFORM}", registry());
    let body = get(
        &url,
        "application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8",
        Duration::from_secs(10),
        METADATA_LIMIT,
    )?;
    serde_json::from_slice(&body).map_err(|e| format!("{url} is not JSON: {e}"))
}

/// One GET, trusting the system's certificates first (so TLS-intercepting proxies work) and
/// Mozilla's roots second (for minimal images without a CA bundle). Proxies come from
/// HTTPS_PROXY / ALL_PROXY / NO_PROXY.
fn get(url: &str, accept: &str, timeout: Duration, limit: u64) -> Result<Vec<u8>, String> {
    let fetch = |roots: ureq::tls::RootCerts| -> Result<Vec<u8>, ureq::Error> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .tls_config(ureq::tls::TlsConfig::builder().root_certs(roots).build())
            .user_agent(format!("preview/{VERSION}"))
            .build()
            .into();
        agent
            .get(url)
            .header("Accept", accept)
            .call()?
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
    };
    match fetch(ureq::tls::RootCerts::PlatformVerifier) {
        Ok(body) => Ok(body),
        Err(e @ ureq::Error::StatusCode(_)) => Err(format!("GET {url}: {e}")),
        Err(_) => fetch(ureq::tls::RootCerts::WebPki).map_err(|e| format!("GET {url}: {e}")),
    }
}

fn install(meta: &Value, v: Version) -> Result<(), String> {
    let dist = &meta["versions"][v.to_string()]["dist"];
    let tarball = dist["tarball"]
        .as_str()
        .ok_or_else(|| format!("the registry has no tarball for {v}"))?;
    let integrity = dist["integrity"]
        .as_str()
        .ok_or_else(|| format!("the registry has no integrity for {v}"))?;
    let bytes = get(
        tarball,
        "application/octet-stream",
        Duration::from_secs(120),
        TARBALL_LIMIT,
    )?;
    verify_integrity(&bytes, integrity).map_err(|e| format!("{tarball}: {e}"))?;
    let exe = extract_binary(&bytes).map_err(|e| format!("{tarball}: {e}"))?;

    let versions = dir().join("versions");
    let partial = versions.join(format!("{v}.partial"));
    let _ = fs::remove_dir_all(&partial);
    fs::create_dir_all(&partial)
        .map_err(|e| format!("cannot create {}: {e}", partial.display()))?;
    let new = partial.join("preview");
    fs::write(&new, exe).map_err(|e| format!("cannot write {}: {e}", new.display()))?;
    fs::set_permissions(&new, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("cannot chmod {}: {e}", new.display()))?;

    let printed = run_version(&new)?;
    if !reports(&printed, v) {
        let _ = fs::remove_dir_all(&partial);
        return Err(format!("the {v} binary printed {printed:?} for --version"));
    }

    let done = versions.join(v.to_string());
    let _ = fs::remove_dir_all(&done);
    fs::rename(&partial, &done).map_err(|e| format!("cannot install {}: {e}", done.display()))
}

/// Accepts `sha512-<base64>` among the space-separated hashes of an SRI string.
fn verify_integrity(bytes: &[u8], integrity: &str) -> Result<(), String> {
    let got = base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes));
    let wanted: Vec<&str> = integrity
        .split_whitespace()
        .filter_map(|h| h.strip_prefix("sha512-"))
        .map(|h| h.split('?').next().unwrap_or(h))
        .collect();
    if wanted.is_empty() {
        return Err(format!("no sha512 in integrity {integrity:?}"));
    }
    if wanted.contains(&got.as_str()) {
        Ok(())
    } else {
        Err(format!(
            "sha512 mismatch (registry {integrity}, download sha512-{got})"
        ))
    }
}

/// `package/bin/preview` out of an npm tarball.
fn extract_binary(tgz: &[u8]) -> Result<Vec<u8>, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tgz));
    let entries = archive
        .entries()
        .map_err(|e| format!("not a tarball: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("bad tarball: {e}"))?;
        let is_binary = entry
            .path()
            .is_ok_and(|p| p.as_os_str() == "package/bin/preview");
        if is_binary {
            let mut out = Vec::new();
            entry
                .read_to_end(&mut out)
                .map_err(|e| format!("bad tarball: {e}"))?;
            return Ok(out);
        }
    }
    Err(String::from("no package/bin/preview in the tarball"))
}

/// Runs `<binary> --version` with updates off, for at most 5 seconds.
fn run_version(bin: &Path) -> Result<String, String> {
    let mut child = Command::new(bin)
        .arg("--version")
        .env("PREVIEW_UPDATE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run the new binary: {e}"))?;
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < until => sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(String::from(
                    "the new binary did not answer --version in 5 s",
                ));
            }
        }
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    Ok(out.trim().to_string())
}

/// `preview 0.4.1` or `preview 0.4.1 (…)`.
fn reports(printed: &str, v: Version) -> bool {
    let first = printed.lines().next().unwrap_or("");
    first
        .strip_prefix("preview ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(Version::parse)
        == Some(v)
}

/// Keeps the given versions (and a pinned one) and deletes every other version folder.
fn prune(keep: &[Version]) {
    let Ok(entries) = fs::read_dir(dir().join("versions")) else {
        return;
    };
    let mut keep: Vec<String> = keep.iter().map(|v| v.to_string()).collect();
    keep.extend(pinned().map(|v| v.to_string()));
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !keep.contains(&name) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// The text of `preview --version`: `preview 0.4.1 (installed 0.3.0, checked 2 h ago)`.
pub fn version_line() -> String {
    let mut notes: Vec<String> = Vec::new();
    if let Some(from) = handed_off_from().filter(|f| f != VERSION) {
        notes.push(format!("installed {from}"));
    }
    if !enabled() {
        notes.push(String::from("updates off"));
    } else {
        if let Some(p) = pinned() {
            notes.push(format!("pinned to {p}"));
        }
        let record = Record::load();
        notes.push(match record.checked_at {
            0 => String::from("not checked yet"),
            at => format!("checked {}", ago(now().saturating_sub(at))),
        });
        if record.last_error.is_some() {
            notes.push(String::from("last check failed, see preview upgrade"));
        }
    }
    format!("preview {VERSION} ({})", notes.join(", "))
}

fn ago(secs: u64) -> String {
    match secs {
        0..60 => String::from("just now"),
        60..3600 => format!("{} min ago", secs / 60),
        3600..172800 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(Version::parse("0.4.1"), Some(Version(0, 4, 1)));
        assert_eq!(Version::parse("v1.2.3\n"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("0.0.0-stage"), None);
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert!(Version::parse("0.10.0") > Version::parse("0.9.9"));
    }

    #[test]
    fn checks_integrity() {
        let data = b"hello";
        let sri = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(data))
        );
        assert!(verify_integrity(data, &sri).is_ok());
        assert!(verify_integrity(data, &format!("sha1-abc {sri}")).is_ok());
        assert!(verify_integrity(b"other", &sri).is_err());
        assert!(verify_integrity(data, "sha1-abc").is_err());
    }

    #[test]
    fn reads_version_lines() {
        let v = Version(0, 4, 1);
        assert!(reports("preview 0.4.1", v));
        assert!(reports(
            "preview 0.4.1 (installed 0.3.0, checked 2 h ago)\n",
            v
        ));
        assert!(!reports("preview 0.4.10", v));
        assert!(!reports("something else", v));
    }

    #[test]
    fn extracts_the_binary() {
        let mut tgz = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut tgz, flate2::Compression::fast());
            let mut tar = tar::Builder::new(gz);
            for (path, body) in [
                ("package/package.json", &b"{}"[..]),
                ("package/bin/preview", &b"#!/bin/sh\n"[..]),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                tar.append_data(&mut header, path, body).unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
        assert_eq!(extract_binary(&tgz).unwrap(), b"#!/bin/sh\n");
        assert!(extract_binary(b"junk").is_err());
    }
}
