use std::fmt::Display;
use std::path::Path;
use std::process::Command;

#[derive(Debug)]
pub enum ForgeError {
    ForgeNotFound,
    CommandFailed {
        stdout: String,
        stderr: String,
        workspace: String,
    },
    IoError(std::io::Error),
}

impl From<std::io::Error> for ForgeError {
    fn from(err: std::io::Error) -> Self {
        ForgeError::IoError(err)
    }
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForgeError::ForgeNotFound => {
                write!(f, "Forge command not found in PATH or common locations")
            }
            ForgeError::CommandFailed {
                stdout,
                stderr,
                workspace,
            } => {
                write!(
                    f,
                    "Forge command failed in {workspace}: stdout: {stdout}, stderr: {stderr}",
                )
            }
            ForgeError::IoError(err) => write!(f, "IO error: {err}"),
        }
    }
}

impl std::error::Error for ForgeError {}

pub type Result<T> = std::result::Result<T, ForgeError>;

#[derive(Debug, Clone)]
pub struct CommandArgs {
    args: Vec<String>,
}

impl CommandArgs {
    pub fn new() -> Self {
        Self { args: Vec::new() }
    }

    pub fn arg(&mut self, arg: &str) -> &mut Self {
        self.args.push(arg.to_string());
        self
    }
}

impl Default for CommandArgs {
    fn default() -> Self {
        Self::new()
    }
}

impl IntoIterator for CommandArgs {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;

    fn into_iter(self) -> Self::IntoIter {
        self.args.into_iter()
    }
}

impl Display for CommandArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.args.join(" "))
    }
}

pub struct Forge {
    forge_path: String,
    workspace_path: Option<String>,
}

impl Default for Forge {
    fn default() -> Self {
        Self::new().expect("Failed to find forge")
    }
}

impl Forge {
    /// Create a new Forge instance by finding forge in PATH or common locations
    pub fn new() -> Result<Self> {
        let forge_path = find_forge_path().ok_or(ForgeError::ForgeNotFound)?;

        Ok(Self {
            forge_path,
            workspace_path: None,
        })
    }

    /// Set the workspace path for commands
    pub fn workspace_path<P: AsRef<str>>(mut self, path: P) -> Self {
        self.workspace_path = Some(path.as_ref().to_string());
        self
    }

    /// Build a test command
    pub fn test(&self, function_name: &str, test_path: &str) -> ForgeCommand {
        let mut cmd = CommandArgs::new();
        cmd.arg("test")
            .arg("--match-test")
            .arg(function_name)
            .arg("--match-path")
            .arg(test_path);

        ForgeCommand {
            forge_path: self.forge_path.clone(),
            workspace_path: self.workspace_path.clone(),
            args: cmd,
        }
    }

    /// Build a debug command
    pub fn debug(&self, function_name: &str, test_path: &str, output_path: &str) -> ForgeCommand {
        let mut cmd = CommandArgs::new();
        cmd.arg("test")
            .arg("--debug")
            .arg("--match-test")
            .arg(function_name)
            .arg("--match-path")
            .arg(test_path)
            .arg("--dump")
            .arg(output_path)
            .arg("--ast")
            .arg("--optimizer-runs")
            .arg("0")
            .arg("--optimize")
            .arg("false")
            .arg("-vvvvv"); // we need to run with this flag to export the storage changes

        ForgeCommand {
            forge_path: self.forge_path.clone(),
            workspace_path: self.workspace_path.clone(),
            args: cmd,
        }
    }
}

/// A forge command that can be executed or converted to args
pub struct ForgeCommand {
    forge_path: String,
    workspace_path: Option<String>,
    args: CommandArgs,
}

impl ForgeCommand {
    /// Execute the command
    pub fn execute(self) -> Result<std::process::Output> {
        let mut command = Command::new(&self.forge_path);

        if let Some(workspace) = &self.workspace_path {
            command.current_dir(workspace);
        }

        let output = command
            .args(self.args.clone())
            .env("RUST_LOG", "info")
            .output()?;

        if output.status.success() {
            Ok(output)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let workspace = self.workspace_path.as_deref().unwrap_or(".").to_string();

            Err(ForgeError::CommandFailed {
                stdout,
                stderr,
                workspace,
            })
        }
    }

    /// Get the command arguments without executing
    pub fn args(&self) -> CommandArgs {
        self.args.clone()
    }
}

fn find_forge_path() -> Option<String> {
    // Check if 'forge' command exists in PATH
    if command_exists("forge") {
        Some("forge".to_string())
    } else {
        // Try to find the forge binary in common locations
        let paths = [
            "/usr/local/bin/forge",
            "/usr/bin/forge",
            "~/.foundry/bin/forge",
        ];
        for path in paths.iter() {
            let expanded_path = shellexpand::tilde(path).to_string();
            if Path::new(&expanded_path).exists() {
                return Some(expanded_path);
            }
        }
        None
    }
}

// Check if a command exists in PATH
fn command_exists(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
