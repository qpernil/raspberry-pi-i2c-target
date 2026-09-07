//! BSC target driver lifecycle with a built-in responder or profile-based worker.
//!
//! Run this binary as root. It applies the model-specific device-tree overlay,
//! loads the out-of-tree module, serves requests, and removes both on exit.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::raw::c_int;
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output};
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
const SIGHUP: c_int = 1;
const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;
const SUPERVISOR: &str = "/opt/usb-gadget-supervisor/usb-gadget-supervisor";

const USAGE: &str = "usage: target-driver [--receive-only] [--ready-gpio GPIO] [--idle-pull none|down|up] [address] [kernel-directory]\n       target-driver --profile NAME_OR_PATH [--ready-gpio GPIO] [address] [kernel-directory]\n       target-driver --unload";

static RUNNING: AtomicBool = AtomicBool::new(true);
static RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn geteuid() -> u32;
    fn kill(pid: c_int, signal: c_int) -> c_int;
    #[cfg(target_os = "linux")]
    fn prctl(option: c_int, ...) -> c_int;
    #[cfg(target_os = "linux")]
    fn getppid() -> c_int;
    fn signal(signal: c_int, handler: usize) -> usize;
}

extern "C" fn reload(_signal: c_int) {
    RELOAD_REQUESTED.store(true, Ordering::Relaxed);
}

extern "C" fn stop(_signal: c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

#[derive(Clone, Copy)]
struct Hardware {
    name: &'static str,
    overlay: &'static str,
    target_pins: [u32; 2],
}

impl Hardware {
    fn detect() -> io::Result<Self> {
        let model = fs::read("/proc/device-tree/model")?;
        let model = String::from_utf8_lossy(&model);
        if model.contains("Raspberry Pi 3 Model B") {
            return Ok(Self {
                name: "Raspberry Pi 3",
                overlay: "bsc-target-pi3",
                target_pins: [18, 19],
            });
        }
        if model.contains("Raspberry Pi 4 Model B") {
            return Ok(Self {
                name: "Raspberry Pi 4",
                overlay: "bsc-target-pi4",
                target_pins: [10, 11],
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
    profile: Option<String>,
    unload: bool,
    receive_only: bool,
    address: Option<String>,
    kernel_directory: Option<String>,
    idle_pull: IdlePull,
    ready_gpio: Option<u32>,
}

impl Options {
    fn parse(arguments: &[String]) -> io::Result<Self> {
        let mut profile = None;
        let mut unload = false;
        let mut receive_only = false;
        let mut idle_pull = IdlePull::None;
        let mut ready_gpio = None;
        let mut positionals = Vec::new();
        let mut index = 1;

        while index < arguments.len() {
            let argument = &arguments[index];
            if argument == "--profile" {
                if profile.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--profile may be specified only once",
                    ));
                }
                index += 1;
                let value = arguments
                    .get(index)
                    .filter(|value| !value.is_empty() && !value.starts_with('-'))
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--profile requires a name or absolute path",
                        )
                    })?;
                profile = Some(value.clone());
            } else if argument == "--unload" {
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
            } else if argument == "--ready-gpio" {
                if ready_gpio.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--ready-gpio may be specified only once",
                    ));
                }
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--ready-gpio requires a GPIO number",
                    )
                })?;
                ready_gpio = Some(parse_gpio(value)?);
            } else if let Some(value) = argument.strip_prefix("--ready-gpio=") {
                if ready_gpio.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--ready-gpio may be specified only once",
                    ));
                }
                ready_gpio = Some(parse_gpio(value)?);
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
            && (profile.is_some()
                || receive_only
                || ready_gpio.is_some()
                || !positionals.is_empty()
                || !matches!(idle_pull, IdlePull::None)))
            || positionals.len() > 2
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE));
        }
        if profile.is_some() && receive_only {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--profile cannot be combined with built-in receive/response options",
            ));
        }

        Ok(Self {
            profile,
            unload,
            receive_only,
            address: positionals.first().cloned(),
            kernel_directory: positionals.get(1).cloned(),
            idle_pull,
            ready_gpio,
        })
    }
}

