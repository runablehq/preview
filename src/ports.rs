//! The free-port test and the choice of the port a run is offered.

use std::collections::HashSet;
use std::io::Read;
use std::mem::size_of;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener};
use std::time::{Duration, Instant};

/// Whether nothing listens on the port on any local address.
///
/// Binds without listening, so the macOS firewall never asks about it. SO_REUSEADDR keeps
/// TIME_WAIT connections from an earlier run from making its port look taken.
pub fn is_free(port: u16) -> bool {
    [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ]
    .into_iter()
    .all(|ip| bindable(ip, port))
}

/// Polls until the port is free or the time is up. Returns whether it is free.
pub fn wait_free(port: u16, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    loop {
        if port == 0 || is_free(port) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// False only when the address is in use; an address family the box lacks counts as free.
fn bindable(ip: IpAddr, port: u16) -> bool {
    // SAFETY: plain socket calls on a descriptor this function owns and closes; the sockaddr
    // structs are zeroed and live for the duration of the bind call.
    unsafe {
        let family = if ip.is_ipv4() {
            libc::AF_INET
        } else {
            libc::AF_INET6
        };
        let fd = libc::socket(family, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return true;
        }
        let one: libc::c_int = 1;
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &one as *const _ as *const libc::c_void,
            size_of::<libc::c_int>() as libc::socklen_t,
        );
        let rc = match ip {
            IpAddr::V4(v4) => {
                let mut sa: libc::sockaddr_in = std::mem::zeroed();
                #[cfg(target_os = "macos")]
                {
                    sa.sin_len = size_of::<libc::sockaddr_in>() as u8;
                }
                sa.sin_family = libc::AF_INET as libc::sa_family_t;
                sa.sin_port = port.to_be();
                sa.sin_addr = libc::in_addr {
                    s_addr: u32::from_ne_bytes(v4.octets()),
                };
                libc::bind(
                    fd,
                    &sa as *const _ as *const libc::sockaddr,
                    size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            }
            IpAddr::V6(v6) => {
                let mut sa: libc::sockaddr_in6 = std::mem::zeroed();
                #[cfg(target_os = "macos")]
                {
                    sa.sin6_len = size_of::<libc::sockaddr_in6>() as u8;
                }
                sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                sa.sin6_port = port.to_be();
                sa.sin6_addr = libc::in6_addr {
                    s6_addr: v6.octets(),
                };
                libc::bind(
                    fd,
                    &sa as *const _ as *const libc::sockaddr,
                    size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                )
            }
        };
        let err = std::io::Error::last_os_error().raw_os_error();
        libc::close(fd);
        rc == 0 || err != Some(libc::EADDRINUSE)
    }
}

/// The name's last port when free and nobody else remembers it; else a random free port in
/// 3000–9999 nobody remembers; else whatever the OS hands out.
pub fn choose(last: Option<u16>, remembered: &HashSet<u16>) -> u16 {
    if let Some(p) = last.filter(|p| *p > 0 && !remembered.contains(p) && is_free(*p)) {
        return p;
    }
    let mut seed = random_seed();
    for _ in 0..200 {
        // xorshift64
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let p = 3000 + (seed % 7000) as u16;
        if !remembered.contains(&p) && is_free(p) {
            return p;
        }
    }
    for _ in 0..20 {
        if let Some(p) = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .ok()
            .filter(|p| is_free(*p))
        {
            return p;
        }
    }
    0
}

fn random_seed() -> u64 {
    let mut buf = [0u8; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
    {
        let seed = u64::from_ne_bytes(buf);
        if seed != 0 {
            return seed;
        }
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    nanos ^ ((std::process::id() as u64) << 32) | 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_listeners_on_every_address() {
        for host in ["127.0.0.1", "0.0.0.0", "::1", "::"] {
            let Ok(l) = TcpListener::bind((host, 0)) else {
                continue; // no IPv6 on this box
            };
            let port = l.local_addr().unwrap().port();
            assert!(!is_free(port), "listener on {host}:{port} not seen");
            drop(l);
            assert!(is_free(port), "{host}:{port} still busy after close");
        }
    }

    #[test]
    fn avoids_remembered_ports() {
        let p = choose(None, &HashSet::new());
        assert!((3000..10000).contains(&p));
        let mut remembered = HashSet::new();
        remembered.insert(p);
        assert_ne!(choose(Some(p), &remembered), p);
        assert_eq!(choose(Some(p), &HashSet::new()), p);
    }
}
