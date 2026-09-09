//! Explicit CEA-608 caption-channel/control subset, without glyph rendering.
use super::CaptionImportError as Error;

/// Decoded 608 operation; parity bits never become text bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cea608Operation {
    /// Two parity-bearing null characters.
    Null,
    /// Printable basic characters addressed to one caption channel.
    Text { channel: u8, characters: [u8; 2] },
    /// Recognized caption control; repeated transmissions are retained on wire.
    Control { channel: u8, code: u8 },
    /// Preamble address on the 15 by 32 caption grid.
    Preamble { channel: u8, row: u8, column: u8 },
    /// Recognized styling or extended/special character operation.
    AttributeOrCharacter { channel: u8, first: u8, second: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    PopOn,
    PaintOn,
    RollUp,
}
#[derive(Default)]
pub(super) struct Decoder {
    channel: [Option<usize>; 2],
    mode: [Option<Mode>; 4],
    column: [u8; 4],
    previous: [Option<[u8; 2]>; 2],
    pub channels: u8,
    pub pairs: u64,
}
impl Decoder {
    pub fn push(&mut self, field: usize, pair: [u8; 2]) -> Result<Cea608Operation, Error> {
        if field > 1 || pair.iter().any(|byte| byte.count_ones().is_multiple_of(2)) {
            return Err(Error::Cea608("invalid field or odd parity"));
        }
        let [first, second] = pair.map(|byte| byte & 0x7f);
        if first == 0 && second == 0 {
            self.previous[field] = None;
            return Ok(Cea608Operation::Null);
        }
        self.pairs += 1;
        if first >= 0x20 {
            self.previous[field] = None;
            let channel = self.channel[field]
                .ok_or(Error::Cea608("text before explicit channel selection"))?;
            if second != 0 && second < 0x20 {
                return Err(Error::Cea608("control byte in a text pair"));
            }
            self.advance(channel, 1 + u8::from(second != 0))?;
            return Ok(Cea608Operation::Text {
                channel: channel as u8 + 1,
                characters: [first, second],
            });
        }
        if !(0x10..=0x1f).contains(&first) {
            return Err(Error::Cea608(
                "XDS/text-service/reserved input is unsupported",
            ));
        }
        let channel = field * 2 + usize::from(first & 8 != 0);
        let normalized = first & 0x17;
        self.channel[field] = Some(channel);
        self.channels |= 1 << channel;
        let duplicate = self.previous[field] == Some(pair);
        self.previous[field] = if duplicate { None } else { Some(pair) };
        let number = channel as u8 + 1;
        if (normalized == 0x10 && (0x40..=0x5f).contains(&second))
            || ((0x11..=0x17).contains(&normalized) && (0x40..=0x7f).contains(&second))
        {
            const ROWS: [u8; 16] = [11, 0, 1, 2, 3, 4, 12, 13, 14, 15, 5, 6, 7, 8, 9, 10];
            let row = ROWS[usize::from(((normalized << 1) & 14) | ((second >> 5) & 1))];
            if row == 0 {
                return Err(Error::Cea608("reserved preamble row"));
            }
            let column = if second & 0x10 != 0 {
                ((second & 14) >> 1) * 4
            } else {
                0
            };
            if !duplicate {
                self.column[channel] = column;
            }
            return Ok(Cea608Operation::Preamble { channel: number, row, column });
        }
        if normalized == 0x14 || (normalized == 0x15 && field == 1) {
            if !matches!(second,0x20|0x21|0x24..=0x29|0x2c..=0x2f) {
                return Err(Error::Cea608("unsupported alarm/text/reserved control"));
            }
            if !duplicate {
                match second {
                    0x20 => self.mode[channel] = Some(Mode::PopOn),
                    0x21 => self.column[channel] = self.column[channel].saturating_sub(1),
                    0x25..=0x27 => {
                        self.mode[channel] = Some(Mode::RollUp);
                        self.column[channel] = 0;
                    }
                    0x29 => self.mode[channel] = Some(Mode::PaintOn),
                    0x2d => {
                        if self.mode[channel] != Some(Mode::RollUp) {
                            return Err(Error::Cea608(
                                "carriage return outside supported roll-up mode",
                            ));
                        }
                        self.column[channel] = 0;
                    }
                    0x2f => {
                        if self.mode[channel] != Some(Mode::PopOn) {
                            return Err(Error::Cea608(
                                "end-of-caption before pop-on initialization",
                            ));
                        }
                        self.column[channel] = 0;
                    }
                    _ => {}
                }
            }
            return Ok(Cea608Operation::Control { channel: number, code: second });
        }
        let advance = match (normalized, second) {
            (0x11, 0x20..=0x3f) => 1,
            (0x10, 0x20..=0x2f) | (0x17, 0x2d..=0x2f) => 0,
            (0x12 | 0x13, 0x20..=0x3f) => {
                if !duplicate {
                    self.column[channel] = self.column[channel].checked_sub(1).ok_or(
                        Error::Cea608("extended character has no preceding character"),
                    )?;
                }
                1
            }
            (0x17, 0x21..=0x23) => second - 0x20,
            _ => return Err(Error::Cea608("unknown caption pair")),
        };
        if !duplicate && advance != 0 {
            self.advance(channel, advance)?;
        }
        Ok(Cea608Operation::AttributeOrCharacter { channel: number, first: normalized, second })
    }
    fn advance(&mut self, channel: usize, count: u8) -> Result<(), Error> {
        if self.mode[channel].is_none() {
            return Err(Error::Cea608("text/attribute before mode initialization"));
        }
        self.column[channel] = self.column[channel]
            .checked_add(count)
            .filter(|column| *column <= 32)
            .ok_or(Error::Cea608("caption row exceeds 32 columns"))?;
        Ok(())
    }
}
