//! `preview run`: stop the earlier run, pick a port, start the command in its own session, wait
//! until its tree listens, and report the port it really listens on.

use crate::json::{self, Object};
use crate::messages;
use crate::ports;
use crate::procs::Listener;
use crate::signals;
use crate::state::{self, State};
use crate::stop;
use crate::tree;
use std::collections::HashSet;
use std::net::IpAddr;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct Options {
    pub name: Option<String>,
    pub port: Option<u16>,
    pub wait: f64,
    pub command: Vec<String>,
}

const TAIL_LINES: usize = 20;

enum Outcome {
    Listening(Vec<Listener>),
    Exited(ExitStatus),
    Starting,
}

pub fn run(opts: Options) -> u8 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("preview: cannot read the current folder: {e}");
            return 1;
        }
    };
    let name = match &opts.name {
        Some(n) => match slug(n) {
            s if s.is_empty() => {
                eprintln!("preview: --name needs letters or digits.");
                return 1;
            }
            s => s,
        },
        None => folder_name(&cwd, &state::home(), |n| state::load(n).map(|s| s.cwd)),
    };
    let _lock = match state::lock(&name) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!(
                "preview: cannot lock {} in {}: {e}",
                name,
                state::dir().display()
            );
            return 1;
        }
    };

    // 2. Stop the earlier run of this name.
    let previous = state::load(&name);
    if let Some(prev) = &previous
        && !stop::live_tree(prev, &tree::snapshot()).is_empty()
    {
        if !stop::stop_state(prev) {
            eprintln!(
                "preview: the earlier run of {name} (process group {}) did not exit; starting anyway.",
                prev.pid
            );
        }
        // A socket can outlive its process by a moment; give the port it held that moment.
        ports::wait_free(opts.port.unwrap_or(prev.port), Duration::from_secs(1));
    }

    // 3. Choose the offered port.
    let port = match opts.port {
        Some(p) if !ports::is_free(p) => {
            eprintln!("{}", stop::holder_message(p));
            return 1;
        }
        Some(p) => p,
        None => {
            let remembered: HashSet<u16> = state::all()
                .into_iter()
                .filter(|s| s.name != name && s.port > 0)
                .map(|s| s.port)
                .collect();
            ports::choose(previous.as_ref().map(|s| s.port), &remembered)
        }
    };

    // 4–5. Start it.
    let host = std::env::var("PREVIEW_HOST")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| String::from(DEFAULT_HOST));
    let display = display_command(&opts.command);
    let script = script(&opts.command, port, &host);
    let log = state::log_path(&name);
    let (out, err) = match state::open_log(&name) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("preview: cannot write {}: {e}", log.display());
            return 1;
        }
    };
    let mut command = Command::new(shell());
    command
        .arg("-c")
        .arg(&script)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .env("PORT", port.to_string())
        .env("HOST", &host)
        .env("BROWSER", "none")
        .env("PYTHONUNBUFFERED", "1")
        .env("NG_CLI_ANALYTICS", "false");
    let fd_limit = signals::fd_limit();
    // SAFETY: child_setup only makes async-signal-safe calls.
    unsafe {
        command.pre_exec(move || signals::child_setup(fd_limit));
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("preview: cannot start `{display}`: {e}");
            return 1;
        }
    };
    let pid = child.id() as i32;

    // Written now, not at the end, so a `preview run` killed while waiting leaves a run that the
    // next one can still find and stop.
    let mut st = State {
        name: name.clone(),
        cwd: cwd.clone(),
        command: display.clone(),
        pid,
        port,
        started: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        leader_start: tree::start_token(pid).unwrap_or_default(),
    };
    save(&st);

    // 6. Wait.
    let deadline = Instant::now() + Duration::from_secs_f64(opts.wait.max(0.0));
    let outcome = loop {
        let exited = child.try_wait().ok().flatten();
        let listeners = tree_listeners(pid);
        if !listeners.is_empty() {
            sleep(Duration::from_millis(300));
            let again = tree_listeners(pid);
            break Outcome::Listening(if again.is_empty() { listeners } else { again });
        }
        if let Some(status) = exited
            && tree::snapshot().tree(pid).is_empty()
        {
            break Outcome::Exited(status);
        }
        if Instant::now() >= deadline {
            break Outcome::Starting;
        }
        sleep(Duration::from_millis(100));
    };

    match outcome {
        Outcome::Listening(listeners) => {
            // 7. The offered port when the tree listens on it, else the lowest.
            let mut found: Vec<u16> = listeners.iter().map(|l| l.port).collect();
            found.sort_unstable();
            found.dedup();
            let reported = if found.contains(&port) {
                port
            } else {
                found[0]
            };
            st.port = reported;
            save(&st);

            // 8. Reachability.
            let on_port: Vec<&Listener> = listeners.iter().filter(|l| l.port == reported).collect();
            if !is_loopback_host(&host) && on_port.iter().all(|l| l.is_loopback()) {
                let mut addrs: Vec<String> = on_port.iter().map(|l| l.ip.to_string()).collect();
                addrs.sort();
                addrs.dedup();
                let tail = state::tail(&log, TAIL_LINES);
                let hint = messages::loopback_hint(&hint_text(&cwd, &display), &tail);
                eprintln!(
                    "{}",
                    messages::loopback(reported, &addrs.join(" and "), hint)
                );
                return 1;
            }

            // 9. Report.
            json::print(
                &Object::new()
                    .str("name", &name)
                    .num("port", reported)
                    .raw("ports", &json::numbers(&found))
                    .num("pid", pid)
                    .str("url", &format!("http://localhost:{reported}"))
                    .str("log", &log.to_string_lossy())
                    .finish(),
            );
            0
        }
        Outcome::Exited(status) => {
            let tail = state::tail(&log, TAIL_LINES);
            let mut msg = messages::exited(&display, &describe(status), &log, &tail);
            match clash_port(&tail, &[&display, &script]) {
                Some(p) => {
                    msg.push('\n');
                    msg.push_str(&stop::holder_message(p));
                }
                None => {
                    msg.push('\n');
                    msg.push_str(messages::exited_action(status.success()));
                }
            }
            eprintln!("{msg}");
            1
        }
        Outcome::Starting => {
            let tail = state::tail(&log, TAIL_LINES);
            eprintln!(
                "{}",
                messages::still_starting(&display, opts.wait, &log, &tail, &name)
            );
            2
        }
    }
}

