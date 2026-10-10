//! Five-minute and hourly summaries, in the same shape Home Assistant keeps.
//!
//! A sensor with a `state_class` gets a row every five minutes. Those rows are deleted with the
//! raw changes. Each finished hour is rolled up from its five-minute rows and kept for
//! `summary_days`, or forever when that is unset. A measurement stores a time-weighted mean,
//! the min, the max, and the last value. A total stores how much the number changed, and the
//! last value. A `total_increasing` sensor treats a drop as a reset: the new value is added,
//! not subtracted.

use irori_types::{Availability, EntityId, EntityState, State, StateClass, Timestamp, Typed};

use rusqlite::{Connection, OptionalExtension};

/// Five minutes, in nanoseconds. Periods are aligned to the Unix epoch, as Home Assistant's are.
pub(crate) const FIVE_MIN_NS: i64 = 300_000_000_000;

/// One hour, in nanoseconds. Twelve five-minute periods.
pub(crate) const HOUR_NS: i64 = FIVE_MIN_NS * 12;

/// Hourly points one summary read returns. A year of hours is 8760; the newest are kept.
const MAX_SUMMARIES: usize = 9_000;

/// One stored summary. `mean` is set for a measurement, `sum` for a total. The other is empty.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stat {
    pub mean: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub sum: Option<f64>,
    pub last: Option<f64>,
}

/// One hourly point, as the page and the API show it. `value` is the mean, or the sum when the
/// sensor counts rather than measures.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryPoint {
    pub start: Timestamp,
    pub value: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// The greatest multiple of `period` that is still `<= ns`.
pub(crate) fn align_down(ns: i64, period: i64) -> i64 {
    ns - ns.rem_euclid(period)
}

/// A measurement over `[start, end)`. The mean is weighted by how long each value held. A value
/// carried in from before `start` holds until the first sample. Time before any known value is
/// not part of the mean.
pub(crate) fn measurement(
    start: i64,
    end: i64,
    carry: Option<f64>,
    samples: &[(i64, f64)],
) -> Option<Stat> {
    if end <= start {
        return None;
    }
    let mut weighted = 0.0;
    let mut weight = 0.0;
    let mut current = carry;
    let mut cursor = start;
    let mut low = None;
    let mut high = None;
    let mut last = None;
    if let Some(value) = carry {
        low = Some(value);
        high = Some(value);
        last = Some(value);
    }
    for &(at, value) in samples {
        if at < start || at >= end {
            continue;
        }
        if let Some(held) = current {
            let span = (at - cursor) as f64;
            if span > 0.0 {
                weighted += held * span;
                weight += span;
            }
        }
        current = Some(value);
        cursor = at;
        low = Some(low.map_or(value, |so_far: f64| so_far.min(value)));
        high = Some(high.map_or(value, |so_far: f64| so_far.max(value)));
        last = Some(value);
    }
    if let Some(held) = current {
        let span = (end - cursor) as f64;
        if span > 0.0 {
            weighted += held * span;
            weight += span;
        }
    }
    if weight <= 0.0 {
        return None;
    }
    Some(Stat {
        mean: Some(weighted / weight),
        min: low,
        max: high,
        sum: None,
        last,
    })
}

/// How much a total changed across `samples`. A drop on `total_increasing` is a meter reset:
/// the new reading is added, and the drop is not subtracted. No previous reading and no sample
/// is nothing. A carried value and no sample is a change of zero.
pub(crate) fn change(
    class: StateClass,
    carry: Option<f64>,
    samples: &[(i64, f64)],
) -> Option<Stat> {
    if carry.is_none() && samples.is_empty() {
        return None;
    }
    let mut sum = 0.0;
    let mut previous = carry;
    let mut last = carry;
    let mut moved = false;
    for &(_, value) in samples {
        if let Some(before) = previous {
            let delta = value - before;
            match class {
                StateClass::Total => sum += delta,
                StateClass::TotalIncreasing => {
                    if delta >= 0.0 {
                        sum += delta;
                    } else {
                        sum += value;
                    }
                }
                StateClass::Measurement => return None,
            }
            moved = true;
        }
        previous = Some(value);
        last = Some(value);
    }
    let sum = if moved || carry.is_some() {
        Some(sum)
    } else {
        None
    };
    Some(Stat {
        mean: None,
        min: None,
        max: None,
        sum,
        last,
    })
}

