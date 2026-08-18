//! Self-contained virtual SSD1306/SH1106 display on the BSC target driver.
//!
//! This executable owns the I2C target, SDL window, and optional button-output
//! GPIOs. It does not launch or communicate with `target-driver`.

use std::env;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::raw::c_int;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use raspberry_i2c_link::display_protocol::{DisplayController, DisplayParser};
use raspberry_i2c_link::gpio_buttons::GpioButtonOutputs;
use raspberry_i2c_link::kernel_target::{
    infer_kernel_directory, parse_address, require_root, unload_existing, DriverGuard, IdlePull,
    DEVICE, MAX_TRANSFER,
};
use raspberry_i2c_link::sdl_display::SdlDisplay;

const O_NONBLOCK: c_int = 0x800;
const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;
const DEFAULT_ADDRESS: u16 = 0x3c;
const DEFAULT_BUTTON_OUTPUTS: (u32, u32) = (5, 26);
const PARTIAL_FRAME_TIMEOUT: Duration = Duration::from_millis(75);
const USAGE: &str = "usage: virtual-display [--display ssd1306|sh1106] [--title TEXT] [--button-outputs LEFT,RIGHT|--no-button-outputs] [--vsync] [--idle-pull none|down|up] [address] [kernel-directory]\n       virtual-display --unload";

static RUNNING: AtomicBool = AtomicBool::new(true);

unsafe extern "C" {
    fn signal(signal: c_int, handler: usize) -> usize;
}

extern "C" fn stop(_signal: c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

struct Options {
    unload: bool,
    display: Option<DisplayController>,
    title: Option<String>,
    button_outputs: Option<(u32, u32)>,
    vsync: bool,
    address: Option<String>,
    kernel_directory: Option<String>,
    idle_pull: IdlePull,
}

impl Options {
    fn parse(arguments: &[String]) -> io::Result<Self> {
        let mut unload = false;
        let mut display = None;
        let mut title = None;
        let mut button_outputs = None;
        let mut button_outputs_seen = false;
        let mut vsync = false;
        let mut idle_pull = IdlePull::None;
        let mut positionals = Vec::new();
        let mut index = 1;

        while index < arguments.len() {
            let argument = &arguments[index];
            if argument == "--unload" {
                unload = true;
            } else if argument == "--title" {
                if title.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--title may be specified only once",
                    ));
                }
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--title requires text")
                })?;
                title = Some(value.clone());
            } else if let Some(value) = argument.strip_prefix("--title=") {
                if title.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--title may be specified only once",
                    ));
                }
                title = Some(value.to_owned());
            } else if argument == "--button-outputs" {
                if button_outputs_seen {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "button-output selection may be specified only once",
                    ));
                }
                button_outputs_seen = true;
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--button-outputs requires LEFT,RIGHT GPIO numbers",
                    )
                })?;
                button_outputs = Some(parse_button_outputs(value)?);
            } else if let Some(value) = argument.strip_prefix("--button-outputs=") {
                if button_outputs_seen {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "button-output selection may be specified only once",
                    ));
                }
                button_outputs_seen = true;
                button_outputs = Some(parse_button_outputs(value)?);
            } else if argument == "--no-button-outputs" {
                if button_outputs_seen {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "button-output selection may be specified only once",
                    ));
                }
                button_outputs_seen = true;
            } else if argument == "--vsync" {
                vsync = true;
            } else if argument == "--display" {
                if display.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--display may be specified only once",
                    ));
                }
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--display requires ssd1306 or sh1106",
                    )
                })?;
                display = Some(DisplayController::parse(value).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--display requires ssd1306 or sh1106",
                    )
                })?);
            } else if let Some(value) = argument.strip_prefix("--display=") {
                if display.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--display may be specified only once",
                    ));
                }
                display = Some(DisplayController::parse(value).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--display requires ssd1306 or sh1106",
                    )
                })?);
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
            && (display.is_some()
                || title.is_some()
                || button_outputs_seen
                || vsync
                || !positionals.is_empty()
                || !matches!(idle_pull, IdlePull::None)))
            || positionals.len() > 2
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE));
        }

        if !unload {
            display.get_or_insert(DisplayController::Sh1106);
            if !button_outputs_seen {
                button_outputs = Some(DEFAULT_BUTTON_OUTPUTS);
            }
        }

        Ok(Self {
            unload,
            display,
            title,
            button_outputs,
            vsync,
            address: positionals.first().cloned(),
            kernel_directory: positionals.get(1).cloned(),
            idle_pull,
        })
    }
}

