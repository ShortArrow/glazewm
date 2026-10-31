use anyhow::Context;
use tracing::info;
use wm_common::WmEvent;
use wm_platform::Display;

use crate::{
  commands::{
    container::{attach_container, move_container_within_tree},
    workspace::{activate_workspace, sort_workspaces},
  },
  models::{Monitor, NativeMonitorProperties, Workspace},
  traits::{CommonGetters, PositionGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

pub fn add_monitor(
  native_display: Display,
  native_properties: NativeMonitorProperties,
  state: &mut WmState,
) -> anyhow::Result<Monitor> {
  // Create `Monitor` instance. This uses the working area of the monitor
  // instead of the bounds of the display. The working area excludes
  // taskbars and other reserved display space.
  let monitor = Monitor::new(native_display, native_properties);

  attach_container(
    &monitor.clone().into(),
    &state.root_container.clone().into(),
    None,
  )?;

  info!("Monitor added: {monitor}");

  state.emit_event(WmEvent::MonitorAdded {
    added_monitor: monitor.to_dto()?,
  });

  Ok(monitor)
}

pub fn move_bounded_workspaces_to_new_monitor(
  monitor: &Monitor,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let bound_workspace_configs = config
    .value
    .workspaces
    .iter()
    .filter(|config| {
      config.bind_to_monitor.is_some_and(|monitor_index| {
        monitor.index() == monitor_index as usize
      })
    })
    .collect::<Vec<_>>();

  for workspace_config in bound_workspace_configs {
    let existing_workspace =
      state.workspace_by_name(&workspace_config.name);

    if let Some(existing_workspace) = existing_workspace {
      // Move workspaces that should be bound to the newly added monitor.
      move_workspace_to_monitor(
        &existing_workspace,
        monitor,
        state,
        config,
      )?;
    } else if workspace_config.keep_alive {
      // Activate all `keep_alive` workspaces for this monitor.
      activate_workspace(
        Some(&workspace_config.name),
        Some(monitor.clone()),
        state,
        config,
      )?;
    }
  }

  // Make sure the monitor has at least one workspace. This will
  // automatically prioritize bound workspace configs and fall back to the
  // first available one if needed. When every configured workspace is
  // already active, a workspace another monitor is not displaying is
  // moved here instead.
  if monitor.child_count() == 0 {
    let lent_workspace = config
      .inactive_workspace_configs(&state.workspaces())
      .is_empty()
      .then(|| workspace_to_lend(monitor, &state.monitors()))
      .flatten();

    match lent_workspace {
      Some(workspace) => {
        move_workspace_to_monitor(&workspace, monitor, state, config)?;
      }
      None => {
        activate_workspace(None, Some(monitor.clone()), state, config)?;
      }
    }
  }

  Ok(())
}

// TODO: Move to its own file once `swap-workspace` PR is merged.
// Ref: https://github.com/glzr-io/glazewm/pull/980.
pub fn move_workspace_to_monitor(
  workspace: &Workspace,
  target_monitor: &Monitor,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let origin_monitor = workspace.monitor().context("No monitor.")?;

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

  // Get currently displayed workspace on the target monitor.
  let displayed_workspace = target_monitor
    .displayed_workspace()
    .context("No displayed workspace.")?;

  state
    .pending_sync
    .queue_container_to_redraw(workspace.clone())
    .queue_container_to_redraw(displayed_workspace);

  match origin_monitor.child_count() {
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

  sort_workspaces(target_monitor, config)?;

  state.emit_event(WmEvent::WorkspaceUpdated {
    updated_workspace: workspace.to_dto()?,
  });

  Ok(())
}

/// Finds a workspace that another monitor can give up to `monitor`.
///
/// The workspace is one its monitor is not displaying, is not bound to
/// its monitor, and is not its monitor's only workspace, so that every
/// monitor keeps a displayed workspace.
fn workspace_to_lend(
  monitor: &Monitor,
  monitors: &[Monitor],
) -> Option<Workspace> {
  monitors
    .iter()
    .filter(|other| other.id() != monitor.id() && other.child_count() > 1)
    .find_map(|other| {
      let displayed = other.displayed_workspace()?;
      other.workspaces().into_iter().find(|workspace| {
        workspace.id() != displayed.id()
          && workspace.config().bind_to_monitor
            != Some(u32::try_from(other.index()).unwrap_or(u32::MAX))
      })
    })
}

#[cfg(test)]
mod tests {
  use wm_common::{GapsConfig, TilingDirection, WorkspaceConfig};

  use super::workspace_to_lend;
  use crate::{
    commands::container::attach_container,
    models::{Monitor, RootContainer, Workspace},
  };

  fn workspace(name: &str, bind_to_monitor: Option<u32>) -> Workspace {
    Workspace::new(
      WorkspaceConfig {
        name: name.to_string(),
        display_name: None,
        bind_to_monitor,
        keep_alive: true,
      },
      GapsConfig::default(),
      TilingDirection::Horizontal,
    )
  }

  /// Monitors attached in order to one root, each holding its
  /// workspaces, of which the first is displayed.
  fn monitors(workspaces: Vec<Vec<Workspace>>) -> Vec<Monitor> {
    let root = RootContainer::new();
    workspaces
      .into_iter()
      .map(|workspaces| {
        let monitor = Monitor::mock().workspaces(workspaces).call();
        attach_container(
          &monitor.clone().into(),
          &root.clone().into(),
          None,
        )
        .unwrap();
        monitor
      })
      .collect()
  }

  fn name(workspace: Option<Workspace>) -> Option<String> {
    workspace.map(|workspace| workspace.config().name)
  }

  #[test]
  fn lends_a_workspace_that_another_monitor_is_not_displaying() {
    let monitors = monitors(vec![
      vec![workspace("1", None), workspace("2", None)],
      vec![],
    ]);

    assert_eq!(
      name(workspace_to_lend(&monitors[1], &monitors)),
      Some("2".to_string())
    );
  }

  #[test]
  fn does_not_take_the_only_workspace_of_another_monitor() {
    let monitors = monitors(vec![vec![workspace("1", None)], vec![]]);

    assert_eq!(name(workspace_to_lend(&monitors[1], &monitors)), None);
  }

  #[test]
  fn does_not_take_a_workspace_bound_to_the_monitor_it_is_on() {
    let monitors = monitors(vec![
      vec![workspace("1", None), workspace("2", Some(0))],
      vec![],
    ]);

    assert_eq!(name(workspace_to_lend(&monitors[1], &monitors)), None);
  }

  #[test]
  fn takes_a_workspace_bound_to_a_monitor_index_that_is_not_its_own() {
    let monitors = monitors(vec![
      vec![workspace("1", None), workspace("2", Some(1))],
      vec![],
      vec![workspace("3", None), workspace("4", None)],
    ]);

    assert_eq!(
      name(workspace_to_lend(&monitors[2], &monitors)),
      Some("2".to_string())
    );
  }
}
