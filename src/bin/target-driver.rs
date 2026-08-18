//! Self-contained userspace responder/receiver for the BSC target kernel driver.
//!
//! Run this binary as root. It applies the model-specific device-tree overlay,
//! loads the out-of-tree module, serves requests, and removes both on exit.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::raw::c_int;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const DEVICE: &str = "/dev/bsc-target0";
const MODULE_NAME: &str = "bcm27xx_bsc_target";
const MODULE_FILE: &str = "bcm27xx_bsc_target.ko";
const OVERLAY_NAMES: [&str; 2] = ["bsc-target-pi3", "bsc-target-pi4"];
const MAX_TRANSFER: usize = 8192;
const DEFAULT_ADDRESS: u16 = 0x13;
const PREFIX: &[u8] = b"ACK: ";
const O_NONBLOCK: c_int = 0x800;
const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;

const USAGE: &str = "usage: target-driver [--receive-only] [--idle-pull none|down|up] [address] [kernel-directory]\n       target-driver --unload";

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" {
    fn geteuid() -> u32;
    fn signal(signal: c_int, handler: usize) -> usize;
}

extern "C" fn stop(_signal: c_int) {
    RUNNING.store(false, Ordering::Relaxed);
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

#[derive(Clone, Copy)]
enum IdlePull {
    None,
    Down,
    Up,
}

impl IdlePull {
    fn parse(value: &str) -> io::Result<Self> {
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

    fn name(self) -> &'static str {
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

struct Options {
    unload: bool,
    receive_only: bool,
    address: Option<String>,
    kernel_directory: Option<String>,
    idle_pull: IdlePull,
}

impl Options {
    fn parse(arguments: &[String]) -> io::Result<Self> {
        let mut unload = false;
        let mut receive_only = false;
        let mut idle_pull = IdlePull::None;
        let mut positionals = Vec::new();
        let mut index = 1;

        while index < arguments.len() {
            let argument = &arguments[index];
            if argument == "--unload" {
                unload = true;
            } else if argument == "--receive-only" || argument == "--no-answer" {
                receive_only = true;
            } else if argument == "--idle-pull" {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--idle-pull requires none, down, or up",
                    )
                })?;
                idle_pull = IdlePull::parse(value)?;
            } else if let Some(value) = argument.strip_prefix("--idle-pull=") {
                idle_pull = IdlePull::parse(value)?;
            } else if argument.starts_with('-') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown option {argument}"),
                ));
            } else {
                positionals.push(argument.clone());
            }
            index += 1;
        }

        if (unload
            && (receive_only || !positionals.is_empty() || !matches!(idle_pull, IdlePull::None)))
            || positionals.len() > 2
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE));
        }

        Ok(Self {
            unload,
            receive_only,
            address: positionals.first().cloned(),
            kernel_directory: positionals.get(1).cloned(),
            idle_pull,
        })
    }
}

fn parse_address(value: Option<&String>) -> io::Result<u16> {
    let Some(value) = value else {
        return Ok(DEFAULT_ADDRESS);
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

fn infer_kernel_directory(value: Option<&String>) -> io::Result<PathBuf> {
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
            "a BSC target overlay is already active; run `target-driver --unload` first",
        ));
    }
    Ok(())
}

