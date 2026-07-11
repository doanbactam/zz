use zerozero_exec::Event;
use zerozero_multi_agent::{AgentStatus, ThreadId};

/// Events that drive the TUI app loop.
pub enum AppEvent {
    /// Crossterm terminal event (key press, resize, etc).
    Crossterm(crossterm::event::Event),
    /// Core engine event (LLM streaming, tool calls, etc).
    Core(Event),
    /// Event from a specific agent thread (non-active thread events).
    /// When `tid == active_thread_id`, the event is rendered normally.
    /// Otherwise, only the registry status is updated .
    AgentThread(ThreadId, Event),
    /// Agent thread status changed (spawned/completed/failed/stopped).
    AgentStatusChanged(ThreadId, AgentStatus),
    /// Approval request from an inactive thread.
    /// Shows an overlay with the source thread label; press `o` to switch
    /// to the source thread before approving .
    AgentApprovalRequest {
        /// Thread ID that sent the approval request.
        source_thread_id: ThreadId,
        /// Tool call ID.
        tool_call_id: String,
        /// Tool name.
        tool_name: String,
        /// Tool arguments.
        args: serde_json::Value,
        /// Danger level.
        danger_level: String,
    },
    /// Periodic timer tick (250ms) for spinner animation.
    Tick,
}
