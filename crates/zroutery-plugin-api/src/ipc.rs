//! Bounded length-prefixed private-pipe IPC. Identity belongs to the pipe, not JSON.
use crate::ui::{Projection, UiEffect, UiNode};
use crate::{strict, PluginError, ResourceLimits, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> {
    pub ipc_version: u16,
    pub call_id: String,
    pub worker_generation: String,
    pub grant_epoch: String,
    pub value: T,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionsInput {
    pub schema_version: u32,
    pub revision: String,
    pub options: BTreeMap<String, serde_json::Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionsMigrationInput {
    pub schema_version: u32,
    pub revision: String,
    pub options: BTreeMap<String, serde_json::Value>,
    pub target_schema_version: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionsValidation {
    pub errors: Vec<FieldError>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiInput {
    pub contribution_id: String,
    pub locale: Locale,
    pub view_revision: String,
    pub projection: Projection,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiActionInput {
    pub contribution_id: String,
    pub locale: Locale,
    pub view_revision: String,
    pub projection: Projection,
    pub action: String,
    pub action_id: String,
    pub fields: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locale {
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "zh-TW")]
    ZhTw,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerCall {
    ValidateOptions { input: OptionsInput },
    MigrateOptions { input: OptionsMigrationInput },
    UiRender { hook: String, input: UiInput },
    UiAction { hook: String, input: UiActionInput },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldError {
    pub field: String,
    pub message_code: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerReply {
    OptionsValidation {
        result: OptionsValidation,
    },
    OptionsMigration {
        result: BTreeMap<String, serde_json::Value>,
    },
    UiTree {
        result: UiNode,
    },
    UiEffect {
        result: UiEffect,
    },
    Error {
        code: PluginError,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerBootstrap {
    pub entry: String,
    pub modules: BTreeMap<String, String>,
    pub limits: ResourceLimits,
    pub diagnostics: Vec<String>,
}

pub fn read_frame<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    let mut length = [0u8; 4];
    reader
        .read_exact(&mut length)
        .map_err(|_| PluginError::PluginCrashed)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > strict::MAX_FRAME_BYTES {
        return Err(PluginError::InvalidResult);
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| PluginError::PluginCrashed)?;
    strict::decode(&bytes, strict::MAX_FRAME_BYTES)
}
pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<usize> {
    let bytes = serde_json::to_vec(value).map_err(|_| PluginError::InvalidResult)?;
    if bytes.len() > strict::MAX_FRAME_BYTES {
        return Err(PluginError::InvalidResult);
    }
    writer
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|_| writer.write_all(&bytes))
        .and_then(|_| writer.flush())
        .map_err(|_| PluginError::PluginCrashed)?;
    Ok(bytes.len() + 4)
}

impl WorkerCall {
    pub fn accepts(&self, reply: &WorkerReply) -> bool {
        matches!(
            (self, reply),
            (_, WorkerReply::Error { .. })
                | (
                    Self::ValidateOptions { .. },
                    WorkerReply::OptionsValidation { .. }
                )
                | (
                    Self::MigrateOptions { .. },
                    WorkerReply::OptionsMigration { .. }
                )
                | (Self::UiRender { .. }, WorkerReply::UiTree { .. })
                | (Self::UiAction { .. }, WorkerReply::UiEffect { .. })
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_declared_length_is_checked_before_allocating_or_reading() {
        let bytes = u32::MAX.to_le_bytes();
        assert_eq!(
            read_frame::<WorkerCall>(&mut bytes.as_slice()),
            Err(PluginError::InvalidResult)
        );
    }
}
