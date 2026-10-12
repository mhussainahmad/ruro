//! Byte-stream transports shared by the serial protocol drivers.
//!
//! Protocol code talks to a [`Transport`] instead of a serial port, so every
//! driver can be exercised against [`MockTransport`] without hardware.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

/// A bidirectional byte stream with a read timeout, typically a serial port.
pub trait Transport: Send {
    /// Writes all of `data`.
    fn send(&mut self, data: &[u8]) -> io::Result<()>;

    /// Reads up to `buf.len()` bytes, waiting at most `timeout` for the first
    /// byte. Returns `Ok(0)` on timeout.
    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;

    /// Discards unread input.
    fn clear_input(&mut self) -> io::Result<()> {
        let mut scratch = [0u8; 256];
        while self.recv(&mut scratch, Duration::ZERO)? > 0 {}
        Ok(())
    }

    /// Sets the DTR modem line, where the transport has one.
    fn set_dtr(&mut self, _level: bool) -> io::Result<()> {
        Ok(())
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn send(&mut self, data: &[u8]) -> std::io::Result<()> {
        (**self).send(data)
    }

    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> std::io::Result<usize> {
        (**self).recv(buf, timeout)
    }

    fn clear_input(&mut self) -> std::io::Result<()> {
        (**self).clear_input()
    }

    fn set_dtr(&mut self, level: bool) -> std::io::Result<()> {
        (**self).set_dtr(level)
    }
}

/// Reads exactly `buf.len()` bytes or fails with `TimedOut` once `timeout` has passed.
pub fn recv_exact(t: &mut dyn Transport, buf: &mut [u8], timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut filled = 0;
    while filled < buf.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        let n = t.recv(&mut buf[filled..], left)?;
        if n == 0 && left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed out after {filled} of {} bytes", buf.len()),
            ));
        }
        filled += n;
    }
    Ok(())
}

/// A serial port opened with the `serialport` crate (no libudev needed).
pub struct SerialTransport {
    port: serialport::TTYPort,
}

impl SerialTransport {
    /// Opens `device` at `baud`, 8N1, no flow control.
    pub fn open(device: &str, baud: u32) -> io::Result<Self> {
        let port = serialport::new(device, baud)
            .timeout(Duration::from_millis(100))
            .open_native()
            .map_err(io::Error::other)?;
        Ok(Self { port })
    }

    /// Wraps an already open port, e.g. one end of a pseudo-terminal pair.
    pub fn from_port(port: serialport::TTYPort) -> Self {
        Self { port }
    }
}

impl Transport for SerialTransport {
    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.port.write_all(data)?;
        self.port.flush()
    }

    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        use serialport::SerialPort;
        // A zero timeout would block forever in some termios setups; poll briefly instead.
        let timeout = timeout.max(Duration::from_millis(1));
        if self.port.timeout() != timeout {
            self.port.set_timeout(timeout).map_err(io::Error::other)?;
        }
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e),
        }
    }

    fn clear_input(&mut self) -> io::Result<()> {
        use serialport::SerialPort;
        self.port
            .clear(serialport::ClearBuffer::Input)
            .map_err(io::Error::other)
    }

    fn set_dtr(&mut self, level: bool) -> io::Result<()> {
        use serialport::SerialPort;
        self.port
            .write_data_terminal_ready(level)
            .map_err(io::Error::other)
    }
}

/// Answers written packets with scripted replies. Used by tests and examples.
pub type Responder = Box<dyn FnMut(&[u8]) -> Vec<u8> + Send>;

/// In-memory transport: records what is sent and plays back queued input.
#[derive(Default)]
pub struct MockTransport {
    /// Everything sent so far.
    pub sent: Vec<u8>,
    /// Bytes waiting to be read.
    pub input: VecDeque<u8>,
    /// Called on every `send`; whatever it returns is queued as input.
    pub responder: Option<Responder>,
    /// Current DTR level, if it was ever set.
    pub dtr: Option<bool>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// A transport whose device replies to each write with `f(written)`.
    pub fn with_responder(f: impl FnMut(&[u8]) -> Vec<u8> + Send + 'static) -> Self {
        Self {
            responder: Some(Box::new(f)),
            ..Self::default()
        }
    }

    /// Queues bytes for the next reads.
    pub fn push_input(&mut self, bytes: &[u8]) {
        self.input.extend(bytes);
    }

    /// Takes and clears the record of sent bytes.
    pub fn take_sent(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.sent)
    }
}

impl Transport for MockTransport {
    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.sent.extend_from_slice(data);
        if let Some(r) = self.responder.as_mut() {
            let reply = r(data);
            self.input.extend(reply);
        }
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        if self.input.is_empty() {
            // Nothing will ever arrive; avoid callers spinning at full speed.
            std::thread::sleep(timeout.min(Duration::from_millis(1)));
        }
        let n = buf.len().min(self.input.len());
        for (dst, src) in buf.iter_mut().zip(self.input.drain(..n)) {
            *dst = src;
        }
        Ok(n)
    }

    fn set_dtr(&mut self, level: bool) -> io::Result<()> {
        self.dtr = Some(level);
        Ok(())
    }
}

/// Parses a comma separated list such as `"1, 2,3"`.
pub fn parse_list<T: std::str::FromStr>(s: &str) -> Result<Vec<T>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().map_err(|_| format!("invalid list entry `{p}`")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_replies_and_times_out() {
        let mut t = MockTransport::with_responder(|w| w.iter().rev().copied().collect());
        t.send(&[1, 2, 3]).unwrap();
        let mut buf = [0u8; 3];
        recv_exact(&mut t, &mut buf, Duration::from_millis(10)).unwrap();
        assert_eq!(buf, [3, 2, 1]);
        let err = recv_exact(&mut t, &mut buf, Duration::from_millis(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn lists_parse() {
        assert_eq!(parse_list::<u8>(" 1, 2,3 ,").unwrap(), [1, 2, 3]);
        assert!(parse_list::<u8>("1,x").is_err());
        assert_eq!(parse_list::<f64>("").unwrap(), Vec::<f64>::new());
    }

    #[test]
    fn pty_pair_carries_bytes() {
        let (a, b) = serialport::TTYPort::pair().expect("openpty");
        let (mut a, mut b) = (SerialTransport::from_port(a), SerialTransport::from_port(b));
        a.send(b"hello").unwrap();
        let mut buf = [0u8; 5];
        recv_exact(&mut b, &mut buf, Duration::from_secs(1)).unwrap();
        assert_eq!(&buf, b"hello");
        b.send(&[0xFF, 0x00, 0x7F]).unwrap();
        let mut buf = [0u8; 3];
        recv_exact(&mut a, &mut buf, Duration::from_secs(1)).unwrap();
        assert_eq!(buf, [0xFF, 0x00, 0x7F]);
    }
}
