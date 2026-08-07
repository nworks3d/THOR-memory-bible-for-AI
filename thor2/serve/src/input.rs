//! What was about to happen, in the shape `rank::select` needs: zero or more
//! detected moments, zero or more targets, and the raw context text the
//! closeness key is derived from. Every constructor here is the ONLY place a
//! caller (hook, check, why) turns a real command/path/target into this
//! shape, so all three channels build it identically.

use intent::Action;
use model::item::TargetKind;

#[derive(Debug, Clone, Default)]
pub struct ServeInput {
    pub moments: Vec<Action>,
    pub targets: Vec<(TargetKind, String)>,
    /// Raw text of what actually happened (command, file path, or both) -
    /// never anything an item declared about itself. Source of the closeness
    /// key in `rank::closeness`.
    pub context: String,
    /// Which project this session is standing in, resolved once by
    /// `project::resolve_project` and carried here so `rank::select` can apply
    /// the same scoping session start already applies. `None` means the
    /// project could not be resolved, and then only global items are served -
    /// see `project::applies_to` for why that is the safe direction.
    pub project: Option<String>,
}

impl ServeInput {
    pub fn is_empty(&self) -> bool {
        self.moments.is_empty() && self.targets.is_empty()
    }

    fn push_context(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.context.is_empty() {
            self.context.push(' ');
        }
        self.context.push_str(text);
    }

    /// A command about to run: derives moments via `intent::from_command` and
    /// treats the command text itself as a possible `Target::Command` doel.
    pub fn add_command(&mut self, command: &str) {
        if command.is_empty() {
            return;
        }
        self.moments.extend(intent::from_command(command).into_iter().map(|s| s.action));
        self.targets.push((TargetKind::Command, command.to_string()));
        self.push_context(command);
    }

    /// A file about to be read or written: derives moments via
    /// `intent::from_path` and treats the path itself as a `Target::Path` doel.
    pub fn add_file(&mut self, path: &str) {
        if path.is_empty() {
            return;
        }
        self.moments.extend(intent::from_path(path).into_iter().map(|s| s.action));
        self.targets.push((TargetKind::Path, path.to_string()));
        self.push_context(path);
    }

    /// An explicit target of any kind (symbol/route/host/project/...), for a
    /// human-driven `check`/`why` call that is not shaped like a real
    /// Claude Code hook payload.
    pub fn add_target(&mut self, kind: TargetKind, value: &str) {
        if value.is_empty() {
            return;
        }
        self.targets.push((kind, value.to_string()));
        self.push_context(value);
    }

    /// An explicit moment, named directly rather than derived from a command
    /// or path - lets a human ask "what fires on `publish`" without having to
    /// spell a command that would trigger it.
    pub fn add_moment(&mut self, action: Action) {
        if !self.moments.contains(&action) {
            self.moments.push(action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_derives_its_own_moments_and_becomes_a_command_target() {
        let mut input = ServeInput::default();
        input.add_command("git push --force origin main");
        assert!(input.moments.contains(&Action::Push));
        assert!(input.targets.contains(&(TargetKind::Command, "git push --force origin main".to_string())));
        assert!(input.context.contains("push"));
    }

    #[test]
    fn a_file_derives_its_own_moments_and_becomes_a_path_target() {
        let mut input = ServeInput::default();
        input.add_file("app/.env");
        assert!(input.moments.contains(&Action::Credentials));
        assert!(input.targets.contains(&(TargetKind::Path, "app/.env".to_string())));
    }

    #[test]
    fn an_empty_input_is_empty() {
        assert!(ServeInput::default().is_empty());
    }

    #[test]
    fn adding_the_same_moment_twice_is_not_duplicated() {
        let mut input = ServeInput::default();
        input.add_moment(Action::Publish);
        input.add_moment(Action::Publish);
        assert_eq!(input.moments.len(), 1);
    }
}
