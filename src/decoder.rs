use std::time::Duration;

use bitvec::vec::BitVec;

pub trait DelayDecoder {
    fn push_duration(&mut self, duration: Duration);
    fn close(self) -> BitVec;
}

#[derive(Debug)]
pub struct ThresholdDelayDecoder {
    threshold: Duration,
    bits: BitVec,
}

impl DelayDecoder for ThresholdDelayDecoder {
    fn push_duration(&mut self, duration: Duration) {
        self.bits.push(duration >= self.threshold);
    }

    fn close(self) -> BitVec {
        self.bits
    }
}

#[derive(Default, Debug)]
pub struct AverageDelayDecoder {
    durations: Vec<Duration>,
}

impl AverageDelayDecoder {
    pub const fn new() -> Self {
        Self {
            durations: Vec::new(),
        }
    }
}

impl DelayDecoder for AverageDelayDecoder {
    fn push_duration(&mut self, duration: Duration) {
        self.durations.push(duration);
    }

    fn close(self) -> BitVec {
        if self.durations.len() < 2 {
            BitVec::EMPTY
        } else {
            let duration_sum: Duration = self.durations.iter().sum();
            let average_duration = duration_sum / self.durations.len() as u32;

            self.durations
                .into_iter()
                .map(|duration| duration >= average_duration)
                .collect()
        }
    }
}