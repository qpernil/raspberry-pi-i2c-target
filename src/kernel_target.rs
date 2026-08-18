// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lifecycle support for self-contained applications using the BSC target driver.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

pub const DEVICE: &str = "/dev/bsc-target0";
pub const MAX_TRANSFER: usize = 8192;

const MODULE_NAME: &str = "bcm27xx_bsc_target";
const MODULE_FILE: &str = "bcm27xx_bsc_target.ko";
const OVERLAY_NAMES: [&str; 2] = ["bsc-target-pi3", "bsc-target-pi4"];

unsafe extern "C" {
    fn geteuid() -> u32;
}

#[derive(Clone, Copy)]
struct Hardware {
    name: &'static str,
    overlay: &'static str,
}

impl Hardware {
    fn detect() -> io::Result<Self> {
        let model = fs::read("/proc/device-tree/model")?;
        let model = String::from_utf8_lossy(&model);
        if model.contains("Raspberry Pi 3 Model B") {
            return Ok(Self {
                name: "Raspberry Pi 3",
                overlay: "bsc-target-pi3",
            });
        }
        if model.contains("Raspberry Pi 4 Model B") {
            return Ok(Self {
                name: "Raspberry Pi 4",
                overlay: "bsc-target-pi4",
            });
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "kernel target supports Pi 3B/3B+ and Pi 4B; detected {:?}",
                model.trim_end_matches('\0')
            ),
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdlePull {
    None,
    Down,
    Up,
}

impl IdlePull {
    pub fn parse(value: &str) -> io::Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "down" => Ok(Self::Down),
            "up" => Ok(Self::Up),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "idle pull must be `none`, `down`, or `up`",
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Down => "down",
            Self::Up => "up",
        }
    }

    fn overlay_parameter(self) -> &'static str {
        match self {
            Self::None => "idle_pull=0",
            Self::Down => "idle_pull=1",
            Self::Up => "idle_pull=2",
        }
    }
}

pub fn require_root() -> io::Result<()> {
    if unsafe { geteuid() } == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "run with sudo so the app can load and unload the kernel driver",
        ))
    }
}

pub fn parse_address(value: Option<&String>, default: u16) -> io::Result<u16> {
    let Some(value) = value else {
        return Ok(default);
    };
    let parsed = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u16::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid I2C address"))?;
    if !(0x08..=0x77).contains(&parsed) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "choose a non-reserved address in 0x08..=0x77",
        ));
    }
    Ok(parsed)
}

fn project_root(executable: &Path) -> io::Result<&Path> {
    executable
        .ancestors()
        .skip(1)
        .find(|directory| {
            directory.join("Cargo.toml").is_file() && directory.join("kernel").is_dir()
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot locate project root"))
}

pub fn infer_kernel_directory(value: Option<&String>) -> io::Result<PathBuf> {
    if let Some(value) = value {
        return fs::canonicalize(value);
    }
    let executable = env::current_exe()?;
    let project = project_root(&executable)?;
    Ok(project.join("kernel"))
}

fn newer_source(output: &Path, sources: &[PathBuf]) -> io::Result<Option<PathBuf>> {
    let output_time = output.metadata()?.modified()?;
    for source in sources {
        if source.is_file() && source.metadata()?.modified()? > output_time {
            return Ok(Some(source.clone()));
        }
    }
    Ok(None)
}

fn ensure_artifacts_current(directory: &Path, hardware: Hardware) -> io::Result<()> {
    let module = directory.join(MODULE_FILE);
    let overlay = directory.join(format!("{}.dtbo", hardware.overlay));
    if !module.is_file() || !overlay.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "driver artifacts are missing in {}; run `make -C {}` first",
                directory.display(),
                directory.display()
            ),
        ));
    }

    let makefile = directory.join("Makefile");
    let module_sources = [
        directory.join("bcm27xx_bsc_target.c"),
        directory.join("bsc_target_uapi.h"),
        makefile.clone(),
    ];
    let overlay_sources = [
        directory.join(format!("{}-overlay.dts", hardware.overlay)),
        makefile,
    ];
    let stale_source =
        newer_source(&module, &module_sources)?.or(newer_source(&overlay, &overlay_sources)?);
    if let Some(source) = stale_source {
        return Err(io::Error::other(format!(
            "driver artifact is older than {}; run `make -C {}` before starting the target",
            source.display(),
            directory.display()
        )));
    }
    Ok(())
}

fn command_output(program: &str, arguments: &[&OsStr]) -> io::Result<Output> {
    Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| io::Error::new(error.kind(), format!("cannot execute {program}: {error}")))
}

