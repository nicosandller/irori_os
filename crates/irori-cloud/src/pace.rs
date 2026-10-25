use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};

/// How long to wait before trying again after a failure: 1 s, doubling to 5 minutes, back to the
/// start after a success.
#[derive(Debug, Clone)]
pub struct Backoff {
    next: Duration,
}

impl Backoff {
    const FIRST: Duration = Duration::from_secs(1);
    const MOST: Duration = Duration::from_secs(5 * 60);

    pub fn new() -> Self {
        Self { next: Self::FIRST }
    }

    /// Something failed: how long to wait before the next try.
    pub fn failed(&mut self) -> Duration {
        let wait = self.next;
        self.next = (self.next * 2).min(Self::MOST);
        wait
    }

    pub fn succeeded(&mut self) {
        self.next = Self::FIRST;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// The longest a vendor's "slow down" is taken at its word. Past it, Irori asks again: a
/// garbled or huge header mustn't silence an extension for good.
pub const LONGEST_WAIT: Duration = Duration::from_secs(60 * 60);

/// How long a vendor asked to be left alone (`Retry-After` in seconds, or the `RateLimit-Reset`
/// draft's seconds), at most [`LONGEST_WAIT`]. Many APIs send `X-RateLimit-Reset` as the Unix
/// time the limit resets instead: a value that large is read as that time. An HTTP date isn't
/// read: no vendor here sends one.
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    wait_from(headers, now)
}

fn wait_from(headers: &HeaderMap, now: u64) -> Option<Duration> {
    // A count of seconds this large (over a year) can only be a Unix time.
    const A_YEAR: u64 = 365 * 86_400;
    [RETRY_AFTER.as_str(), "ratelimit-reset", "x-ratelimit-reset"]
        .iter()
        .filter_map(|name| headers.get(*name))
        .filter_map(|value| value.to_str().ok())
        .filter_map(|text| text.trim().parse::<u64>().ok())
        .map(|value| {
            let seconds = if value > A_YEAR {
                value.saturating_sub(now)
            } else {
                value
            };
            Duration::from_secs(seconds).min(LONGEST_WAIT)
        })
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn backoff_doubles_to_five_minutes_and_starts_again_after_a_success() {
        let mut backoff = Backoff::new();
        let waits: Vec<u64> = (0..11).map(|_| backoff.failed().as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
        backoff.succeeded();
        assert_eq!(backoff.failed(), Duration::from_secs(1));
    }

    #[test]
    fn retry_after_reads_seconds() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert("ratelimit-reset", HeaderValue::from_static("30"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
        headers.insert(RETRY_AFTER, HeaderValue::from_static(" 120 "));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(120)));
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
    }

    #[test]
    fn a_reset_given_as_unix_time_is_read_as_that_time_and_no_wait_is_endless() {
        let now = 1_800_000_000;
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-ratelimit-reset",
            HeaderValue::from_str(&(now + 90).to_string()).expect("a header"),
        );
        assert_eq!(wait_from(&headers, now), Some(Duration::from_secs(90)));
        // Already past: try now.
        headers.insert(
            "x-ratelimit-reset",
            HeaderValue::from_str(&(now - 5).to_string()).expect("a header"),
        );
        assert_eq!(wait_from(&headers, now), Some(Duration::ZERO));
        // Nothing waits longer than an hour, however large the number.
        headers.insert(RETRY_AFTER, HeaderValue::from_static("86400"));
        assert_eq!(wait_from(&headers, now), Some(LONGEST_WAIT));
        headers.insert(
            RETRY_AFTER,
            HeaderValue::from_static("18446744073709551615"),
        );
        assert_eq!(wait_from(&headers, now), Some(LONGEST_WAIT));
    }
}
