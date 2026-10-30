/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Reads the serial stream as esp_idf_monitor does and hands panics and core
 * dumps to the same Python packages. The markers are listed in
 * docs/devkit-cli.md for checking on an ESP-IDF update.
 */

use std::path::{Path, PathBuf};
use std::process::Command;

const PANIC_STACK_DUMP: &str = "Stack memory:";
const PANIC_END: &str = "ELF file SHA256:";
const COREDUMP_PROMPT: &str = "Press Enter to print core dump to UART...";
const COREDUMP_START: &str = "================= CORE DUMP START =================";
const COREDUMP_END: &str = "================= CORE DUMP END =================";
const COREDUMP_INFO: &str =
    "import esp_coredump, sys; esp_coredump.CoreDump(core=sys.argv[1], core_format='b64', prog=sys.argv[2:]).info_corefile()";
const SHF_EXECINSTR: u32 = 4;

pub type Job = Box<dyn FnOnce() -> Vec<String> + Send>;

/// What to do with one firmware line.
pub struct Feed {
    pub show: bool,
    /// Printed after the line.
    pub extra: Vec<String>,
    /// Slow work whose lines are printed when it finishes.
    pub job: Option<Job>,
    /// Sends an empty line to the firmware.
    pub answer: bool,
}

impl Feed {
    fn shown(extra: Vec<String>) -> Self {
        Self { show: true, extra, job: None, answer: false }
    }

    fn muted() -> Self {
        Self { show: false, extra: Vec::new(), job: None, answer: false }
    }
}

struct Elf {
    path: PathBuf,
    code: Vec<(u32, u32)>,
}

#[derive(PartialEq)]
enum Panic {
    Idle,
    Reading,
    Stack,
}

pub struct Decoder {
    tools: Tools,
    elfs: Vec<Elf>,
    panic: Panic,
    panic_buffer: String,
    coredump: Option<String>,
}

#[derive(Clone)]
struct Tools {
    prefix: String,
    target: String,
    python: PathBuf,
}

impl Decoder {
    /// From an IDF build directory's `project_description.json`.
    pub fn for_build(build: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(build.join("project_description.json")).ok()?;
        let tools = Tools {
            prefix: json_string(&text, "monitor_toolprefix")?,
            target: json_string(&text, "target")?,
            python: std::env::var_os("IDF_PYTHON_ENV_PATH")
                .map_or_else(|| PathBuf::from("python3"), |p| PathBuf::from(p).join("bin/python")),
        };
        let mut paths = vec![build.join(json_string(&text, "app_elf")?)];
        paths.extend(json_string(&text, "bootloader_elf").map(PathBuf::from));
        let rev = json_string(&text, "min_rev").and_then(|r| r.parse().ok()).unwrap_or(0);
        paths.extend(rom_elf(&tools.target, rev));
        let elfs = paths.into_iter().filter_map(|path| Some(Elf { code: code_ranges(&path)?, path })).collect();
        Some(Self { tools, elfs, panic: Panic::Idle, panic_buffer: String::new(), coredump: None })
    }

    pub fn feed(&mut self, line: &str) -> Feed {
        if line.contains(COREDUMP_PROMPT) {
            return Feed { answer: true, ..Feed::shown(Vec::new()) };
        }
        if line.contains(COREDUMP_START) {
            self.coredump = Some(String::new());
            return Feed { extra: vec!["--- Core dump started (further output muted)".into()], ..Feed::muted() };
        }
        if line.contains(COREDUMP_END) {
            let Some(data) = self.coredump.take() else { return Feed::shown(Vec::new()) };
            return Feed { job: Some(self.coredump_job(data)), ..Feed::muted() };
        }
        if let Some(buffer) = &mut self.coredump {
            buffer.push_str(line);
            buffer.push('\n');
            return Feed::muted();
        }

        let mut extra = Vec::new();
        if self.panic == Panic::Idle && is_panic_start(line) {
            self.panic = Panic::Reading;
            extra.push("--- Stack dump detected".to_string());
        }
        if self.panic == Panic::Reading && line.contains(PANIC_END) {
            self.panic = Panic::Idle;
            self.panic_buffer.clear();
        }
        if self.panic == Panic::Reading && line.contains(PANIC_STACK_DUMP) {
            self.panic = Panic::Stack;
        }
        if self.panic != Panic::Idle {
            self.panic_buffer.push_str(line);
            self.panic_buffer.push('\n');
        }
        if self.panic == Panic::Stack {
            if !line.is_empty() {
                return Feed::muted();
            }
            self.panic = Panic::Idle;
            let dump = std::mem::take(&mut self.panic_buffer);
            return Feed { job: Some(self.panic_job(dump)), ..Feed::muted() };
        }
        extra.extend(self.decode_addresses(line));
        Feed::shown(extra)
    }

