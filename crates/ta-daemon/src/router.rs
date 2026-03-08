// router.rs — Message routing with channel-to-project resolution.
//
// When a message arrives from an external channel (Discord, Slack, email),
// the router determines which project it should be dispatched to using
// a precedence-based resolution strategy:
//
// 1. Dedicated channel route (from office.yaml config)
// 2. Thread context (reply in a goal thread → same project)
// 3. Explicit prefix (`@ta <project-name> <command>`)
// 4. User's `default_project` setting
// 5. Ambiguous → return `RoutingResult::Ambiguous` so the caller can ask
//
// In single-project mode, routing always succeeds to the sole project.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::office::{OfficeConfig, ProjectRegistry};

/// The result of attempting to route a message to a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingResult {
    /// Successfully resolved to a specific project.
    Resolved(String),
    /// Message targets all projects (broadcast/notify route).
    Broadcast,
    /// Cannot determine the target project — caller should ask the user.
    Ambiguous {
        available_projects: Vec<String>,
        reason: String,
    },
}

/// Contextual information for routing a message.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingContext {
    /// The channel type (e.g., "discord", "slack", "email").
    pub channel_type: Option<String>,
    /// The channel identifier (e.g., "#backend-reviews", "user@example.com").
    pub channel_id: Option<String>,
    /// The goal/thread context (if replying in a goal thread).
    pub thread_project: Option<String>,
    /// Explicit project prefix from the message (e.g., "@ta inventory-service ...").
    pub explicit_project: Option<String>,
    /// The user's default project preference.
    pub user_default_project: Option<String>,
    /// Raw message text (for prefix extraction).
    pub message: Option<String>,
}

/// Routes messages to projects based on precedence rules.
pub struct MessageRouter {
    /// Channel routes from office.yaml.
    channel_routes: HashMap<(String, String), Option<String>>,
    /// Available project names.
    project_names: Vec<String>,
}

impl MessageRouter {
    /// Create a router from an office config.
    pub fn from_config(config: &OfficeConfig, registry: &ProjectRegistry) -> Self {
        let mut channel_routes = HashMap::new();

        for (channel_type, routing) in &config.channels {
            for (channel_id, target) in &routing.routes {
                channel_routes.insert(
                    (channel_type.clone(), channel_id.clone()),
                    target.project.clone(),
                );
            }
        }

        Self {
            channel_routes,
            project_names: registry.names(),
        }
    }

    /// Create a trivial router for single-project mode.
    pub fn single_project(project_name: String) -> Self {
        Self {
            channel_routes: HashMap::new(),
            project_names: vec![project_name],
        }
    }

    /// Route a message to a project using the precedence rules.
    pub fn route(&self, ctx: &RoutingContext) -> RoutingResult {
        // Single-project mode: always resolves.
        if self.project_names.len() == 1 {
            return RoutingResult::Resolved(self.project_names[0].clone());
        }

        // 1. Dedicated channel route.
        if let (Some(channel_type), Some(channel_id)) = (&ctx.channel_type, &ctx.channel_id) {
            let key = (channel_type.clone(), channel_id.clone());
            if let Some(target) = self.channel_routes.get(&key) {
                match target {
                    Some(project) => return RoutingResult::Resolved(project.clone()),
                    None => return RoutingResult::Broadcast,
                }
            }
        }

        // 2. Thread context.
        if let Some(ref project) = ctx.thread_project {
            if self.project_names.contains(project) {
                return RoutingResult::Resolved(project.clone());
            }
        }

        // 3. Explicit prefix.
        if let Some(ref project) = ctx.explicit_project {
            if self.project_names.contains(project) {
                return RoutingResult::Resolved(project.clone());
            }
        }

        // 3b. Try to extract from message text.
        if let Some(ref message) = ctx.message {
            if let Some(project) = self.extract_project_prefix(message) {
                return RoutingResult::Resolved(project);
            }
        }

        // 4. User's default project.
        if let Some(ref project) = ctx.user_default_project {
            if self.project_names.contains(project) {
                return RoutingResult::Resolved(project.clone());
            }
        }

        // 5. Ambiguous.
        RoutingResult::Ambiguous {
            available_projects: self.project_names.clone(),
            reason: "Cannot determine target project. Specify with `@ta <project> <command>` \
                     or set a default_project."
                .into(),
        }
    }