fn parse_gpio(value: &str) -> io::Result<u32> {
    let gpio = value.parse::<u32>().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "ready GPIO must be a BCM GPIO number in 0..=53",
        )
    })?;
    if gpio > 53 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ready GPIO must be a BCM GPIO number in 0..=53",
        ));
    }
    Ok(gpio)
}

fn validate_ready_gpio(hardware: Hardware, ready_gpio: Option<u32>) -> io::Result<()> {
    if let Some(gpio) = ready_gpio {
        if hardware.target_pins.contains(&gpio) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "GPIO{gpio} is used by the I2C target peripheral on {}",
                    hardware.name
                ),
            ));
        }
    }
    Ok(())
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
        ready_gpio: Option<u32>,
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
        let ready_gpio_parameter = ready_gpio.map(|gpio| format!("ready_gpio={gpio}"));
        let mut overlay_arguments = vec![
            OsStr::new("-d"),
            kernel_directory.as_os_str(),
            OsStr::new(hardware.overlay),
            OsStr::new(&address_parameter),
            OsStr::new(idle_pull_parameter),
        ];
        if let Some(parameter) = ready_gpio_parameter.as_deref() {
            overlay_arguments.push(OsStr::new(parameter));
        }
        run_command("dtoverlay", &overlay_arguments)?;
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
                        "response slot or I2C bus did not become available within five seconds",
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
        println!("receive-only final totals: {received_transactions} transactions, {received_bytes} bytes");
    }
    Ok(())
}

// Own the child until it exits, including error paths, before unloading the driver.
struct ProfileProcess(Child);

impl ProfileProcess {
    fn wait(&mut self, running: &AtomicBool) -> io::Result<ExitStatus> {
        while running.load(Ordering::Relaxed) {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            if RELOAD_REQUESTED.swap(false, Ordering::Relaxed)
                && unsafe { kill(self.0.id() as c_int, SIGHUP) } != 0
            {
                return Err(io::Error::last_os_error());
            }
            thread::sleep(Duration::from_millis(50));
        }
        if let Some(status) = self.0.try_wait()? {
            return Ok(status);
        }
        // SAFETY: an unreaped child PID cannot have been reused by another process.
        if unsafe { kill(self.0.id() as c_int, SIGTERM) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(50));
        }
        self.0.kill()?;
        self.0.wait()
    }
}

impl Drop for ProfileProcess {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn serve_profile(profile: &str) -> io::Result<()> {
    if !RUNNING.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut command = Command::new(SUPERVISOR);
    command.args(["--profile", profile]);
    #[cfg(target_os = "linux")]
    {
        let parent = std::process::id() as c_int;
        // SAFETY: only async-signal-safe operations run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                // PR_SET_PDEATHSIG: a killed driver launcher must stop its supervisor.
                if prctl(1, SIGTERM as std::os::raw::c_ulong, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if getppid() != parent {
                    return Err(io::Error::from_raw_os_error(32)); // Linux EPIPE
                }
                Ok(())
            });
        }
    }
    let mut process = ProfileProcess(command.spawn()?);
    let status = process.wait(&RUNNING)?;
    if status.success() || !RUNNING.load(Ordering::Relaxed) {
        Ok(())
    } else {
        Err(io::Error::other(format!("supervisor exited with {status}")))
    }
}

