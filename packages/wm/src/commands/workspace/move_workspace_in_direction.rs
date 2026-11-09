use anyhow::Context;
use tracing::warn;
use wm_common::WmEvent;
use wm_platform::Direction;

use super::{activate_workspace, deactivate_workspace, sort_workspaces};
use crate::{
  commands::{
    container::move_container_within_tree, monitor::move_workspace_to_monitor,
  },
  models::Workspace,
  traits::{CommonGetters, PositionGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

pub fn move_workspace_in_direction(
  workspace: &Workspace,
  direction: &Direction,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let origin_monitor = workspace.monitor().context("No monitor.")?;
  let target_monitor =
    state.monitor_in_direction(&origin_monitor, direction)?;

  if let Some(target_monitor) = target_monitor {
    // Get currently displayed workspace on the target monitor.
    let displayed_workspace = target_monitor
      .displayed_workspace()
      .context("No displayed workspace.")?;

    // When the workspace is the only one on its monitor and no workspace
    // config is left to activate there, the two monitors swap workspaces.
    let swaps_workspaces = origin_monitor.child_count() == 1
      && config
        .inactive_workspace_configs(&state.workspaces())
        .is_empty();

    if swaps_workspaces
      && displayed_workspace
        .config()
        .bind_to_monitor
        .is_some_and(|index| index as usize == target_monitor.index())
    {
      warn!(
        "Not moving workspace {workspace}: its monitor would be left without a workspace."
      );
      return Ok(());
    }

    move_container_within_tree(
      &workspace.clone().into(),
      &target_monitor.clone().into(),
      target_monitor.child_count(),
      state,
    )?;

    let windows = workspace
      .descendants()
      .filter_map(|descendant| descendant.as_window_container().ok());

    for window in windows {
      window.set_has_pending_dpi_adjustment(true);

      window.set_floating_placement(
        window
          .floating_placement()
          .translate_to_center(&workspace.to_rect()?),
      );
    }

    state
      .pending_sync
      .queue_cursor_jump()
      .queue_container_to_redraw(workspace.clone())
      .queue_container_to_redraw(displayed_workspace.clone());

    match origin_monitor.child_count() {
      0 if swaps_workspaces => {
        move_workspace_to_monitor(
          &displayed_workspace,
          &origin_monitor,
          state,
          config,
        )?;
      }
      0 => {
        // Prevent origin monitor from having no workspaces.
        activate_workspace(None, Some(origin_monitor), state, config)?;
      }
      _ => {
        // Redraw the workspace on the origin monitor.
        state.pending_sync.queue_container_to_redraw(
          origin_monitor
            .displayed_workspace()
            .context("No displayed workspace.")?,
        );
      }
    }

    // Get empty workspace to destroy (if one is found). Cannot destroy
    // empty workspaces if they're the only workspace on the monitor.
    let workspace_to_destroy =
      target_monitor.workspaces().into_iter().find(|workspace| {
        !workspace.config().keep_alive
          && !workspace.has_children()
          && !workspace.is_displayed()
      });

    if let Some(workspace) = workspace_to_destroy {
      deactivate_workspace(workspace, state)?;
    }

    sort_workspaces(&target_monitor, config)?;

    state.emit_event(WmEvent::WorkspaceUpdated {
      updated_workspace: workspace.to_dto()?,
    });
  }

  Ok(())
}
