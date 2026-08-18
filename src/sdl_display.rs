// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::display_protocol::FRAMEBUFFER_SIZE;
use std::io;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ButtonState {
    pub left: bool,
    pub right: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SdlInput {
    pub close: bool,
    pub buttons: ButtonState,
}

#[cfg(any(target_os = "linux", test))]
fn mouse_buttons_for_x(x: i32, width: i32) -> ButtonState {
    if x < width / 3 {
        ButtonState {
            left: true,
            right: false,
        }
    } else if x >= (width * 2) / 3 {
        ButtonState {
            left: false,
            right: true,
        }
    } else {
        ButtonState {
            left: true,
            right: true,
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use crate::display_protocol::{DISPLAY_HEIGHT, DISPLAY_WIDTH};
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::ptr;

    const SDL_INIT_VIDEO: u32 = 0x0000_0020;
    const SDL_WINDOWPOS_CENTERED: c_int = 0x2fff_0000_u32 as c_int;
    const SDL_WINDOW_SHOWN: u32 = 0x0000_0004;
    const SDL_WINDOW_RESIZABLE: u32 = 0x0000_0020;
    const SDL_RENDERER_SOFTWARE: u32 = 0x0000_0001;
    const SDL_RENDERER_ACCELERATED: u32 = 0x0000_0002;
    const SDL_RENDERER_PRESENTVSYNC: u32 = 0x0000_0004;
    const SDL_QUIT: u32 = 0x0000_0100;
    const SDL_PIXELFORMAT_ARGB8888: u32 = 0x1636_2004;
    const SDL_TEXTUREACCESS_STREAMING: c_int = 1;
    const PIXEL_OFF: u32 = 0xff00_0000;
    const PIXEL_ON: u32 = 0xfff2_f2f2;

    #[repr(C, align(8))]
    struct SdlEvent {
        bytes: [u8; 56],
    }

    #[link(name = "SDL2")]
    unsafe extern "C" {
        fn SDL_Init(flags: u32) -> c_int;
        fn SDL_Quit();
        fn SDL_GetError() -> *const c_char;
        fn SDL_CreateWindow(
            title: *const c_char,
            x: c_int,
            y: c_int,
            width: c_int,
            height: c_int,
            flags: u32,
        ) -> *mut c_void;
        fn SDL_DestroyWindow(window: *mut c_void);
        fn SDL_CreateRenderer(window: *mut c_void, index: c_int, flags: u32) -> *mut c_void;
        fn SDL_DestroyRenderer(renderer: *mut c_void);
        fn SDL_CreateTexture(
            renderer: *mut c_void,
            format: u32,
            access: c_int,
            width: c_int,
            height: c_int,
        ) -> *mut c_void;
        fn SDL_DestroyTexture(texture: *mut c_void);
        fn SDL_LockTexture(
            texture: *mut c_void,
            rectangle: *const c_void,
            pixels: *mut *mut c_void,
            pitch: *mut c_int,
        ) -> c_int;
        fn SDL_UnlockTexture(texture: *mut c_void);
        fn SDL_RenderSetLogicalSize(renderer: *mut c_void, width: c_int, height: c_int) -> c_int;
        fn SDL_SetRenderDrawColor(
            renderer: *mut c_void,
            red: u8,
            green: u8,
            blue: u8,
            alpha: u8,
        ) -> c_int;
        fn SDL_RenderClear(renderer: *mut c_void) -> c_int;
        fn SDL_RenderCopy(
            renderer: *mut c_void,
            texture: *mut c_void,
            source: *const c_void,
            destination: *const c_void,
        ) -> c_int;
        fn SDL_RenderPresent(renderer: *mut c_void);
        fn SDL_PollEvent(event: *mut SdlEvent) -> c_int;
        fn SDL_GetMouseState(x: *mut c_int, y: *mut c_int) -> u32;
        fn SDL_GetWindowSize(window: *mut c_void, width: *mut c_int, height: *mut c_int);
    }

    fn sdl_error(context: &str) -> io::Error {
        let detail = unsafe {
            let pointer = SDL_GetError();
            if pointer.is_null() {
                "unknown SDL error".to_owned()
            } else {
                CStr::from_ptr(pointer).to_string_lossy().into_owned()
            }
        };
        io::Error::other(format!("{context}: {detail}"))
    }

    pub struct SdlDisplay {
        window: *mut c_void,
        renderer: *mut c_void,
        texture: *mut c_void,
        mouse_buttons: ButtonState,
        reported_buttons: ButtonState,
    }

    impl SdlDisplay {
        pub fn new(title: &str, vsync: bool) -> io::Result<Self> {
            if unsafe { SDL_Init(SDL_INIT_VIDEO) } != 0 {
                return Err(sdl_error("SDL video initialization failed"));
            }

            let title = CString::new(title).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SDL window title must not contain a NUL byte",
                )
            })?;
            let window = unsafe {
                SDL_CreateWindow(
                    title.as_ptr(),
                    SDL_WINDOWPOS_CENTERED,
                    SDL_WINDOWPOS_CENTERED,
                    768,
                    384,
                    SDL_WINDOW_SHOWN | SDL_WINDOW_RESIZABLE,
                )
            };
            if window.is_null() {
                let error = sdl_error("SDL window creation failed");
                unsafe { SDL_Quit() };
                return Err(error);
            }

            let renderer_flags =
                SDL_RENDERER_ACCELERATED | if vsync { SDL_RENDERER_PRESENTVSYNC } else { 0 };
            let mut renderer = unsafe { SDL_CreateRenderer(window, -1, renderer_flags) };
            if renderer.is_null() && !vsync {
                renderer = unsafe { SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE) };
            }
            if renderer.is_null() {
                let error = sdl_error("SDL renderer creation failed");
                unsafe {
                    SDL_DestroyWindow(window);
                    SDL_Quit();
                }
                return Err(error);
            }
            if vsync {
                eprintln!("SDL renderer synchronized to the display refresh");
            }
            if unsafe {
                SDL_RenderSetLogicalSize(renderer, DISPLAY_WIDTH as c_int, DISPLAY_HEIGHT as c_int)
            } != 0
            {
                let error = sdl_error("SDL logical-size configuration failed");
                unsafe {
                    SDL_DestroyRenderer(renderer);
                    SDL_DestroyWindow(window);
                    SDL_Quit();
                }
                return Err(error);
            }
            let texture = unsafe {
                SDL_CreateTexture(
                    renderer,
                    SDL_PIXELFORMAT_ARGB8888,
                    SDL_TEXTUREACCESS_STREAMING,
                    DISPLAY_WIDTH as c_int,
                    DISPLAY_HEIGHT as c_int,
                )
            };
            if texture.is_null() {
                let error = sdl_error("SDL streaming-texture creation failed");
                unsafe {
                    SDL_DestroyRenderer(renderer);
                    SDL_DestroyWindow(window);
                    SDL_Quit();
                }
                return Err(error);
            }

            let mut display = Self {
                window,
                renderer,
                texture,
                mouse_buttons: ButtonState::default(),
                reported_buttons: ButtonState::default(),
            };
            display.render(&[0; FRAMEBUFFER_SIZE])?;
            Ok(display)
        }

        pub fn render(&mut self, framebuffer: &[u8; FRAMEBUFFER_SIZE]) -> io::Result<()> {
            let mut pixels = ptr::null_mut();
            let mut pitch = 0;
            if unsafe { SDL_LockTexture(self.texture, ptr::null(), &mut pixels, &mut pitch) } != 0 {
                return Err(sdl_error("SDL streaming-texture lock failed"));
            }
            for output_y in 0..DISPLAY_HEIGHT {
                let source_y = DISPLAY_HEIGHT - 1 - output_y;
                let page = source_y / 8;
                let bit = source_y % 8;
                for output_x in 0..DISPLAY_WIDTH {
                    let source_x = DISPLAY_WIDTH - 1 - output_x;
                    let byte = framebuffer[page * DISPLAY_WIDTH + source_x];
                    let color = if byte & (1 << bit) != 0 {
                        PIXEL_ON
                    } else {
                        PIXEL_OFF
                    };
                    unsafe {
                        let row = pixels.cast::<u8>().add(output_y * pitch as usize);
                        ptr::write_unaligned(row.add(output_x * 4).cast::<u32>(), color);
                    }
                }
            }
            unsafe { SDL_UnlockTexture(self.texture) };

            unsafe {
                SDL_SetRenderDrawColor(self.renderer, 0x00, 0x00, 0x00, 0xff);
                if SDL_RenderClear(self.renderer) != 0 {
                    return Err(sdl_error("SDL display clear failed"));
                }
                if SDL_RenderCopy(self.renderer, self.texture, ptr::null(), ptr::null()) != 0 {
                    return Err(sdl_error("SDL texture copy failed"));
                }
                SDL_RenderPresent(self.renderer);
            }
            Ok(())
        }

        pub fn poll_input(&mut self) -> SdlInput {
            let mut close = false;
            let mut event = SdlEvent { bytes: [0; 56] };
            while unsafe { SDL_PollEvent(&mut event) } != 0 {
                let event_type = u32::from_ne_bytes(event.bytes[0..4].try_into().unwrap());
                if event_type == SDL_QUIT {
                    close = true;
                }
                event.bytes.fill(0);
            }
            let mut mouse_x = 0;
            let mut mouse_y = 0;
            let mouse_mask = unsafe { SDL_GetMouseState(&mut mouse_x, &mut mouse_y) };
            if mouse_mask & 1 != 0 {
                let mut width = 0;
                unsafe { SDL_GetWindowSize(self.window, &mut width, ptr::null_mut()) };
                self.mouse_buttons = mouse_buttons_for_x(mouse_x, width);
            } else {
                self.mouse_buttons = ButtonState::default();
            }
            let buttons = self.mouse_buttons;
            if buttons != self.reported_buttons {
                eprintln!("SDL held buttons: {buttons:?}");
                self.reported_buttons = buttons;
            }
            SdlInput { close, buttons }
        }
    }

    impl Drop for SdlDisplay {
        fn drop(&mut self) {
            unsafe {
                if !self.texture.is_null() {
                    SDL_DestroyTexture(self.texture);
                    self.texture = ptr::null_mut();
                }
                if !self.renderer.is_null() {
                    SDL_DestroyRenderer(self.renderer);
                    self.renderer = ptr::null_mut();
                }
                if !self.window.is_null() {
                    SDL_DestroyWindow(self.window);
                    self.window = ptr::null_mut();
                }
                SDL_Quit();
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    pub struct SdlDisplay;

    impl SdlDisplay {
        pub fn new(_title: &str, _vsync: bool) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the I2C target SDL viewer is currently supported on Linux",
            ))
        }

        pub fn render(&mut self, _framebuffer: &[u8; FRAMEBUFFER_SIZE]) -> io::Result<()> {
            Ok(())
        }

        pub fn poll_input(&mut self) -> SdlInput {
            SdlInput::default()
        }
    }
}

pub use platform::SdlDisplay;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_window_thirds_to_left_both_and_right() {
        assert_eq!(
            mouse_buttons_for_x(0, 300),
            ButtonState {
                left: true,
                right: false
            }
        );
        assert_eq!(
            mouse_buttons_for_x(99, 300),
            ButtonState {
                left: true,
                right: false
            }
        );
        assert_eq!(
            mouse_buttons_for_x(100, 300),
            ButtonState {
                left: true,
                right: true
            }
        );
        assert_eq!(
            mouse_buttons_for_x(199, 300),
            ButtonState {
                left: true,
                right: true
            }
        );
        assert_eq!(
            mouse_buttons_for_x(200, 300),
            ButtonState {
                left: false,
                right: true
            }
        );
    }
}