fn unload_existing() -> io::Result<bool> {
    let module_path = Path::new("/sys/module").join(MODULE_NAME);
    let module_loaded = module_path.exists();
    if Path::new(DEVICE).exists() && !module_loaded {
        return Err(io::Error::other(format!(
            "{DEVICE} exists but {MODULE_NAME} is not loaded; refusing automatic cleanup"
        )));
    }

    let mut changed = false;
    if module_loaded {
        // This fails safely with EBUSY if another responder has the device open.
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

struct DriverGuard {
    overlay: &'static str,
    overlay_loaded: bool,
    module_loaded: bool,
}

impl DriverGuard {
    fn load(
        hardware: Hardware,
        kernel_directory: &Path,
        address: u16,
        idle_pull: IdlePull,
    ) -> io::Result<Self> {
        ensure_unloaded()?;
        ensure_artifacts_current(kernel_directory, hardware)?;
        let module = kernel_directory.join(MODULE_FILE);

        let mut guard = Self {
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

    fn unload(&mut self) -> io::Result<()> {
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

fn queue_response(target: &mut File, response: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match target.write(response) {
            Ok(length) if length == response.len() => return Ok(()),
            Ok(length) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    format!("target accepted only {length} response bytes"),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "previous response was not consumed within five seconds",
                    ));
                }
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

fn serve(receive_only: bool) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(O_NONBLOCK);
    if !receive_only {
        options.write(true);
    }
    let mut target = options.open(DEVICE)?;
    let mut request = vec![0_u8; MAX_TRANSFER];
    let mut received_transactions = 0_u64;
    let mut received_bytes = 0_u64;
    let mut next_report = Instant::now() + Duration::from_secs(1);

    if receive_only {
        println!("waiting for I2C writes on {DEVICE}; receive-only mode will not queue responses");
    } else {
        println!("waiting for I2C requests on {DEVICE}; press Ctrl+C to stop");
    }
    while RUNNING.load(Ordering::Relaxed) {
        let length = match target.read(&mut request) {
            Ok(length) => length,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if receive_only {
            received_transactions += 1;
            received_bytes += length as u64;
            let now = Instant::now();
            if received_transactions == 1 {
                println!("received first transaction: {length} bytes");
            } else if now >= next_report {
                println!(
                    "receive-only totals: {received_transactions} transactions, {received_bytes} bytes"
                );
                next_report = now + Duration::from_secs(1);
            }
        } else {
            println!(
                "received {length} bytes: {:?}",
                String::from_utf8_lossy(&request[..length])
            );

            let echoed = length.min(MAX_TRANSFER - PREFIX.len());
            let mut response = Vec::with_capacity(PREFIX.len() + echoed);
            response.extend_from_slice(PREFIX);
            response.extend_from_slice(&request[..echoed]);
            queue_response(&mut target, &response)?;
            println!("queued {} response bytes", response.len());
        }
    }
    if receive_only {
        println!(
            "receive-only final totals: {received_transactions} transactions, {received_bytes} bytes"
        );
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let arguments: Vec<String> = env::args().collect();
    let options = Options::parse(&arguments)?;
    if unsafe { geteuid() } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "run with sudo so the app can load and unload the kernel driver",
        ));
    }

    if options.unload {
        if unload_existing()? {
            println!("target driver and overlay removed");
        } else {
            println!("target driver and overlay were already unloaded");
        }
        return Ok(());
    }

    let hardware = Hardware::detect()?;
    let address = parse_address(options.address.as_ref())?;
    let kernel_directory = infer_kernel_directory(options.kernel_directory.as_ref())?;
    unsafe {
        signal(SIGINT, stop as *const () as usize);
        signal(SIGTERM, stop as *const () as usize);
    }

    println!(
        "temporarily loading {} target driver at 0x{address:02x}, idle pull {}",
        hardware.name,
        options.idle_pull.name()
    );
    println!("using driver artifacts from {}", kernel_directory.display());
    let mut guard = DriverGuard::load(hardware, &kernel_directory, address, options.idle_pull)?;
    let serve_result = serve(options.receive_only);
    let unload_result = guard.unload();
    match (serve_result, unload_result) {
        (Ok(()), Ok(())) => {
            println!("target driver and overlay removed");
            Ok(())
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(serve_error), Err(unload_error)) => Err(io::Error::other(format!(
            "target failed: {serve_error}; cleanup also failed: {unload_error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_receive_only_mode() {
        let options = Options::parse(&arguments(&[
            "target-driver",
            "--receive-only",
            "--idle-pull=up",
            "0x3c",
            "kernel",
        ]))
        .unwrap();
        assert!(options.receive_only);
        assert_eq!(options.address.as_deref(), Some("0x3c"));
        assert_eq!(options.kernel_directory.as_deref(), Some("kernel"));
        assert!(matches!(options.idle_pull, IdlePull::Up));
    }

    #[test]
    fn accepts_no_answer_alias() {
        let options = Options::parse(&arguments(&["target-driver", "--no-answer", "60"])).unwrap();
        assert!(options.receive_only);
        assert_eq!(options.address.as_deref(), Some("60"));
    }

    #[test]
    fn rejects_receive_only_with_unload() {
        let error = Options::parse(&arguments(&["target-driver", "--unload", "--receive-only"]))
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("usage: target-driver"));
    }
}
// SPDX-License-Identifier: MIT OR Apache-2.0
