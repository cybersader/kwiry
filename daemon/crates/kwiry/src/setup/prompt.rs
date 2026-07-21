use std::collections::VecDeque;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kwiry_core::VaultRegistration;

use super::model::{ResolvedSetupInput, SetupErrorCode, SetupRequest, SetupSnapshot};

pub const SEMANTIC_DISCLOSURE: &str = "Semantic search downloads 133 MB on first use and may use up to 784 MiB of memory while indexing.";

pub trait Prompt {
    fn note(&mut self, message: &str) -> Result<(), SetupErrorCode>;
    fn text(&mut self, label: &str, default: Option<&str>) -> Result<String, SetupErrorCode>;
    fn confirm(&mut self, label: &str, default: bool) -> Result<bool, SetupErrorCode>;
}

#[derive(Debug, Default)]
pub struct StdioPrompt;

impl Prompt for StdioPrompt {
    fn note(&mut self, message: &str) -> Result<(), SetupErrorCode> {
        let mut output = io::stderr().lock();
        writeln!(output, "{message}").map_err(|_| SetupErrorCode::PromptIo)
    }

    fn text(&mut self, label: &str, default: Option<&str>) -> Result<String, SetupErrorCode> {
        loop {
            let suffix = default.map_or(String::new(), |value| format!(" [{value}]"));
            let answer = read_stdio_line(&format!("{label}{suffix}: "))?;
            if !answer.is_empty() {
                return Ok(answer);
            }
            if let Some(default) = default {
                return Ok(default.to_owned());
            }
        }
    }

    fn confirm(&mut self, label: &str, default: bool) -> Result<bool, SetupErrorCode> {
        loop {
            let suffix = if default { " [Y/n]: " } else { " [y/N]: " };
            let answer = read_stdio_line(&format!("{label}{suffix}"))?;
            match answer.trim().to_ascii_lowercase().as_str() {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => {
                    self.note("Please answer yes or no.")?;
                }
            }
        }
    }
}

fn read_stdio_line(prompt: &str) -> Result<String, SetupErrorCode> {
    let mut output = io::stderr().lock();
    write!(output, "{prompt}").map_err(|_| SetupErrorCode::PromptIo)?;
    output.flush().map_err(|_| SetupErrorCode::PromptIo)?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|_| SetupErrorCode::PromptIo)?;
    Ok(answer.trim().to_owned())
}

#[derive(Debug, Default)]
pub struct NoPrompt;

impl Prompt for NoPrompt {
    fn note(&mut self, _message: &str) -> Result<(), SetupErrorCode> {
        Err(SetupErrorCode::PromptRequired)
    }

    fn text(&mut self, _label: &str, _default: Option<&str>) -> Result<String, SetupErrorCode> {
        Err(SetupErrorCode::PromptRequired)
    }

    fn confirm(&mut self, _label: &str, _default: bool) -> Result<bool, SetupErrorCode> {
        Err(SetupErrorCode::PromptRequired)
    }
}

#[derive(Debug, Default)]
pub struct ScriptedPrompt {
    answers: VecDeque<String>,
    transcript: Vec<String>,
}

impl ScriptedPrompt {
    pub fn new(answers: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            answers: answers.into_iter().map(Into::into).collect(),
            transcript: Vec::new(),
        }
    }

    pub fn transcript(&self) -> &[String] {
        &self.transcript
    }

    fn answer(&mut self) -> Result<String, SetupErrorCode> {
        self.answers
            .pop_front()
            .ok_or(SetupErrorCode::PromptRequired)
    }
}

impl Prompt for ScriptedPrompt {
    fn note(&mut self, message: &str) -> Result<(), SetupErrorCode> {
        self.transcript.push(message.to_owned());
        Ok(())
    }

    fn text(&mut self, label: &str, default: Option<&str>) -> Result<String, SetupErrorCode> {
        self.transcript
            .push(format!("{label} [{}]", default.unwrap_or("no default")));
        let answer = self.answer()?;
        if answer.trim().is_empty() {
            default
                .map(str::to_owned)
                .ok_or(SetupErrorCode::PromptRequired)
        } else {
            Ok(answer.trim().to_owned())
        }
    }

    fn confirm(&mut self, label: &str, default: bool) -> Result<bool, SetupErrorCode> {
        self.transcript
            .push(format!("{label} [{}]", if default { "yes" } else { "no" }));
        let answer = self.answer()?;
        match answer.trim().to_ascii_lowercase().as_str() {
            "" => Ok(default),
            "y" | "yes" | "true" => Ok(true),
            "n" | "no" | "false" => Ok(false),
            _ => Err(SetupErrorCode::PromptRequired),
        }
    }
}

pub fn resolve_setup_input(
    request: &SetupRequest,
    snapshot: &SetupSnapshot,
    prompt: &mut dyn Prompt,
) -> Result<ResolvedSetupInput, SetupErrorCode> {
    let persisted_path =
        (snapshot.config.vaults.len() == 1).then(|| snapshot.config.vaults[0].path.as_path());
    let path = match request.vault_path.as_deref() {
        Some(path) => path.to_path_buf(),
        None => {
            let default = persisted_path
                .map(|path| path.to_string_lossy().into_owned())
                .or_else(|| {
                    std::env::current_dir()
                        .ok()
                        .map(|path| path.to_string_lossy().into_owned())
                });
            PathBuf::from(prompt.text("Vault path", default.as_deref())?)
        }
    };
    if !path.is_absolute() || !path.is_dir() {
        return Err(SetupErrorCode::InvalidVaultPath);
    }
    let path = fs::canonicalize(path).map_err(|_| SetupErrorCode::InvalidVaultPath)?;
    let persisted = snapshot
        .config
        .vaults
        .iter()
        .find(|vault| vault.path == path);

    let suggested_id = persisted
        .map(|vault| vault.id.clone())
        .unwrap_or_else(|| suggest_vault_id(&path));
    let vault_id = match request.vault_id.as_deref() {
        Some(id) => id.trim().to_owned(),
        None => prompt.text("Vault ID", Some(&suggested_id))?,
    };
    if !valid_vault_id(&vault_id) {
        return Err(SetupErrorCode::InvalidVaultId);
    }

    let semantic_enabled = match request.semantic {
        Some(enabled) => enabled,
        None => {
            prompt.note(SEMANTIC_DISCLOSURE)?;
            let default = if snapshot.config_exists {
                snapshot.config.semantic.enabled
            } else {
                true
            };
            prompt.confirm("Enable semantic search?", default)?
        }
    };

    Ok(ResolvedSetupInput {
        vault: VaultRegistration {
            id: vault_id,
            path,
            room: request
                .room
                .clone()
                .or_else(|| persisted.and_then(|vault| vault.room.clone())),
        },
        semantic_enabled,
        dry_run: request.dry_run,
    })
}

pub fn suggest_vault_id(path: &Path) -> String {
    let source = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("vault");
    let mut id = String::new();
    let mut separated = false;
    for character in source.chars() {
        if character.is_ascii_alphanumeric() {
            id.push(character.to_ascii_lowercase());
            separated = false;
        } else if !id.is_empty() && !separated {
            id.push('-');
            separated = true;
        }
    }
    while id.ends_with('-') {
        id.pop();
    }
    if id.is_empty() {
        "vault".to_owned()
    } else {
        id
    }
}

fn valid_vault_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        && id
            .chars()
            .any(|character| character.is_ascii_alphanumeric())
}
