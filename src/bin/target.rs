//! Hardware I2C target for a Raspberry Pi 3B/3B+ or Pi 4B running Linux.
//!
//! This maps the otherwise-unused SPI/BSC target peripheral through /dev/mem.
//! The Pi model is checked before mapping memory because Pi 3 and Pi 4 use
//! different peripheral addresses, target pins, and pull-control registers.

use std::env;
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::raw::c_int;
use std::os::unix::io::AsRawFd;
use std::ptr::{read_volatile, write_volatile};
use std::sync::atomic::{fence, AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const DEFAULT_ADDRESS: u16 = 0x13;

const PAGE_LENGTH: usize = 4096;

const PROT_READ: c_int = 0x1;
const PROT_WRITE: c_int = 0x2;
const MAP_SHARED: c_int = 0x01;

const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;

// GPIO register indices (byte offset / 4).
const GPPUD: usize = 37;
const GPPUDCLK0: usize = 38;
const GPIO_PUP_PDN_CNTRL_REG0: usize = 57;
const GPIO_ALT3: u32 = 0b111;

// SPI/BSC target register indices (byte offset / 4).
const BSC_DR: usize = 0;
const BSC_RSR: usize = 1;
const BSC_SLV: usize = 2;
const BSC_CR: usize = 3;
const BSC_FR: usize = 4;
const BSC_IMSC: usize = 6;
const BSC_ICR: usize = 9;

const BSC_CR_ENABLE: u32 = 1 << 0;
const BSC_CR_I2C: u32 = 1 << 2;
const BSC_CR_TX_ENABLE: u32 = 1 << 8;
const BSC_CR_RX_ENABLE: u32 = 1 << 9;
const BSC_CONTROL: u32 = BSC_CR_ENABLE | BSC_CR_I2C | BSC_CR_TX_ENABLE | BSC_CR_RX_ENABLE;

const BSC_FR_TX_BUSY: u32 = 1 << 0;
const BSC_FR_RX_EMPTY: u32 = 1 << 1;
const BSC_FR_TX_FULL: u32 = 1 << 2;
const BSC_FR_RX_BUSY: u32 = 1 << 5;

// Keep each transaction within the BSC target's 16-byte FIFO. A response also
// has a one-byte length prefix, leaving 15 bytes for its payload.
const MAX_COMMAND: usize = 15;
const MAX_RESPONSE: usize = 15;

static RUNNING: AtomicBool = AtomicBool::new(true);

#[derive(Clone, Copy, Debug)]
enum PullControl {
    Legacy,
    Bcm2711,
}

#[derive(Clone, Copy, Debug)]
struct Hardware {
    name: &'static str,
    gpio_physical: usize,
    bsc_target_physical: usize,
    target_sda: u32,
    target_scl: u32,
    sda_header_pin: u8,
    scl_header_pin: u8,
    pull_control: PullControl,
}

impl Hardware {
    fn detect() -> io::Result<Self> {
        let model = fs::read("/proc/device-tree/model")?;
        Self::from_model(&String::from_utf8_lossy(&model))
    }

    fn from_model(model: &str) -> io::Result<Self> {
        if model.contains("Raspberry Pi 3 Model B") {
            return Ok(Self {
                name: "Pi 3",
                gpio_physical: 0x3f20_0000,
                bsc_target_physical: 0x3f21_4000,
                target_sda: 18,
                target_scl: 19,
                sda_header_pin: 12,
                scl_header_pin: 35,
                pull_control: PullControl::Legacy,
            });
        }

        if model.contains("Raspberry Pi 4 Model B") {
            return Ok(Self {
                name: "Pi 4",
                gpio_physical: 0xfe20_0000,
                bsc_target_physical: 0xfe21_4000,
                target_sda: 10,
                target_scl: 11,
                sda_header_pin: 19,
                scl_header_pin: 23,
                pull_control: PullControl::Bcm2711,
            });
        }

        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "target supports Raspberry Pi 3B/3B+ and Pi 4B; detected {:?}",
                model.trim_end_matches('\0')
            ),
        ))
    }
}

extern "C" {
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        fd: c_int,
        offset: isize,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> c_int;
    fn signal(signal: c_int, handler: usize) -> usize;
}

