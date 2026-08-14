//! Long-transfer controller test for the BSC target kernel driver.

use std::env;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::raw::{c_int, c_ulong};
use std::os::unix::io::AsRawFd;
use std::thread;
use std::time::Duration;

const I2C_SLAVE: c_ulong = 0x0703;
const DEFAULT_ADDRESS: u16 = 0x13;
const DEFAULT_BUS: &str = "/dev/i2c-1";
const MAX_TRANSFER: usize = 8192;

extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
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

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() > 4 {
        eprintln!("usage: controller-long [message] [address] [bus]");
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many arguments",
        ));
    }

    let message = args
        .get(1)
        .map(String::as_bytes)
        .unwrap_or(b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
    if message.is_empty() || message.len() > MAX_TRANSFER - 5 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("message must contain 1..={} bytes", MAX_TRANSFER - 5),
        ));
    }
    let address = parse_address(args.get(2))?;
    let bus_path = args.get(3).map(String::as_str).unwrap_or(DEFAULT_BUS);
    let mut bus = OpenOptions::new().read(true).write(true).open(bus_path)?;

    if unsafe { ioctl(bus.as_raw_fd(), I2C_SLAVE, c_ulong::from(address)) } < 0 {
        return Err(io::Error::last_os_error());
    }

    bus.write_all(message)?;
    println!("sent {} bytes to 0x{address:02x}", message.len());

    // Userspace must consume the request and queue the complete response first.
    thread::sleep(Duration::from_millis(20));
    let mut response = vec![0_u8; message.len() + 5];
    bus.read_exact(&mut response)?;
    println!(
        "received {} bytes: {:?}",
        response.len(),
        String::from_utf8_lossy(&response)
    );
    Ok(())
}
// SPDX-License-Identifier: MIT OR Apache-2.0