    /// Try to extract a project name from a `@ta <project> ...` prefix.
    fn extract_project_prefix(&self, message: &str) -> Option<String> {
        let trimmed = message.trim();
        let after_ta = trimmed
            .strip_prefix("@ta ")
            .or_else(|| trimmed.strip_prefix("ta "))?;

        // The next word should be a project name.
        let next_word = after_ta.split_whitespace().next()?;
        if self.project_names.contains(&next_word.to_string()) {
            Some(next_word.to_string())
        } else {
            None
        }
    }
}

/// Extract the project query parameter from a URL query string.
pub fn extract_project_query(query: Option<&str>) -> Option<String> {
    query.and_then(|q| {
        q.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            if key == "project" {
                Some(value.to_string())
            } else {
                None
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_router() -> MessageRouter {
        let mut channel_routes = HashMap::new();
        channel_routes.insert(
            ("discord".into(), "#backend".into()),
            Some("inventory".into()),
        );
        channel_routes.insert(
            ("discord".into(), "#frontend".into()),
            Some("portal".into()),
        );
        channel_routes.insert(
            ("discord".into(), "#office".into()),
            None, // broadcast
        );

        MessageRouter {
            channel_routes,
            project_names: vec!["inventory".into(), "portal".into()],
        }
    }

    #[test]
    fn single_project_always_resolves() {
        let router = MessageRouter::single_project("only-project".into());
        let ctx = RoutingContext::default();
        assert_eq!(
            router.route(&ctx),
            RoutingResult::Resolved("only-project".into())
        );
    }

    #[test]
    fn channel_route_resolves() {
        let router = test_router();
        let ctx = RoutingContext {
            channel_type: Some("discord".into()),
            channel_id: Some("#backend".into()),
            ..Default::default()
        };
        assert_eq!(
            router.route(&ctx),
            RoutingResult::Resolved("inventory".into())
        );
    }

    #[test]
    fn channel_broadcast_route() {
        let router = test_router();
        let ctx = RoutingContext {
            channel_type: Some("discord".into()),
            channel_id: Some("#office".into()),
            ..Default::default()
        };
        assert_eq!(router.route(&ctx), RoutingResult::Broadcast);
    }

    #[test]
    fn thread_context_resolves() {
        let router = test_router();
        let ctx = RoutingContext {
            thread_project: Some("portal".into()),
            ..Default::default()
        };
        assert_eq!(router.route(&ctx), RoutingResult::Resolved("portal".into()));
    }

    #[test]
    fn explicit_prefix_resolves() {
        let router = test_router();
        let ctx = RoutingContext {
            explicit_project: Some("inventory".into()),
            ..Default::default()
        };
        assert_eq!(
            router.route(&ctx),
            RoutingResult::Resolved("inventory".into())
        );
    }

    #[test]
    fn message_prefix_extraction() {
        let router = test_router();
        let ctx = RoutingContext {
            message: Some("@ta inventory status".into()),
            ..Default::default()
        };
        assert_eq!(
            router.route(&ctx),
            RoutingResult::Resolved("inventory".into())
        );
    }

    #[test]
    fn user_default_resolves() {
        let router = test_router();
        let ctx = RoutingContext {
            user_default_project: Some("portal".into()),
            ..Default::default()
        };
        assert_eq!(router.route(&ctx), RoutingResult::Resolved("portal".into()));
    }

    #[test]
    fn ambiguous_when_no_context() {
        let router = test_router();
        let ctx = RoutingContext::default();
        match router.route(&ctx) {
            RoutingResult::Ambiguous {
                available_projects, ..
            } => {
                assert_eq!(available_projects.len(), 2);
            }
            other => panic!("Expected Ambiguous, got {:?}", other),
        }
    }

    #[test]
    fn precedence_channel_over_thread() {
        let router = test_router();
        let ctx = RoutingContext {
            channel_type: Some("discord".into()),
            channel_id: Some("#backend".into()),
            thread_project: Some("portal".into()), // should be ignored
            ..Default::default()
        };
        // Channel route wins: inventory, not portal.
        assert_eq!(
            router.route(&ctx),
            RoutingResult::Resolved("inventory".into())
        );
    }

    #[test]
    fn extract_project_query_param() {
        assert_eq!(
            extract_project_query(Some("project=my-app&format=json")),
            Some("my-app".into())
        );
        assert_eq!(extract_project_query(Some("format=json")), None);
        assert_eq!(extract_project_query(None), None);
    }
}
