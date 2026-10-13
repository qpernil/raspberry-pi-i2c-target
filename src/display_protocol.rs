// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fmt;

pub const DISPLAY_WIDTH: usize = 128;
pub const DISPLAY_HEIGHT: usize = 64;
pub const FRAMEBUFFER_SIZE: usize = DISPLAY_WIDTH * DISPLAY_HEIGHT / 8;

const SSD1306_INIT: &[u8] = &[
    0x00, 0xae, 0xd5, 0x80, 0xa8, 0x3f, 0xd3, 0x00, 0x40, 0x8d, 0x14, 0x20, 0x00, 0xa1, 0xc8, 0xda,
    0x12, 0x81, 0xcf, 0xd9, 0xf1, 0xdb, 0x40, 0xa4, 0xa6, 0xaf,
];
#[cfg(test)]
const SSD1306_ADDRESS: &[u8] = &[0x00, 0x21, 0x00, 0x7f, 0x22, 0x00, 0x07];
const SH1106_INIT: &[u8] = &[
    0x00, 0xae, 0x02, 0x10, 0x40, 0x81, 0xa0, 0xa1, 0xc8, 0xa6, 0xa8, 0x3f, 0xd3, 0x00, 0xd5, 0x80,
    0xd9, 0xf1, 0xda, 0x12, 0xdb, 0x40, 0x20, 0x02, 0xa4, 0xa6,
];
const SH1106_DISPLAY_ON: &[u8] = &[0x00, 0xaf];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayController {
    Ssd1306,
    Sh1106,
}

impl DisplayController {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ssd1306" => Some(Self::Ssd1306),
            "sh1106" => Some(Self::Sh1106),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Ssd1306 => "ssd1306",
            Self::Sh1106 => "sh1106",
        }
    }
}

impl fmt::Display for DisplayController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParseUpdate {
    pub completed_frames: u64,
    pub partial_frame_boundary: bool,
    pub discarded_bytes: u64,
    pub display_updates: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParseState {
    Scan,
    Ssd1306Data {
        x: usize,
        width: usize,
        page: usize,
        pages: usize,
    },
    Sh1106PageData {
        page: u8,
        x: usize,
        width: usize,
    },
}

pub struct DisplayParser {
    controller: DisplayController,
    pending: Vec<u8>,
    state: ParseState,
    framebuffer: [u8; FRAMEBUFFER_SIZE],
    sh1106_pages: u8,
    sh1106_frame_started: bool,
    sh1106_last_page_command: Option<u8>,
}

impl DisplayParser {
    pub fn new(controller: DisplayController) -> Self {
        Self {
            controller,
            pending: Vec::new(),
            state: ParseState::Scan,
            framebuffer: [0; FRAMEBUFFER_SIZE],
            sh1106_pages: 0,
            sh1106_frame_started: false,
            sh1106_last_page_command: None,
        }
    }

    pub fn framebuffer(&self) -> &[u8; FRAMEBUFFER_SIZE] {
        &self.framebuffer
    }

    pub fn push(&mut self, input: &[u8]) -> ParseUpdate {
        self.pending.extend_from_slice(input);
        let mut update = ParseUpdate::default();

        loop {
            let progressed = match self.state {
                ParseState::Scan => self.scan(&mut update),
                ParseState::Ssd1306Data {
                    x,
                    width,
                    page,
                    pages,
                } => self.read_ssd1306_data(x, width, page, pages, &mut update),
                ParseState::Sh1106PageData { page, x, width } => {
                    self.read_sh1106_page_data(page, x, width, &mut update)
                }
            };
            if update.completed_frames > 0 || update.partial_frame_boundary {
                break;
            }
            if !progressed {
                break;
            }
        }
        update
    }

