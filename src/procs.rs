//! A snapshot of the process table and the tree walks `run` and `stop` need. The platform files
//! (`tree_linux.rs`, `tree_macos.rs`) fill it in.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    /// Session id, 0 where the platform does not report it.
    pub sid: i32,
    pub zombie: bool,
    /// Opaque start time; equal for the same process across snapshots.
    pub start: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Listener {
    pub pid: i32,
    pub ip: IpAddr,
    pub port: u16,
}

impl Listener {
    pub fn is_loopback(&self) -> bool {
        match self.ip {
            IpAddr::V4(ip) => ip.is_loopback(),
            IpAddr::V6(ip) => {
                ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
            }
        }
    }
}

#[derive(Default)]
pub struct Table {
    procs: HashMap<i32, Proc>,
    children: HashMap<i32, Vec<i32>>,
}

impl Table {
    pub fn new(list: Vec<Proc>) -> Self {
        let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
        for p in &list {
            children.entry(p.ppid).or_default().push(p.pid);
        }
        let procs = list.into_iter().map(|p| (p.pid, p)).collect();
        Table { procs, children }
    }

    pub fn get(&self, pid: i32) -> Option<&Proc> {
        self.procs.get(&pid)
    }

    /// Present and not a zombie. A box without an init process keeps zombies around.
    pub fn alive(&self, pid: i32) -> bool {
        self.get(pid).is_some_and(|p| !p.zombie)
    }

    pub fn children(&self, pid: i32) -> Vec<i32> {
        let mut out: Vec<i32> = self
            .children
            .get(&pid)
            .map(|c| c.iter().copied().filter(|c| self.alive(*c)).collect())
            .unwrap_or_default();
        out.sort_unstable();
        out
    }

    /// Living members of a process group.
    pub fn group(&self, pgid: i32) -> Vec<i32> {
        let mut out: Vec<i32> = self
            .procs
            .values()
            .filter(|p| p.pgid == pgid && !p.zombie)
            .map(|p| p.pid)
            .collect();
        out.sort_unstable();
        out
    }

    /// Everything a preview started: its session and group, and their descendants.
    pub fn tree(&self, leader: i32) -> Vec<i32> {
        let roots = self
            .procs
            .values()
            .filter(|p| p.pgid == leader || (p.sid > 0 && p.sid == leader))
            .map(|p| p.pid);
        self.walk(roots)
    }

    /// A process and its descendants.
    pub fn subtree(&self, root: i32) -> Vec<i32> {
        self.walk(std::iter::once(root))
    }

    fn walk(&self, roots: impl Iterator<Item = i32>) -> Vec<i32> {
        let mut seen: HashSet<i32> = HashSet::new();
        let mut queue: VecDeque<i32> = roots.collect();
        while let Some(pid) = queue.pop_front() {
            if !self.procs.contains_key(&pid) || !seen.insert(pid) {
                continue;
            }
            if let Some(kids) = self.children.get(&pid) {
                queue.extend(kids.iter().copied());
            }
        }
        let mut out: Vec<i32> = seen.into_iter().filter(|p| self.alive(*p)).collect();
        out.sort_unstable();
        out
    }

    /// Parent, grandparent, … up to (not including) pid 0.
    pub fn ancestors(&self, pid: i32) -> Vec<i32> {
        let mut out = Vec::new();
        let mut cur = pid;
        while let Some(p) = self.get(cur) {
            if p.ppid <= 0 || p.ppid == cur || out.contains(&p.ppid) {
                break;
            }
            out.push(p.ppid);
            cur = p.ppid;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: i32, ppid: i32, pgid: i32) -> Proc {
        Proc {
            pid,
            ppid,
            pgid,
            sid: 0,
            zombie: false,
            start: String::new(),
        }
    }

    #[test]
    fn tree_covers_group_session_and_descendants() {
        let mut zombie = p(14, 10, 10);
        zombie.zombie = true;
        let mut session = p(15, 1, 15);
        session.sid = 10;
        let t = Table::new(vec![
            p(1, 0, 1),
            p(10, 1, 10),
            p(11, 10, 10),
            p(12, 11, 12), // left the group, still a descendant
            p(13, 12, 12),
            zombie,
            session,
            p(20, 1, 20),
        ]);
        assert_eq!(t.tree(10), vec![10, 11, 12, 13, 15]);
        assert_eq!(t.subtree(11), vec![11, 12, 13]);
        assert_eq!(t.group(10), vec![10, 11]);
        assert_eq!(t.ancestors(13), vec![12, 11, 10, 1]);
        assert_eq!(t.children(10), vec![11]);
        assert!(!t.alive(14));
    }
}