/// An hour from its five-minute rows: mean of means, min of mins, max of maxes, sum of sums,
/// and the last value. An hour with no rows is skipped.
pub(crate) fn rollup(parts: &[Stat]) -> Option<Stat> {
    if parts.is_empty() {
        return None;
    }
    let mut mean_sum = 0.0;
    let mut mean_count = 0.0;
    let mut low = None;
    let mut high = None;
    let mut sum = None;
    let mut last = None;
    for part in parts {
        if let Some(mean) = part.mean {
            mean_sum += mean;
            mean_count += 1.0;
        }
        if let Some(value) = part.min {
            low = Some(low.map_or(value, |so_far: f64| so_far.min(value)));
        }
        if let Some(value) = part.max {
            high = Some(high.map_or(value, |so_far: f64| so_far.max(value)));
        }
        if let Some(value) = part.sum {
            sum = Some(sum.unwrap_or(0.0) + value);
        }
        if part.last.is_some() {
            last = part.last;
        }
    }
    if mean_count == 0.0 && sum.is_none() && last.is_none() {
        return None;
    }
    Some(Stat {
        mean: (mean_count > 0.0).then_some(mean_sum / mean_count),
        min: low,
        max: high,
        sum,
        last,
    })
}

pub(crate) fn number_of(state: &EntityState) -> Option<f64> {
    if state.availability != Availability::Available {
        return None;
    }
    match state.state.as_ref().map(State::primary) {
        Some(Typed::Number(value)) if value.is_finite() => Some(value),
        _ => None,
    }
}

struct Tracked {
    entity_id: String,
    class: StateClass,
    compiled_until: i64,
}

/// Compiles up to `budget` closed five-minute periods, and the hours those periods finish.
/// `only` limits the work to one entity, which a summary read uses so the answer includes the
/// periods that have already closed. Periods that are still open are left for a later pass.
pub(crate) fn compile(
    connection: &mut Connection,
    budget: usize,
    only: Option<&EntityId>,
    now_ns: i64,
) -> Result<(), String> {
    if budget == 0 {
        return Ok(());
    }
    let tracked = list_tracked(connection, only)?;
    let mut left = budget;
    for entity in tracked {
        if left == 0 {
            break;
        }
        let used = compile_entity(connection, &entity, left, now_ns)?;
        left = left.saturating_sub(used);
    }
    Ok(())
}

