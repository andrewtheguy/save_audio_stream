use chrono::Timelike;

/// Time represented as (hour, minute) tuple
pub type HourMinute = (u32, u32);

/// Parse a time string in "HH:MM" format and return (hour, minute)
pub fn parse_time(time_str: &str) -> Result<HourMinute, String> {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid time format '{}', expected HH:MM",
            time_str
        ));
    }
    let hour: u32 = parts[0]
        .parse()
        .map_err(|_| format!("Invalid hour in '{}'", time_str))?;
    let minute: u32 = parts[1]
        .parse()
        .map_err(|_| format!("Invalid minute in '{}'", time_str))?;
    if hour >= 24 || minute >= 60 {
        return Err(format!("Time '{}' out of range", time_str));
    }
    Ok((hour, minute))
}

/// Compare two (hour, minute) times: returns true if a < b
fn time_lt(a: HourMinute, b: HourMinute) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 < b.1)
}

/// Compare two (hour, minute) times: returns true if a <= b
fn time_le(a: HourMinute, b: HourMinute) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1)
}

/// Check if current time is in the active recording window
/// Handles overnight windows (e.g., 14:00 to 07:00)
fn is_in_active_window(current: HourMinute, start: HourMinute, end: HourMinute) -> bool {
    if time_le(start, end) {
        // Same day window (e.g., 09:00 to 17:00)
        // Active when: start <= current < end
        time_le(start, current) && time_lt(current, end)
    } else {
        // Overnight window (e.g., 14:00 to 07:00)
        // Active when: current >= start OR current < end
        time_le(start, current) || time_lt(current, end)
    }
}

/// Get current UTC time as (hour, minute)
fn now_hm() -> HourMinute {
    let now = chrono::Utc::now();
    (now.hour(), now.minute())
}

/// Check if we're currently in the active recording window
pub fn is_in_active_window_now(start: HourMinute, end: HourMinute) -> bool {
    is_in_active_window(now_hm(), start, end)
}

/// Calculate seconds until the end time from current time
/// Handles overnight windows
fn seconds_until_end(current: HourMinute, end: HourMinute) -> u64 {
    let current_mins = current.0 * 60 + current.1;
    let end_mins = end.0 * 60 + end.1;
    let minutes_until = if current_mins < end_mins {
        end_mins - current_mins
    } else {
        // End is tomorrow
        (24 * 60 - current_mins) + end_mins
    };
    minutes_until as u64 * 60
}

/// Block until we enter the active recording window
/// Returns immediately if already in the window
/// Checks every second for accurate timing
pub fn wait_for_active_window(start: HourMinute, end: HourMinute, name: &str) {
    let mut logged = false;
    loop {
        if is_in_active_window_now(start, end) {
            return;
        }
        if !logged {
            println!(
                "[{}] Waiting for recording window ({:02}:{:02} - {:02}:{:02} UTC)...",
                name, start.0, start.1, end.0, end.1
            );
            logged = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Get the duration in seconds until the end of the recording window
pub fn get_window_duration_secs(end: HourMinute) -> u64 {
    seconds_until_end(now_hm(), end)
}
