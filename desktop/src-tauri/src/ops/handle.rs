//! An owned way to reach [`AppState`] from a task that outlives its caller.
//!
//! A send runs as its own task so that cancelling the tool call that started
//! it can never leave it half-done (see [`super::send`]). That task needs the
//! state by value: Tauri owns the production `AppState` and hands out
//! borrows through an `AppHandle`, while tests own the state directly. This
//! trait is the one shape both can provide.

use std::sync::Arc;

use crate::state::AppState;

/// Something that can produce `&AppState` for as long as it is held.
pub trait AppStateHandle: Send + Sync + 'static {
    fn app_state(&self) -> &AppState;
}

impl AppStateHandle for Arc<AppState> {
    fn app_state(&self) -> &AppState {
        self
    }
}
