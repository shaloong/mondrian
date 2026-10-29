//! Independent, fixed-storage native DSP oracle. No host scheduler/IPC types.
#![deny(unsafe_code)]

pub const VOICES: usize = 8;
pub const RELEASE: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    pub port: i16,
    pub channel: i16,
    pub key: i16,
    pub id: i32,
}

impl Address {
    pub fn valid(self) -> bool {
        (0..=1).contains(&self.port)
            && (0..=15).contains(&self.channel)
            && (0..=127).contains(&self.key)
            && self.id >= 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Held,
    Release(u8),
    // The real native voice is ended, but retain its original tuple until Off
    // so the fixture can reject host pairing corruption independently.
    EndedAwaitingOff,
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    address: Address,
    weight: f64,
    scenario: u8,
    early_left: u8,
    phase: Phase,
}

#[derive(Debug, Clone, Copy)]
pub struct End {
    pub address: Address,
    pub scenario: u8,
}

pub struct Gate {
    voices: [Option<Voice>; VOICES],
    pub exact_offs: u32,
}

impl Gate {
    pub const fn new() -> Self {
        Self { voices: [None; VOICES], exact_offs: 0 }
    }

    pub fn on(&mut self, address: Address, velocity: f64, scenario: u8) -> bool {
        if !address.valid()
            || !velocity.is_finite()
            || !(0.0..=1.0).contains(&velocity)
            || scenario > 7
            || self.voices.iter().flatten().any(|voice| voice.address.id == address.id)
        {
            return false;
        }
        let Some(slot) = self.voices.iter_mut().find(|slot| slot.is_none()) else {
            return false;
        };
        // Zero velocity remains a real Held voice: weight is never zero.
        let weight = (1.0 + velocity) * f64::from(address.port + 1) / 32.0;
        *slot = Some(Voice {
            address,
            weight,
            scenario,
            early_left: if scenario == 1 { 2 } else { 0 },
            phase: Phase::Held,
        });
        true
    }

    pub fn off(&mut self, address: Address, velocity: f64) -> bool {
        if !address.valid() || !velocity.is_finite() || !(0.0..=1.0).contains(&velocity) {
            return false;
        }
        let Some(next) = self.exact_offs.checked_add(1) else {
            return false;
        };
        let Some(slot) = self
            .voices
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|voice| voice.address == address))
        else {
            return false;
        };
        let Some(voice) = slot.as_mut() else {
            return false;
        };
        match voice.phase {
            Phase::Held => voice.phase = Phase::Release(RELEASE),
            Phase::EndedAwaitingOff => *slot = None,
            Phase::Release(_) => return false,
        }
        self.exact_offs = next;
        true
    }

    // Each accepted sample advances precisely once, independent of block size.
    // Ends occur AFTER the final audible sample; the caller emits their ignored
    // native timestamps, never infers an author or transport position.
    pub fn sample(&mut self) -> (f64, [Option<End>; VOICES]) {
        let mut coefficient = 0.0;
        let mut ends = [None; VOICES];
        for (index, slot) in self.voices.iter_mut().enumerate() {
            let Some(voice) = slot.as_mut() else { continue };
            let ended = match voice.phase {
                Phase::Held => {
                    coefficient += voice.weight;
                    if voice.early_left != 0 {
                        voice.early_left -= 1;
                        if voice.early_left == 0 {
                            voice.phase = Phase::EndedAwaitingOff;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                Phase::Release(left) => {
                    coefficient += voice.weight * f64::from(left) / f64::from(RELEASE);
                    if left == 1 {
                        true
                    } else {
                        voice.phase = Phase::Release(left - 1);
                        false
                    }
                }
                Phase::EndedAwaitingOff => false,
            };
            if ended {
                ends[index] = Some(End { address: voice.address, scenario: voice.scenario });
                if voice.phase != Phase::EndedAwaitingOff {
                    *slot = None;
                }
            }
        }
        (coefficient, ends)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(port: i16, id: i32) -> Address {
        Address { port, channel: 0, key: 60, id }
    }

    #[test]
    fn zero_on_two_stacked_ids_and_release_have_independent_exact_pcm() {
        let mut gate = Gate::new();
        assert!(gate.on(address(0, 10), 0.0, 0));
        assert!(gate.on(address(1, 11), 1.0, 0));
        assert_eq!(gate.sample().0, 5.0 / 32.0);
        assert!(gate.off(address(0, 10), 0.25));
        for expected in [5.0 / 32.0, 19.0 / 128.0, 9.0 / 64.0] {
            let (sample, ends) = gate.sample();
            assert_eq!(sample, expected);
            assert!(ends.iter().all(Option::is_none));
        }
        let (sample, ends) = gate.sample();
        assert_eq!(sample, 17.0 / 128.0);
        assert_eq!(ends[0].unwrap().address, address(0, 10));
        assert_eq!(gate.sample().0, 1.0 / 8.0);
    }

    #[test]
    fn early_end_retains_tuple_and_rejects_changed_off_or_second_capacity_credit() {
        let mut gate = Gate::new();
        for id in 0..8 {
            assert!(gate.on(address(0, id), 0.0, 1));
        }
        assert_eq!(gate.sample().0, 1.0 / 4.0);
        let (sample, ends) = gate.sample();
        assert_eq!(sample, 1.0 / 4.0);
        assert!(ends.iter().all(Option::is_some));
        assert_eq!(gate.sample().0, 0.0);
        assert!(!gate.on(address(0, 9), 1.0, 0));
        assert!(!gate.off(address(1, 0), 0.0));
        assert_eq!(gate.exact_offs, 0);
        assert!(gate.off(address(0, 0), 0.0));
        assert_eq!(gate.exact_offs, 1);
        assert!(gate.on(address(0, 9), 1.0, 0));
        assert!(!gate.off(address(0, 0), 0.0));
    }

    #[test]
    fn same_port_same_key_off_does_not_release_the_other_id() {
        let mut gate = Gate::new();
        assert!(gate.on(address(0, 20), 0.0, 0));
        assert!(gate.on(address(0, 21), 1.0, 0));
        assert_eq!(gate.sample().0, 3.0 / 32.0);
        assert!(gate.off(address(0, 20), 0.0));
        for expected in [3.0 / 32.0, 11.0 / 128.0, 5.0 / 64.0, 9.0 / 128.0] {
            assert_eq!(gate.sample().0, expected);
        }
        assert_eq!(gate.sample().0, 1.0 / 16.0);
        assert_eq!(gate.exact_offs, 1);
    }
}
