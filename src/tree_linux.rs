//! Process table, listening sockets and port holders from `/proc`.

use crate::procs::{Listener, Proc, Table};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub fn snapshot() -> Table {
    let boot = boot_id();
    let list = pids().filter_map(|pid| read_proc(pid, &boot)).collect();
    Table::new(list)
}

pub fn start_token(pid: i32) -> Option<String> {
    read_proc(pid, &boot_id()).map(|p| p.start)
}

/// The listening TCP sockets held by these processes.
pub fn listeners(pids: &[i32]) -> Vec<Listener> {
    if pids.is_empty() {
        return Vec::new();
    }
    let sockets = listening_sockets();
    if sockets.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for &pid in pids {
        for inode in socket_inodes(pid) {
            if let Some(&(ip, port)) = sockets.get(&inode) {
                out.push(Listener { pid, ip, port });
            }
        }
    }
    out
}

/// Processes holding a listening socket on this port.
pub fn holders(port: u16) -> Vec<i32> {
    let inodes: HashSet<u64> = listening_sockets()
        .into_iter()
        .filter(|(_, (_, p))| *p == port)
        .map(|(inode, _)| inode)
        .collect();
    if inodes.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<i32> = pids()
        .filter(|pid| socket_inodes(*pid).iter().any(|i| inodes.contains(i)))
        .collect();
    out.sort_unstable();
    out
}

pub fn cmdline(pid: i32) -> String {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let text: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    if !text.is_empty() {
        return text.join(" ");
    }
    // Kernel threads and zombies have no command line; show the name instead.
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    match (stat.find('('), stat.rfind(')')) {
        (Some(a), Some(b)) if a < b => format!("[{}]", &stat[a + 1..b]),
        _ => String::from("?"),
    }
}

fn pids() -> impl Iterator<Item = i32> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
}

fn boot_id() -> String {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn read_proc(pid: i32, boot: &str) -> Option<Proc> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut proc = parse_stat(pid, &stat, boot)?;
    // A process whose main thread exited first shows `Z` while its other threads still run and
    // still hold its sockets (Node does this on every exit). It is gone only once they are.
    if proc.zombie && other_threads(pid) {
        proc.zombie = false;
    }
    Some(proc)
}

fn other_threads(pid: i32) -> bool {
    fs::read_dir(format!("/proc/{pid}/task"))
        .map(|tasks| {
            tasks
                .flatten()
                .any(|t| t.file_name().to_str() != Some(&pid.to_string()))
        })
        .unwrap_or(false)
}

pub fn parse_stat(pid: i32, stat: &str, boot: &str) -> Option<Proc> {
    // `pid (comm) state ppid pgrp session …`; comm may hold spaces and parentheses.
    let rest: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    // Field n of stat(5) is rest[n - 3]; starttime is field 22.
    Some(Proc {
        pid,
        ppid: rest.get(1)?.parse().ok()?,
        pgid: rest.get(2)?.parse().ok()?,
        sid: rest.get(3)?.parse().ok()?,
        zombie: matches!(*rest.first()?, "Z" | "X" | "x"),
        start: format!("{boot}:{}", rest.get(19)?),
    })
}

fn socket_inodes(pid: i32) -> Vec<u64> {
    let Ok(dir) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| {
            let link = fs::read_link(e.path()).ok()?;
            let link = link.to_str()?;
            link.strip_prefix("socket:[")?
                .strip_suffix(']')?
                .parse()
                .ok()
        })
        .collect()
}

/// inode → (address, port) for every TCP socket in LISTEN state.
fn listening_sockets() -> HashMap<u64, (IpAddr, u16)> {
    let mut out = HashMap::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(text) = fs::read_to_string(path) {
            parse_net_tcp(&text, &mut out);
        }
    }
    out
}

pub fn parse_net_tcp(text: &str, out: &mut HashMap<u64, (IpAddr, u16)>) {
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 || f[3] != "0A" {
            continue;
        }
        let Some((addr, port)) = f[1].split_once(':') else {
            continue;
        };
        let (Some(ip), Ok(port), Ok(inode)) = (
            parse_addr(addr),
            u16::from_str_radix(port, 16),
            f[9].parse::<u64>(),
        ) else {
            continue;
        };
        if inode != 0 {
            out.insert(inode, (ip, port));
        }
    }
}

/// The kernel prints each 32-bit word of the address in host byte order.
fn parse_addr(hex: &str) -> Option<IpAddr> {
    let word = |i: usize| -> Option<[u8; 4]> {
        Some(
            u32::from_str_radix(hex.get(i * 8..i * 8 + 8)?, 16)
                .ok()?
                .to_ne_bytes(),
        )
    };
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(word(0)?))),
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            Some(IpAddr::V6(Ipv6Addr::from(bytes)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_with_odd_names() {
        let stat = "412 (my (odd) name) S 1 412 412 0 -1 4194560 100 0 0 0 0 0 0 0 20 0 1 0 98765 1000 100";
        let p = parse_stat(412, stat, "b").unwrap();
        assert_eq!((p.ppid, p.pgid, p.sid, p.zombie), (1, 412, 412, false));
        assert_eq!(p.start, "b:98765");
    }

    #[test]
    fn parses_proc_net_tcp() {
        let v4 = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 111 1 0 100 0 0 10 0\n   1: 00000000:1435 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 222 1 0 100 0 0 10 0\n   2: 0100007F:1F91 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 333 1 0 100 0 0 10 0\n";
        let v6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 00000000000000000000000001000000:1436 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 444 1 0 100 0 0 10 0\n";
        let mut out = HashMap::new();
        parse_net_tcp(v4, &mut out);
        parse_net_tcp(v6, &mut out);
        assert_eq!(out.len(), 3);
        if cfg!(target_endian = "little") {
            assert_eq!(out[&111], ("127.0.0.1".parse().unwrap(), 8080));
            assert_eq!(out[&444], ("::1".parse().unwrap(), 5174));
        }
        assert_eq!(out[&222], ("0.0.0.0".parse().unwrap(), 5173));
    }
}
