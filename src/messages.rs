//! Failure texts and the framework hint table. Every failure ends with the action to take.

use std::path::Path;

/// Words in the command, then the bind hint for that framework. Checked in order, so the more
/// specific entry (astro, which runs vite) comes first.
const LOOPBACK_HINTS: &[(&[&str], &str)] = &[
    (&["flask"], "flask run --port $PORT --host 0.0.0.0"),
    (
        &["manage.py", "runserver"],
        "python manage.py runserver 0.0.0.0:$PORT",
    ),
    (&["uvicorn"], "uvicorn app:app --port $PORT --host 0.0.0.0"),
    (
        &["streamlit"],
        "streamlit run app.py --server.port $PORT --server.address 0.0.0.0",
    ),
    (
        &["gradio"],
        "GRADIO_SERVER_NAME=0.0.0.0 GRADIO_SERVER_PORT=$PORT",
    ),
    (&["astro"], "astro dev --host 0.0.0.0 --port $PORT"),
    (&["vite"], "vite --host 0.0.0.0 --port $PORT"),
    (&["ng"], "ng serve --host 0.0.0.0 --port $PORT"),
    (&["webpack"], "--host 0.0.0.0 --port $PORT"),
    (&["rails"], "rails server -b 0.0.0.0 -p $PORT"),
];

const LOOPBACK_DEFAULT: &str = "pass its host flag as 0.0.0.0 and its port flag as $PORT";

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || "/'\"=;&|()`,<>".contains(c))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect()
}

fn has_word(words: &[String], key: &str) -> bool {
    words.iter().any(|w| {
        w == key
            || w.strip_prefix(key)
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

/// `command` is the command text (with any package.json script it runs); `log` is its output.
pub fn loopback_hint(command: &str, log: &str) -> &'static str {
    let words = words(command);
    let gradio_app = words.iter().any(|w| w.starts_with("python"))
        && log.to_ascii_lowercase().contains("gradio");
    for (keys, hint) in LOOPBACK_HINTS {
        if keys.iter().all(|k| has_word(&words, k)) || (keys == &["gradio"] && gradio_app) {
            return hint;
        }
    }
    LOOPBACK_DEFAULT
}

/// Removes terminal color codes and indents each line, for log tails inside messages.
fn quote_tail(tail: &str) -> String {
    let mut clean = String::with_capacity(tail.len());
    let mut chars = tail.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
            continue;
        }
        clean.push(c);
    }
    if clean.trim().is_empty() {
        return String::from("  (the log is empty)");
    }
    clean
        .lines()
        .map(|l| format!("  {}", l.trim_end_matches('\r')))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn exited(command: &str, status: &str, log: &Path, tail: &str) -> String {
    format!(
        "`{command}` exited ({status}) before listening on a port. Last lines of {}:\n{}",
        log.display(),
        quote_tail(tail)
    )
}

pub fn exited_action(success: bool) -> &'static str {
    if success {
        "If it starts its server in the background (pm2, docker compose up -d, a daemon flag), \
         run it in the foreground instead, then run the same preview run again."
    } else {
        "Fix the error above, then run the same preview run again."
    }
}

pub fn held_by_preview(port: u16, name: &str, pid: i32, cmd: &str) -> String {
    format!(
        "Port {port} is held by preview {name} (pid {pid}: {cmd}). Stop it with preview stop {name}, \
         or free the port with preview stop --port {port}."
    )
}

pub fn held_by_pid(port: u16, pid: i32, cmd: &str) -> String {
    format!("Port {port} is held by pid {pid} ({cmd}). Free it with preview stop --port {port}.")
}

pub fn held_by_unseen(port: u16) -> String {
    format!(
        "Port {port} is in use by a process preview cannot see, probably another user's. \
         Use another port, or free it as that user."
    )
}

pub fn loopback(port: u16, addrs: &str, hint: &str) -> String {
    format!(
        "Port {port} listens only on {addrs}, which a preview cannot reach. Start the app bound to \
         0.0.0.0, for example {hint}, then run the same preview run again."
    )
}

pub fn still_starting(command: &str, secs: f64, log: &Path, tail: &str, name: &str) -> String {
    format!(
        "`{command}` is still starting after {secs} s and listens on no port yet. Last lines of {}:\n{}\n\
         Check again with preview list, or read preview logs {name}.",
        log.display(),
        quote_tail(tail)
    )
}

pub fn unknown_name(name: &str) -> String {
    format!("No preview named {name}. `preview list` shows them.")
}

/// The last log line that reports a port clash.
pub fn clash_line(tail: &str) -> Option<&str> {
    tail.lines().rev().find(|l| {
        let l = l.to_ascii_lowercase();
        l.contains("eaddrinuse") || l.contains("already in use")
    })
}

/// Numbers of 2–5 digits that could be ports, in order, skipping digits glued to letters
/// (`python3`, `v18`).
pub fn port_numbers(text: &str) -> Vec<u16> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let glued = (start > 0 && bytes[start - 1].is_ascii_alphabetic())
            || (i < bytes.len() && bytes[i].is_ascii_alphabetic());
        if (2..=5).contains(&(i - start))
            && !glued
            && let Ok(n) = text[start..i].parse::<u16>()
        {
            out.push(n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_by_word() {
        assert_eq!(
            loopback_hint("source .venv/bin/activate && flask run", ""),
            "flask run --port $PORT --host 0.0.0.0"
        );
        assert_eq!(
            loopback_hint("python manage.py runserver", ""),
            "python manage.py runserver 0.0.0.0:$PORT"
        );
        assert_eq!(
            loopback_hint("bun run dev vite", ""),
            "vite --host 0.0.0.0 --port $PORT"
        );
        assert_eq!(
            loopback_hint("npm run dev astro dev", ""),
            "astro dev --host 0.0.0.0 --port $PORT"
        );
        assert_eq!(
            loopback_hint("webpack-dev-server", ""),
            "--host 0.0.0.0 --port $PORT"
        );
        assert_eq!(
            loopback_hint("python3 app.py", "Running on local URL: (gradio)"),
            "GRADIO_SERVER_NAME=0.0.0.0 GRADIO_SERVER_PORT=$PORT"
        );
        assert_eq!(loopback_hint("vite.config.ts", ""), LOOPBACK_DEFAULT);
        assert_eq!(loopback_hint("ngrok", ""), LOOPBACK_DEFAULT);
        assert_eq!(
            loopback_hint("python3 -m http.server", ""),
            LOOPBACK_DEFAULT
        );
    }

    #[test]
    fn finds_clash_ports() {
        let tail = "VITE v5\nError: listen EADDRINUSE: address already in use :::5173\n    at x";
        assert_eq!(
            clash_line(tail),
            Some("Error: listen EADDRINUSE: address already in use :::5173")
        );
        assert_eq!(port_numbers(clash_line(tail).unwrap()), vec![5173]);
        assert_eq!(
            port_numbers("OSError: [Errno 48] Address already in use"),
            vec![48]
        );
        assert_eq!(
            port_numbers("python3 -m http.server 8765 v18 123456"),
            vec![8765]
        );
        assert_eq!(clash_line("all good"), None);
    }

    #[test]
    fn quotes_tails() {
        assert_eq!(quote_tail("\u{1b}[32mok\u{1b}[0m\r\nnext"), "  ok\n  next");
        assert_eq!(quote_tail(""), "  (the log is empty)");
    }
}
