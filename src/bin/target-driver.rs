//! Self-contained diagnostic responder for the BSC target kernel driver.
//!
//! Run this binary as root. It applies the model-specific device-tree overlay,
//! loads the out-of-tree module, serves requests, and removes both on exit.

use raspberry_i2c_link::kernel_target::{self, DriverGuard, IdlePull, DEVICE, MAX_TRANSFER};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::raw::c_int;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_ADDRESS: u16 = 0x13;
const PREFIX: &[u8] = b"ACK: ";
const O_NONBLOCK: c_int = 0x800;
const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;

const USAGE: &str = "usage: target-driver [--receive-only] [--ready-gpio GPIO] [--idle-pull none|down|up] [address] [kernel-directory]\n       target-driver --unload";

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" {
    fn signal(signal: c_int, handler: usize) -> usize;
}

extern "C" fn stop(_signal: c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

struct Options {
    unload: bool,
    receive_only: bool,
    address: Option<String>,
    kernel_directory: Option<String>,
    idle_pull: IdlePull,
    ready_gpio: Option<u32>,
}

impl Options {
    fn parse(arguments: &[String]) -> io::Result<Self> {
        let mut unload = false;
        let mut receive_only = false;
        let mut idle_pull = IdlePull::None;
        let mut ready_gpio = None;
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
                ready_gpio = Some(kernel_target::parse_gpio(value)?);
            } else if let Some(value) = argument.strip_prefix("--ready-gpio=") {
                if ready_gpio.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--ready-gpio may be specified only once",
                    ));
                }
                ready_gpio = Some(kernel_target::parse_gpio(value)?);
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
            && (receive_only
                || ready_gpio.is_some()
                || !positionals.is_empty()
                || !matches!(idle_pull, IdlePull::None)))
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
            ready_gpio,
        })
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
    kernel_target::require_root()?;

    if options.unload {
        if kernel_target::unload_existing()? {
            println!("target driver and overlay removed");
        } else {
            println!("target driver and overlay were already unloaded");
        }
        return Ok(());
    }

    if !options.receive_only && options.ready_gpio.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "response mode requires --ready-gpio",
        ));
    }
    let address = kernel_target::parse_address(options.address.as_ref(), DEFAULT_ADDRESS)?;
    let kernel_directory =
        kernel_target::infer_kernel_directory(options.kernel_directory.as_ref())?;
    unsafe {
        signal(SIGINT, stop as *const () as usize);
        signal(SIGTERM, stop as *const () as usize);
    }

    let ready_description = options
        .ready_gpio
        .map(|gpio| format!(", active-low ready GPIO {gpio}"))
        .unwrap_or_default();
    println!("using driver artifacts from {}", kernel_directory.display());
    let mut guard = DriverGuard::load(
        &kernel_directory,
        address,
        options.idle_pull,
        options.ready_gpio,
    )?;
    println!(
        "temporarily loaded {} target driver at 0x{address:02x}, idle pull {}{}",
        guard.hardware_name(),
        options.idle_pull.name(),
        ready_description
    );
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
        assert_eq!(kernel_target::parse_gpio("17").unwrap(), 17);
        assert!(kernel_target::parse_gpio("54").is_err());
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