fn run_command(program: &str, arguments: &[&OsStr]) -> io::Result<()> {
    let output = command_output(program, arguments)?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(io::Error::other(format!(
        "{program} failed with {}: {}",
        output.status,
        detail.trim()
    )))
}

fn active_overlays() -> io::Result<String> {
    let output = command_output("dtoverlay", &[OsStr::new("-l")])?;
    if !output.status.success() {
        return Err(io::Error::other("cannot list active device-tree overlays"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn overlay_is_active(list: &str, overlay: &str) -> bool {
    list.split_ascii_whitespace().any(|word| word == overlay)
}

fn ensure_unloaded() -> io::Result<()> {
    if Path::new(DEVICE).exists() || Path::new("/sys/module").join(MODULE_NAME).exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "target module is already loaded; unload it before starting the app",
        ));
    }
    let overlays = active_overlays()?;
    if OVERLAY_NAMES
        .iter()
        .any(|overlay| overlay_is_active(&overlays, overlay))
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a BSC target overlay is already active; unload it first",
        ));
    }
    Ok(())
}

pub fn unload_existing() -> io::Result<bool> {
    let module_path = Path::new("/sys/module").join(MODULE_NAME);
    let module_loaded = module_path.exists();
    if Path::new(DEVICE).exists() && !module_loaded {
        return Err(io::Error::other(format!(
            "{DEVICE} exists but {MODULE_NAME} is not loaded; refusing automatic cleanup"
        )));
    }

    let mut changed = false;
    if module_loaded {
        run_command("rmmod", &[OsStr::new(MODULE_NAME)])?;
        changed = true;
    }
    for overlay in OVERLAY_NAMES {
        loop {
            let overlays = active_overlays()?;
            if !overlay_is_active(&overlays, overlay) {
                break;
            }
            run_command("dtoverlay", &[OsStr::new("-r"), OsStr::new(overlay)])?;
            changed = true;
        }
    }
    Ok(changed)
}

pub struct DriverGuard {
    hardware_name: &'static str,
    overlay: &'static str,
    overlay_loaded: bool,
    module_loaded: bool,
}

impl DriverGuard {
    pub fn load(kernel_directory: &Path, address: u16, idle_pull: IdlePull) -> io::Result<Self> {
        let hardware = Hardware::detect()?;
        ensure_unloaded()?;
        ensure_artifacts_current(kernel_directory, hardware)?;
        let module = kernel_directory.join(MODULE_FILE);
        let mut guard = Self {
            hardware_name: hardware.name,
            overlay: hardware.overlay,
            overlay_loaded: false,
            module_loaded: false,
        };
        let address_parameter = format!("addr=0x{address:02x}");
        let idle_pull_parameter = idle_pull.overlay_parameter();
        run_command(
            "dtoverlay",
            &[
                OsStr::new("-d"),
                kernel_directory.as_os_str(),
                OsStr::new(hardware.overlay),
                OsStr::new(&address_parameter),
                OsStr::new(idle_pull_parameter),
            ],
        )?;
        guard.overlay_loaded = true;
        run_command("insmod", &[module.as_os_str()])?;
        guard.module_loaded = true;

        let deadline = Instant::now() + Duration::from_secs(1);
        while !Path::new(DEVICE).exists() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{DEVICE} was not created after loading the module"),
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(guard)
    }

    pub fn hardware_name(&self) -> &'static str {
        self.hardware_name
    }

    pub fn unload(&mut self) -> io::Result<()> {
        if self.module_loaded {
            run_command("rmmod", &[OsStr::new(MODULE_NAME)])?;
            self.module_loaded = false;
        }
        if self.overlay_loaded {
            run_command("dtoverlay", &[OsStr::new("-r"), OsStr::new(self.overlay)])?;
            self.overlay_loaded = false;
        }
        Ok(())
    }
}

impl Drop for DriverGuard {
    fn drop(&mut self) {
        if let Err(error) = self.unload() {
            eprintln!("warning: target cleanup was incomplete: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_and_decimal_addresses() {
        assert_eq!(parse_address(Some(&"0x3c".to_owned()), 0x13).unwrap(), 0x3c);
        assert_eq!(parse_address(Some(&"60".to_owned()), 0x13).unwrap(), 0x3c);
        assert_eq!(parse_address(None, 0x3c).unwrap(), 0x3c);
    }

    #[test]
    fn validates_idle_pull() {
        assert_eq!(IdlePull::parse("none").unwrap(), IdlePull::None);
        assert!(IdlePull::parse("invalid").is_err());
    }
}
