// conversation.rs — `ta conversation <goal_id>`: view interactive conversation history.
//
// Reads the JSONL conversation log for a goal and displays it as a formatted
// timeline with turn numbers, roles, and timestamps.

use ta_goal::{ConversationStore, GoalRunStore, TurnRole};
use ta_mcp_gateway::GatewayConfig;
use uuid::Uuid;

pub fn execute(config: &GatewayConfig, goal_id: &str, json: bool) -> anyhow::Result<()> {
    // Resolve goal ID by prefix.
    let resolved = resolve_goal_id(goal_id, config)?;

    let conversations_dir = config.workspace_root.join(".ta").join("conversations");
    let store = ConversationStore::new(&conversations_dir);

    let turns = store.load(resolved)?;

    if turns.is_empty() {
        println!(
            "No conversation history for goal {}.",
            &resolved.to_string()[..8]
        );
        println!("Interactive conversations are created when agents use ta_ask_human.");
        println!();
        println!("To start an interactive goal:");
        println!("  ta run \"title\" --interactive");
        return Ok(());
    }

    if json {
        for turn in &turns {
            println!("{}", serde_json::to_string(turn)?);
        }
        return Ok(());
    }

    println!(
        "Conversation for goal {} ({} turns)\n",
        &resolved.to_string()[..8],
        turns.len()
    );

    for turn in &turns {
        let role_label = match turn.role {
            TurnRole::Agent => "Agent",
            TurnRole::Human => {
                if let Some(ref r) = turn.responder_id {
                    // We'll just print "Human" and show responder in parens.
                    let _ = r; // used below
                    "Human"
                } else {
                    "Human"
                }
            }
        };

        let timestamp = turn.timestamp.format("%H:%M:%S");
        let responder_suffix = if let Some(ref r) = turn.responder_id {
            format!(" ({})", r)
        } else {
            String::new()
        };

        println!(
            "  [{}] {} {}{}: {}",
            turn.turn, timestamp, role_label, responder_suffix, turn.text
        );
    }

    println!();
    Ok(())
}

fn resolve_goal_id(prefix: &str, config: &GatewayConfig) -> anyhow::Result<Uuid> {
    // Try parsing as a full UUID first.
    if let Ok(id) = Uuid::parse_str(prefix) {
        return Ok(id);
    }

    // Otherwise, match by prefix against known goals.
    let goal_store = GoalRunStore::new(&config.goals_dir)?;
    let all_goals = goal_store.list()?;
    let matches: Vec<_> = all_goals
        .iter()
        .filter(|g| g.goal_run_id.to_string().starts_with(prefix))
        .collect();

    match matches.len() {
        0 => anyhow::bail!(
            "No goal found matching prefix '{}'. Use `ta goal list --all` to see all goals.",
            prefix
        ),
        1 => Ok(matches[0].goal_run_id),
        n => anyhow::bail!(
            "Ambiguous prefix '{}' matches {} goals. Use a longer prefix.",
            prefix,
            n
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn execute_no_conversation_succeeds() {
        let dir = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(dir.path());

        // Create a goal so the ID resolves.
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goal = ta_goal::GoalRun::new(
            "Test goal",
            "objective",
            "claude-code",
            dir.path().to_path_buf(),
            config.store_dir.join("test"),
        );
        goal_store.save(&goal).unwrap();

        let result = execute(&config, &goal.goal_run_id.to_string(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn execute_with_conversation_shows_turns() {
        let dir = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(dir.path());

        // Create a goal.
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goal = ta_goal::GoalRun::new(
            "Test goal",
            "objective",
            "claude-code",
            dir.path().to_path_buf(),
            config.store_dir.join("test"),
        );
        goal_store.save(&goal).unwrap();

        // Write conversation turns.
        let conversations_dir = config.workspace_root.join(".ta").join("conversations");
        let conv_store = ConversationStore::new(&conversations_dir);
        let iid = Uuid::new_v4();
        conv_store
            .append_question(goal.goal_run_id, iid, 1, "Which database?")
            .unwrap();
        conv_store
            .append_answer(goal.goal_run_id, iid, 1, "PostgreSQL", "user1")
            .unwrap();

        // Execute in JSON mode to verify output.
        let result = execute(&config, &goal.goal_run_id.to_string(), true);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_goal_id_prefix() {
        let dir = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        let goal = ta_goal::GoalRun::new(
            "Test",
            "",
            "test",
            dir.path().to_path_buf(),
            config.store_dir.join("test"),
        );
        let full_id = goal.goal_run_id.to_string();
        goal_store.save(&goal).unwrap();

        // Full ID.
        let resolved = resolve_goal_id(&full_id, &config).unwrap();
        assert_eq!(resolved, goal.goal_run_id);

        // Prefix.
        let resolved = resolve_goal_id(&full_id[..8], &config).unwrap();
        assert_eq!(resolved, goal.goal_run_id);
    }

    #[test]
    fn resolve_goal_id_unknown_prefix_fails() {
        let dir = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let _goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        let result = resolve_goal_id("nonexistent", &config);
        assert!(result.is_err());
    }
}
