//! preview: run any web app on a free port, restart it cleanly, and report where it listens.

mod json;
mod messages;
mod ports;
mod procs;
mod run;
mod signals;
mod state;
mod stop;

#[cfg(target_os = "linux")]
#[path = "tree_linux.rs"]
mod tree;
#[cfg(target_os = "macos")]
#[path = "tree_macos.rs"]
mod tree;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("preview runs on Linux and macOS only");

use std::process::ExitCode;

const USAGE: &str = "\
Run any web app on a free port, restart it cleanly, and report where it listens.

Usage:
  preview run [--name <name>] [--port <port>] [--wait <seconds>] [--] <command>...
  preview stop <name>
  preview stop --port <port>
  preview list
  preview logs <name> [--lines <count>]
  preview --version

preview run starts <command> from the current folder with PORT set to a free port, waits until
it listens, and prints one JSON line: {\"name\",\"port\",\"ports\",\"pid\",\"url\",\"log\"}.
Run the same command again to restart it. A single argument runs as a bash script; with several,
$PORT and $HOST inside them are filled in.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    match dispatch(&args) {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::from(1)
        }
    }
}

fn dispatch(args: &[String]) -> Result<u8, String> {
    let Some(cmd) = args.first() else {
        return Err(USAGE.to_string());
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "run" => run_options(rest).map(run::run),
        "stop" => stop(rest),
        "list" | "ls" => {
            no_more(rest)?;
            Ok(stop::list())
        }
        "logs" | "log" => logs(rest),
        "--version" | "-V" | "version" => {
            json::print(&format!("preview {}", env!("CARGO_PKG_VERSION")));
            Ok(0)
        }
        "--help" | "-h" | "help" => {
            json::print(USAGE);
            Ok(0)
        }
        other => Err(format!(
            "preview: unknown command {other}. Run preview --help for the commands."
        )),
    }
}

/// Splits `--flag=value` and `--flag value`; `at` points at the flag and moves past its value.
fn value(args: &[String], at: &mut usize) -> Result<(String, String), String> {
    let arg = &args[*at];
    if let Some((flag, v)) = arg.split_once('=')
        && flag.starts_with("--")
    {
        return Ok((flag.to_string(), v.to_string()));
    }
    *at += 1;
    match args.get(*at) {
        Some(v) => Ok((arg.clone(), v.clone())),
        None => Err(format!("preview: {arg} needs a value.")),
    }
}

fn number<T: std::str::FromStr>(flag: &str, v: &str) -> Result<T, String> {
    v.parse()
        .map_err(|_| format!("preview: {flag} takes a number, not {v}."))
}

fn run_options(args: &[String]) -> Result<run::Options, String> {
    let mut opts = run::Options {
        name: None,
        port: None,
        wait: 45.0,
        command: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            i += 1;
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            break;
        }
        let flag = arg.split('=').next().unwrap_or(arg);
        match flag {
            "--name" | "--port" | "--wait" => {
                let (flag, v) = value(args, &mut i)?;
                match flag.as_str() {
                    "--name" => opts.name = Some(v),
                    "--port" => opts.port = Some(number::<u16>(&flag, &v)?).filter(|p| *p > 0),
                    _ => {
                        let secs: f64 = number(&flag, &v)?;
                        if !secs.is_finite() || secs < 0.0 {
                            return Err(format!("preview: --wait takes seconds, not {v}."));
                        }
                        opts.wait = secs;
                    }
                }
            }
            "-h" | "--help" => {
                json::print(USAGE);
                std::process::exit(0);
            }
            _ => {
                return Err(format!(
                    "preview: unknown option {arg}. Put -- before a command that starts with -."
                ));
            }
        }
        i += 1;
    }
    opts.command = args[i..].to_vec();
    if opts.command.is_empty() {
        return Err(String::from(
            "preview: run needs a command, for example preview run -- npm run dev",
        ));
    }
    Ok(opts)
}

fn stop(args: &[String]) -> Result<u8, String> {
    match args {
        [flag, ..] if flag == "--port" || flag.starts_with("--port=") => {
            let mut i = 0;
            let (flag, v) = value(args, &mut i)?;
            no_more(&args[i + 1..])?;
            Ok(stop::stop_port(number(&flag, &v)?))
        }
        [name] if !name.starts_with('-') => Ok(stop::stop_name(&name_arg(name)?)),
        _ => Err(String::from(
            "preview: stop takes a name or --port <port>. preview list shows the names.",
        )),
    }
}

fn logs(args: &[String]) -> Result<u8, String> {
    let mut name = None;
    let mut lines = 40;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--lines" || arg.starts_with("--lines=") || arg == "-n" {
            let (flag, v) = value(args, &mut i)?;
            lines = number(&flag, &v)?;
        } else if name.is_none() && !arg.starts_with('-') {
            name = Some(name_arg(arg)?);
        } else {
            return Err(format!("preview: unexpected {arg}."));
        }
        i += 1;
    }
    match name {
        Some(name) => Ok(stop::logs(&name, lines)),
        None => Err(String::from(
            "preview: logs takes a name. preview list shows the names.",
        )),
    }
}

/// Names are looked up the way `run` makes them, so `preview stop MyApp` finds `myapp`.
fn name_arg(name: &str) -> Result<String, String> {
    let slug = run::slug(name);
    if slug.is_empty() {
        return Err(messages::unknown_name(name));
    }
    Ok(slug)
}

fn no_more(args: &[String]) -> Result<(), String> {
    match args.first() {
        Some(extra) => Err(format!("preview: unexpected {extra}.")),
        None => Ok(()),
    }
}
