use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub id: String,
    pub title: String,
    pub description: String,
    pub repository: String,
    pub branch: String,
    pub platforms: Vec<String>,
    pub channel: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema: u32,
    pub tools: Vec<Tool>,
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && id.as_bytes()[0].is_ascii_alphanumeric()
}

impl Catalog {
    pub fn from_json(json: &str) -> Result<Self> {
        let catalog: Self = serde_json::from_str(json)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 {
            bail!("Unsupported catalog schema: {}", self.schema);
        }
        let mut ids = HashSet::new();
        for tool in &self.tools {
            if !valid_id(&tool.id) || !ids.insert(&tool.id) {
                bail!("Invalid or duplicate tool ID: {}", tool.id);
            }
            if tool.title.trim().is_empty()
                || tool.branch.trim().is_empty()
                || tool.repository.trim().is_empty()
            {
                bail!("Incomplete catalog entry: {}", tool.id);
            }
            if tool.channel != "preview" {
                bail!("Only the explicit preview channel is supported by this prototype");
            }
            if tool.platforms.is_empty()
                || tool
                    .platforms
                    .iter()
                    .any(|p| !matches!(p.as_str(), "windows" | "macos" | "linux"))
            {
                bail!("Invalid platforms for {}", tool.id);
            }
        }
        Ok(())
    }
}