    fn decode_addresses(&self, line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let addrs = addresses(line);
        for elf in &self.elfs {
            let hits: Vec<String> = addrs
                .iter()
                .filter(|&&a| elf.code.iter().any(|&(start, end)| (start..end).contains(&a)))
                .map(|a| format!("0x{a:08x}"))
                .collect();
            if hits.is_empty() {
                continue;
            }
            let run = Command::new(format!("{}addr2line", self.tools.prefix)).arg("-pfiaC").arg("-e").arg(&elf.path).args(&hits).output();
            let Ok(result) = run else { continue };
            out.extend(
                String::from_utf8_lossy(&result.stdout)
                    .lines()
                    .filter(|l| !l.contains("?? ??:0"))
                    .map(|l| format!("--- {l}")),
            );
        }
        out
    }

    fn app_elfs(&self) -> Vec<PathBuf> {
        self.elfs.iter().take(2).map(|e| e.path.clone()).collect()
    }

    fn panic_job(&self, dump: String) -> Job {
        let tools = self.tools.clone();
        let elfs = self.app_elfs();
        Box::new(move || {
            let hidden = || dump.lines().skip_while(|l| !l.contains(PANIC_STACK_DUMP)).map(String::from).collect();
            let Some(((first, rest), file)) = elfs.split_first().zip(temp_file("panic", &dump)) else { return hidden() };
            let mut gdb = Command::new(format!("{}gdb", tools.prefix));
            gdb.args(["--batch", "-n"]).arg(first);
            for elf in rest {
                gdb.arg("-ex").arg(format!("add-symbol-file {}", elf.display()));
            }
            gdb.arg("-ex").arg(format!(
                "target remote | \"{}\" -m esp_idf_panic_decoder --target {} \"{}\"",
                tools.python.display(),
                tools.target,
                file.display()
            ));
            let run = gdb.args(["-ex", "bt"]).output();
            let _ = std::fs::remove_file(&file);
            match run {
                Ok(out) if out.status.success() => {
                    let mut lines = vec!["--- Backtrace:".to_string()];
                    lines.extend(String::from_utf8_lossy(&out.stdout).lines().map(String::from));
                    lines
                }
                Ok(out) => {
                    let mut lines = vec![format!("--- gdb failed: {}", String::from_utf8_lossy(&out.stderr).trim())];
                    lines.extend(hidden());
                    lines
                }
                Err(err) => {
                    let mut lines = vec![format!("--- gdb: {err}")];
                    lines.extend(hidden());
                    lines
                }
            }
        })
    }

    fn coredump_job(&self, data: String) -> Job {
        let tools = self.tools.clone();
        let elfs = self.app_elfs();
        Box::new(move || {
            let raw = |why: String| {
                let mut lines = vec![format!("--- core dump: {why}"), COREDUMP_START.to_string()];
                lines.extend(data.lines().map(String::from));
                lines.push(COREDUMP_END.to_string());
                lines
            };
            let Some(file) = temp_file("coredump", &data) else { return raw("no temporary file".into()) };
            let run = Command::new(&tools.python).args(["-c", COREDUMP_INFO]).arg(&file).args(&elfs).output();
            let _ = std::fs::remove_file(&file);
            match run {
                Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).lines().map(String::from).collect(),
                Ok(out) => raw(String::from_utf8_lossy(&out.stderr).trim().to_string()),
                Err(err) => raw(err.to_string()),
            }
        })
    }
}

fn temp_file(kind: &str, data: &str) -> Option<PathBuf> {
    let path = std::env::temp_dir().join(format!("devkit-{kind}-{}", std::process::id()));
    std::fs::write(&path, data).ok()?;
    Some(path)
}

/// `Core <n> register dump:`
fn is_panic_start(line: &str) -> bool {
    let Some(at) = line.find("register dump:") else { return false };
    let before = line[..at].trim_end();
    let core = before.trim_end_matches(|c: char| c.is_ascii_digit());
    core.len() < before.len() && core.trim_end().ends_with("Core")
}

fn addresses(line: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for word in line.split(|c: char| !c.is_ascii_alphanumeric()) {
        let Some(hex) = word.strip_prefix("0x") else { continue };
        if hex.len() == 8 {
            if let Ok(v) = u32::from_str_radix(hex, 16) {
                if !out.contains(&v) {
                    out.push(v);
                }
            }
        }
    }
    out
}