    fn scan(&mut self, update: &mut ParseUpdate) -> bool {
        match self.controller {
            DisplayController::Ssd1306 => {
                if self.consume_pattern(SSD1306_INIT) {
                    return true;
                }
                if self.pending.starts_with(&[0x00, 0x21]) {
                    if self.pending.len() < 7 {
                        return false;
                    }
                    let x = self.pending[2] as usize;
                    let end_x = self.pending[3] as usize;
                    let page = self.pending[5] as usize;
                    let end_page = self.pending[6] as usize;
                    if self.pending[4] == 0x22
                        && x <= end_x
                        && end_x < DISPLAY_WIDTH
                        && page <= end_page
                        && end_page < DISPLAY_HEIGHT / 8
                    {
                        self.pending.drain(..7);
                        self.state = ParseState::Ssd1306Data {
                            x,
                            width: end_x - x + 1,
                            page,
                            pages: end_page - page + 1,
                        };
                        return true;
                    }
                }
                if SSD1306_INIT.starts_with(&self.pending)
                    || [0x00, 0x21].starts_with(&self.pending)
                {
                    return false;
                }
            }
            DisplayController::Sh1106 => {
                if self.consume_pattern(SH1106_INIT) || self.consume_pattern(SH1106_DISPLAY_ON) {
                    return true;
                }
                if self.pending.starts_with(&[0x00]) {
                    if self.pending.len() < 2 {
                        return false;
                    }
                    let page_command = self.pending[1];
                    if (0xb0..=0xb7).contains(&page_command) {
                        let page = page_command & 7;
                        if self.pending.len() >= 4
                            && decode_column(self.pending[2], self.pending[3]).is_some()
                            && self
                                .sh1106_last_page_command
                                .is_some_and(|previous| page < previous)
                        {
                            self.sh1106_pages = 0;
                            self.sh1106_frame_started = false;
                            self.sh1106_last_page_command = None;
                            update.partial_frame_boundary = true;
                            return false;
                        }
                        if self.pending.len() < 5 {
                            return false;
                        }
                        let end_column = decode_column(self.pending[2], self.pending[3]);
                        let range = if self.pending[4] == 0x40 {
                            // Legacy streams are unambiguous only for complete visible pages.
                            end_column
                                .filter(|&column| column == 2)
                                .map(|_| (4, 0, DISPLAY_WIDTH))
                        } else {
                            if self.pending.len() < 6 {
                                return false;
                            }
                            let start_column = decode_column(self.pending[4], self.pending[5]);
                            end_column.zip(start_column).and_then(|(end, start)| {
                                if start >= 2 && start <= end && end < DISPLAY_WIDTH + 2 {
                                    Some((6, start - 2, end - start + 1))
                                } else {
                                    None
                                }
                            })
                        };
                        if let Some((command_size, x, width)) = range {
                            self.pending.drain(..command_size);
                            if page == 0 {
                                self.sh1106_pages = 0;
                                self.sh1106_frame_started = true;
                            }
                            self.sh1106_last_page_command = Some(page);
                            self.state = ParseState::Sh1106PageData { page, x, width };
                            return true;
                        }
                    }
                }
                if SH1106_INIT.starts_with(&self.pending)
                    || SH1106_DISPLAY_ON.starts_with(&self.pending)
                {
                    return false;
                }
            }
        }

        if self.pending.is_empty() {
            return false;
        }
        self.pending.remove(0);
        update.discarded_bytes += 1;
        true
    }

    fn consume_pattern(&mut self, pattern: &[u8]) -> bool {
        if self.pending.len() < pattern.len() || !self.pending.starts_with(pattern) {
            return false;
        }
        self.pending.drain(..pattern.len());
        true
    }

    fn read_ssd1306_data(
        &mut self,
        x: usize,
        width: usize,
        page: usize,
        pages: usize,
        update: &mut ParseUpdate,
    ) -> bool {
        let message_size = width * pages + 1;
        if self.pending.is_empty() {
            return false;
        }
        if self.pending[0] != 0x40 {
            self.state = ParseState::Scan;
            return true;
        }
        if self.pending.len() < message_size {
            return false;
        }
        for offset in 0..pages {
            let target = (page + offset) * DISPLAY_WIDTH + x;
            let source = 1 + offset * width;
            self.framebuffer[target..target + width]
                .copy_from_slice(&self.pending[source..source + width]);
        }
        update.display_updates += 1;
        self.pending.drain(..message_size);
        self.state = ParseState::Scan;
        if x == 0 && width == DISPLAY_WIDTH && page == 0 && pages == DISPLAY_HEIGHT / 8 {
            update.completed_frames += 1;
        } else {
            update.partial_frame_boundary = true;
        }
        true
    }

