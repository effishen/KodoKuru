//! Task-board tasks and per-agent checklists.

use serde::{Deserialize, Serialize};

use crate::event::Sender;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Todo,
    InProgress,
    Review,
    Done,
    Blocked,
}

impl TaskStatus {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
            "todo" => Self::Todo,
            "in_progress" => Self::InProgress,
            "review" => Self::Review,
            "done" => Self::Done,
            "blocked" => Self::Blocked,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskNote {
    pub by: Sender,
    pub text: String,
    pub at: crate::Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub acceptance: String,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub assignee: Option<String>,
    pub created_by: Sender,
    #[serde(default)]
    pub notes: Vec<TaskNote>,
    pub created_at: crate::Timestamp,
    pub updated_at: crate::Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ChecklistStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChecklistItem {
    pub content: String,
    #[serde(default)]
    pub status: ChecklistStatus,
}

/// Rules that keep the checklist useful: at most one item in progress,
/// no empty items.
pub fn validate_checklist(items: &[ChecklistItem]) -> Result<(), String> {
    if items.len() > 50 {
        return Err("checklist is limited to 50 items".into());
    }
    if items.iter().any(|i| i.content.trim().is_empty()) {
        return Err("checklist items must have content".into());
    }
    let active = items
        .iter()
        .filter(|i| i.status == ChecklistStatus::InProgress)
        .count();
    if active > 1 {
        return Err(format!(
            "{active} items are in_progress; work on exactly one thing at a time (at most one in_progress)"
        ));
    }
    Ok(())
}

pub fn open_items(items: &[ChecklistItem]) -> Vec<&ChecklistItem> {
    items
        .iter()
        .filter(|i| i.status != ChecklistStatus::Completed)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(s: ChecklistStatus) -> ChecklistItem {
        ChecklistItem { content: "x".into(), status: s }
    }

    #[test]
    fn one_in_progress_only() {
        assert!(validate_checklist(&[item(ChecklistStatus::InProgress), item(ChecklistStatus::Pending)]).is_ok());
        assert!(validate_checklist(&[item(ChecklistStatus::InProgress), item(ChecklistStatus::InProgress)]).is_err());
    }

    #[test]
    fn status_parse() {
        assert_eq!(TaskStatus::parse("In Progress"), Some(TaskStatus::InProgress));
        assert_eq!(TaskStatus::parse("nope"), None);
    }
}