extern "C" fn stop(_signal: c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

struct RegisterPage {
    pointer: *mut u32,
}

impl RegisterPage {
    fn map(memory: &File, physical_address: usize) -> io::Result<Self> {
        let pointer = unsafe {
            mmap(
                std::ptr::null_mut(),
                PAGE_LENGTH,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                memory.as_raw_fd(),
                physical_address as isize,
            )
        };

        if pointer as isize == -1 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            pointer: pointer.cast(),
        })
    }

    fn read(&self, register: usize) -> u32 {
        fence(Ordering::SeqCst);
        let value = unsafe { read_volatile(self.pointer.add(register)) };
        fence(Ordering::SeqCst);
        value
    }

    fn write(&self, register: usize, value: u32) {
        fence(Ordering::SeqCst);
        unsafe { write_volatile(self.pointer.add(register), value) };
        fence(Ordering::SeqCst);
    }
}

impl Drop for RegisterPage {
    fn drop(&mut self) {
        unsafe {
            munmap(self.pointer.cast(), PAGE_LENGTH);
        }
    }
}

struct HardwareTarget {
    gpio: RegisterPage,
    bsc: RegisterPage,
    address: u16,
    hardware: Hardware,
}

impl HardwareTarget {
    fn open(address: u16) -> io::Result<Self> {
        let hardware = Hardware::detect()?;

        let memory = OpenOptions::new().read(true).write(true).open("/dev/mem")?;

        let target = Self {
            gpio: RegisterPage::map(&memory, hardware.gpio_physical)?,
            bsc: RegisterPage::map(&memory, hardware.bsc_target_physical)?,
            address,
            hardware,
        };

        target.configure();
        Ok(target)
    }

    fn configure(&self) {
        self.bsc.write(BSC_CR, 0);
        self.bsc.write(BSC_RSR, 0);
        self.bsc.write(BSC_SLV, 0);
        self.bsc.write(BSC_IMSC, 0x0f);
        self.bsc.write(BSC_ICR, 0x0f);

        self.disable_internal_pulls();
        self.set_gpio_function(self.hardware.target_sda, GPIO_ALT3);
        self.set_gpio_function(self.hardware.target_scl, GPIO_ALT3);

        self.bsc.write(BSC_SLV, u32::from(self.address));
        self.bsc.write(BSC_CR, BSC_CONTROL);
        self.bsc.write(BSC_RSR, 0);
    }

    fn set_gpio_function(&self, pin: u32, function: u32) {
        let register = usize::try_from(pin / 10).expect("GPIO register index");
        let shift = (pin % 10) * 3;
        let old = self.gpio.read(register);
        let new = (old & !(0b111 << shift)) | (function << shift);
        self.gpio.write(register, new);
    }

    fn disable_internal_pulls(&self) {
        match self.hardware.pull_control {
            PullControl::Legacy => {
                let pins =
                    (1_u32 << self.hardware.target_sda) | (1_u32 << self.hardware.target_scl);
                self.gpio.write(GPPUD, 0);
                thread::sleep(Duration::from_micros(5));
                self.gpio.write(GPPUDCLK0, pins);
                thread::sleep(Duration::from_micros(5));
                self.gpio.write(GPPUD, 0);
                self.gpio.write(GPPUDCLK0, 0);
            }
            PullControl::Bcm2711 => {
                self.set_bcm2711_pull_none(self.hardware.target_sda);
                self.set_bcm2711_pull_none(self.hardware.target_scl);
            }
        }
    }

    fn set_bcm2711_pull_none(&self, pin: u32) {
        let register =
            GPIO_PUP_PDN_CNTRL_REG0 + usize::try_from(pin / 16).expect("GPIO pull register index");
        let shift = (pin % 16) * 2;
        let old = self.gpio.read(register);
        self.gpio.write(register, old & !(0b11 << shift));
    }

    fn drain_received(&self, destination: &mut Vec<u8>) {
        while self.bsc.read(BSC_FR) & BSC_FR_RX_EMPTY == 0 {
            destination.push(self.bsc.read(BSC_DR) as u8);
        }
    }

    fn receiving(&self) -> bool {
        self.bsc.read(BSC_FR) & BSC_FR_RX_BUSY != 0
    }