#[cfg(target_os = "linux")]
const DEFAULT_HOST: &str = "0.0.0.0";
#[cfg(not(target_os = "linux"))]
const DEFAULT_HOST: &str = "127.0.0.1";

fn save(st: &State) {
    if let Err(e) = state::save(st) {
        eprintln!(
            "preview: cannot write the state of {} in {}: {e}",
            st.name,
            state::dir().display()
        );
    }
}

fn tree_listeners(pid: i32) -> Vec<Listener> {
    tree::listeners(&tree::snapshot().tree(pid))
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn describe(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("code {code}"),
        (None, Some(sig)) => format!("signal {sig}"),
        _ => String::from("unknown status"),
    }
}

fn shell() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("bash"))
        .find(|bash| bash.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

/// The port a clash message is about: the last number on the clash line that something listens
/// on, else the first such number in the command.
fn clash_port(tail: &str, commands: &[&str]) -> Option<u16> {
    let line = messages::clash_line(tail)?;
    let listening = |p: &u16| !ports::is_free(*p);
    let mut on_line = messages::port_numbers(line);
    on_line.reverse();
    on_line.into_iter().find(listening).or_else(|| {
        commands
            .iter()
            .flat_map(|c| messages::port_numbers(c))
            .find(listening)
    })
}

/// The command text, plus the package.json script it runs, for picking a framework hint.
fn hint_text(cwd: &Path, command: &str) -> String {
    match package_script(cwd, command) {
        Some(script) => format!("{command} {script}"),
        None => command.to_string(),
    }
}

/// `npm run dev`, `bun --env-file=.env run dev`, `pnpm dev`, `yarn start` → that script's text.
fn package_script(cwd: &Path, command: &str) -> Option<String> {
    let words: Vec<&str> = command.split_whitespace().collect();
    let at = words
        .iter()
        .position(|w| matches!(*w, "npm" | "pnpm" | "yarn" | "bun"))?;
    let script = words[at + 1..]
        .iter()
        .find(|w| !w.starts_with('-') && !matches!(**w, "run" | "run-script"))?;
    let text = std::fs::read_to_string(cwd.join("package.json")).ok()?;
    let scripts = &text[text.find("\"scripts\"")?..];
    let rest = &scripts[scripts.find(&format!("\"{script}\""))? + script.len() + 2..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let mut value = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(value),
            '\\' => value.extend(chars.next()),
            c => value.push(c),
        }
    }
    None
}

