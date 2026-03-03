//! # ta-mcp-gateway
//!
//! MCP (Model Context Protocol) gateway server for Trusted Autonomy.
//!
//! Exposes TA's staging, policy, and goal lifecycle as MCP tools that
//! any MCP-compatible agent (Claude Code, Codex, etc.) can call. All
//! file operations flow through policy → staging → changeset → audit,
//! enforcing the core thesis: every agent action is mediated.
//!
//! ## Architecture
//!
//! The gateway server runs as a stdio MCP server. Each tool call:
//! 1. Checks the policy engine (default deny)
//! 2. Routes to the appropriate connector (filesystem, etc.)
//! 3. Records an audit event
//! 4. Emits a notification event
//!
//! ## Tools
//!
//! - `ta_goal_start` — create a GoalRun, issue manifest, allocate workspace
//! - `ta_goal_status` — get current state of a GoalRun
//! - `ta_goal_list` — list GoalRuns
//! - `ta_fs_read` — read file from source directory
//! - `ta_fs_write` — write file to staging
//! - `ta_fs_list` — list staged files
//! - `ta_fs_diff` — show diff for a staged file
//! - `ta_pr_build` — bundle staged changes into PR package
//! - `ta_pr_status` — check PR package status

pub mod config;
pub mod error;
pub mod interceptor;
pub mod server;

pub use config::GatewayConfig;
pub use error::GatewayError;
pub use server::{GatewayState, TaGatewayServer};
