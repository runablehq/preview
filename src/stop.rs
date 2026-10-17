//! `preview stop`, `preview list` and `preview logs`, and the port-holder lookups `run` shares.

use crate::json::{self, Object};
use crate::messages;
use crate::ports;
use crate::procs::Table;
use crate::signals;
use crate::state::{self, State};
use crate::tree;
use std::collections::HashSet;

/// The living processes of a preview, or none when its pid now belongs to someone else.
pub fn live_tree(st: &State, table: &Table) -> Vec<i32> {
    if st.pid <= 1 {
        return Vec::new();
    }
    // A pid is not reused while its group exists, so a dead leader with a living group is still
    // ours. A living leader must have started when ours did.
    if let Some(leader) = table.get(st.pid)
        && !leader.zombie
        && !st.leader_start.is_empty()
        && leader.start != st.leader_start
    {
        return Vec::new();
    }
    table.tree(st.pid)
}

/// Stops a preview's whole tree. Returns false if something survived SIGKILL.
pub fn stop_state(st: &State) -> bool {
    let members = live_tree(st, &tree::snapshot());
    members.is_empty() || signals::terminate(Some(st.pid), &members)
}

pub struct Holder {
    pub pid: i32,
    pub cmd: String,
    pub preview: Option<State>,
}

fn shorten(cmd: String) -> String {
    const MAX: usize = 120;
    if cmd.chars().count() <= MAX {
        return cmd;
    }
    let cut: String = cmd.chars().take(MAX - 1).collect();
    format!("{cut}…")
}

pub fn holders(port: u16) -> Vec<Holder> {
    let pids = tree::holders(port);
    if pids.is_empty() {
        return Vec::new();
    }
    let table = tree::snapshot();
    let previews: Vec<(State, Vec<i32>)> = state::all()
        .into_iter()
        .map(|st| {
            let members = live_tree(&st, &table);
            (st, members)
        })
        .filter(|(_, members)| !members.is_empty())
        .collect();
    pids.into_iter()
        .map(|pid| Holder {
            pid,
            cmd: shorten(tree::cmdline(pid)),
            preview: previews
                .iter()
                .find(|(_, members)| members.contains(&pid))
                .map(|(st, _)| st.clone()),
        })
        .collect()
}

/// One line naming what holds the port and how to free it.
pub fn holder_message(port: u16) -> String {
    match holders(port).into_iter().next() {
        Some(Holder {
            pid,
            cmd,
            preview: Some(st),
        }) => messages::held_by_preview(port, &st.name, pid, &cmd),
        Some(Holder { pid, cmd, .. }) => messages::held_by_pid(port, pid, &cmd),
        None => messages::held_by_unseen(port),
    }
}

pub fn stop_name(name: &str) -> u8 {
    let _lock = match state::lock(name) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("preview: cannot lock {name}: {e}");
            return 1;
        }
    };
    let Some(st) = state::load(name) else {
        eprintln!("{}", messages::unknown_name(name));
        return 1;
    };
    if live_tree(&st, &tree::snapshot()).is_empty() {
        json::print(&format!("Preview {name} is not running."));
        return 0;
    }
    if !stop_state(&st) {
        eprintln!(
            "Preview {name} (process group {}) did not exit after SIGKILL. Check it with ps -g {}.",
            st.pid, st.pid
        );
        return 1;
    }
    json::print(&format!("Stopped preview {name}."));
    0
}

pub fn stop_port(port: u16) -> u8 {
    let found = holders(port);
    if found.is_empty() {
        if ports::is_free(port) {
            json::print(&format!("Nothing listens on port {port}."));
            return 0;
        }
        eprintln!("{}", messages::held_by_unseen(port));
        return 1;
    }
    let mut stopped_previews: HashSet<String> = HashSet::new();
    let mut stopped_pids: HashSet<i32> = HashSet::new();
    for holder in found {
        if stopped_pids.contains(&holder.pid) {
            continue;
        }
        if let Some(st) = holder.preview {
            if !stopped_previews.insert(st.name.clone()) {
                continue;
            }
            let _lock = state::lock(&st.name);
            let st = state::load(&st.name).unwrap_or(st);
            stop_state(&st);
            json::print(&format!("Stopped preview {}.", st.name));
            continue;
        }
        let table = tree::snapshot();
        let (group, members) = loose_target(&table, holder.pid);
        signals::terminate(group, &members);
        stopped_pids.extend(members);
        match group {
            Some(g) => json::print(&format!(
                "Stopped process group {g} (pid {}: {}).",
                holder.pid, holder.cmd
            )),
            None => json::print(&format!("Stopped pid {} ({}).", holder.pid, holder.cmd)),
        }
    }
    if !ports::is_free(port) {
        eprintln!("{}", holder_message(port));
        return 1;
    }
    0
}

