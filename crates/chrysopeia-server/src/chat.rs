//! Chat command interpreter.
//!
//! Parses natural language messages into actionable commands and returns
//! structured responses.

use crate::state::AppState;

/// Interpret a user chat message and return a response string.
///
/// Supports commands like:
/// - "transcode all non-AV1 files"
/// - "show library stats"
/// - "scan /media"
/// - "show hardware"
/// - "cancel job <id>"
pub async fn interpret_command(message: &str, state: &AppState) -> String {
    let lower = message.to_lowercase();

    if lower.contains("stats") || lower.contains("status") || lower.contains("overview") {
        return handle_stats(state).await;
    }

    if lower.contains("scan") {
        return handle_scan(message, state).await;
    }

    if lower.contains("transcode") {
        return handle_transcode(message, state).await;
    }

    if lower.contains("hardware") || lower.contains("gpu") || lower.contains("capabilities") {
        return handle_hardware(state).await;
    }

    if lower.contains("cancel") {
        return handle_cancel(message, state).await;
    }

    if lower.contains("help") {
        return handle_help();
    }

    // Default response for unrecognized commands
    format!(
        "I didn't understand that command. Try:\n\
         - \"show stats\" - view library statistics\n\
         - \"scan /path/to/media\" - scan a directory\n\
         - \"transcode all\" - transcode all pending files\n\
         - \"show hardware\" - view detected hardware\n\
         - \"cancel job <id>\" - cancel a running job\n\
         - \"help\" - show all commands"
    )
}

/// Handle stats/status requests.
async fn handle_stats(state: &AppState) -> String {
    let _ = state;
    // TODO: Query database for real stats
    "Library stats: 0 files total, 0 transcoded, 0 pending.".to_string()
}

/// Handle scan requests.
async fn handle_scan(message: &str, state: &AppState) -> String {
    let _ = (message, state);
    // TODO: Parse path from message, trigger scan
    "Starting library scan... I'll notify you when it's complete.".to_string()
}

/// Handle transcode requests.
async fn handle_transcode(message: &str, state: &AppState) -> String {
    let _ = (message, state);
    // TODO: Parse targets, create jobs
    "Creating transcode jobs for pending files...".to_string()
}

/// Handle hardware info requests.
async fn handle_hardware(state: &AppState) -> String {
    let caps = &state.capabilities;
    if caps.is_empty() {
        return "No hardware acceleration detected. CPU encoding will be used.".to_string();
    }
    let mut response = format!("Detected {} hardware device(s):\n", caps.len());
    for cap in caps.iter() {
        response.push_str(&format!(
            "- {} ({:?}): AV1={}, VP9={}, HEVC={}\n",
            cap.device_name, cap.api, cap.supports_av1, cap.supports_vp9, cap.supports_hevc
        ));
    }
    response
}

/// Handle job cancellation requests.
async fn handle_cancel(message: &str, state: &AppState) -> String {
    let _ = (message, state);
    // TODO: Parse job ID from message, cancel via engine
    "Job cancellation requested.".to_string()
}

/// Return help text listing all available commands.
fn handle_help() -> String {
    "Available commands:\n\
     - \"show stats\" / \"status\" - Library statistics\n\
     - \"scan <path>\" - Scan a directory for media files\n\
     - \"transcode all\" - Transcode all pending files\n\
     - \"transcode <file>\" - Transcode a specific file\n\
     - \"show hardware\" - Detected encoding hardware\n\
     - \"cancel job <id>\" - Cancel a running job\n\
     - \"show profiles\" - Available transcode profiles"
        .to_string()
}
