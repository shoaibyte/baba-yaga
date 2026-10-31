//! The fix/review/approve loop: posts each new finding as a thread in the
//! room, routes replies parsed by `room::commands` to state changes, hands
//! approved findings to a fixer agent, and tracks the resulting pull request
//! through review.

use anyhow::Result;

use crate::config::Config;

/// Connect to Switch and run the loop until stopped.
pub async fn serve(_config: &Config) -> Result<()> {
    todo!("connect via switch-client and run the scheduler and room loop")
}

/// Post a status report of open findings to the room once and exit.
pub async fn report(_config: &Config) -> Result<()> {
    todo!("post a summary of State::list_open to the room")
}