/// Lowercase, runs of anything but ASCII letters and digits turned into one `-`.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The folder relative to `$HOME`, slugged. When another folder already owns that name, the
/// name gets 4 hex digits of a hash of this folder's path.
fn folder_name(cwd: &Path, home: &Path, owner: impl Fn(&str) -> Option<PathBuf>) -> String {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let base = match cwd.strip_prefix(&home) {
        Ok(rel) if rel.as_os_str().is_empty() => String::from("home"),
        Ok(rel) => slug(&rel.to_string_lossy()),
        Err(_) => slug(&cwd.to_string_lossy()),
    };
    let base = if base.is_empty() {
        String::from("root")
    } else {
        base
    };
    match owner(&base) {
        Some(other) if other != cwd => {
            format!(
                "{base}-{:04x}",
                fnv1a(cwd.to_string_lossy().as_bytes()) >> 48
            )
        }
        _ => base,
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

/// How the command appears in state and messages.
fn display_command(args: &[String]) -> String {
    match args {
        [one] => one.clone(),
        _ => args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "),
    }
}

/// What `bash -c` runs. One argument is a script as is; several have `$PORT`/`$HOST` filled in
/// and are quoted back into one command.
fn script(args: &[String], port: u16, host: &str) -> String {
    match args {
        [one] => one.clone(),
        _ => args
            .iter()
            .map(|a| quote(&substitute(a, port, host)))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn substitute(arg: &str, port: u16, host: &str) -> String {
    let port = port.to_string();
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let mut matched = false;
        for (var, value) in [("PORT", port.as_str()), ("HOST", host)] {
            if let Some(tail) = after.strip_prefix(&format!("{{{var}}}")) {
                out.push_str(value);
                rest = tail;
                matched = true;
                break;
            }
            if let Some(tail) = after.strip_prefix(var)
                && !tail.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            {
                out.push_str(value);
                rest = tail;
                matched = true;
                break;
            }
        }
        if !matched {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn quote(arg: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("apps/shop"), "apps-shop");
        assert_eq!(slug("My App!!/v2_"), "my-app-v2");
        assert_eq!(slug("/srv/www/"), "srv-www");
        assert_eq!(slug("..."), "");
    }

    #[test]
    fn names_folders() {
        let home = Path::new("/nonexistent-home");
        let none = |_: &str| None;
        assert_eq!(
            folder_name(Path::new("/nonexistent-home/apps/shop"), home, none),
            "apps-shop"
        );
        assert_eq!(folder_name(home, home, none), "home");
        assert_eq!(folder_name(Path::new("/srv/app"), home, none), "srv-app");
        assert_eq!(folder_name(Path::new("/"), home, none), "root");
        let taken = |_: &str| Some(PathBuf::from("/nonexistent-home/apps-shop"));
        let name = folder_name(Path::new("/nonexistent-home/apps/shop"), home, taken);
        assert!(name.starts_with("apps-shop-") && name.len() == "apps-shop-".len() + 4);
        let same = |_: &str| Some(PathBuf::from("/nonexistent-home/apps/shop"));
        assert_eq!(
            folder_name(Path::new("/nonexistent-home/apps/shop"), home, same),
            "apps-shop"
        );
    }

    #[test]
    fn builds_scripts() {
        let one = args(&["source .venv/bin/activate && flask run"]);
        assert_eq!(script(&one, 4000, "0.0.0.0"), one[0]);
        let many = args(&["python3", "-m", "http.server", "$PORT", "--bind", "${HOST}"]);
        assert_eq!(
            script(&many, 4000, "0.0.0.0"),
            "python3 -m http.server 4000 --bind 0.0.0.0"
        );
        assert_eq!(
            display_command(&many),
            "python3 -m http.server '$PORT' --bind '${HOST}'"
        );
        let tricky = args(&["sh", "-c", "echo 'boom'; exit 3", "$PORTAL", "--x=$PORT"]);
        assert_eq!(
            script(&tricky, 4000, "h"),
            r#"sh -c 'echo '\''boom'\''; exit 3' '$PORTAL' --x=4000"#
        );
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn reads_package_scripts() {
        let dir = std::env::temp_dir().join(format!("preview-pkg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"x","scripts":{"build":"vite build","dev": "vite --open \"x\""}}"#,
        )
        .unwrap();
        assert_eq!(
            package_script(&dir, "bun --env-file=.env run dev").as_deref(),
            Some(r#"vite --open "x""#)
        );
        assert_eq!(
            package_script(&dir, "pnpm build").as_deref(),
            Some("vite build")
        );
        assert_eq!(package_script(&dir, "npm run missing"), None);
        assert_eq!(package_script(&dir, "python3 app.py"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clash_port_falls_back_to_the_command() {
        let held = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let p = held.local_addr().unwrap().port();
        let cmd = format!("python3 -m http.server {p}");
        let tail = "OSError: [Errno 48] Address already in use";
        assert_eq!(clash_port(tail, &[&cmd]), Some(p));
        assert_eq!(clash_port(&format!("EADDRINUSE :::{p}"), &[]), Some(p));
        assert_eq!(clash_port("fine", &[&cmd]), None);
    }
}
