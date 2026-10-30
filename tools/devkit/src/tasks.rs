/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use crate::client;
use crate::host::decode::Decoder;
use crate::host::serial::{self, Serial};
use crate::host::sim::{Device, Status};
use crate::host::{Flow, Host, Link, Session};
use crate::project::{Project, Target};
use crate::view;

const DEFAULT_BAUD: u32 = 115200;
const DEFAULT_PSRAM_MB: usize = 32;

#[derive(Default)]
pub struct Options {
    pub port: Option<PathBuf>,
    pub headless: bool,
    pub psram_mb: Option<usize>,
    pub backlog: Option<Option<usize>>,
    pub fw_args: Vec<OsString>,
}

fn status(cmd: &mut Command) -> Result<(), String> {
    let shown = format!("{cmd:?}");
    let ok = cmd.status().map_err(|e| format!("{shown}: {e}"))?.success();
    if ok { Ok(()) } else { Err(format!("{shown} failed")) }
}

fn shell(dir: &Path, script: &str) -> Result<(), String> {
    status(Command::new("sh").arg("-c").arg(script).current_dir(dir))
}

fn sim_configure(dir: &Path) -> Result<(), String> {
    status(Command::new("cmake").arg("--fresh").arg("-S").arg(dir).arg("-B").arg(dir.join("build")).args(["-G", "Ninja"]))
}

fn sim_build(dir: &Path, cmake_target: Option<&str>) -> Result<(), String> {
    let build = dir.join("build");
    if !build.is_dir() {
        sim_configure(dir)?;
    }
    let mut cmd = Command::new("cmake");
    cmd.arg("--build").arg(&build);
    if let Some(t) = cmake_target {
        cmd.args(["--target", t]);
    }
    status(&mut cmd)
}

pub fn idf(dir: &Path) -> Command {
    let mut cmd = Command::new("idf.py");
    cmd.arg("-C").arg(dir);
    cmd
}

pub fn build(project: &Project, name: &str, target: &Target) -> Result<(), String> {
    let dir = project.dir(name, target);
    match target {
        Target::Idf { .. } => status(idf(&dir).arg("build")),
        Target::Sim { build, .. } => sim_build(&dir, build.as_deref()),
        Target::Cmd { build, .. } => build.as_deref().map_or(Ok(()), |b| shell(&dir, b)),
    }
}

/// `menuconfig`, `save-defconfig` or `reconfigure`.
pub fn config(project: &Project, name: &str, target: &Target, action: &str) -> Result<(), String> {
    let dir = project.dir(name, target);
    match (target, action) {
        (Target::Idf { .. }, _) => status(idf(&dir).arg(action)),
        (Target::Sim { .. }, "reconfigure") => sim_configure(&dir),
        (Target::Sim { .. }, _) => sim_build(&dir, Some(action)),
        (Target::Cmd { .. }, _) => Err(format!("{name} has no sdkconfig")),
    }
}

/// Flashes, borrowing the port from a host holding it for the duration.
pub fn flash(project: &Project, name: &str, target: &Target, port: &Path) -> Result<(), String> {
    let Target::Idf { flash_baud, .. } = target else { return Err(format!("{name} is not an idf target")) };
    let holder = client::hosts().into_iter().find(|h| h.kind == format!("serial:{}", port.display()));
    let lend = |line: &str| -> Result<(), String> {
        let Some(h) = &holder else { return Ok(()) };
        let mut conn = client::Conn::open(&h.socket).map_err(|e| e.to_string())?;
        let reply = conn.run(line, false).map_err(|e| e.to_string())?;
        if reply.starts_with("OK") { Ok(()) } else { Err(reply) }
    };
    lend("port release")?;
    let mut cmd = idf(&project.dir(name, target));
    cmd.arg("-p").arg(port);
    if let Some(b) = flash_baud {
        cmd.arg("-b").arg(b.to_string());
    }
    let result = status(cmd.arg("flash"));
    lend("port acquire")?;
    result
}

pub fn port(opts: &Options) -> Result<PathBuf, String> {
    match &opts.port {
        Some(p) => Ok(p.clone()),
        None => serial::find_port(),
    }
}

