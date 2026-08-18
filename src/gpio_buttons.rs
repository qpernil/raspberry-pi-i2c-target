// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::sdl_display::ButtonState;
use std::io;

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::ffi::{c_int, c_ulong, c_void};
    use std::fs::OpenOptions;
    use std::mem::size_of;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    const GPIO_LINES_MAX: usize = 64;
    const GPIO_NAME_SIZE: usize = 32;
    const GPIO_ATTRS_MAX: usize = 10;
    const GPIO_V2_LINE_FLAG_OUTPUT: u64 = 1 << 3;
    const GPIO_V2_LINE_FLAG_OPEN_DRAIN: u64 = 1 << 6;
    const GPIO_V2_LINE_ATTR_ID_OUTPUT_VALUES: u32 = 2;

    const IOC_NRBITS: usize = 8;
    const IOC_TYPEBITS: usize = 8;
    const IOC_SIZEBITS: usize = 14;
    const IOC_NRSHIFT: usize = 0;
    const IOC_TYPESHIFT: usize = IOC_NRSHIFT + IOC_NRBITS;
    const IOC_SIZESHIFT: usize = IOC_TYPESHIFT + IOC_TYPEBITS;
    const IOC_DIRSHIFT: usize = IOC_SIZESHIFT + IOC_SIZEBITS;
    const IOC_WRITE: usize = 1;
    const IOC_READ: usize = 2;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct LineAttribute {
        id: u32,
        padding: u32,
        value: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct LineConfigAttribute {
        attr: LineAttribute,
        mask: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct LineConfig {
        flags: u64,
        num_attrs: u32,
        padding: [u32; 5],
        attrs: [LineConfigAttribute; GPIO_ATTRS_MAX],
    }

    #[repr(C)]
    struct LineRequest {
        offsets: [u32; GPIO_LINES_MAX],
        consumer: [u8; GPIO_NAME_SIZE],
        config: LineConfig,
        num_lines: u32,
        event_buffer_size: u32,
        padding: [u32; 5],
        fd: c_int,
    }

    impl Default for LineRequest {
        fn default() -> Self {
            Self {
                offsets: [0; GPIO_LINES_MAX],
                consumer: [0; GPIO_NAME_SIZE],
                config: LineConfig::default(),
                num_lines: 0,
                event_buffer_size: 0,
                padding: [0; 5],
                fd: -1,
            }
        }
    }

    #[repr(C)]
    struct LineValues {
        bits: u64,
        mask: u64,
    }

    const fn ioctl_read_write(number: usize, size: usize) -> c_ulong {
        (((IOC_READ | IOC_WRITE) << IOC_DIRSHIFT)
            | (0xb4 << IOC_TYPESHIFT)
            | (number << IOC_NRSHIFT)
            | (size << IOC_SIZESHIFT)) as c_ulong
    }

    const GPIO_V2_GET_LINE_IOCTL: c_ulong = ioctl_read_write(0x07, size_of::<LineRequest>());
    const GPIO_V2_LINE_SET_VALUES_IOCTL: c_ulong = ioctl_read_write(0x0f, size_of::<LineValues>());

    unsafe extern "C" {
        fn ioctl(fd: c_int, request: c_ulong, argument: *mut c_void) -> c_int;
    }

    pub struct GpioButtonOutputs {
        lines: OwnedFd,
        state: ButtonState,
    }

    impl GpioButtonOutputs {
        pub fn new(chip: &str, left: u32, right: u32) -> io::Result<Self> {
            let chip = OpenOptions::new().read(true).write(true).open(chip)?;
            let mut request = LineRequest::default();
            request.offsets[0] = left;
            request.offsets[1] = right;
            let consumer = b"virtual-display-buttons";
            request.consumer[..consumer.len()].copy_from_slice(consumer);
            request.config.flags = GPIO_V2_LINE_FLAG_OUTPUT | GPIO_V2_LINE_FLAG_OPEN_DRAIN;
            request.config.num_attrs = 1;
            request.config.attrs[0] = LineConfigAttribute {
                attr: LineAttribute {
                    id: GPIO_V2_LINE_ATTR_ID_OUTPUT_VALUES,
                    padding: 0,
                    value: 0x3,
                },
                mask: 0x3,
            };
            request.num_lines = 2;

            if unsafe {
                ioctl(
                    chip.as_raw_fd(),
                    GPIO_V2_GET_LINE_IOCTL,
                    (&mut request as *mut LineRequest).cast(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            if request.fd < 0 {
                return Err(io::Error::other(
                    "GPIO line request returned an invalid descriptor",
                ));
            }

            Ok(Self {
                lines: unsafe { OwnedFd::from_raw_fd(request.fd) },
                state: ButtonState::default(),
            })
        }

        pub fn set(&mut self, state: ButtonState) -> io::Result<()> {
            if state == self.state {
                return Ok(());
            }
            let mut values = LineValues {
                bits: u64::from(!state.left) | (u64::from(!state.right) << 1),
                mask: 0x3,
            };
            if unsafe {
                ioctl(
                    self.lines.as_raw_fd(),
                    GPIO_V2_LINE_SET_VALUES_IOCTL,
                    (&mut values as *mut LineValues).cast(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            eprintln!(
                "button output GPIOs: left={}, right={}",
                state.left, state.right
            );
            self.state = state;
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn gpio_v2_abi_layout_matches_linux() {
            assert_eq!(size_of::<LineAttribute>(), 16);
            assert_eq!(size_of::<LineConfigAttribute>(), 24);
            assert_eq!(size_of::<LineConfig>(), 272);
            assert_eq!(size_of::<LineRequest>(), 592);
            assert_eq!(size_of::<LineValues>(), 16);
            assert_eq!(GPIO_V2_GET_LINE_IOCTL, 0xc250_b407);
            assert_eq!(GPIO_V2_LINE_SET_VALUES_IOCTL, 0xc010_b40f);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    pub struct GpioButtonOutputs;

    impl GpioButtonOutputs {
        pub fn new(_chip: &str, _left: u32, _right: u32) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "GPIO button outputs are currently supported only on Linux",
            ))
        }

        pub fn set(&mut self, _state: ButtonState) -> io::Result<()> {
            Ok(())
        }
    }
}

pub use platform::GpioButtonOutputs;
