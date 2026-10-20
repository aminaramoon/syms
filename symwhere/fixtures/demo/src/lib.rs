pub mod mixer {
    pub struct Mixer {
        pub channels: u8,
        gain: f32,
    }

    impl Mixer {
        pub fn new(channels: u8) -> Self {
            Mixer { channels, gain: 1.0 }
        }

        pub fn apply(&mut self, sample: f32) -> f32 {
            sample * self.gain
        }
    }

    pub enum Mode {
        Stereo,
        Surround(u8),
    }

    pub const MAX_GAIN: f32 = 4.0;
}

pub fn normalize(samples: &mut [f32]) {
    for s in samples.iter_mut() {
        *s /= mixer::MAX_GAIN;
    }
}

pub struct GainNode {
    pub target: f32,
    ramp_ms: u32,
}

impl GainNode {
    pub fn ramp_to(&mut self, target: f32, ms: u32) { self.target = target; self.ramp_ms = ms; }
}
