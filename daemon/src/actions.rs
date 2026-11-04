//! Named commands, set up on the PC and shown as buttons on the phone. The phone only ever knows
//! the names and ids, and only ever sends an id back, so a paired phone can't run anything else.

use lis_core::protocol::ActionInfo;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::time::timeout;

/// A long-running command counts as started once this has passed.
const RESULT_LIMIT: Duration = Duration::from_secs(30);
const MAX_ACTIONS: usize = 64;
const MAX_NAME_LEN: usize = 64;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Action {
    pub id: String,
    pub name: String,
    pub command: String,
}

struct Loaded {
    modified: Option<SystemTime>,
    actions: Vec<Action>,
}

pub struct Actions {
    path: PathBuf,
    loaded: Mutex<Loaded>,
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|metadata| metadata.modified()).ok()
}

/// A file that doesn't parse has no actions, and stays as it is for fixing by hand.
fn read(path: &Path) -> Vec<Action> {
    let Ok(bytes) = fs::read(path) else { return Vec::new() };
    let actions: Vec<Action> = match serde_json::from_slice(&bytes) {
        Ok(actions) => actions,
        Err(error) => {
            eprintln!("lis-daemon: can't read {}: {error}", path.display());
            return Vec::new();
        }
    };
    match validate(&actions) {
        Ok(()) => actions,
        Err(error) => {
            eprintln!("lis-daemon: {}: {error}", path.display());
            Vec::new()
        }
    }
}

pub fn validate(actions: &[Action]) -> Result<(), String> {
    if actions.len() > MAX_ACTIONS {
        return Err(format!("At most {MAX_ACTIONS} actions"));
    }
    for (index, action) in actions.iter().enumerate() {
        if action.id.is_empty() || actions[..index].iter().any(|other| other.id == action.id) {
            return Err("Every action needs its own id".to_string());
        }
        if action.name.trim().is_empty() || action.name.len() > MAX_NAME_LEN {
            return Err("Every action needs a short name".to_string());
        }
        if action.command.trim().is_empty() {
            return Err(format!("{} has no command", action.name));
        }
    }
    Ok(())
}

impl Actions {
    pub fn new(path: PathBuf) -> Self {
        let loaded = Loaded { modified: modified(&path), actions: read(&path) };
        Self { path, loaded: Mutex::new(loaded) }
    }

    pub fn list(&self) -> Vec<Action> {
        self.loaded.lock().unwrap().actions.clone()
    }

    /// Without the commands, which never leave this PC.
    pub fn buttons(&self) -> Vec<ActionInfo> {
        self.loaded.lock().unwrap().actions.iter().map(|action| ActionInfo { id: action.id.clone(), name: action.name.clone() }).collect()
    }

    pub fn set(&self, actions: Vec<Action>) -> Result<(), String> {
        validate(&actions)?;
        let bytes = serde_json::to_vec_pretty(&actions).map_err(|error| error.to_string())?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        lis_core::identity::write_private(&self.path, &bytes).map_err(|error| error.to_string())?;
        let mut loaded = self.loaded.lock().unwrap();
        loaded.modified = modified(&self.path);
        loaded.actions = actions;
        Ok(())
    }

    /// True when the file was changed by hand.
    pub fn reload_if_changed(&self) -> bool {
        let now = modified(&self.path);
        let mut loaded = self.loaded.lock().unwrap();
        if loaded.modified == now {
            return false;
        }
        loaded.modified = now;
        let actions = read(&self.path);
        let changed = actions != loaded.actions;
        loaded.actions = actions;
        changed
    }

    pub fn command_of(&self, id: &str) -> Option<String> {
        self.loaded.lock().unwrap().actions.iter().find(|action| action.id == id).map(|action| action.command.clone())
    }
}

/// Runs by the shell, with nothing from the phone in it. Whether it worked, once it ends.
pub async fn run(command: String) -> bool {
    let child = Command::new("/bin/sh").args(["-c", &command]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = child else { return false };
    match timeout(RESULT_LIMIT, child.wait()).await {
        Ok(status) => status.is_ok_and(|status| status.success()),
        // Still running, so it did start. It goes on without being waited for.
        Err(_) => {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(id: &str, name: &str, command: &str) -> Action {
        Action { id: id.into(), name: name.into(), command: command.into() }
    }

    #[test]
    fn buttons_carry_no_commands() {
        let dir = tempfile::tempdir().unwrap();
        let actions = Actions::new(dir.path().join("actions.json"));
        actions.set(vec![action("a", "Main scene", "secret-command --flag")]).unwrap();
        let sent = serde_json::to_string(&actions.buttons()).unwrap();
        assert!(sent.contains("Main scene"));
        assert!(!sent.contains("secret-command"));
    }

    #[test]
    fn set_actions_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deeper/actions.json");
        Actions::new(path.clone()).set(vec![action("a", "A", "true")]).unwrap();
        assert_eq!(Actions::new(path).list(), vec![action("a", "A", "true")]);
    }

    #[test]
    fn bad_lists_are_refused() {
        for bad in [
            vec![action("", "A", "true")],
            vec![action("a", " ", "true")],
            vec![action("a", "A", " ")],
            vec![action("a", "A", "true"), action("a", "B", "true")],
            vec![action("a", &"x".repeat(MAX_NAME_LEN + 1), "true")],
        ] {
            assert!(validate(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_hand_edit_is_picked_up_and_a_broken_one_means_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.json");
        let actions = Actions::new(path.clone());
        assert!(!actions.reload_if_changed());

        fs::write(&path, serde_json::to_vec(&[action("a", "A", "true")]).unwrap()).unwrap();
        assert!(actions.reload_if_changed());
        assert_eq!(actions.command_of("a").as_deref(), Some("true"));
        assert_eq!(actions.command_of("other"), None);

        std::thread::sleep(Duration::from_millis(20));
        fs::write(&path, "{broken").unwrap();
        assert!(actions.reload_if_changed());
        assert!(actions.list().is_empty());
    }

    #[tokio::test]
    async fn reports_how_a_command_ended() {
        assert!(run("exit 0".to_string()).await);
        assert!(!run("exit 3".to_string()).await);
    }
}
