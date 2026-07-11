//! Agent navigation logic for Alt+Left/Alt+Right Phase 3).
//!
//! Provides helper functions for switching to the previous/next agent
//! thread in the live agents list.

use zerozero_multi_agent::{AgentMetadata, ThreadId};

/// Find the index of the active thread in the live agents list.
///
/// Returns `None` if the active thread is not found (should not happen
/// in normal operation).
pub fn active_index(agents: &[AgentMetadata], active_thread_id: &ThreadId) -> Option<usize> {
    agents.iter().position(|a| a.thread_id == *active_thread_id)
}

/// Get the thread ID for the previous agent (wrapping around).
///
/// Returns `None` if the list is empty or has only one agent.
pub fn prev_agent(agents: &[AgentMetadata], active_thread_id: &ThreadId) -> Option<ThreadId> {
    if agents.len() <= 1 {
        return None;
    }
    let idx = active_index(agents, active_thread_id)?;
    let prev_idx = if idx == 0 { agents.len() - 1 } else { idx - 1 };
    Some(agents[prev_idx].thread_id.clone())
}

/// Get the thread ID for the next agent (wrapping around).
///
/// Returns `None` if the list is empty or has only one agent.
pub fn next_agent(agents: &[AgentMetadata], active_thread_id: &ThreadId) -> Option<ThreadId> {
    if agents.len() <= 1 {
        return None;
    }
    let idx = active_index(agents, active_thread_id)?;
    let next_idx = if idx + 1 >= agents.len() { 0 } else { idx + 1 };
    Some(agents[next_idx].thread_id.clone())
}

/// Get the thread ID at a specific index in the live agents list.
///
/// Returns `None` if the index is out of bounds.
pub fn agent_at(agents: &[AgentMetadata], index: usize) -> Option<ThreadId> {
    agents.get(index).map(|a| a.thread_id.clone())
}
