/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * esp-devkit command line: builds, flashes and runs a project's targets, and
 * talks to the debug host of a running one. See docs/devkit-cli.md.
 */

mod client;
mod frame;
mod host;
mod project;
mod protocol;
mod tasks;
mod view;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use project::{Project, Target};
use tasks::Options;

const USAGE: &str = "\
usage: devkit [-C <dir>] <command> [args]

project (the nearest devkit.toml):
  targets                         list the targets
  build   [target]
  flash   [target] [--port P]
  run     [target] [--port P] [--headless] [--psram-mb N] [-- fw args]
  monitor [target] [--port P] [--backlog [N]]
                                  log and commands; attaches to a running host
  menuconfig [target]             edit the target's sdkconfig
  save-defconfig [target]         write its sdkconfig.defaults
  reconfigure [target]            regenerate its sdkconfig and build files
  idf     [target] <idf.py args>  idf.py in the target's directory

running hosts (`run` and `monitor` start one):
  hosts                           list them
  cmd     [-t H] [--log] <line>...
  script  [-t H] [--log] <file|->";

struct Args {
    rest: Vec<OsString>,
}

impl Args {
    fn take_flag(&mut self, name: &str) -> bool {
        let before = self.rest.len();
        self.rest.retain(|a| a != name);
        self.rest.len() != before
    }

    fn take_value(&mut self, name: &str) -> Result<Option<OsString>, String> {
        let Some(i) = self.rest.iter().position(|a| a == name) else { return Ok(None) };
        if i + 1 >= self.rest.len() {
            return Err(format!("{name} needs a value"));
        }
        let value = self.rest.remove(i + 1);
        self.rest.remove(i);
        Ok(Some(value))
    }

    /// `--backlog` with an optional count.
    fn take_optional(&mut self, name: &str) -> Option<Option<usize>> {
        let i = self.rest.iter().position(|a| a == name)?;
        self.rest.remove(i);
        let n = self.rest.get(i).and_then(|v| v.to_str()?.parse().ok());
        if n.is_some() {
            self.rest.remove(i);
        }
        Some(n)
    }

    fn strings(&self) -> Vec<String> {
        self.rest.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("devkit: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut all: Vec<OsString> = std::env::args_os().skip(1).collect();
    let fw_args = match all.iter().position(|a| a == "--") {
        Some(i) => {
            let tail = all.split_off(i);
            tail[1..].to_vec()
        }
        None => Vec::new(),
    };
    let mut args = Args { rest: all };
    let dir = args.take_value("-C")?.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if args.rest.is_empty() || args.take_flag("-h") || args.take_flag("--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let command = args.rest.remove(0).to_string_lossy().into_owned();
    match command.as_str() {
        "hosts" => {
            let hosts = client::hosts();
            if hosts.is_empty() {
                println!("no running host");
            } else {
                println!("{}", client::list(&hosts));
            }
            Ok(())
        }
        "cmd" | "script" => {
            let target = args.take_value("-t")?;
            let log = args.take_flag("--log");
            let host = client::select(target.as_ref().and_then(|t| t.to_str()))?;
            let lines = if command == "cmd" {
                args.strings()
            } else {
                let path = args.strings().into_iter().next().ok_or("script needs a file or -")?;
                client::read_script(&path)?
            };
            client::run_lines(&host, lines, log)
        }
        _ => project_command(&command, args, &dir, fw_args),
    }
}

fn project_command(command: &str, mut args: Args, dir: &std::path::Path, fw_args: Vec<OsString>) -> Result<(), String> {
    let project = Project::find(dir)?;
    let opts = Options {
        port: args.take_value("--port")?.map(PathBuf::from),
        headless: args.take_flag("--headless"),
        psram_mb: match args.take_value("--psram-mb")? {
            Some(v) => Some(v.to_str().and_then(|v| v.parse().ok()).ok_or("--psram-mb needs a number")?),
            None => None,
        },
        backlog: args.take_optional("--backlog"),
        fw_args,
    };
    let positional = args.strings();
    if command == "targets" {
        for (name, target) in &project.targets {
            let kind = match target {
                Target::Idf { .. } => "idf",
                Target::Sim { .. } => "sim",
                Target::Cmd { .. } => "cmd",
            };
            let default = if project.default.as_deref() == Some(name) { "  (default)" } else { "" };
            println!("{name}  {kind}{default}");
        }
        return Ok(());
    }
    if command == "idf" {
        let (name, target, rest) = match positional.first().filter(|p| project.targets.contains_key(*p)) {
            Some(p) => (Some(p.as_str()), project.target(Some(p))?.1, &positional[1..]),
            None => (None, project.target(None)?.1, &positional[..]),
        };
        let name = name.map(String::from).unwrap_or_else(|| project.target(None).unwrap().0);
        if !matches!(target, Target::Idf { .. }) {
            return Err(format!("{name} is not an idf target"));
        }
        let ok = tasks::idf(&project.dir(&name, &target)).args(rest).status().map_err(|e| e.to_string())?.success();
        return if ok { Ok(()) } else { Err("idf.py failed".into()) };
    }
    if positional.len() > 1 {
        return Err(format!("unexpected arguments: {}", positional[1..].join(" ")));
    }
    let (name, target) = project.target(positional.first().map(String::as_str))?;
    match command {
        "build" => tasks::build(&project, &name, &target),
        "flash" => tasks::flash(&project, &name, &target, &tasks::port(&opts)?),
        "run" => tasks::run(&project, &name, &target, &opts),
        "monitor" => tasks::monitor(&project, &name, &target, &opts),
        "menuconfig" | "save-defconfig" | "reconfigure" => tasks::config(&project, &name, &target, command),
        _ => Err(format!("unknown command {command}\n{USAGE}")),
    }
}
