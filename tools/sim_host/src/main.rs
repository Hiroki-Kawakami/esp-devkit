/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Simulator host: runs a simulator firmware binary as a child process, shows
 * its panel and feeds it input. See docs/simulator.md.
 */

mod device;
mod frame;
mod harness;
mod protocol;
mod view;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;

use device::Device;

const DEFAULT_PSRAM_MB: usize = 32;
const USAGE: &str = "usage: sim_host [--headless] [--psram-mb N] <firmware> [args...]";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let mut headless = false;
    let mut psram_mb = DEFAULT_PSRAM_MB;
    let fw = loop {
        let Some(arg) = args.next() else {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        };
        match arg.to_str() {
            Some("--headless") => headless = true,
            Some("--psram-mb") => match args.next().and_then(|v| v.to_str()?.parse().ok()) {
                Some(mb) => psram_mb = mb,
                None => {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
            },
            _ => break PathBuf::from(arg),
        }
    };
    let fw_args: Vec<OsString> = args.collect();

    let device = match Device::new(fw, fw_args, psram_mb << 20) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("[sim_host] PSRAM region: {err}");
            return ExitCode::FAILURE;
        }
    };
    let (tx, rx) = mpsc::channel::<String>();
    device.forward_lines(tx);
    thread::spawn(move || {
        for line in rx {
            println!("{line}");
        }
    });
    if let Err(err) = device.start() {
        eprintln!("[sim_host] start: {err}");
        return ExitCode::FAILURE;
    }

    if headless {
        harness::serve(&device);
        device.stop();
        return ExitCode::SUCCESS;
    }
    let harness_device = device.clone();
    thread::spawn(move || {
        if harness::serve(&harness_device) {
            harness_device.stop();
            std::process::exit(0);
        }
    });
    let result = view::run(device.clone());
    device.stop();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("[sim_host] window: {err}");
            ExitCode::FAILURE
        }
    }
}