fn main() -> io::Result<()> {
    let arguments: Vec<String> = env::args().collect();
    if arguments
        .iter()
        .skip(1)
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{USAGE}");
        return Ok(());
    }
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

    if let Some(profile) = options.profile.as_deref() {
        // Fail before loading hardware if the installed supervisor or profile is invalid.
        run_command(
            SUPERVISOR,
            &[
                OsStr::new("--profile"),
                OsStr::new(profile),
                OsStr::new("--check-profile"),
            ],
        )?;
    }
    let hardware = Hardware::detect()?;
    if !options.receive_only && options.ready_gpio.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "response mode requires --ready-gpio",
        ));
    }
    validate_ready_gpio(hardware, options.ready_gpio)?;
    let address = parse_address(options.address.as_ref())?;
    let kernel_directory = infer_kernel_directory(options.kernel_directory.as_ref())?;
    unsafe {
        signal(SIGINT, stop as *const () as usize);
        signal(SIGTERM, stop as *const () as usize);
        if options.profile.is_some() {
            signal(SIGHUP, reload as *const () as usize);
        }
    }

    let ready_description = options
        .ready_gpio
        .map(|gpio| format!(", active-low ready GPIO {gpio}"))
        .unwrap_or_default();
    println!(
        "temporarily loading {} target driver at 0x{address:02x}, idle pull {}{}",
        hardware.name,
        options.idle_pull.name(),
        ready_description
    );
    println!("using driver artifacts from {}", kernel_directory.display());
    let mut guard = DriverGuard::load(
        hardware,
        &kernel_directory,
        address,
        options.idle_pull,
        options.ready_gpio,
    )?;
    let serve_result = if let Some(profile) = options.profile.as_deref() {
        serve_profile(profile)
    } else {
        serve(options.receive_only)
    };
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
    fn parses_profile_without_builtin_responder_options() {
        let options = Options::parse(&arguments(&[
            "target-driver",
            "--profile",
            "virtual-yubihsm-i2c",
            "0x24",
            "kernel",
        ]))
        .unwrap();
        assert_eq!(options.profile.as_deref(), Some("virtual-yubihsm-i2c"));
        assert_eq!(options.address.as_deref(), Some("0x24"));
        for suffix in [
            &["--receive-only"][..],
            &["--unload"],
            &["--empty-response", "000000"],
            &["--profile", "other"],
        ] {
            let mut args = arguments(&["target-driver", "--profile", "example"]);
            args.extend(arguments(suffix));
            assert!(Options::parse(&args).is_err());
        }
        assert!(Options::parse(&arguments(&["target-driver", "--profile"])).is_err());
    }

    #[test]
    fn waits_for_exit_and_stops_a_running_child() {
        let mut child = ProfileProcess(
            Command::new("/bin/sh")
                .args(["-c", "exit 7"])
                .spawn()
                .unwrap(),
        );
        assert_eq!(child.wait(&AtomicBool::new(true)).unwrap().code(), Some(7));
        let mut child = ProfileProcess(Command::new("/bin/sleep").arg("30").spawn().unwrap());
        let started = Instant::now();
        assert!(!child.wait(&AtomicBool::new(false)).unwrap().success());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn parses_receive_only_mode() {
        let options = Options::parse(&arguments(&[
            "target-driver",
            "--receive-only",
            "--ready-gpio=17",
            "--idle-pull=up",
            "0x3c",
            "kernel",
        ]))
        .unwrap();
        assert!(options.receive_only);
        assert_eq!(options.address.as_deref(), Some("0x3c"));
        assert_eq!(options.kernel_directory.as_deref(), Some("kernel"));
        assert!(matches!(options.idle_pull, IdlePull::Up));
        assert_eq!(options.ready_gpio, Some(17));
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

    #[test]
    fn validates_ready_gpio() {
        let hardware = Hardware {
            name: "Raspberry Pi 4",
            overlay: "bsc-target-pi4",
            target_pins: [10, 11],
        };
        assert!(validate_ready_gpio(hardware, Some(17)).is_ok());
        assert!(validate_ready_gpio(hardware, None).is_ok());
        assert!(validate_ready_gpio(hardware, Some(10)).is_err());
        assert!(parse_gpio("54").is_err());
        assert!(Options::parse(&arguments(&[
            "target-driver",
            "--ready-gpio=17",
            "--ready-gpio",
            "27",
        ]))
        .is_err());
    }
}
// SPDX-License-Identifier: MIT OR Apache-2.0