    fn queue_response(&self, response: &[u8]) -> io::Result<()> {
        if response.is_empty() || response.len() > MAX_RESPONSE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "response length is outside the protocol limit",
            ));
        }

        // We only permit one outstanding response. Resetting here also clears
        // any underflow state caused if the controller polled slightly early.
        while self.bsc.read(BSC_FR) & (BSC_FR_TX_BUSY | BSC_FR_RX_BUSY) != 0 {
            thread::sleep(Duration::from_micros(20));
        }
        self.bsc.write(BSC_CR, 0);
        self.bsc.write(BSC_RSR, 0);
        self.bsc.write(BSC_SLV, u32::from(self.address));
        self.bsc.write(BSC_CR, BSC_CONTROL);

        let mut framed = Vec::with_capacity(response.len() + 1);
        framed.push(response.len() as u8);
        framed.extend_from_slice(response);

        for byte in framed {
            while self.bsc.read(BSC_FR) & BSC_FR_TX_FULL != 0 {
                thread::sleep(Duration::from_micros(20));
            }
            self.bsc.write(BSC_DR, u32::from(byte));
        }
        Ok(())
    }

    fn shutdown(&self) {
        self.bsc.write(BSC_CR, 0);
        self.bsc.write(BSC_RSR, 0);
        self.bsc.write(BSC_SLV, 0);
        self.set_gpio_function(self.hardware.target_sda, 0); // input
        self.set_gpio_function(self.hardware.target_scl, 0); // input
    }
}

impl Drop for HardwareTarget {
    fn drop(&mut self) {
        self.shutdown();
    }
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

    // Exclude reserved 7-bit ranges 0x00..0x07 and 0x78..0x7f.
    if !(0x08..=0x77).contains(&parsed) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "choose a non-reserved I2C address in 0x08..=0x77",
        ));
    }
    Ok(parsed)
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() > 2 {
        eprintln!("usage: target [address]");
        eprintln!("example: sudo target 0x13");
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many arguments",
        ));
    }
    let address = parse_address(args.get(1))?;

    unsafe {
        signal(SIGINT, stop as *const () as usize);
        signal(SIGTERM, stop as *const () as usize);
    }

    let target = HardwareTarget::open(address).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            io::Error::new(
                error.kind(),
                "cannot map /dev/mem; run as root (sudo) and check Ubuntu's /dev/mem policy",
            )
        } else {
            error
        }
    })?;

    println!(
        "{} hardware I2C target listening at 0x{address:02x}",
        target.hardware.name
    );
    println!(
        "SDA: GPIO{} / physical pin {}",
        target.hardware.target_sda, target.hardware.sda_header_pin
    );
    println!(
        "SCL: GPIO{} / physical pin {}",
        target.hardware.target_scl, target.hardware.scl_header_pin
    );
    println!("press Ctrl-C to restore the pins and exit");

    let mut received = Vec::with_capacity(MAX_COMMAND);
    while RUNNING.load(Ordering::Relaxed) {
        target.drain_received(&mut received);

        if !received.is_empty() && !target.receiving() {
            if received.len() > MAX_COMMAND {
                eprintln!("discarding oversized command ({} bytes)", received.len());
                received.clear();
                continue;
            }

            let printable = String::from_utf8_lossy(&received);
            println!("received: {printable:?}");

            let mut response = b"ACK: ".to_vec();
            response.extend_from_slice(&received[..received.len().min(10)]);
            target.queue_response(&response)?;
            received.clear();
        }

        thread::sleep(Duration::from_millis(1));
    }

    println!("shutting down target peripheral");
    drop(target);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_pi3b_plus() {
        let hardware = Hardware::from_model("Raspberry Pi 3 Model B Plus Rev 1.3\0").unwrap();
        assert_eq!(hardware.gpio_physical, 0x3f20_0000);
        assert_eq!(hardware.bsc_target_physical, 0x3f21_4000);
        assert_eq!((hardware.target_sda, hardware.target_scl), (18, 19));
    }

    #[test]
    fn detects_pi4b() {
        let hardware = Hardware::from_model("Raspberry Pi 4 Model B Rev 1.1\0").unwrap();
        assert_eq!(hardware.gpio_physical, 0xfe20_0000);
        assert_eq!(hardware.bsc_target_physical, 0xfe21_4000);
        assert_eq!((hardware.target_sda, hardware.target_scl), (10, 11));
    }

    #[test]
    fn rejects_unsupported_model() {
        let error = Hardware::from_model("Raspberry Pi 5 Model B Rev 1.0\0").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
}
// SPDX-License-Identifier: MIT OR Apache-2.0
