#!/usr/bin/env python3
"""LangGraph adapter for TA's WorkflowEngine protocol.

Bridge between LangGraph's graph execution and TA's JSON-over-stdio protocol.
Configure in .ta/config.yaml:

    workflow:
      engine: process
      command: "python3 templates/workflows/adapters/langraph_adapter.py"

Protocol: Reads JSON lines from stdin, writes JSON lines to stdout.
"""

import json
import sys
from typing import Any


def handle_start(definition: dict) -> dict:
    """Start a workflow from a TA WorkflowDefinition."""
    # TODO: Convert TA WorkflowDefinition to a LangGraph StateGraph
    # and start execution.
    workflow_id = f"lg-{definition.get('name', 'unnamed')}"
    return {"type": "started", "workflow_id": workflow_id}


def handle_stage_completed(workflow_id: str, stage: str, verdicts: list) -> dict:
    """Process stage completion verdicts and decide routing."""
    # TODO: Feed verdicts into the LangGraph state and let the graph
    # decide the next node.
    return {
        "type": "action",
        "action": {
            "action": "proceed",
            "next_stage": "next",
            "context": {
                "previous_summary": f"Stage {stage} completed",
                "feedback_findings": [],
                "context_from": [],
            },
        },
    }


def handle_status(workflow_id: str) -> dict:
    """Return current workflow status."""
    return {
        "type": "status_response",
        "status": {
            "workflow_id": workflow_id,
            "name": "langraph-workflow",
            "current_stage": None,
            "state": "running",
            "stages_completed": [],
            "stages_remaining": [],
            "retry_counts": {},
            "started_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        },
    }


def handle_cancel(workflow_id: str) -> dict:
    """Cancel a running workflow."""
    return {"type": "cancelled"}


def handle_inject_feedback(workflow_id: str, stage: str, feedback: dict) -> dict:
    """Inject human feedback into the graph state."""
    return {"type": "ack"}


def main():
    """Main loop: read JSON lines from stdin, write responses to stdout."""
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            msg = json.loads(line)
        except json.JSONDecodeError as e:
            response = {"type": "error", "message": f"Invalid JSON: {e}"}
            print(json.dumps(response), flush=True)
            continue

        msg_type = msg.get("type", "")
        try:
            if msg_type == "start":
                response = handle_start(msg["definition"])
            elif msg_type == "stage_completed":
                response = handle_stage_completed(
                    msg["workflow_id"], msg["stage"], msg["verdicts"]
                )
            elif msg_type == "status":
                response = handle_status(msg["workflow_id"])
            elif msg_type == "cancel":
                response = handle_cancel(msg["workflow_id"])
            elif msg_type == "inject_feedback":
                response = handle_inject_feedback(
                    msg["workflow_id"], msg["stage"], msg["feedback"]
                )
            else:
                response = {"type": "error", "message": f"Unknown message type: {msg_type}"}
        except Exception as e:
            response = {"type": "error", "message": str(e)}

        print(json.dumps(response), flush=True)


if __name__ == "__main__":
    main()