pub fn run(project: &Project, name: &str, target: &Target, opts: &Options) -> Result<(), String> {
    let dir = project.dir(name, target);
    match target {
        Target::Idf { .. } => {
            let port = port(opts)?;
            flash(project, name, target, &port)?;
            match running(&format!("serial:{}", port.display())) {
                Some(host) => attach(&host, opts),
                None => serve_board(project, name, target, &port, true),
            }
        }
        Target::Sim { build: Some(_), .. } => build(project, name, target),
        Target::Sim { window, headless, .. } => {
            build(project, name, target)?;
            let fw = dir.join("build").join("simulator");
            let psram = opts.psram_mb.unwrap_or(DEFAULT_PSRAM_MB) << 20;
            let headless = opts.headless || headless.unwrap_or(false);
            // The C bsp's own SDL window.
            let env = if headless { vec![("SIMULATOR_HEADLESS".into(), "1".into())] } else { Vec::new() };
            let device = Device::new(fw, opts.fw_args.clone(), env, psram).map_err(|e| format!("PSRAM region: {e}"))?;
            let host = Host::new(Link::Sim(device.clone()), format!("{}/{name}", project.name()), None);
            let windowed = window.unwrap_or(true) && !headless;
            serve(&host, || {
                device.start().map_err(|e| format!("start: {e}"))?;
                if windowed {
                    return view::run(host.clone(), device.clone()).map_err(|e| format!("window: {e}"));
                }
                let (h, d) = (host.clone(), device.clone());
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    if let Status::Exited(code) = d.status() {
                        h.shutdown();
                        std::process::exit(code.unwrap_or(1));
                    }
                });
                Ok(())
            }, windowed)
        }
        Target::Cmd { run, .. } => {
            build(project, name, target)?;
            run.as_deref().map_or(Ok(()), |r| shell(&dir, r))
        }
    }
}

/// Attaches to the target's running host, or becomes it for an idf target.
pub fn monitor(project: &Project, name: &str, target: &Target, opts: &Options) -> Result<(), String> {
    let label = format!("{}/{name}", project.name());
    if let Target::Idf { .. } = target {
        let port = port(opts)?;
        match running(&format!("serial:{}", port.display())) {
            Some(host) => attach(&host, opts),
            None => serve_board(project, name, target, &port, false),
        }
    } else {
        let host = client::hosts().into_iter().find(|h| h.label == label);
        attach(&host.ok_or_else(|| format!("{label} is not running; start it with `devkit run`"))?, opts)
    }
}

fn running(kind: &str) -> Option<client::HostInfo> {
    client::hosts().into_iter().find(|h| h.kind == kind)
}

fn attach(host: &client::HostInfo, opts: &Options) -> Result<(), String> {
    let mut conn = client::Conn::open(&host.socket).map_err(|e| e.to_string())?;
    if let Some(n) = opts.backlog {
        let line = n.map_or("backlog".to_string(), |n| format!("backlog {n}"));
        conn.run(&line, true).map_err(|e| e.to_string())?;
    }
    conn.attach()
}

fn serve_board(project: &Project, name: &str, target: &Target, port: &Path, reset: bool) -> Result<(), String> {
    let Target::Idf { monitor_baud, .. } = target else { return Err(format!("{name} is not an idf target")) };
    let serial = Serial::open(port, monitor_baud.unwrap_or(DEFAULT_BAUD)).map_err(|e| format!("{}: {e}", port.display()))?;
    let decoder = Decoder::for_build(&project.dir(name, target).join("build"));
    let host = Host::new(Link::Serial(serial.clone()), format!("{}/{name}", project.name()), decoder);
    serve(&host, || if reset { serial.reset().map_err(|e| e.to_string()) } else { Ok(()) }, false)
}

/// Serves clients and this terminal; `foreground` runs on the main thread
/// (the window) when `windowed`, else after the terminal has attached.
fn serve(host: &Arc<Host>, foreground: impl FnOnce() -> Result<(), String>, windowed: bool) -> Result<(), String> {
    host.serve().map_err(|e| format!("socket: {e}"))?;
    let color = std::io::stdout().is_terminal();
    let session = Session::new(Box::new(std::io::stdout()), true, color);
    let result = if windowed {
        let h = host.clone();
        std::thread::spawn(move || {
            if h.terminal(&session) == Flow::Quit {
                h.shutdown();
                std::process::exit(0);
            }
        });
        foreground()
    } else {
        host.attach(session.clone());
        let started = foreground();
        // Without a terminal (stdin at EOF) the host keeps serving clients
        // until it is killed or the firmware exits.
        if started.is_ok() && host.terminal(&session) == Flow::Continue {
            loop {
                std::thread::park();
            }
        }
        started
    };
    host.shutdown();
    result
}
