//! I2C controller for a Pi 4 or Pi 5 running Linux.
//!
//! Sends one command to address 0x13, then reads a length-prefixed reply from
//! the Pi 3/4 BSC target. This uses only `open`, `write`, `read`, and `ioctl`.

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
// The BSC target has a 16-byte hardware FIFO. Keep the entire command in
// that FIFO so target-side Linux scheduling cannot cause an overrun.
const MAX_COMMAND: usize = 15;

extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

fn parse_address(value: Option<&String>) -> io::Result<u16> {
    let Some(value) = value else {
        return Ok(DEFAULT_ADDRESS);
    };

    let value = value.trim();
    let parsed = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u16::from_str_radix(hex, 16)
    } else {
        value.parse::<u16>()
    }
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid I2C address"))?;

    if parsed > 0x7f {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "I2C address must be a 7-bit value (0..0x7f)",
        ));
    }

    Ok(parsed)
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() > 4 {
        eprintln!("usage: controller [message] [address] [bus]");
        eprintln!("example: controller 'hello Pi 3' 0x13 /dev/i2c-1");
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many arguments",
        ));
    }

    let message = args.get(1).map(String::as_str).unwrap_or("hello Pi 3");
    let address = parse_address(args.get(2))?;
    let bus_path = args.get(3).map(String::as_str).unwrap_or(DEFAULT_BUS);

    if message.is_empty() || message.len() > MAX_COMMAND {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("message must contain 1..={MAX_COMMAND} bytes"),
        ));
    }

    let mut bus = OpenOptions::new().read(true).write(true).open(bus_path)?;

    // I2C_SLAVE chooses the remote target address for subsequent read/write
    // calls. Despite its name, it does not put this Pi into target mode.
    let result = unsafe { ioctl(bus.as_raw_fd(), I2C_SLAVE, c_ulong::from(address)) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }

    bus.write_all(message.as_bytes())?;
    println!("sent to 0x{address:02x}: {message:?}");

    // The BSC hardware receives the command, but Linux userspace on the target
    // still needs a scheduling opportunity to construct and queue its reply.
    thread::sleep(Duration::from_millis(20));

    // A reply is [one non-zero length byte][payload]. An early read from an
    // empty target FIFO commonly yields zero, so poll for up to one second.
    let mut length = [0_u8; 1];
    let response_length = (0..100)
        .find_map(|_| {
            if let Err(error) = bus.read_exact(&mut length) {
                return Some(Err(error));
            }
            if length[0] != 0 {
                Some(Ok(usize::from(length[0])))
            } else {
                thread::sleep(Duration::from_millis(10));
                None
            }
        })
        .transpose()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "target reply timed out"))?;

    let mut response = vec![0_u8; response_length];
    bus.read_exact(&mut response)?;

    println!("reply: {}", String::from_utf8_lossy(&response));
    Ok(())
}
// SPDX-License-Identifier: MIT OR Apache-2.0
