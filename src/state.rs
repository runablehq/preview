//! State files, logs and locks under `PREVIEW_HOME` (default `~/.runable/previews`).
//!
//! A state file is `key=value` lines. Every version reads every older `state_version`, and a new
//! key never changes the meaning of an old one, so a binary of any version can stop a preview
//! started by another.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub name: String,
    pub cwd: PathBuf,
    pub command: String,
    /// Process group id of the run (the session leader's pid).
    pub pid: i32,
    /// The port the run was offered, replaced by the port it reported once it listens.
    pub port: u16,
    pub started: u64,
    /// Start time of the leader, used to tell a reused pid from ours.
    pub leader_start: String,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn dir() -> PathBuf {
    match std::env::var_os("PREVIEW_HOME").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => home().join(".runable").join("previews"),
    }
}

fn state_path(name: &str) -> PathBuf {
    dir().join(format!("{name}.state"))
}

pub fn log_path(name: &str) -> PathBuf {
    dir().join(format!("{name}.log"))
}

pub fn load(name: &str) -> Option<State> {
    let text = fs::read_to_string(state_path(name)).ok()?;
    Some(parse(name, &text))
}

/// All states, sorted by name.
pub fn all() -> Vec<State> {
    let Ok(entries) = fs::read_dir(dir()) else {
        return Vec::new();
    };
    let mut states: Vec<State> = entries
        .flatten()
        .filter_map(|e| {
            let file = e.file_name();
            let name = file.to_str()?.strip_suffix(".state")?.to_string();
            load(&name)
        })
        .collect();
    states.sort_by(|a, b| a.name.cmp(&b.name));
    states
}

pub fn save(st: &State) -> io::Result<()> {
    let dir = dir();
    fs::create_dir_all(&dir)?;
    let partial = dir.join(format!("{}.state.partial", st.name));
    fs::write(&partial, render(st))?;
    fs::rename(&partial, state_path(&st.name))
}

/// Takes the exclusive lock for one name. Held until the returned file is dropped.
pub fn lock(name: &str) -> io::Result<File> {
    let dir = dir();
    fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{name}.lock")))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            eprintln!("Waiting for another preview command on {name} to finish…");
            file.lock()?;
            Ok(file)
        }
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// Empties the log and opens it for appending, once for stdout and once for stderr.
pub fn open_log(name: &str) -> io::Result<(File, File)> {
    let path = log_path(name);
    fs::create_dir_all(dir())?;
    File::create(&path)?;
    let out = OpenOptions::new().append(true).open(&path)?;
    let err = out.try_clone()?;
    Ok((out, err))
}

pub fn parse(name: &str, text: &str) -> State {
    let mut st = State {
        name: name.to_string(),
        ..State::default()
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unescape(value);
        match key {
            "cwd" => st.cwd = PathBuf::from(value),
            "command" => st.command = value,
            "pid" => st.pid = value.parse().unwrap_or(0),
            "port" => st.port = value.parse().unwrap_or(0),
            "started" => st.started = value.parse().unwrap_or(0),
            "leader_start" => st.leader_start = value,
            _ => {}
        }
    }
    st
}

pub fn render(st: &State) -> String {
    format!(
        "state_version={STATE_VERSION}\ncwd={}\ncommand={}\npid={}\nport={}\nstarted={}\nleader_start={}\n",
        escape(&st.cwd.to_string_lossy()),
        escape(&st.command),
        st.pid,
        st.port,
        st.started,
        escape(&st.leader_start),
    )
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The last `lines` lines of a file, read from the end so a large log costs little.
pub fn tail(path: &Path, lines: usize) -> String {
    const CHUNK: u64 = 16 * 1024;
    const LIMIT: usize = 4 * 1024 * 1024;
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let Ok(mut pos) = file.seek(SeekFrom::End(0)) else {
        return String::new();
    };
    let mut buf: Vec<u8> = Vec::new();
    while pos > 0 && buf.len() < LIMIT {
        let step = CHUNK.min(pos);
        pos -= step;
        let mut chunk = vec![0; step as usize];
        if file.seek(SeekFrom::Start(pos)).is_err() || file.read_exact(&mut chunk).is_err() {
            break;
        }
        chunk.extend_from_slice(&buf);
        buf = chunk;
        if buf.iter().filter(|b| **b == b'\n').count() > lines {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    if text.is_empty() || lines == 0 {
        return String::new();
    }
    let all: Vec<&str> = text.split('\n').collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let st = State {
            name: "apps-shop".into(),
            cwd: PathBuf::from("/home/user/apps/shop"),
            command: "echo 'a\\b'\nnext".into(),
            pid: 412,
            port: 5173,
            started: 1792249078,
            leader_start: "boot:123".into(),
        };
        assert_eq!(parse("apps-shop", &render(&st)), st);
    }

    #[test]
    fn reads_older_and_unknown_keys() {
        let st = parse(
            "x",
            "state_version=1\ncwd=/a\ncommand=bun run dev\npid=7\nport=3000\nstarted=1\nfuture=yes\n",
        );
        assert_eq!(st.pid, 7);
        assert_eq!(st.port, 3000);
        assert_eq!(st.leader_start, "");
    }

    #[test]
    fn tails_files() {
        let dir = std::env::temp_dir().join(format!("preview-tail-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log");
        let body: String = (1..=50_000).map(|i| format!("line {i}\n")).collect();
        fs::write(&path, body).unwrap();
        assert_eq!(tail(&path, 2), "line 49999\nline 50000");
        fs::write(&path, "only").unwrap();
        assert_eq!(tail(&path, 5), "only");
        fs::write(&path, "").unwrap();
        assert_eq!(tail(&path, 5), "");
        fs::remove_dir_all(&dir).unwrap();
    }
}
