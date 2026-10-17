//! Process table, listening sockets and port holders from `ps` and `lsof`.

use crate::procs::{Listener, Proc, Table};
use std::net::{IpAddr, Ipv4Addr};
use std::process::{Command, Stdio};

fn tool(path: &str, args: &[&str]) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let program = if std::path::Path::new(path).exists() {
        path
    } else {
        name
    };
    Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn ps(args: &[&str]) -> String {
    tool("/bin/ps", args)
}

fn lsof(args: &[&str]) -> String {
    tool("/usr/sbin/lsof", args)
}

pub fn snapshot() -> Table {
    let out = ps(&["-A", "-o", "pid=,ppid=,pgid=,stat=,lstart="]);
    Table::new(out.lines().filter_map(parse_ps_line).collect())
}

pub fn parse_ps_line(line: &str) -> Option<Proc> {
    let mut f = line.split_whitespace();
    let pid = f.next()?.parse().ok()?;
    let ppid = f.next()?.parse().ok()?;
    let pgid = f.next()?.parse().ok()?;
    let stat = f.next()?;
    Some(Proc {
        pid,
        ppid,
        pgid,
        sid: 0,
        zombie: stat.starts_with('Z'),
        start: f.collect::<Vec<_>>().join(" "),
    })
}

pub fn start_token(pid: i32) -> Option<String> {
    let out = ps(&["-o", "lstart=", "-p", &pid.to_string()]);
    let token = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (!token.is_empty()).then_some(token)
}

/// The listening TCP sockets held by these processes.
pub fn listeners(pids: &[i32]) -> Vec<Listener> {
    if pids.is_empty() {
        return Vec::new();
    }
    let list: Vec<String> = pids.iter().map(|p| p.to_string()).collect();
    let out = lsof(&[
        "-nP",
        "-a",
        "-p",
        &list.join(","),
        "-iTCP",
        "-sTCP:LISTEN",
        "-Fpn",
    ]);
    parse_lsof(&out)
}

pub fn parse_lsof(out: &str) -> Vec<Listener> {
    let mut found = Vec::new();
    let mut pid = 0;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().unwrap_or(0);
        } else if let Some(name) = line.strip_prefix('n')
            && let Some((ip, port)) = parse_name(name)
            && pid > 0
        {
            found.push(Listener { pid, ip, port });
        }
    }
    found
}

/// `*:5173`, `127.0.0.1:5173`, `[::1]:5173`, `[fe80::1%lo0]:5173`.
fn parse_name(name: &str) -> Option<(IpAddr, u16)> {
    let (host, port) = name.rsplit_once(':')?;
    let port = port.parse().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = host.split('%').next()?;
    let ip = if host == "*" {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        host.parse().ok()?
    };
    Some((ip, port))
}

/// Processes holding a listening socket on this port.
pub fn holders(port: u16) -> Vec<i32> {
    let out = lsof(&["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"]);
    let mut pids: Vec<i32> = out.lines().filter_map(|l| l.trim().parse().ok()).collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

pub fn cmdline(pid: i32) -> String {
    let out = ps(&["-o", "command=", "-p", &pid.to_string()]);
    let out = out.trim();
    if out.is_empty() {
        String::from("?")
    } else {
        out.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps() {
        let p = parse_ps_line("  412     1   412 Ss   Sat Oct 17 21:42:56 2026").unwrap();
        assert_eq!((p.pid, p.ppid, p.pgid, p.zombie), (412, 1, 412, false));
        assert_eq!(p.start, "Sat Oct 17 21:42:56 2026");
        assert!(
            parse_ps_line(" 9 1 9 Z+ Sat Oct 17 21:42:56 2026")
                .unwrap()
                .zombie
        );
    }

    #[test]
    fn parses_lsof() {
        let out = "p412\nf20\nn*:5173\nf21\nn127.0.0.1:24678\np500\nf3\nn[::1]:3000\nn[fe80::1%lo0]:3001\n";
        let l = parse_lsof(out);
        assert_eq!(l.len(), 4);
        assert_eq!(
            (l[0].pid, l[0].port, l[0].is_loopback()),
            (412, 5173, false)
        );
        assert_eq!((l[1].port, l[1].is_loopback()), (24678, true));
        assert_eq!((l[2].pid, l[2].port, l[2].is_loopback()), (500, 3000, true));
        assert_eq!(l[3].port, 3001);
    }
}
