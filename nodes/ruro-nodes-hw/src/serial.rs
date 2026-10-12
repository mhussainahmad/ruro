//! The `hw/serial` node: raw bytes to and from a serial port.

use std::time::Duration;

use ruro::prelude::*;

use crate::io::{SerialTransport, Transport};

/// Raw serial port. Writes every `tx` message as-is and publishes whatever
/// bytes arrived since the previous step on `rx`.
#[derive(Node)]
#[node(kind = "hw/serial", rate = 200)]
pub struct Serial {
    #[input]
    tx: Input<Bytes>,
    #[output]
    rx: Output<Bytes>,
    /// Serial device.
    #[param(default = "/dev/ttyUSB0")]
    device: String,
    #[param(default = 115200, min = 50, max = 4000000)]
    baud: u32,
    /// Largest chunk published at once.
    #[param(default = 4096, min = 1, max = 1048576)]
    max_chunk: u32,
    port: Option<Box<dyn Transport>>,
}

impl Node for Serial {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        let t = SerialTransport::open(&self.device, self.baud)
            .with_context(|| format!("opening {}", self.device))?;
        self.port = Some(Box::new(t));
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let port = self.port.as_mut().context("port not open")?;
        while let Some(m) = self.tx.recv() {
            port.send(&m.0)?;
        }
        let mut buf = vec![0u8; self.max_chunk as usize];
        let mut n = 0;
        while n < buf.len() {
            let got = port.recv(&mut buf[n..], Duration::ZERO)?;
            if got == 0 {
                break;
            }
            n += got;
        }
        if n > 0 {
            buf.truncate(n);
            self.rx.send(Bytes(buf))?;
        }
        Ok(())
    }
}
