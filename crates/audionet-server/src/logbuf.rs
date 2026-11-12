//! The server's recent log lines, kept in memory for the admin page, so it
//! can show them on any system (with or without journald). Bounded: the
//! oldest lines go first.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Lines kept.
pub const KEEP: usize = 2000;

static LINES: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn lines() -> &'static Mutex<VecDeque<String>> {
    LINES.get_or_init(|| Mutex::new(VecDeque::with_capacity(KEEP)))
}

/// The last `n` lines, oldest first.
pub fn recent(n: usize) -> Vec<String> {
    let g = lines().lock().unwrap_or_else(|e| e.into_inner());
    let skip = g.len().saturating_sub(n);
    g.iter().skip(skip).cloned().collect()
}

fn push(line: String) {
    let mut g = lines().lock().unwrap_or_else(|e| e.into_inner());
    if g.len() == KEEP {
        g.pop_front();
    }
    g.push_back(line);
}

/// The tracing layer that fills the buffer.
#[derive(Debug)]
pub struct BufferLayer;

struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.rest, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for BufferLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut f = Fields {
            message: String::new(),
            rest: String::new(),
        };
        event.record(&mut f);
        push(format!(
            "{} {} {}: {}{}",
            utc_now(),
            meta.level(),
            meta.target(),
            f.message,
            f.rest
        ));
    }
}

/// "YYYY-MM-DD HH:MM:SS UTC" without a date library.
pub fn utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn utc_now() -> String {
    utc(crate::db::now_s())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(utc(0), "1970-01-01 00:00:00");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(utc(1_794_444_000), "2026-11-12 00:40:00");
    }

    #[test]
    fn bounded_and_ordered() {
        for i in 0..KEEP + 5 {
            push(format!("line {i}"));
        }
        let last = recent(3);
        assert_eq!(
            last,
            vec![
                format!("line {}", KEEP + 2),
                format!("line {}", KEEP + 3),
                format!("line {}", KEEP + 4)
            ]
        );
        assert_eq!(recent(KEEP * 2).len(), KEEP);
    }
}