fn parse_button_outputs(value: &str) -> io::Result<(u32, u32)> {
    let (left, right) = value.split_once(',').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "--button-outputs requires LEFT,RIGHT GPIO numbers",
        )
    })?;
    let parse = |number: &str| {
        number.parse::<u32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "--button-outputs requires LEFT,RIGHT GPIO numbers",
            )
        })
    };
    let outputs = (parse(left)?, parse(right)?);
    if outputs.0 == outputs.1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "button output GPIOs must be different",
        ));
    }
    Ok(outputs)
}

struct DisplayRuntime {
    parser: DisplayParser,
    window: SdlDisplay,
    button_outputs: Option<GpioButtonOutputs>,
    visual_renders: u64,
    completed_frames: u64,
    discarded_bytes: u64,
    display_updates: u64,
    timeout_renders: u64,
    partial_frame_deadline: Option<Instant>,
}

impl DisplayRuntime {
    fn start(
        controller: DisplayController,
        title: &str,
        button_outputs: Option<(u32, u32)>,
        vsync: bool,
    ) -> io::Result<Self> {
        let window = SdlDisplay::new(title, vsync)?;
        let button_outputs = button_outputs
            .map(|(left, right)| {
                GpioButtonOutputs::new("/dev/gpiochip0", left, right).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!(
                            "cannot request open-drain button outputs on GPIO{left}/{right}: {error}"
                        ),
                    )
                })
            })
            .transpose()?;
        Ok(Self {
            parser: DisplayParser::new(controller),
            window,
            button_outputs,
            visual_renders: 0,
            completed_frames: 0,
            discarded_bytes: 0,
            display_updates: 0,
            timeout_renders: 0,
            partial_frame_deadline: None,
        })
    }

    fn push(&mut self, input: &[u8]) -> io::Result<()> {
        let mut first = true;
        loop {
            let update = self.parser.push(if first { input } else { &[] });
            first = false;
            let present = update.completed_frames > 0 || update.partial_frame_boundary;
            let changed = update.display_updates > 0;
            self.completed_frames += update.completed_frames;
            self.discarded_bytes += update.discarded_bytes;
            self.display_updates += update.display_updates;
            if present {
                self.window.render(self.parser.framebuffer())?;
                self.visual_renders += 1;
                self.partial_frame_deadline = None;
            } else if changed {
                self.partial_frame_deadline = Some(Instant::now() + PARTIAL_FRAME_TIMEOUT);
            }
            if !present {
                break;
            }
        }
        self.poll_input()
    }

    fn poll_idle(&mut self) -> io::Result<()> {
        if self
            .partial_frame_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.window.render(self.parser.framebuffer())?;
            self.visual_renders += 1;
            self.timeout_renders += 1;
            self.partial_frame_deadline = None;
            eprintln!("presented an incomplete display frame after a 75 ms receive timeout");
        }
        self.poll_input()
    }

    fn poll_input(&mut self) -> io::Result<()> {
        let input = self.window.poll_input();
        if let Some(outputs) = self.button_outputs.as_mut() {
            outputs.set(input.buttons).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("cannot update button output GPIOs: {error}"),
                )
            })?;
        }
        if input.close {
            RUNNING.store(false, Ordering::Relaxed);
        }
        Ok(())
    }
}

