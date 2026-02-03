use core::{fmt::Write, panic::PanicInfo};

#[panic_handler]
fn panic_handler(panic: &PanicInfo) -> ! {
    let mut report = ErrorReport::begin();
    _ = writeln!(report, "v5gdb {panic}");

    loop {
        unsafe {
            vex_sdk::vexTasksRun();
        }
    }
}

pub struct ErrorReport {
    pub y_offset: i32,

    // One extra leading byte for a null terminator when word-breaking.
    buf: [u8; Self::LINE_MAX_WIDTH + 1],
    pos: usize,
}

impl ErrorReport {
    pub const DISPLAY_WIDTH: i32 = 480;
    pub const DISPLAY_HEIGHT: i32 = 240;
    pub const BOX_MARGIN: i32 = 16;
    pub const BOX_PADDING: i32 = 16;
    pub const LINE_HEIGHT: i32 = 20;
    pub const LINE_MAX_WIDTH: usize = 52;

    pub const COLOR_RED: u32 = 0x8b0_000;
    pub const COLOR_WHITE: u32 = 0xFFF_FFF;

    pub fn begin() -> Self {
        unsafe {
            vex_sdk::vexDisplayDoubleBufferDisable();
            vex_sdk::vexDisplayForegroundColor(Self::COLOR_RED);
            vex_sdk::vexDisplayRectFill(
                Self::BOX_MARGIN,
                Self::BOX_MARGIN,
                Self::DISPLAY_WIDTH - Self::BOX_MARGIN,
                Self::DISPLAY_HEIGHT - Self::BOX_MARGIN,
            );
            vex_sdk::vexDisplayForegroundColor(Self::COLOR_WHITE);
            vex_sdk::vexDisplayRectDraw(
                Self::BOX_MARGIN,
                Self::BOX_MARGIN,
                Self::DISPLAY_WIDTH - Self::BOX_MARGIN,
                Self::DISPLAY_HEIGHT - Self::BOX_MARGIN,
            );
            vex_sdk::vexDisplayFontNamedSet(c"monospace".as_ptr());
        }

        Self {
            buf: [0; 53],
            pos: 0,
            y_offset: Self::BOX_MARGIN + Self::BOX_PADDING,
        }
    }
}

impl Write for ErrorReport {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        unsafe {
            vex_sdk::vexDisplayTextSize(1, 4);
        }

        for mut character in s.chars() {
            // Brain's default font handling only supports ASCII (though there are CJK fonts).
            if !character.is_ascii() {
                character = '?';
            }

            self.buf[self.pos] = character as u8;

            if character == '\n' || self.pos == Self::LINE_MAX_WIDTH - 1 {
                let wrap_point = if character == '\n' {
                    // Wrap due to early LF.
                    self.buf[self.pos] = 0; // insert null terminator at newline
                    self.pos
                } else if let Some(space_position) = self.buf.iter().rposition(|x| *x == b' ') {
                    // Word wrap if there's a space on the current line.
                    self.buf[space_position] = 0; // insert null terminator at space
                    space_position
                } else {
                    // Fallback to letter wrapping if there's no space on the line.
                    Self::LINE_MAX_WIDTH - 1
                };

                // Put the thing on the screen!
                unsafe {
                    vex_sdk::vexDisplayPrintf(
                        Self::BOX_MARGIN + Self::BOX_PADDING,
                        self.y_offset,
                        0,
                        self.buf.as_ptr().cast(),
                    );
                }
                self.y_offset += Self::LINE_HEIGHT;

                // Since we just wrapped, we need to move the remaining characters in the string (if
                // there are any) to the front of the next line.
                if self.pos == Self::LINE_MAX_WIDTH - 1 {
                    self.buf.copy_within(wrap_point + 1.., 0);
                    self.pos -= wrap_point;
                } else {
                    self.pos = 0;
                }

                continue;
            }

            self.pos += 1;
        }

        unsafe {
            vex_sdk::vexSerialWriteBuffer(1, s.as_ptr(), s.len() as u32);
        }

        Ok(())
    }
}
