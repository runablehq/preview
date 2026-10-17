//! New sessions, signals, and stopping a process tree: SIGTERM, up to 2 s, SIGKILL, up to 1 s.

use crate::tree;
use std::collections::HashSet;
use std::io;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// The highest descriptor `child_setup` needs to look at. Read before forking.
pub fn fd_limit() -> i32 {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes only into `limit`.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return 1024;
    }
    limit.rlim_cur.clamp(256, 65536) as i32
}

/// Runs in the child between fork and exec: a new session, so the app survives the caller and
/// stop can signal its whole group; and every inherited descriptor above stderr marked
/// close-on-exec, so the app never holds a pipe its caller waits on.
pub fn child_setup(fd_limit: i32) -> io::Result<()> {
    // SAFETY: setsid and fcntl are async-signal-safe and touch no memory of ours.
    unsafe {
        if libc::setsid() < 0 {
            return Err(io::Error::last_os_error());
        }
        #[cfg(target_os = "linux")]
        {
            const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
            if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, CLOSE_RANGE_CLOEXEC) == 0 {
                return Ok(());
            }
        }
        for fd in 3..fd_limit {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
    Ok(())
}

pub fn own_group() -> i32 {
    // SAFETY: getpgrp cannot fail and touches no memory.
    unsafe { libc::getpgrp() }
}

fn kill(pid: i32, sig: i32) {
    // SAFETY: kill only sends a signal. Callers never pass 0, -1 or our own group.
    unsafe {
        libc::kill(pid, sig);
    }
}

/// Stops a process group and these processes, never touching this process, its ancestors, or
/// the caller's group. Returns whether everything is gone.
pub fn terminate(group: Option<i32>, pids: &[i32]) -> bool {
    let table = tree::snapshot();
    let me = std::process::id() as i32;
    let mut protected: HashSet<i32> = table.ancestors(me).into_iter().collect();
    protected.insert(me);
    let group = group.filter(|&g| {
        g > 1 && g != own_group() && !table.group(g).iter().any(|p| protected.contains(p))
    });
    let pids: Vec<i32> = pids
        .iter()
        .copied()
        .filter(|p| *p > 1 && !protected.contains(p))
        .collect();

    let remaining = || -> Vec<i32> {
        let table = tree::snapshot();
        let mut left: Vec<i32> = pids.iter().copied().filter(|p| table.alive(*p)).collect();
        if let Some(g) = group {
            left.extend(table.tree(g));
        }
        left.retain(|p| !protected.contains(p));
        left.sort_unstable();
        left.dedup();
        left
    };
    let send = |sig: i32, targets: &[i32]| {
        if let Some(g) = group {
            kill(-g, sig);
        }
        for &pid in targets {
            kill(pid, sig);
        }
    };
    let wait = |limit: Duration| -> Vec<i32> {
        let until = Instant::now() + limit;
        loop {
            let left = remaining();
            if left.is_empty() || Instant::now() >= until {
                return left;
            }
            sleep(Duration::from_millis(50));
        }
    };

    send(libc::SIGTERM, &pids);
    let left = wait(Duration::from_secs(2));
    if left.is_empty() {
        return true;
    }
    send(libc::SIGKILL, &left);
    wait(Duration::from_secs(1)).is_empty()
}
