/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Device {
    pub port: PathBuf,
    pub usb: Option<(u16, u16)>,
    pub serial: Option<String>,
    pub product: Option<String>,
}

impl Device {
    /// What a pin or a name stores: stable across USB ports when possible.
    pub fn id(&self) -> String {
        self.serial.clone().unwrap_or_else(|| self.port.display().to_string())
    }
}

#[cfg(target_os = "macos")]
pub fn list() -> Vec<Device> {
    let out = std::process::Command::new("ioreg").args(["-r", "-c", "IOUSBHostDevice", "-l", "-w0"]).output();
    let mut devices = out.map_or_else(|_| Vec::new(), |o| parse_ioreg(&String::from_utf8_lossy(&o.stdout)));
    devices.sort_by(|a, b| a.port.cmp(&b.port));
    devices
}

#[cfg(not(target_os = "macos"))]
pub fn list() -> Vec<Device> {
    let read = |dir: &Path, file: &str| std::fs::read_to_string(dir.join(file)).ok().map(|s| s.trim().to_string());
    let Ok(entries) = std::fs::read_dir("/sys/class/tty") else { return Vec::new() };
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("ttyACM") && !name.starts_with("ttyUSB") {
            continue;
        }
        let Ok(sys) = entry.path().join("device").canonicalize() else { continue };
        let usb_dir = sys.ancestors().find(|d| d.join("idVendor").is_file());
        let hex = |d: &Path, f: &str| read(d, f).and_then(|v| u16::from_str_radix(&v, 16).ok());
        devices.push(Device {
            port: Path::new("/dev").join(&name),
            usb: usb_dir.and_then(|d| Some((hex(d, "idVendor")?, hex(d, "idProduct")?))),
            serial: usb_dir.and_then(|d| read(d, "serial")),
            product: usb_dir.and_then(|d| read(d, "product")),
        });
    }
    devices.sort_by(|a, b| a.port.cmp(&b.port));
    devices
}

#[derive(Default)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct UsbNode {
    depth: usize,
    is_device: bool,
    vendor: Option<u16>,
    product_id: Option<u16>,
    serial: Option<String>,
    product: Option<String>,
}

/// Walks `ioreg -l`: a serial port's USB device is the nearest enclosing
/// IOUSBHostDevice in the tree.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_ioreg(text: &str) -> Vec<Device> {
    let mut stack: Vec<UsbNode> = Vec::new();
    let mut devices = Vec::new();
    for line in text.lines() {
        if let Some(depth) = line.find("+-o ") {
            while stack.last().is_some_and(|n| n.depth >= depth) {
                stack.pop();
            }
            stack.push(UsbNode { depth, is_device: line.contains("<class IOUSBHostDevice,"), ..Default::default() });
            continue;
        }
        let Some((key, value)) = line.split_once(" = ") else { continue };
        let key = key.trim_start_matches([' ', '|']).trim();
        let text_value = || value.trim().trim_matches('"').to_string();
        let number = || value.trim().parse::<u16>().ok();
        if key == "\"IOCalloutDevice\"" {
            let usb = stack.iter().rev().find(|n| n.is_device);
            devices.push(Device {
                port: PathBuf::from(text_value()),
                usb: usb.and_then(|n| Some((n.vendor?, n.product_id?))),
                serial: usb.and_then(|n| n.serial.clone()),
                product: usb.and_then(|n| n.product.clone()),
            });
            continue;
        }
        let Some(node) = stack.last_mut().filter(|n| n.is_device) else { continue };
        match key {
            "\"idVendor\"" => node.vendor = number(),
            "\"idProduct\"" => node.product_id = number(),
            "\"USB Serial Number\"" => node.serial = Some(text_value()),
            "\"USB Product Name\"" => node.product = Some(text_value()),
            _ => {}
        }
    }
    devices
}

#[derive(Serialize, Deserialize, Default)]
pub struct Names {
    #[serde(default)]
    pub names: BTreeMap<String, String>,
}

fn names_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(base.join("esp-devkit").join("devices.toml"))
}

impl Names {
    pub fn load() -> Self {
        names_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = names_path().ok_or("no home directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn name_of(&self, id: &str) -> Option<&str> {
        self.names.iter().find(|(_, v)| *v == id).map(|(k, _)| k.as_str())
    }
}

/// The device a port, serial number or name refers to, if connected.
fn find<'a>(devices: &'a [Device], names: &Names, spec: &str) -> Option<&'a Device> {
    let id = names.names.get(spec).map_or(spec, String::as_str);
    devices.iter().find(|d| d.port == Path::new(id) || d.serial.as_deref() == Some(id))
}

/// The id to remember for `spec`, which must be connected.
pub fn id_of(spec: &str) -> Result<String, String> {
    let names = Names::load();
    let devices = list();
    find(&devices, &names, spec).map(Device::id).ok_or_else(|| format!("{spec} is not connected"))
}

