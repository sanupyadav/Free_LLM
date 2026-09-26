//! Skills system: files as the source of truth + SQLite index + on-demand roster injection + quality gate.
//!
//! Directory layout:
//! - `<skills_dir>/<id>/SKILL.md` -- full skill text (frontmatter + body); the file is the single source of truth
//! - `<db_path>` -- SQLite index (enabled state / source / built-in flag / content hash)
//!
//! Difference from the old `prompts.rs`: skills are no longer fully concatenated into the system prefix.
//! Instead only the roster (name + description) of enabled skills is injected, and the body is read on demand;
//! custom skills are persisted to disk and the database, so they survive a restart.

pub mod frontmatter;
pub mod gate;
pub mod inject;
pub mod store;

pub use store::{SkillInfo, SkillInput, SkillsManager};