/// What to stop for a listener that no preview started. Climbs from the listener through parents
/// in its group that have no other children (`npm` → `sh` → `vite`). When that reaches the group
/// leader and covers the whole group, the group is stopped; otherwise only that subtree, so a
/// server started from an agent's shell never takes the agent down with it.
fn loose_target(table: &Table, pid: i32) -> (Option<i32>, Vec<i32>) {
    let Some(proc) = table.get(pid) else {
        return (None, vec![pid]);
    };
    let group = proc.pgid;
    let mut top = pid;
    while let Some(cur) = table.get(top) {
        let parent = cur.ppid;
        if parent <= 1 || table.get(parent).is_none_or(|p| p.pgid != group) {
            break;
        }
        if table.children(parent) != [top] {
            break;
        }
        top = parent;
    }
    let subtree = table.subtree(top);
    let whole_group = top == group && table.group(group).iter().all(|p| subtree.contains(p));
    (whole_group.then_some(group), subtree)
}

pub fn list() -> u8 {
    let table = tree::snapshot();
    let states = state::all();
    let trees: Vec<Vec<i32>> = states.iter().map(|st| live_tree(st, &table)).collect();
    let all: Vec<i32> = trees.iter().flatten().copied().collect();
    let listeners = tree::listeners(&all);
    let items: Vec<String> = states
        .iter()
        .zip(&trees)
        .map(|(st, members)| {
            let mut ports: Vec<u16> = listeners
                .iter()
                .filter(|l| members.contains(&l.pid))
                .map(|l| l.port)
                .collect();
            ports.sort_unstable();
            ports.dedup();
            let running = !members.is_empty();
            Object::new()
                .str("name", &st.name)
                .bool("running", running)
                .raw("ports", &json::numbers(&ports))
                .num("port", st.port)
                .num("pid", if running { st.pid } else { 0 })
                .str("cwd", &st.cwd.to_string_lossy())
                .str("command", &st.command)
                .str("log", &state::log_path(&st.name).to_string_lossy())
                .finish()
        })
        .collect();
    json::print(&format!("[{}]", items.join(",")));
    0
}

pub fn logs(name: &str, lines: usize) -> u8 {
    if state::load(name).is_none() {
        eprintln!("{}", messages::unknown_name(name));
        return 1;
    }
    let tail = state::tail(&state::log_path(name), lines);
    if !tail.is_empty() {
        json::print(&tail);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::Proc;

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
    fn stops_a_terminal_job_as_a_group() {
        // zsh(100) → npm(200, leader) → sh(201) → vite(202) → esbuild(203)
        let t = Table::new(vec![
            p(100, 1, 100),
            p(200, 100, 200),
            p(201, 200, 200),
            p(202, 201, 200),
            p(203, 202, 200),
        ]);
        assert_eq!(loose_target(&t, 202), (Some(200), vec![200, 201, 202, 203]));
    }

    #[test]
    fn spares_an_agent_sharing_the_group() {
        // agent(300, leader) has an MCP server(301) and a tool shell(302) → npm(303) → vite(304)
        let t = Table::new(vec![
            p(300, 1, 300),
            p(301, 300, 300),
            p(302, 300, 300),
            p(303, 302, 300),
            p(304, 303, 300),
        ]);
        assert_eq!(loose_target(&t, 304), (None, vec![302, 303, 304]));
    }

    #[test]
    fn rejects_a_reused_pid() {
        let mut leader = p(400, 1, 400);
        leader.start = "new".into();
        let t = Table::new(vec![leader, p(401, 400, 400)]);
        let mut st = State {
            pid: 400,
            leader_start: "old".into(),
            ..State::default()
        };
        assert!(live_tree(&st, &t).is_empty());
        st.leader_start = "new".into();
        assert_eq!(live_tree(&st, &t), vec![400, 401]);
    }
}