    fn read_sh1106_page_data(
        &mut self,
        page: u8,
        x: usize,
        width: usize,
        update: &mut ParseUpdate,
    ) -> bool {
        let message_size = width + 1;
        if self.pending.is_empty() {
            return false;
        }
        if self.pending[0] != 0x40 {
            self.state = ParseState::Scan;
            return true;
        }
        if self.pending.len() < message_size {
            return false;
        }
        let start = page as usize * DISPLAY_WIDTH + x;
        self.framebuffer[start..start + width].copy_from_slice(&self.pending[1..message_size]);
        update.display_updates += 1;
        self.pending.drain(..message_size);
        self.state = ParseState::Scan;
        if x == 0 && width == DISPLAY_WIDTH {
            self.sh1106_pages |= 1 << page;
        }
        if page == 7 {
            if self.sh1106_frame_started && self.sh1106_pages == 0xff {
                update.completed_frames += 1;
            } else {
                update.partial_frame_boundary = true;
            }
            self.sh1106_frame_started = false;
            self.sh1106_pages = 0;
            self.sh1106_last_page_command = None;
        }
        true
    }
}

fn decode_column(low: u8, high: u8) -> Option<usize> {
    (low <= 0x0f && (0x10..=0x1f).contains(&high))
        .then_some(((high as usize & 0x0f) << 4) | low as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framebuffer() -> [u8; FRAMEBUFFER_SIZE] {
        let mut output = [0_u8; FRAMEBUFFER_SIZE];
        for (index, byte) in output.iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(37).wrapping_add(0x40);
        }
        output
    }

    fn feed_fragmented(parser: &mut DisplayParser, stream: &[u8]) -> ParseUpdate {
        let sizes = [1, 3, 17, 2, 131, 7, 511];
        let mut offset = 0;
        let mut update = ParseUpdate::default();
        let mut size_index = 0;
        while offset < stream.len() {
            let end = (offset + sizes[size_index % sizes.len()]).min(stream.len());
            let part = parser.push(&stream[offset..end]);
            update.completed_frames += part.completed_frames;
            update.partial_frame_boundary |= part.partial_frame_boundary;
            update.discarded_bytes += part.discarded_bytes;
            update.display_updates += part.display_updates;
            offset = end;
            size_index += 1;
        }
        update
    }

    #[test]
    fn reconstructs_fragmented_ssd1306_stream() {
        let expected = framebuffer();
        let mut stream = SSD1306_INIT.to_vec();
        stream.extend_from_slice(SSD1306_ADDRESS);
        stream.push(0x40);
        stream.extend_from_slice(&expected);

        let mut parser = DisplayParser::new(DisplayController::Ssd1306);
        let update = feed_fragmented(&mut parser, &stream);
        assert_eq!(update.completed_frames, 1);
        assert_eq!(update.display_updates, 1);
        assert_eq!(update.discarded_bytes, 0);
        assert_eq!(parser.framebuffer(), &expected);
    }

    #[test]
    fn ssd1306_resynchronizes_after_an_orphaned_framebuffer() {
        let expected = framebuffer();
        let mut stream = vec![0x40];
        stream.extend_from_slice(&expected);
        stream.extend_from_slice(SSD1306_ADDRESS);
        stream.push(0x40);
        stream.extend_from_slice(&expected);

        let mut parser = DisplayParser::new(DisplayController::Ssd1306);
        let update = parser.push(&stream);
        assert_eq!(update.completed_frames, 1);
        assert_eq!(update.display_updates, 1);
        assert_eq!(update.discarded_bytes, FRAMEBUFFER_SIZE as u64 + 1);
        assert_eq!(parser.framebuffer(), &expected);
    }

    #[test]
    fn reconstructs_fragmented_sh1106_stream() {
        let expected = framebuffer();
        let mut stream = SH1106_INIT.to_vec();
        stream.extend_from_slice(SH1106_DISPLAY_ON);
        for page in 0_u8..8 {
            stream.extend_from_slice(&[0x00, 0xb0 | page, 0x02, 0x10, 0x40]);
            let start = page as usize * DISPLAY_WIDTH;
            stream.extend_from_slice(&expected[start..start + DISPLAY_WIDTH]);
        }

        let mut parser = DisplayParser::new(DisplayController::Sh1106);
        let update = feed_fragmented(&mut parser, &stream);
        assert_eq!(update.completed_frames, 1);
        assert_eq!(update.display_updates, 8);
        assert_eq!(update.discarded_bytes, 0);
        assert_eq!(parser.framebuffer(), &expected);
    }

    #[test]
    fn resynchronizes_at_a_page_command() {
        let expected = framebuffer();
        let mut parser = DisplayParser::new(DisplayController::Sh1106);
        let mut stream = vec![0x55, 0xaa, 0x40, 0x00];
        for page in 0_u8..8 {
            stream.extend_from_slice(&[0x00, 0xb0 | page, 0x02, 0x10, 0x40]);
            let start = page as usize * DISPLAY_WIDTH;
            stream.extend_from_slice(&expected[start..start + DISPLAY_WIDTH]);
        }
        let update = parser.push(&stream);
        assert_eq!(update.completed_frames, 1);
        assert_eq!(update.display_updates, 8);
        assert_eq!(update.discarded_bytes, 4);
        assert_eq!(parser.framebuffer(), &expected);
    }

    #[test]
    fn sh1106_does_not_present_orphaned_pages_as_a_complete_frame() {
        let expected = framebuffer();
        let mut parser = DisplayParser::new(DisplayController::Sh1106);
        let mut orphaned = Vec::new();
        for page in 2_u8..8 {
            orphaned.extend_from_slice(&[0x00, 0xb0 | page, 0x02, 0x10, 0x40]);
            let start = page as usize * DISPLAY_WIDTH;
            orphaned.extend_from_slice(&expected[start..start + DISPLAY_WIDTH]);
        }

        let update = parser.push(&orphaned);
        assert_eq!(update.completed_frames, 0);
        assert!(update.partial_frame_boundary);
        assert_eq!(update.display_updates, 6);

        let mut complete = Vec::new();
        for page in 0_u8..8 {
            complete.extend_from_slice(&[0x00, 0xb0 | page, 0x02, 0x10, 0x40]);
            let start = page as usize * DISPLAY_WIDTH;
            complete.extend_from_slice(&expected[start..start + DISPLAY_WIDTH]);
        }
        let update = parser.push(&complete);
        assert_eq!(update.completed_frames, 1);
        assert_eq!(update.display_updates, 8);
        assert_eq!(parser.framebuffer(), &expected);
    }

    #[test]
    fn sh1106_accepts_multiple_pages_in_one_input_chunk() {
        let expected = framebuffer();
        let mut parser = DisplayParser::new(DisplayController::Sh1106);
        let first_command = [0x00, 0xb0, 0x02, 0x10];
        assert_eq!(parser.push(&first_command), ParseUpdate::default());

        let mut transaction = Vec::new();
        for page in 0_u8..8 {
            if page != 0 {
                transaction.extend_from_slice(&[0x00, 0xb0 | page, 0x02, 0x10]);
            }
            transaction.push(0x40);
            let start = page as usize * DISPLAY_WIDTH;
            transaction.extend_from_slice(&expected[start..start + DISPLAY_WIDTH]);
        }

        let update = parser.push(&transaction);
        assert_eq!(update.completed_frames, 1);
        assert_eq!(update.display_updates, 8);
        assert_eq!(update.discarded_bytes, 0);
        assert_eq!(parser.framebuffer(), &expected);
    }

    #[test]
    fn sh1106_lower_page_command_ends_an_incomplete_frame() {
        let expected = framebuffer();
        let mut stream = Vec::new();
        for page in 2_u8..7 {
            stream.extend_from_slice(&[0x00, 0xb0 | page, 0x02, 0x10, 0x40]);
            let start = page as usize * DISPLAY_WIDTH;
            stream.extend_from_slice(&expected[start..start + DISPLAY_WIDTH]);
        }
        stream.extend_from_slice(&[0x00, 0xb0, 0x02, 0x10]);

        let mut parser = DisplayParser::new(DisplayController::Sh1106);
        let update = parser.push(&stream);
        assert_eq!(update.completed_frames, 0);
        assert!(update.partial_frame_boundary);
        assert_eq!(update.display_updates, 5);

        let update = parser.push(&[]);
        assert_eq!(update, ParseUpdate::default());
    }

    #[test]
    fn ssd1306_partial_rectangle_preserves_other_pixels() {
        let mut parser = DisplayParser::new(DisplayController::Ssd1306);
        let expected = framebuffer();
        let mut full = SSD1306_ADDRESS.to_vec();
        full.push(0x40);
        full.extend_from_slice(&expected);
        assert_eq!(parser.push(&full).completed_frames, 1);
        let mut expected = expected;
        expected[128 + 127] = 0x40;
        expected[256 + 127] = 0x00;
        let update = feed_fragmented(
            &mut parser,
            &[0x00, 0x21, 127, 127, 0x22, 1, 2, 0x40, 0x40, 0x00],
        );
        assert_eq!(update.completed_frames, 0);
        assert!(update.partial_frame_boundary);
        assert_eq!(update.display_updates, 1);
        assert_eq!(update.discarded_bytes, 0);
        assert_eq!(parser.framebuffer(), &expected);
    }

    #[test]
    fn sh1106_length_hint_preserves_other_columns() {
        let mut parser = DisplayParser::new(DisplayController::Sh1106);
        // Page 3, ending and starting native column 12 (visible x=10), one pixel byte.
        let update = feed_fragmented(
            &mut parser,
            &[0x00, 0xb3, 0x0c, 0x10, 0x0c, 0x10, 0x40, 0xb0],
        );
        assert_eq!(update.display_updates, 1);
        assert_eq!(update.completed_frames, 0);
        let mut expected = [0; FRAMEBUFFER_SIZE];
        expected[3 * 128 + 10] = 0xb0;
        assert_eq!(parser.framebuffer(), &expected);
    }

    #[test]
    fn invalid_rectangles_resynchronize_without_indexing_outside_ram() {
        for (controller, invalid) in [
            (
                DisplayController::Ssd1306,
                vec![0x00, 0x21, 128, 128, 0x22, 0, 0],
            ),
            (
                DisplayController::Ssd1306,
                vec![0x00, 0x21, 127, 0, 0x22, 0, 0],
            ),
            (
                DisplayController::Ssd1306,
                vec![0x00, 0x21, 0, 127, 0x22, 8, 8],
            ),
            (
                DisplayController::Sh1106,
                vec![0x00, 0xb0, 0x01, 0x18, 0x02, 0x18],
            ),
            (
                DisplayController::Sh1106,
                vec![0x00, 0xb0, 0x00, 0x10, 0x00, 0x10],
            ),
            (
                DisplayController::Sh1106,
                vec![0x00, 0xb0, 0x02, 0x19, 0x02, 0x10],
            ),
        ] {
            let valid = match controller {
                DisplayController::Ssd1306 => vec![0x00, 0x21, 0, 0, 0x22, 0, 0, 0x40, 0x5a],
                DisplayController::Sh1106 => vec![0x00, 0xb0, 0x02, 0x10, 0x02, 0x10, 0x40, 0x5a],
            };
            let mut parser = DisplayParser::new(controller);
            let mut stream = invalid;
            stream.extend(valid);
            let update = parser.push(&stream);
            assert_eq!(update.display_updates, 1);
            assert!(update.discarded_bytes > 0);
            assert_eq!(parser.framebuffer()[0], 0x5a);
        }
    }
}
