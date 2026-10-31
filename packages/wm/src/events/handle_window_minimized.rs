use std::time::{Duration, Instant};

use tracing::info;
use wm_common::{try_warn, WindowState};
use wm_platform::NativeWindow;

use crate::{
  commands::{
    container::set_focused_descendant, window::update_window_state,
  },
  traits::{CommonGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

pub fn handle_window_minimized(
  native_window: &NativeWindow,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let found_window = state.window_from_native(native_window);

  // Update the window's state to be minimized.
  if let Some(window) = found_window {
    let is_minimized = try_warn!(window.native().is_minimized());

    window.update_native_properties(|properties| {
      properties.is_minimized = is_minimized;
    });

    if is_minimized
      && window.state() != WindowState::Minimized
      && state.display_change_timestamp.is_some_and(|changed_at| {
        is_minimized_by_display_change(Instant::now(), changed_at)
      })
    {
      info!("Window minimized by a display change, restoring: {window}");
      state.pending_sync.queue_container_to_redraw(window);
    } else if is_minimized && window.state() != WindowState::Minimized {
      info!("Window minimized: {window}");

      let window = update_window_state(
        window.clone(),
        WindowState::Minimized,
        state,
        config,
      )?;

      let now = Instant::now();
      state.recent_minimizes.retain(|(_, minimized_at)| {
        now.saturating_duration_since(*minimized_at)
          < DISPLAY_CHANGE_MINIMIZE_WINDOW
      });
      state.recent_minimizes.push((window.clone(), now));

      // Clear the drag state, as a window can be minimized while
      // being dragged (e.g. via `toggle-minimized`).
      // TODO: Investigate other code paths where the drag state should be
      // cleared (e.g. most commands that call `update_window_state`).
      window.set_active_drag(None);

      // Focus should be reassigned after a window has been minimized.
      if let Some(focus_target) = state.focus_target_after_removal(&window)
      {
        set_focused_descendant(&focus_target, None);
        state.pending_sync.queue_focus_change().queue_cursor_jump();
        state.unmanaged_or_minimized_timestamp =
          Some(std::time::Instant::now());
      }
    }
  }

  Ok(())
}

/// Restores the windows taken as minimized by the user just before the
/// display change being handled, since the OS minimized them for it.
pub fn restore_windows_minimized_by_display_change(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let now = Instant::now();

  for (window, minimized_at) in std::mem::take(&mut state.recent_minimizes)
  {
    if window.is_detached()
      || window.state() != WindowState::Minimized
      || !is_minimized_by_display_change(minimized_at, now)
    {
      continue;
    }

    info!("Window minimized by a display change, restoring: {window}");

    let target_state = window
      .prev_state()
      .unwrap_or(WindowState::default_from_config(&config.value));

    let window = update_window_state(window, target_state, state, config)?;
    state.pending_sync.queue_container_to_redraw(window);
  }

  Ok(())
}

/// How close to a display change a reported minimize is taken to be the
/// OS's rather than the user's. Windows minimizes the windows of a
/// display it disconnects, and reports that either just before or just
/// after the display change.
const DISPLAY_CHANGE_MINIMIZE_WINDOW: Duration = Duration::from_secs(5);

/// Whether a minimize at `minimized_at` is close enough to a display
/// change at `display_changed_at`, in either order, to be the OS's.
pub fn is_minimized_by_display_change(
  minimized_at: Instant,
  display_changed_at: Instant,
) -> bool {
  let apart = minimized_at
    .saturating_duration_since(display_changed_at)
    .max(display_changed_at.saturating_duration_since(minimized_at));

  apart < DISPLAY_CHANGE_MINIMIZE_WINDOW
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn takes_a_minimize_soon_after_a_display_change_as_the_os_s() {
    let change = Instant::now();

    assert!(is_minimized_by_display_change(
      change + Duration::from_millis(600),
      change
    ));
  }

  #[test]
  fn takes_a_minimize_just_before_a_display_change_as_the_os_s() {
    let minimize = Instant::now();

    assert!(is_minimized_by_display_change(
      minimize,
      minimize + Duration::from_millis(70)
    ));
  }

  #[test]
  fn takes_a_minimize_long_after_a_display_change_as_the_user_s() {
    let change = Instant::now();

    assert!(!is_minimized_by_display_change(
      change + Duration::from_secs(10),
      change
    ));
  }

  #[test]
  fn takes_a_minimize_long_before_a_display_change_as_the_user_s() {
    let minimize = Instant::now();

    assert!(!is_minimized_by_display_change(
      minimize,
      minimize + Duration::from_secs(10)
    ));
  }
}
