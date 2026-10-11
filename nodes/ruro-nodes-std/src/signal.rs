use std::f64::consts::TAU;

use ruro::prelude::*;

/// Sine wave generator.
#[derive(Node)]
#[node(kind = "std/sine", rate = 100)]
pub struct Sine {
    #[output]
    value: Output<Scalar>,
    /// Frequency in Hz.
    #[param(default = 0.5, min = 0, max = 1000)]
    frequency: f64,
    #[param(default = 1.0, min = 0, max = 1000)]
    amplitude: f64,
    #[param(default = 0.0, min = -1000, max = 1000)]
    offset: f64,
    phase: f64,
}

impl Node for Sine {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        // Integrate phase so frequency changes stay continuous.
        self.phase = (self.phase + TAU * self.frequency * ctx.dt().as_secs_f64()) % TAU;
        self.value
            .send(Scalar(self.offset + self.amplitude * self.phase.sin()))
    }
}

/// Multiplies every sample by a constant.
#[derive(Node)]
#[node(kind = "std/gain")]
pub struct Gain {
    #[input]
    input: Input<Scalar>,
    #[output]
    output: Output<Scalar>,
    #[param(default = 1.0, min = -1000, max = 1000)]
    k: f64,
}

impl Node for Gain {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(s) = self.input.recv() {
            self.output.send(Scalar(s.0 * self.k))?;
        }
        Ok(())
    }
}