/// The newest ROM ELF at or below the minimum chip revision.
fn rom_elf(target: &str, rev: u32) -> Option<PathBuf> {
    let dir = std::env::var_os("ESP_ROM_ELF_DIR")?;
    let prefix = format!("{target}_rev");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n: u32 = name.strip_prefix(&prefix)?.strip_suffix("_rom.elf")?.parse().ok()?;
            (n <= rev).then_some((n, e.path()))
        })
        .max_by_key(|&(n, _)| n)
        .map(|(_, path)| path)
}

/// Executable sections of a little-endian ELF32.
fn code_ranges(path: &Path) -> Option<Vec<(u32, u32)>> {
    let elf = std::fs::read(path).ok()?;
    let u16_at = |at: usize| Some(u16::from_le_bytes(elf.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(elf.get(at..at + 4)?.try_into().ok()?));
    if elf.get(..5)? != b"\x7fELF\x01" {
        return None;
    }
    let (table, size, count) = (u32_at(0x20)? as usize, usize::from(u16_at(0x2E)?), usize::from(u16_at(0x30)?));
    let mut ranges = Vec::new();
    for i in 0..count {
        let section = table + i * size;
        let (flags, addr, len) = (u32_at(section + 8)?, u32_at(section + 12)?, u32_at(section + 20)?);
        if flags & SHF_EXECINSTR != 0 && addr != 0 {
            ranges.push((addr, addr.saturating_add(len)));
        }
    }
    Some(ranges)
}

fn json_string(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("\"{key}\""))?;
    let rest = &text[at + key.len() + 2..];
    let start = rest.find('"')? + 1;
    let end = start + rest[start..].find('"')?;
    Some(rest[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::{Decoder, Panic, Tools, addresses, is_panic_start, json_string};

    fn decoder() -> Decoder {
        let tools = Tools { prefix: "none-".into(), target: "esp32p4".into(), python: "python3".into() };
        Decoder { tools, elfs: Vec::new(), panic: Panic::Idle, panic_buffer: String::new(), coredump: None }
    }

    #[test]
    fn finds_addresses_once() {
        let line = "Backtrace: 0x4ff09da6:0x4ff17f60 0x4FF09D58 0x4ff09da6 0x1234";
        assert_eq!(addresses(line), [0x4ff09da6, 0x4ff17f60, 0x4ff09d58]);
    }

    #[test]
    fn matches_the_register_dump_line() {
        assert!(is_panic_start("Core  0 register dump:"));
        assert!(is_panic_start("Core 1 register dump:"));
        assert!(!is_panic_start("register dump:"));
    }

    #[test]
    fn reads_flat_json_strings() {
        let text = r#"{ "app_elf": "tab5_rs.elf", "monitor_toolprefix": "riscv32-esp-elf-" }"#;
        assert_eq!(json_string(text, "app_elf").as_deref(), Some("tab5_rs.elf"));
        assert_eq!(json_string(text, "monitor_toolprefix").as_deref(), Some("riscv32-esp-elf-"));
    }

    #[test]
    fn hides_the_stack_dump_and_ends_at_the_blank_line() {
        let mut d = decoder();
        let start = d.feed("Core  0 register dump:");
        assert!(start.show && start.extra == ["--- Stack dump detected"]);
        assert!(d.feed("MEPC    : 0x4ff09da6").show);
        assert!(!d.feed("Stack memory:").show);
        assert!(!d.feed("4ff17f60: 0x00000001").show);
        let end = d.feed("");
        assert!(!end.show && end.job.is_some());
        assert!(d.feed("I (10) boot: next").show);
        assert!(d.panic_buffer.is_empty());
    }

    #[test]
    fn a_panic_without_stack_memory_ends_at_the_elf_hash() {
        let mut d = decoder();
        d.feed("Core  0 register dump:");
        d.feed("Backtrace: 0x40000000");
        assert!(d.feed("ELF file SHA256: 0123").show);
        assert!(d.panic == Panic::Idle && d.panic_buffer.is_empty());
    }

    #[test]
    fn collects_a_core_dump_silently() {
        let mut d = decoder();
        assert!(d.feed("Press Enter to print core dump to UART...").answer);
        assert!(!d.feed("================= CORE DUMP START =================").show);
        assert!(!d.feed("AAAA").show);
        let end = d.feed("================= CORE DUMP END =================");
        assert!(!end.show && end.job.is_some());
        assert!(d.feed("after").show);
    }
}
