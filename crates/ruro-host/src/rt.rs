//! Real-time setup for a process group: CPU pinning, SCHED_FIFO priority and
//! locked memory.
//!
//! Without the needed privileges (CAP_SYS_NICE / an `rtprio` limit for
//! SCHED_FIFO, a large enough `memlock` limit for mlockall) the group still
//! runs, with a warning saying what was not applied.

use std::io;

use anyhow::{Result, bail};
use ruro_graph::Realtime;

/// True if `rt` asks for real-time scheduling (as opposed to only deadlines).
pub fn wants_realtime(rt: &Realtime) -> bool {
    rt.priority.is_some() || !rt.cpus.is_empty() || rt.deterministic
}

/// Pins the calling thread to `cpus`.
pub fn pin_current_thread(cpus: &[usize]) -> Result<()> {
    // SAFETY: cpu_set_t is plain data; CPU_ZERO/CPU_SET only write into it.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    let max = 8 * std::mem::size_of::<libc::cpu_set_t>();
    for &cpu in cpus {
        if cpu >= max {
            bail!("CPU {cpu} is out of range");
        }
        // SAFETY: `cpu` is within the set's capacity, checked above.
        unsafe { libc::CPU_SET(cpu, &mut set) };
    }
    // SAFETY: pid 0 is the calling thread; `set` is a valid cpu_set_t.
    let r = unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set) };
    if r != 0 {
        bail!(
            "sched_setaffinity({cpus:?}): {}",
            io::Error::last_os_error()
        );
    }
    Ok(())
}

/// CPUs the calling thread may run on.
pub fn current_affinity() -> Result<Vec<usize>> {
    // SAFETY: as above.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: pid 0 is the calling thread; `set` is writable and sized correctly.
    let r = unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&set), &mut set) };
    if r != 0 {
        bail!("sched_getaffinity: {}", io::Error::last_os_error());
    }
    let max = 8 * std::mem::size_of::<libc::cpu_set_t>();
    // SAFETY: reads within the set's capacity.
    Ok((0..max)
        .filter(|&c| unsafe { libc::CPU_ISSET(c, &set) })
        .collect())
}

/// Switches the calling thread to SCHED_FIFO at `priority` (1-99).
pub fn set_fifo_priority(priority: u8) -> Result<()> {
    if !(1..=99).contains(&priority) {
        bail!("SCHED_FIFO priority {priority} is outside 1-99");
    }
    let param = libc::sched_param {
        sched_priority: priority as i32,
    };
    // SAFETY: pid 0 is the calling thread; `param` is valid for the call.
    let r = unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) };
    if r != 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EPERM) {
            bail!(
                "SCHED_FIFO priority {priority} not permitted ({e}); grant CAP_SYS_NICE or raise \
                 the rtprio limit (ulimit -r, /etc/security/limits.conf)"
            );
        }
        bail!("sched_setscheduler(SCHED_FIFO, {priority}): {e}");
    }
    Ok(())
}

/// Locks the process's current and future memory into RAM so page faults
/// cannot stall real-time threads.
pub fn lock_memory() -> Result<()> {
    // SAFETY: mlockall has no memory-safety preconditions.
    let r = unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) };
    if r != 0 {
        bail!(
            "mlockall: {} (raise the memlock limit: ulimit -l)",
            io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Makes timed sleeps of the calling thread wake on time. By default Linux
/// may delay a normal thread's timer wakeups by up to 50 µs to batch them.
pub fn precise_timers() {
    // SAFETY: PR_SET_TIMERSLACK only changes the calling thread's slack.
    unsafe { libc::prctl(libc::PR_SET_TIMERSLACK, 1 as libc::c_ulong, 0, 0, 0) };
}

/// Applies the per-thread settings of `rt` to the calling thread, logging
/// what could not be applied.
pub fn apply_thread(rt: &Realtime, who: &str) {
    if !rt.cpus.is_empty()
        && let Err(e) = pin_current_thread(&rt.cpus)
    {
        tracing::warn!("{who}: cannot pin to CPUs: {e:#}");
    }
    if let Some(p) = rt.priority
        && let Err(e) = set_fifo_priority(p)
    {
        tracing::warn!("{who}: running without real-time priority: {e:#}");
    }
}

/// Applies the process-wide settings of `rt`: memory is locked when the
/// group asks for a real-time priority.
pub fn apply_process(rt: &Realtime) {
    if rt.priority.is_some() {
        match lock_memory() {
            Ok(()) => tracing::info!("memory locked"),
            Err(e) => tracing::warn!("running with unlocked memory: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_a_thread() {
        let first = current_affinity().unwrap()[0];
        let got = std::thread::spawn(move || {
            pin_current_thread(&[first]).unwrap();
            current_affinity().unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(got, vec![first]);
        assert!(pin_current_thread(&[100_000]).is_err());
    }

    #[test]
    fn fifo_priority_fails_cleanly_without_privileges() {
        // On a machine where this succeeds the thread simply runs FIFO until
        // it exits; without CAP_SYS_NICE the error must explain what to do.
        let r = std::thread::spawn(|| set_fifo_priority(10)).join().unwrap();
        if let Err(e) = r {
            let msg = format!("{e:#}");
            assert!(
                msg.contains("not permitted") || msg.contains("sched_setscheduler"),
                "{msg}"
            );
        }
        assert!(set_fifo_priority(0).is_err());
        assert!(set_fifo_priority(100).is_err());
    }

    #[test]
    fn only_scheduling_settings_count_as_realtime() {
        let mut rt = Realtime::default();
        rt.deadlines_us.insert("a".into(), 100);
        assert!(!wants_realtime(&rt));
        rt.priority = Some(50);
        assert!(wants_realtime(&rt));
    }
}
