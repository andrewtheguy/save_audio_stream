/// Parse a time string in "HH:MM" format and return (hour, minute)
pub fn parse_time(time_str: &str) -> Result<(u32, u32), String> {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() != 2 {
        return Err(format!("Invalid time format '{}', expected HH:MM", time_str));
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

/// Convert time to minutes since midnight
pub fn time_to_minutes(hour: u32, minute: u32) -> u32 {
    hour * 60 + minute
}

/// Check if current time (in minutes since midnight) is in the active recording window
/// Handles overnight windows (e.g., 14:00 to 07:00)
pub fn is_in_active_window(current_mins: u32, start_mins: u32, end_mins: u32) -> bool {
    if start_mins <= end_mins {
        // Same day window (e.g., 09:00 to 17:00)
        current_mins >= start_mins && current_mins < end_mins
    } else {
        // Overnight window (e.g., 14:00 to 07:00)
        current_mins >= start_mins || current_mins < end_mins
    }
}

/// Calculate seconds until the end time from current time
/// Handles overnight windows
pub fn seconds_until_end(current_mins: u32, end_mins: u32) -> u64 {
    let minutes_until = if current_mins < end_mins {
        end_mins - current_mins
    } else {
        // End is tomorrow
        (24 * 60 - current_mins) + end_mins
    };
    minutes_until as u64 * 60
}

/// Calculate seconds until the start time from current time
pub fn seconds_until_start(current_mins: u32, start_mins: u32) -> u64 {
    let minutes_until = if current_mins < start_mins {
        start_mins - current_mins
    } else {
        // Start is tomorrow
        (24 * 60 - current_mins) + start_mins
    };
    minutes_until as u64 * 60
}
