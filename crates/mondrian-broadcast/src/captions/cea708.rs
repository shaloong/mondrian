//! Strict DTVCC packet/service/command boundaries; no typography qualification.
use super::CaptionImportError as Error;
use std::collections::BTreeSet;
#[derive(Default)]
pub(super) struct Decoder {
    pending: Vec<u8>,
    required: usize,
    last_sequence: Option<u8>,
    pub services: BTreeSet<u8>,
    pub packets: u64,
}
impl Decoder {
    pub fn push(&mut self, kind: u8, pair: [u8; 2]) -> Result<(), Error> {
        if kind == 3 {
            if !self.pending.is_empty() {
                return Err(Error::Cea708("new packet before previous packet completed"));
            }
            let sequence = pair[0] >> 6;
            if self.last_sequence.is_some_and(|last| (last + 1) % 4 != sequence) {
                return Err(Error::Cea708("DTVCC sequence discontinuity"));
            }
            self.last_sequence = Some(sequence);
            let size = usize::from(pair[0] & 63);
            self.required = if size == 0 { 128 } else { size * 2 };
        } else if kind != 2 || self.pending.is_empty() {
            return Err(Error::Cea708("continuation without packet start"));
        }
        self.pending.extend_from_slice(&pair);
        if self.pending.len() > self.required {
            return Err(Error::Cea708("packet exceeds declared size"));
        }
        if self.pending.len() == self.required {
            let packet = std::mem::take(&mut self.pending);
            let mut at = 1usize;
            while at < packet.len() {
                let header = packet[at];
                at += 1;
                let mut service = header >> 5;
                let count = usize::from(header & 31);
                if service == 0 {
                    if count != 0 || packet[at..].iter().any(|byte| *byte != 0) {
                        return Err(Error::Cea708("invalid null service padding"));
                    }
                    break;
                }
                if service == 7 {
                    let extension =
                        *packet.get(at).ok_or(Error::Cea708("truncated extended service"))?;
                    at += 1;
                    service = extension & 63;
                    if extension & 0xc0 != 0 || service < 7 {
                        return Err(Error::Cea708("invalid extended service number"));
                    }
                }
                let end = at
                    .checked_add(count)
                    .filter(|end| *end <= packet.len())
                    .ok_or(Error::Cea708("service block exceeds packet"))?;
                validate_commands(&packet[at..end])?;
                if count != 0 {
                    self.services.insert(service);
                }
                at = end;
            }
            self.packets += 1;
        }
        Ok(())
    }
    pub fn finish(&self) -> Result<(), Error> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(Error::Cea708("truncated final DTVCC packet"))
        }
    }
}
fn validate_commands(bytes: &[u8]) -> Result<(), Error> {
    let mut at = 0usize;
    while at < bytes.len() {
        let code = bytes[at];
        let count = match code {
            0 | 3 | 8 | 12..=14 | 0x20..=0x87 | 0x8e..=0x8f | 0xa0..=0xff => 1,
            0x88..=0x8d => 2,
            0x90 | 0x92 => 3,
            0x91 => 4,
            0x97 => 5,
            0x98..=0x9f => 7,
            0x18 => 3,
            _ => {
                return Err(Error::Cea708(
                    "reserved/extended command outside supported coding subset",
                ))
            }
        };
        let end = at
            .checked_add(count)
            .filter(|end| *end <= bytes.len())
            .ok_or(Error::Cea708("command crosses service-block boundary"))?;
        if code == 0x18 && (bytes[at + 1] != 0 || bytes[at + 2] < 0x20) {
            return Err(Error::Cea708("unsupported P16 character encoding"));
        }
        if code == 0x92
            && (bytes[at + 1] & 0xf0 != 0
                || bytes[at + 1] & 15 > 14
                || bytes[at + 2] & 0xc0 != 0
                || bytes[at + 2] & 63 > 41)
        {
            return Err(Error::Cea708("invalid pen location"));
        }
        if (0x98..=0x9f).contains(&code)
            && (bytes[at + 1] & 0xc0 != 0
                || bytes[at + 4] & 15 > 14
                || bytes[at + 4] >> 4 > 8
                || bytes[at + 5] & 0xc0 != 0
                || bytes[at + 5] & 63 > 41
                || bytes[at + 6] & 0xc0 != 0)
        {
            return Err(Error::Cea708("invalid define-window extent/reserved bits"));
        }
        at = end;
    }
    Ok(())
}
