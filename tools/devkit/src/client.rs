/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use crate::host::{colorize, socket_dir};

pub struct HostInfo {
    pub socket: PathBuf,
    pub pid: String,
    pub kind: String,
    pub label: String,
}

/// Running hosts; sockets nobody answers on are removed.
pub fn hosts() -> Vec<HostInfo> {
    let Ok(entries) = std::fs::read_dir(socket_dir()) else { return Vec::new() };
    let mut out = Vec::new();
    for path in entries.filter_map(|e| e.ok()).map(|e| e.path()) {
        if path.extension().is_none_or(|e| e != "sock") {
            continue;
        }
        match query(&path) {
            Some(info) => out.push(info),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    out.sort_by(|a, b| a.pid.cmp(&b.pid));
    out
}

fn query(path: &Path) -> Option<HostInfo> {
    let mut conn = Conn::open(path).ok()?;
    let reply = conn.run("host", false).ok()?;
    let mut parts = reply.splitn(5, ' ');
    let (_, _, pid, kind) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    Some(HostInfo { socket: path.to_path_buf(), pid: pid.into(), kind: kind.into(), label: parts.next().unwrap_or("").into() })
}

/// The host matching `selector` (pid or a substring of its label or kind), or
/// the only one running.
pub fn select(selector: Option<&str>) -> Result<HostInfo, String> {
    let mut found: Vec<HostInfo> = hosts()
        .into_iter()
        .filter(|h| selector.is_none_or(|s| h.pid == s || h.label.contains(s) || h.kind.contains(s)))
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(match selector {
            Some(s) => format!("no running host matches {s}"),
            None => "no running host; start one with `devkit run` or `devkit monitor`".into(),
        }),
        _ => Err(format!("several hosts match; pick one with -t:\n{}", list(&found))),
    }
}

pub fn list(hosts: &[HostInfo]) -> String {
    hosts.iter().map(|h| format!("  {}  {}  {}", h.pid, h.kind, h.label)).collect::<Vec<_>>().join("\n")
}

pub struct Conn {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Conn {
    pub fn open(path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self { stream, reader })
    }

    /// Sends one script line and returns its `#` reply without the `#`; log
    /// lines arriving meanwhile are printed when `print_log`.
    pub fn run(&mut self, line: &str, print_log: bool) -> io::Result<String> {
        writeln!(self.stream, "{line}")?;
        loop {
            let mut buf = String::new();
            if self.reader.read_line(&mut buf)? == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "host closed the connection"));
            }
            let buf = buf.trim_end_matches(['\r', '\n']);
            match buf.strip_prefix('#') {
                Some(reply) => return Ok(reply.to_string()),
                None if print_log => println!("{buf}"),
                None => {}
            }
        }
    }

    /// Prints the host's output until it closes the connection, sending this
    /// process's stdin as commands. EOF on stdin leaves the output running.
    pub fn attach(mut self) -> Result<(), String> {
        let mut writer = self.stream.try_clone().map_err(|e| e.to_string())?;
        writeln!(writer, "log on").map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            for line in io::stdin().lock().lines().map_while(Result::ok) {
                if writeln!(writer, "{}", absolutize(&line)).is_err() {
                    break;
                }
            }
        });
        let color = io::stdout().is_terminal();
        loop {
            let mut buf = String::new();
            if self.reader.read_line(&mut buf).map_err(|e| e.to_string())? == 0 {
                return Ok(());
            }
            let line = buf.trim_end_matches(['\r', '\n']);
            if line == "#OK log" {
                continue;
            }
            println!("{}", if color { colorize(line) } else { line.into() });
        }
    }
}

/// Makes a `capture` path absolute against this process, since the host
/// writes the file from its own working directory.
pub fn absolutize(line: &str) -> String {
    let mut parts: Vec<String> = line.split_whitespace().map(String::from).collect();
    if parts.first().is_some_and(|c| c == "capture") && parts.len() > 1 {
        let path = Path::new(&parts[1]);
        if path.is_relative() {
            if let Ok(cwd) = std::env::current_dir() {
                parts[1] = cwd.join(path).display().to_string();
            }
        }
    }
    parts.join(" ")
}

/// Runs `lines` in order, stopping at the first error.
pub fn run_lines(host: &HostInfo, lines: impl IntoIterator<Item = String>, print_log: bool) -> Result<(), String> {
    let mut conn = Conn::open(&host.socket).map_err(|e| e.to_string())?;
    if print_log {
        conn.run("log on", false).map_err(|e| e.to_string())?;
    }
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let reply = conn.run(&absolutize(line), print_log).map_err(|e| e.to_string())?;
        println!("{reply}");
        if reply.starts_with("ERR") {
            return Err(format!("{line} failed"));
        }
        if line == "quit" {
            break;
        }
    }
    Ok(())
}

pub fn read_script(path: &str) -> Result<Vec<String>, String> {
    if path == "-" {
        return io::stdin().lock().lines().collect::<Result<_, _>>().map_err(|e| e.to_string());
    }
    std::fs::read_to_string(path).map(|s| s.lines().map(String::from).collect()).map_err(|e| format!("{path}: {e}"))
}