fn list_tracked(connection: &Connection, only: Option<&EntityId>) -> Result<Vec<Tracked>, String> {
    let mut statement = connection
        .prepare(
            "SELECT entity_id, class, compiled_until_ns FROM statistic_entities
             WHERE ?1 IS NULL OR entity_id = ?1
             ORDER BY entity_id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(rusqlite::params![only.map(EntityId::as_str)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut tracked = Vec::new();
    for row in rows {
        let (entity_id, class, compiled_until) = row.map_err(|error| error.to_string())?;
        let Some(class) = class_from(&class) else {
            tracing::warn!(entity = %entity_id, class, "a statistic has a class this recorder does not know");
            continue;
        };
        tracked.push(Tracked {
            entity_id,
            class,
            compiled_until,
        });
    }
    Ok(tracked)
}

fn compile_entity(
    connection: &mut Connection,
    entity: &Tracked,
    budget: usize,
    now_ns: i64,
) -> Result<usize, String> {
    let open_start = align_down(now_ns, FIVE_MIN_NS);
    let mut cursor = align_down(entity.compiled_until, FIVE_MIN_NS);
    if cursor >= open_start {
        return Ok(0);
    }
    let available = (open_start - cursor) / FIVE_MIN_NS;
    let count = available.min(i64::try_from(budget).unwrap_or(available));
    if count <= 0 {
        return Ok(0);
    }
    let window_end = cursor + count * FIVE_MIN_NS;
    let carry = value_before(connection, &entity.entity_id, cursor)?;
    let samples = values_between(connection, &entity.entity_id, cursor, window_end)?;

    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let mut sample_at = 0;
    let mut carried = carry;
    let mut done = 0usize;
    while done < usize::try_from(count).unwrap_or(usize::MAX) && cursor < window_end {
        let end = cursor + FIVE_MIN_NS;
        let mut period = Vec::new();
        while sample_at < samples.len() && samples[sample_at].0 < end {
            period.push(samples[sample_at]);
            sample_at += 1;
        }
        let stat = match entity.class {
            StateClass::Measurement => measurement(cursor, end, carried, &period),
            StateClass::Total | StateClass::TotalIncreasing => {
                change(entity.class, carried, &period)
            }
        };
        if let Some(stat) = stat {
            insert_period(
                &transaction,
                "statistics_short_term",
                &entity.entity_id,
                cursor,
                &stat,
            )?;
        }
        if let Some(&(_, value)) = period.last() {
            carried = Some(value);
        }
        if end.rem_euclid(HOUR_NS) == 0 {
            let parts = hour_parts(&transaction, &entity.entity_id, end - HOUR_NS, end)?;
            if let Some(hour) = rollup(&parts) {
                insert_period(
                    &transaction,
                    "statistics",
                    &entity.entity_id,
                    end - HOUR_NS,
                    &hour,
                )?;
            }
        }
        cursor = end;
        done += 1;
    }
    transaction
        .execute(
            "UPDATE statistic_entities SET compiled_until_ns = ?1 WHERE entity_id = ?2",
            rusqlite::params![cursor, entity.entity_id],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(done)
}

fn hour_parts(
    connection: &Connection,
    entity_id: &str,
    start: i64,
    end: i64,
) -> Result<Vec<Stat>, String> {
    let mut statement = connection
        .prepare(
            "SELECT mean, min, max, sum, last FROM statistics_short_term
             WHERE entity_id = ?1 AND start_ns >= ?2 AND start_ns < ?3
             ORDER BY start_ns",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(rusqlite::params![entity_id, start, end], read_stat)
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn read_stat(row: &rusqlite::Row<'_>) -> rusqlite::Result<Stat> {
    Ok(Stat {
        mean: row.get(0)?,
        min: row.get(1)?,
        max: row.get(2)?,
        sum: row.get(3)?,
        last: row.get(4)?,
    })
}

fn insert_period(
    connection: &Connection,
    table: &str,
    entity_id: &str,
    start: i64,
    stat: &Stat,
) -> Result<(), String> {
    // The table is one of the two this module creates. It is not a value from outside.
    let sql = match table {
        "statistics_short_term" => {
            "INSERT INTO statistics_short_term (entity_id, start_ns, mean, min, max, sum, last)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(entity_id, start_ns) DO UPDATE SET
                mean = excluded.mean, min = excluded.min, max = excluded.max,
                sum = excluded.sum, last = excluded.last"
        }
        "statistics" => {
            "INSERT INTO statistics (entity_id, start_ns, mean, min, max, sum, last)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(entity_id, start_ns) DO UPDATE SET
                mean = excluded.mean, min = excluded.min, max = excluded.max,
                sum = excluded.sum, last = excluded.last"
        }
        _ => return Err(format!("unknown statistics table {table}")),
    };
    connection
        .execute(
            sql,
            rusqlite::params![
                entity_id, start, stat.mean, stat.min, stat.max, stat.sum, stat.last
            ],
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Remembers that `entity_id` is summarized as `class`. The first class sticks: a later change
/// of class does not rebuild the rows already stored. Compilation starts at the oldest stored
/// change, or at the current period when nothing is stored yet.
pub(crate) fn note_class(
    connection: &Connection,
    entity_id: &EntityId,
    class: StateClass,
    now_ns: i64,
) -> Result<(), String> {
    // MIN of no rows is one row whose value is NULL, not an empty result. Reading that NULL
    // as an integer is what used to fail the write and mark the database unhealthy.
    let oldest: Option<i64> = connection
        .query_row(
            "SELECT MIN(updated_ns) FROM state_history WHERE entity_id = ?1",
            [entity_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let start = align_down(oldest.unwrap_or(now_ns), FIVE_MIN_NS);
    connection
        .execute(
            "INSERT INTO statistic_entities (entity_id, class, compiled_until_ns)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(entity_id) DO NOTHING",
            rusqlite::params![entity_id.as_str(), class_name(class), start],
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Hourly points with `start >= since`, oldest first. At most [`MAX_SUMMARIES`], keeping the
/// newest. `value` is the mean when the hour has one, and the sum otherwise.
pub(crate) fn summaries(
    connection: &Connection,
    entity_id: &EntityId,
    since_ns: i64,
) -> Result<Vec<SummaryPoint>, String> {
    let mut statement = connection
        .prepare(
            "SELECT start_ns, mean, min, max, sum FROM statistics
             WHERE entity_id = ?1 AND start_ns >= ?2
             ORDER BY start_ns DESC
             LIMIT ?3",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            rusqlite::params![
                entity_id.as_str(),
                since_ns,
                i64::try_from(MAX_SUMMARIES).unwrap_or(i64::MAX)
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<f64>>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                    row.get::<_, Option<f64>>(4)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?;
    let mut points = Vec::new();
    for row in rows {
        let (start_ns, mean, min, max, sum) = row.map_err(|error| error.to_string())?;
        let Some(value) = mean.or(sum) else {
            continue;
        };
        let Some(start) = timestamp_from_ns(start_ns) else {
            continue;
        };
        points.push(SummaryPoint {
            start,
            value,
            min,
            max,
        });
    }
    points.reverse();
    Ok(points)
}

/// The value a period carries in. An unavailable row still stores the last number, but it is
/// not a reading, and it is often the newest row when a compile starts on a new call. Once
/// retention has deleted the raw rows, the `last` already stored on a summary is that value.
fn value_before(
    connection: &Connection,
    entity_id: &str,
    before: i64,
) -> Result<Option<f64>, String> {
    if let Some(value) = latest_numeric(connection, entity_id, before)? {
        return Ok(Some(value));
    }
    if let Some(value) = compiled_last(connection, "statistics_short_term", entity_id, before)? {
        return Ok(Some(value));
    }
    compiled_last(connection, "statistics", entity_id, before)
}

fn latest_numeric(
    connection: &Connection,
    entity_id: &str,
    before: i64,
) -> Result<Option<f64>, String> {
    let mut statement = connection
        .prepare(
            "SELECT state FROM state_history
             WHERE entity_id = ?1 AND updated_ns < ?2
             ORDER BY updated_ns DESC, id DESC",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(rusqlite::params![entity_id, before], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        let text = row.map_err(|error| error.to_string())?;
        if let Some(value) = number_in(&text) {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn compiled_last(
    connection: &Connection,
    table: &str,
    entity_id: &str,
    before: i64,
) -> Result<Option<f64>, String> {
    // The table is one of the two this module creates. It is not a value from outside.
    let sql = match table {
        "statistics_short_term" => {
            "SELECT last FROM statistics_short_term
             WHERE entity_id = ?1 AND start_ns < ?2 AND last IS NOT NULL
             ORDER BY start_ns DESC LIMIT 1"
        }
        "statistics" => {
            "SELECT last FROM statistics
             WHERE entity_id = ?1 AND start_ns < ?2 AND last IS NOT NULL
             ORDER BY start_ns DESC LIMIT 1"
        }
        _ => return Err(format!("unknown statistics table {table}")),
    };
    connection
        .query_row(sql, rusqlite::params![entity_id, before], |row| row.get(0))
        .optional()
        .map_err(|error| error.to_string())
}

fn values_between(
    connection: &Connection,
    entity_id: &str,
    start: i64,
    end: i64,
) -> Result<Vec<(i64, f64)>, String> {
    let mut statement = connection
        .prepare(
            "SELECT updated_ns, state FROM state_history
             WHERE entity_id = ?1 AND updated_ns >= ?2 AND updated_ns < ?3
             ORDER BY updated_ns, id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(rusqlite::params![entity_id, start, end], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut samples = Vec::new();
    for row in rows {
        let (at, text) = row.map_err(|error| error.to_string())?;
        if let Some(value) = number_in(&text) {
            samples.push((at, value));
        }
    }
    Ok(samples)
}

fn number_in(text: &str) -> Option<f64> {
    let state: EntityState = serde_json::from_str(text).ok()?;
    number_of(&state)
}

fn timestamp_from_ns(ns: i64) -> Option<Timestamp> {
    jiff::Timestamp::from_nanosecond(i128::from(ns))
        .ok()
        .map(Timestamp::from_jiff)
}

pub(crate) fn class_name(class: StateClass) -> &'static str {
    match class {
        StateClass::Measurement => "measurement",
        StateClass::Total => "total",
        StateClass::TotalIncreasing => "total_increasing",
    }
}

fn class_from(text: &str) -> Option<StateClass> {
    match text {
        "measurement" => Some(StateClass::Measurement),
        "total" => Some(StateClass::Total),
        "total_increasing" => Some(StateClass::TotalIncreasing),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(actual: f64, expected: f64) {
        let difference = (actual - expected).abs();
        assert!(
            difference < 1e-9,
            "{actual} is not near {expected} (off by {difference})"
        );
    }

    #[test]
    fn a_measurement_is_weighted_by_how_long_each_value_held() {
        let stat = measurement(0, 300, Some(10.0), &[(100, 20.0)]).expect("a bucket");
        near(
            stat.mean.expect("a mean"),
            (10.0 * 100.0 + 20.0 * 200.0) / 300.0,
        );
        near(stat.min.expect("a min"), 10.0);
        near(stat.max.expect("a max"), 20.0);
        near(stat.last.expect("a last"), 20.0);
        assert!(stat.sum.is_none());
    }

    #[test]
    fn a_value_that_does_not_change_still_fills_the_period() {
        let stat = measurement(0, 300, Some(10.0), &[]).expect("a bucket");
        near(stat.mean.expect("a mean"), 10.0);
        near(stat.last.expect("a last"), 10.0);
    }

    #[test]
    fn time_before_the_first_reading_is_not_part_of_the_mean() {
        let stat = measurement(0, 300, None, &[(100, 20.0)]).expect("a bucket");
        near(stat.mean.expect("a mean"), 20.0);
    }

    #[test]
    fn a_rising_counter_that_resets_adds_the_new_reading() {
        let stat = change(
            StateClass::TotalIncreasing,
            Some(100.0),
            &[(0, 110.0), (1, 50.0)],
        )
        .expect("a bucket");
        near(stat.sum.expect("a sum"), 60.0);
        near(stat.last.expect("a last"), 50.0);
        assert!(stat.mean.is_none());
    }

    #[test]
    fn a_total_that_falls_is_a_negative_change() {
        let stat = change(StateClass::Total, Some(100.0), &[(0, 80.0)]).expect("a bucket");
        near(stat.sum.expect("a sum"), -20.0);
        near(stat.last.expect("a last"), 80.0);
    }

    #[test]
    fn an_hour_averages_its_five_minute_rows() {
        let hour = rollup(&[
            Stat {
                mean: Some(10.0),
                min: Some(1.0),
                max: Some(4.0),
                sum: None,
                last: Some(10.0),
            },
            Stat {
                mean: Some(20.0),
                min: Some(3.0),
                max: Some(9.0),
                sum: Some(5.0),
                last: Some(20.0),
            },
        ])
        .expect("an hour");
        near(hour.mean.expect("a mean"), 15.0);
        near(hour.min.expect("a min"), 1.0);
        near(hour.max.expect("a max"), 9.0);
        near(hour.sum.expect("a sum"), 5.0);
        near(hour.last.expect("a last"), 20.0);
    }

    #[test]
    fn an_empty_hour_is_skipped() {
        assert!(rollup(&[]).is_none());
    }
}
