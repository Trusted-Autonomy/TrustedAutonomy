#!/usr/bin/env python3
"""TA channel plugin skeleton — reads ChannelQuestion JSON from stdin, delivers it,
and writes DeliveryResult JSON to stdout.

Usage:
  1. Copy this directory to .ta/plugins/channels/my-channel/
  2. Edit channel.toml with your plugin name and command
  3. Implement deliver_question() below
  4. Test: echo '{"interaction_id":"...","question":"test"}' | python3 channel_plugin.py

Protocol:
  - TA writes one JSON line to stdin: a ChannelQuestion object
  - Plugin writes one JSON line to stdout: a DeliveryResult object
  - Human responses go back to TA via HTTP:
    POST {callback_url}/api/interactions/{interaction_id}/respond
"""

import json
import sys


def deliver_question(question: dict) -> dict:
    """Deliver a question through your channel and return a DeliveryResult.

    Args:
        question: ChannelQuestion with fields:
            - interaction_id: str (UUID)
            - goal_id: str (UUID)
            - question: str (the question text)
            - context: str | None (what the agent was doing)
            - response_hint: str ("freeform", "yes_no", "choice")
            - choices: list[str] (for "choice" hint)
            - turn: int (conversation turn number)
            - callback_url: str (daemon URL for posting responses)

    Returns:
        DeliveryResult dict with fields:
            - channel: str (your channel name)
            - delivery_id: str (channel-specific message ID)
            - success: bool
            - error: str | None (error message if failed)
    """
    # TODO: Replace this with your channel delivery logic.
    # Examples:
    #   - Post to a Slack webhook
    #   - Send a Teams Adaptive Card
    #   - Send a push notification
    #   - Post to a custom API

    # For now, just log and succeed.
    print(f"[my-channel] Delivering question: {question['question']}", file=sys.stderr)

    return {
        "channel": "my-channel",
        "delivery_id": f"msg-{question.get('interaction_id', 'unknown')}",
        "success": True,
        "error": None,
    }


def main():
    # Read one JSON line from stdin.
    line = sys.stdin.readline().strip()
    if not line:
        json.dump(
            {"channel": "my-channel", "delivery_id": "", "success": False,
             "error": "No input received on stdin"},
            sys.stdout,
        )
        print()
        sys.exit(1)

    try:
        question = json.loads(line)
    except json.JSONDecodeError as e:
        json.dump(
            {"channel": "my-channel", "delivery_id": "", "success": False,
             "error": f"Invalid JSON input: {e}"},
            sys.stdout,
        )
        print()
        sys.exit(1)

    # Deliver the question and write the result.
    result = deliver_question(question)
    json.dump(result, sys.stdout)
    print()  # Ensure newline after JSON.


if __name__ == "__main__":
    main()
