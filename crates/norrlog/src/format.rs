//! Numbers and times, written exactly as the Electron renderer writes them
//! (`gui/renderer/core.js`), so a port reads the same as what it replaces.
//! The overlay (`crates/norrlogd/assets/overlay.js`) has its own copies of
//! `num` and `dur`; all three agree.

/// `—` for nothing; `1.23B`, `1.23M`, `12.3k` from ten thousand up;
/// otherwise rounded, with thousands separators.
pub fn num(n: Option<f64>) -> String {
    let Some(n) = n else { return "—".into() };
    if n >= 1e9 {
        format!("{:.2}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.2}M", n / 1e6)
    } else if n >= 1e4 {
        format!("{:.1}k", n / 1e3)
    } else {
        grouped(round_half_up(n))
    }
}

/// Copper as whole platinum, rounded, with thousands separators: app.js'
/// `pp`, `(c / 1000).toLocaleString(undefined, { maximumFractionDigits: 0 })`.
pub fn pp(copper: u64) -> String {
    grouped(round_half_up(copper as f64 / 1000.))
}

/// `H:MM:SS`, `M:SS`, or `Ns`. Negative is zero.
pub fn dur(sec: f64) -> String {
    let sec = round_half_up(sec.max(0.0)).max(0) as u64;
    let (h, m, s) = (sec / 3600, sec % 3600 / 60, sec % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else if m > 0 {
        format!("{m}:{s:02}")
    } else {
        format!("{s}s")
    }
}

/// A log timestamp as `YYYY-MM-DD HH:MM`. The log writes local wall-clock
/// time and the daemon reads it as UTC, so formatting it as UTC gives back
/// the time the player saw. `—` for none.
pub fn when(ts: Option<i64>) -> String {
    match ts.filter(|&t| t != 0).and_then(|t| chrono::DateTime::from_timestamp(t, 0)) {
        Some(d) => d.format("%Y-%m-%d %H:%M").to_string(),
        None => "—".into(),
    }
}

/// [`when`] without the year: `MM-DD HH:MM`. Empty for none, as the
/// renderer's `when(ts).slice(5)` is.
pub fn when_short(ts: Option<i64>) -> String {
    let w = when(ts);
    w.get(5..).unwrap_or("").to_string()
}

/// "Now" on the log's clock: this machine's wall clock, read as if it were
/// UTC, as every timestamp from the daemon is.
pub fn now_log() -> i64 {
    let now = chrono::Local::now();
    now.timestamp() + i64::from(now.offset().local_minus_utc())
}

/// A coarse countdown to a log-clock time: `due`, `Hh Mm` or `Mm`.
pub fn until(ts: i64, now_log: i64) -> String {
    let left = ts - now_log;
    if left <= 0 {
        return "due".into();
    }
    let (h, m) = (left / 3600, left % 3600 / 60);
    if h > 0 { format!("{h}h {m}m") } else { format!("{m}m") }
}

/// JavaScript's `Math.round`: halves go up, also below zero.
fn round_half_up(n: f64) -> i64 {
    (n + 0.5).floor() as i64
}

/// `toLocaleString()` in the en-US form the renderer has always shown.
fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platinum_rounds_and_groups_rather_than_abbreviating() {
        assert_eq!(super::pp(169_141_704), "169,142");
        assert_eq!(super::pp(499), "0");
        assert_eq!(super::pp(500), "1");
    }

    #[test]
    fn num_matches_the_renderer() {
        assert_eq!(num(None), "—");
        assert_eq!(num(Some(0.0)), "0");
        assert_eq!(num(Some(1828.0)), "1,828");
        assert_eq!(num(Some(9999.4)), "9,999");
        assert_eq!(num(Some(12_345.0)), "12.3k");
        assert_eq!(num(Some(1_234_567.0)), "1.23M");
        assert_eq!(num(Some(2_500_000_000.0)), "2.50B");
        assert_eq!(num(Some(2.5)), "3");
    }

    #[test]
    fn dur_matches_the_renderer() {
        assert_eq!(dur(-4.0), "0s");
        assert_eq!(dur(18.0), "18s");
        assert_eq!(dur(65.0), "1:05");
        assert_eq!(dur(4316.0), "1:11:56");
    }

    #[test]
    fn when_is_the_log_clock_in_utc() {
        // 2026-09-13 15:07 as the log wrote it.
        let t = 1_789_312_020;
        assert_eq!(when(Some(t)), "2026-09-13 15:07");
        assert_eq!(when_short(Some(t)), "09-13 15:07");
        assert_eq!(when(None), "—");
        assert_eq!(when(Some(0)), "—");
        assert_eq!(when_short(None), "");
    }

    #[test]
    fn until_counts_down_in_hours_and_minutes() {
        assert_eq!(until(100, 200), "due");
        assert_eq!(until(100, 100), "due");
        assert_eq!(until(3600 * 2 + 600, 0), "2h 10m");
        assert_eq!(until(59 * 60 + 59, 0), "59m");
    }
}
