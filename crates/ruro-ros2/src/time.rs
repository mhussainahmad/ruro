//! Conversions between ROS 2 time stamps and nanoseconds.

use crate::msgs::builtin_interfaces::msg::Time;

/// `builtin_interfaces/Time` from nanoseconds since the epoch.
pub fn time_from_ns(ns: u64) -> Time {
    Time {
        sec: (ns / 1_000_000_000) as i32,
        nanosec: (ns % 1_000_000_000) as u32,
    }
}

/// Nanoseconds since the epoch; negative times clamp to zero.
pub fn time_to_ns(t: &Time) -> u64 {
    if t.sec < 0 {
        return 0;
    }
    t.sec as u64 * 1_000_000_000 + u64::from(t.nanosec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let ns = 1_700_000_000_123_456_789;
        let t = time_from_ns(ns);
        assert_eq!((t.sec, t.nanosec), (1_700_000_000, 123_456_789));
        assert_eq!(time_to_ns(&t), ns);
        assert_eq!(
            time_to_ns(&Time {
                sec: -1,
                nanosec: 5
            }),
            0
        );
    }
}
