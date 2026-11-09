use wm_common::WorkspaceConfig;

use super::activate_workspace_from_config;
use crate::{
  commands::monitor::move_workspace_to_monitor,
  models::{Monitor, Workspace},
  traits::CommonGetters,
  user_config::UserConfig,
  wm_state::WmState,
};

/// Gives every monitor without a workspace one, so that every monitor
/// displays a workspace once an event or a command has been handled.
///
/// A monitor gets an inactive workspace config from the user config,
/// preferring one bound to it. Without one, it gets a workspace that
/// another monitor holds without displaying it. Without that either, it
/// gets a temporary workspace that is not in the user config.
pub fn fill_empty_monitors(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  for monitor in state.monitors() {
    if monitor.child_count() > 0 {
      continue;
    }

    let active_workspaces = state.workspaces();
    let inactive_config = config
      .workspace_config_for_monitor(&monitor, &active_workspaces)
      .or_else(|| {
        config.next_inactive_workspace_config(&active_workspaces)
      })
      .cloned();
    let taken_names = config
      .value
      .workspaces
      .iter()
      .map(|workspace_config| workspace_config.name.clone())
      .chain(
        active_workspaces
          .iter()
          .map(|workspace| workspace.config().name),
      )
      .collect::<Vec<_>>();

    match workspace_source(
      &monitor,
      &state.monitors(),
      inactive_config,
      &taken_names,
    ) {
      WorkspaceSource::Config(workspace_config)
      | WorkspaceSource::Temporary(workspace_config) => {
        activate_workspace_from_config(
          workspace_config,
          Some(monitor),
          state,
          config,
        )?;
      }
      WorkspaceSource::Lent(workspace) => {
        move_workspace_to_monitor(&workspace, &monitor, state, config)?;
      }
    }
  }

  Ok(())
}

/// Where a monitor without a workspace gets one from.
#[derive(Debug)]
enum WorkspaceSource {
  /// A config in the user config that no active workspace uses.
  Config(WorkspaceConfig),
  /// A workspace another monitor holds without displaying it.
  Lent(Workspace),
  /// A config that is not in the user config. The workspace made from it
  /// is not kept alive, so it is removed once it is empty and hidden.
  Temporary(WorkspaceConfig),
}

/// Chooses where `monitor` gets a workspace from: `inactive_config` if
/// there is one, otherwise a workspace another of `monitors` can lend,
/// otherwise a temporary config whose name is none of `taken_names`.
fn workspace_source(
  monitor: &Monitor,
  monitors: &[Monitor],
  inactive_config: Option<WorkspaceConfig>,
  taken_names: &[String],
) -> WorkspaceSource {
  inactive_config
    .map(WorkspaceSource::Config)
    .or_else(|| {
      workspace_to_lend(monitor, monitors).map(WorkspaceSource::Lent)
    })
    .unwrap_or_else(|| {
      WorkspaceSource::Temporary(temporary_workspace_config(taken_names))
    })
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

/// A config named with the lowest positive number that is none of
/// `taken_names`, neither kept alive nor bound to a monitor.
fn temporary_workspace_config(taken_names: &[String]) -> WorkspaceConfig {
  let name = (1..=taken_names.len() + 1)
    .map(|number| number.to_string())
    .find(|name| !taken_names.contains(name))
    .unwrap_or_default();

  WorkspaceConfig {
    name,
    display_name: None,
    bind_to_monitor: None,
    keep_alive: false,
  }
}

#[cfg(test)]
mod tests {
  use wm_common::{GapsConfig, TilingDirection, WorkspaceConfig};

  use super::{workspace_source, WorkspaceSource};
  use crate::{
    commands::container::attach_container,
    models::{Monitor, RootContainer, Workspace},
  };

  fn workspace_config(
    name: &str,
    bind_to_monitor: Option<u32>,
  ) -> WorkspaceConfig {
    WorkspaceConfig {
      name: name.to_string(),
      display_name: None,
      bind_to_monitor,
      keep_alive: false,
    }
  }

  fn workspace(name: &str, bind_to_monitor: Option<u32>) -> Workspace {
    Workspace::new(
      workspace_config(name, bind_to_monitor),
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

  /// The source as its kind and the name of its workspace or config.
  fn described(source: &WorkspaceSource) -> (&'static str, String) {
    match source {
      WorkspaceSource::Config(config) => ("config", config.name.clone()),
      WorkspaceSource::Lent(workspace) => {
        ("lent", workspace.config().name)
      }
      WorkspaceSource::Temporary(config) => {
        ("temporary", config.name.clone())
      }
    }
  }

  fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(ToString::to_string).collect()
  }

  #[test]
  fn activates_a_config_no_workspace_uses_first() {
    let monitors = monitors(vec![
      vec![workspace("1", None), workspace("2", None)],
      vec![],
    ]);

    let source = workspace_source(
      &monitors[1],
      &monitors,
      Some(workspace_config("3", None)),
      &names(&["1", "2", "3"]),
    );

    assert_eq!(described(&source), ("config", "3".to_string()));
  }

  #[test]
  fn lends_a_workspace_another_monitor_is_not_displaying_when_no_config_is_left(
  ) {
    let monitors = monitors(vec![
      vec![workspace("1", None), workspace("2", None)],
      vec![],
    ]);

    let source =
      workspace_source(&monitors[1], &monitors, None, &names(&["1", "2"]));

    assert_eq!(described(&source), ("lent", "2".to_string()));
  }

  #[test]
  fn does_not_lend_the_only_workspace_of_another_monitor() {
    let monitors = monitors(vec![vec![workspace("1", None)], vec![]]);

    let source =
      workspace_source(&monitors[1], &monitors, None, &names(&["1"]));

    assert_eq!(described(&source).0, "temporary");
  }

  #[test]
  fn does_not_lend_a_workspace_bound_to_the_monitor_it_is_on() {
    let monitors = monitors(vec![
      vec![workspace("1", None), workspace("2", Some(0))],
      vec![],
    ]);

    let source =
      workspace_source(&monitors[1], &monitors, None, &names(&["1", "2"]));

    assert_eq!(described(&source).0, "temporary");
  }

  #[test]
  fn names_a_temporary_workspace_with_the_lowest_number_not_taken() {
    let monitors = monitors(vec![vec![workspace("1", None)], vec![]]);

    let source = workspace_source(
      &monitors[1],
      &monitors,
      None,
      &names(&["1", "2", "4"]),
    );

    assert_eq!(described(&source), ("temporary", "3".to_string()));
  }

  #[test]
  fn makes_a_temporary_workspace_that_is_neither_kept_alive_nor_bound() {
    let monitors = monitors(vec![vec![workspace("1", None)], vec![]]);

    let source =
      workspace_source(&monitors[1], &monitors, None, &names(&["1"]));

    let WorkspaceSource::Temporary(config) = source else {
      panic!("expected a temporary workspace, got {source:?}");
    };
    assert!(!config.keep_alive);
    assert_eq!(config.bind_to_monitor, None);
  }
}