/// A port for `spec`: a path passes through so that a port USB scanning
/// misses still works.
pub fn resolve(spec: &str) -> Result<PathBuf, String> {
    let names = Names::load();
    let devices = list();
    match find(&devices, &names, spec) {
        Some(d) => Ok(d.port.clone()),
        None if spec.starts_with('/') => Ok(PathBuf::from(spec)),
        None if names.names.get(spec).is_some_and(|v| v.starts_with('/')) => Ok(PathBuf::from(&names.names[spec])),
        None => Err(format!("{spec} is not connected")),
    }
}

/// The only connected device.
pub fn only_port() -> Result<PathBuf, String> {
    let mut found = list();
    match found.len() {
        0 => Err("no serial port found; pass --port or set ESPPORT".into()),
        1 => Ok(found.remove(0).port),
        _ => Err(format!(
            "several serial ports: {}; pass --port or pin one with `devkit use`",
            found.iter().map(|d| d.port.display().to_string()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// `port  vid:pid  serial  name  product`, one line each.
pub fn table(holders: &[(PathBuf, String)]) -> String {
    let names = Names::load();
    let dash = || "-".to_string();
    let rows: Vec<[String; 6]> = list()
        .iter()
        .map(|d| {
            [
                d.port.display().to_string(),
                d.usb.map_or_else(dash, |(v, p)| format!("{v:04x}:{p:04x}")),
                d.serial.clone().unwrap_or_else(dash),
                names.name_of(&d.id()).map_or_else(dash, String::from),
                d.product.clone().unwrap_or_else(dash),
                holders.iter().find(|(p, _)| *p == d.port).map_or_else(dash, |(_, h)| h.clone()),
            ]
        })
        .collect();
    if rows.is_empty() {
        return "no serial devices".into();
    }
    let mut all = vec![["PORT", "USB", "SERIAL", "NAME", "PRODUCT", "HOST"].map(String::from)];
    all.extend(rows);
    let width = |c: usize| all.iter().map(|r| r[c].len()).max().unwrap_or(0);
    let widths: Vec<usize> = (0..6).map(width).collect();
    all.iter()
        .map(|r| r.iter().zip(&widths).map(|(v, w)| format!("{v:<w$}", w = *w)).collect::<Vec<_>>().join("  ").trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{Device, Names, find, parse_ioreg};
    use std::path::PathBuf;

    const IOREG: &str = r#"
+-o USB2.0 Hub@01100000  <class IOUSBHostDevice, id 0x1, registered>
  |   "idProduct" = 22533
  |   "idVendor" = 8584
  +-o AppleUSB20Hub@01100000  <class AppleUSB20Hub, id 0x2>
  | +-o Port@1  <class AppleUSB20HubPort, id 0x3>
  | | +-o USB JTAG@1  <class IOUSBHostDevice, id 0x4, registered>
  | |   |   "idProduct" = 4097
  | |   |   "USB Product Name" = "USB JTAG/serial debug unit"
  | |   |   "idVendor" = 12346
  | |   |   "USB Serial Number" = "AA:BB"
  | |   +-o IOUSBHostInterface@0  <class IOUSBHostInterface, id 0x5>
  | |   | |   "idVendor" = 1
  | |   | +-o AppleUSBACMData  <class AppleUSBACMData, id 0x6>
  | |   |   +-o IOSerialBSDClient  <class IOSerialBSDClient, id 0x7>
  | |   |       "IOCalloutDevice" = "/dev/cu.usbmodem1101"
  | +-o Port@2  <class AppleUSB20HubPort, id 0x8>
  | | +-o Adapter@2  <class IOUSBHostDevice, id 0x9, registered>
  | |   |   "idVendor" = 4292
  | |   |   "idProduct" = 60000
  | |   +-o IOSerialBSDClient  <class IOSerialBSDClient, id 0xa>
  | |       "IOCalloutDevice" = "/dev/cu.usbserial-1"
"#;

    #[test]
    fn ports_get_their_enclosing_usb_device() {
        let devices = parse_ioreg(IOREG);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].port, PathBuf::from("/dev/cu.usbmodem1101"));
        assert_eq!(devices[0].usb, Some((12346, 4097)));
        assert_eq!(devices[0].serial.as_deref(), Some("AA:BB"));
        assert_eq!(devices[0].product.as_deref(), Some("USB JTAG/serial debug unit"));
        assert_eq!(devices[1].usb, Some((4292, 60000)));
        assert_eq!(devices[1].serial, None);
    }

    #[test]
    fn ids_prefer_the_serial_number() {
        let devices = parse_ioreg(IOREG);
        assert_eq!(devices[0].id(), "AA:BB");
        assert_eq!(devices[1].id(), "/dev/cu.usbserial-1");
    }

    #[test]
    fn specs_match_port_serial_or_name() {
        let devices = parse_ioreg(IOREG);
        let mut names = Names::default();
        names.names.insert("board".into(), "AA:BB".into());
        let port = |spec| find(&devices, &names, spec).map(|d: &Device| d.port.clone());
        assert_eq!(port("/dev/cu.usbmodem1101"), port("AA:BB"));
        assert_eq!(port("board"), port("AA:BB"));
        assert_eq!(port("nope"), None);
    }
}
