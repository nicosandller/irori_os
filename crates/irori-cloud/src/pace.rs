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

/// How long a vendor asked to be left alone (`Retry-After` in seconds, or the `RateLimit-Reset`
/// draft's seconds). An HTTP date isn't read: no vendor here sends one.
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    [RETRY_AFTER.as_str(), "ratelimit-reset", "x-ratelimit-reset"]
        .iter()
        .filter_map(|name| headers.get(*name))
        .filter_map(|value| value.to_str().ok())
        .filter_map(|text| text.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
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
}