fn serve(
    controller: DisplayController,
    title: &str,
    button_outputs: Option<(u32, u32)>,
    vsync: bool,
) -> io::Result<()> {
    let mut target = OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open(DEVICE)?;
    let mut display = DisplayRuntime::start(controller, title, button_outputs, vsync)?;
    let mut request = vec![0_u8; MAX_TRANSFER];
    let mut received_records = 0_u64;
    let mut received_bytes = 0_u64;
    let mut next_report = Instant::now() + Duration::from_secs(1);

    println!("waiting for {controller} writes on {DEVICE}; complete frames appear in SDL");
    if let Some((left, right)) = button_outputs {
        println!(
            "SDL left/middle/right thirds drive open-drain GPIO{left}/{right} as left/both/right"
        );
    }
    while RUNNING.load(Ordering::Relaxed) {
        let length = match target.read(&mut request) {
            Ok(length) => length,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                display.poll_idle()?;
                thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        received_records += 1;
        received_bytes += length as u64;
        display.push(&request[..length])?;
        let now = Instant::now();
        if received_records == 1 {
            println!("received first record: {length} bytes");
        } else if now >= next_report {
            println!(
                "display totals: {received_records} records, {received_bytes} bytes, {} frames, {} display updates, {} SDL renders ({} timeout), {} parser-discarded bytes",
                display.completed_frames,
                display.display_updates,
                display.visual_renders,
                display.timeout_renders,
                display.discarded_bytes,
            );
            next_report = now + Duration::from_secs(1);
        }
    }
    println!(
        "display final totals: {received_records} records, {received_bytes} bytes, {} frames, {} display updates, {} SDL renders ({} timeout), {} parser-discarded bytes",
        display.completed_frames,
        display.display_updates,
        display.visual_renders,
        display.timeout_renders,
        display.discarded_bytes,
    );
    Ok(())
}

fn main() -> io::Result<()> {
    let arguments: Vec<String> = env::args().collect();
    let options = Options::parse(&arguments)?;
    require_root()?;

    if options.unload {
        if unload_existing()? {
            println!("target driver and overlay removed");
        } else {
            println!("target driver and overlay were already unloaded");
        }
        return Ok(());
    }

    let controller = options.display.expect("validated display option");
    let title = options
        .title
        .unwrap_or_else(|| format!("Virtual I2C display - {controller}"));
    let address = parse_address(options.address.as_ref(), DEFAULT_ADDRESS)?;
    let kernel_directory = infer_kernel_directory(options.kernel_directory.as_ref())?;
    unsafe {
        signal(SIGINT, stop as *const () as usize);
        signal(SIGTERM, stop as *const () as usize);
    }

    let mut guard = DriverGuard::load(&kernel_directory, address, options.idle_pull)?;
    println!(
        "temporarily loaded {} target driver at 0x{address:02x}, idle pull {}",
        guard.hardware_name(),
        options.idle_pull.name()
    );
    println!("using driver artifacts from {}", kernel_directory.display());
    let serve_result = serve(controller, &title, options.button_outputs, options.vsync);
    let unload_result = guard.unload();
    match (serve_result, unload_result) {
        (Ok(()), Ok(())) => {
            println!("target driver and overlay removed");
            Ok(())
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(serve_error), Err(unload_error)) => Err(io::Error::other(format!(
            "virtual display failed: {serve_error}; cleanup also failed: {unload_error}"
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
    fn parses_complete_sh1106_configuration() {
        let options = Options::parse(&arguments(&[
            "virtual-display",
            "--display=sh1106",
            "--title",
            "Lab display",
            "--button-outputs=5,26",
            "--vsync",
            "--idle-pull=up",
            "0x3c",
            "kernel",
        ]))
        .unwrap();
        assert_eq!(options.display, Some(DisplayController::Sh1106));
        assert_eq!(options.title.as_deref(), Some("Lab display"));
        assert_eq!(options.button_outputs, Some((5, 26)));
        assert!(options.vsync);
        assert_eq!(options.address.as_deref(), Some("0x3c"));
        assert_eq!(options.kernel_directory.as_deref(), Some("kernel"));
        assert_eq!(options.idle_pull, IdlePull::Up);
    }

    #[test]
    fn rejects_duplicate_or_identical_button_outputs() {
        let duplicate = Options::parse(&arguments(&[
            "virtual-display",
            "--display=sh1106",
            "--button-outputs=5,26",
            "--button-outputs=6,27",
        ]))
        .err()
        .unwrap();
        assert!(duplicate.to_string().contains("only once"));

        let identical = Options::parse(&arguments(&[
            "virtual-display",
            "--display=sh1106",
            "--button-outputs=5,5",
        ]))
        .err()
        .unwrap();
        assert!(identical.to_string().contains("must be different"));
    }

    #[test]
    fn parses_separate_ssd1306_display_option() {
        let options =
            Options::parse(&arguments(&["virtual-display", "--display", "ssd1306"])).unwrap();
        assert_eq!(options.display, Some(DisplayController::Ssd1306));
        assert_eq!(options.button_outputs, Some(DEFAULT_BUTTON_OUTPUTS));
    }

    #[test]
    fn defaults_to_sh1106_and_standard_button_outputs() {
        let options = Options::parse(&arguments(&["virtual-display"])).unwrap();
        assert_eq!(options.display, Some(DisplayController::Sh1106));
        assert_eq!(options.button_outputs, Some((5, 26)));
    }

    #[test]
    fn can_disable_default_button_outputs() {
        let options =
            Options::parse(&arguments(&["virtual-display", "--no-button-outputs"])).unwrap();
        assert_eq!(options.display, Some(DisplayController::Sh1106));
        assert_eq!(options.button_outputs, None);

        let duplicate = Options::parse(&arguments(&[
            "virtual-display",
            "--no-button-outputs",
            "--button-outputs=6,27",
        ]))
        .err()
        .unwrap();
        assert!(duplicate.to_string().contains("only once"));
    }

    #[test]
    fn rejects_options_with_unload() {
        let error = Options::parse(&arguments(&[
            "virtual-display",
            "--unload",
            "--display=sh1106",
        ]))
        .err()
        .unwrap();
        assert!(error.to_string().contains("usage: virtual-display"));
    }
}
