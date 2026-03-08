#!/usr/bin/env node
/**
 * TA channel plugin skeleton — reads ChannelQuestion JSON from stdin,
 * delivers it, and writes DeliveryResult JSON to stdout.
 *
 * Usage:
 *   1. Copy this directory to .ta/plugins/channels/my-channel/
 *   2. Edit channel.toml with your plugin name and command
 *   3. Implement deliverQuestion() below
 *   4. Test: echo '{"interaction_id":"...","question":"test"}' | node channel_plugin.js
 *
 * Protocol:
 *   - TA writes one JSON line to stdin: a ChannelQuestion object
 *   - Plugin writes one JSON line to stdout: a DeliveryResult object
 *   - Human responses go back to TA via HTTP:
 *     POST {callback_url}/api/interactions/{interaction_id}/respond
 */

/**
 * Deliver a question through your channel.
 *
 * @param {Object} question - ChannelQuestion
 * @param {string} question.interaction_id - UUID
 * @param {string} question.goal_id - UUID
 * @param {string} question.question - The question text
 * @param {string|null} question.context - What the agent was doing
 * @param {string} question.response_hint - "freeform", "yes_no", "choice"
 * @param {string[]} question.choices - For "choice" hint
 * @param {number} question.turn - Conversation turn number
 * @param {string} question.callback_url - Daemon URL for posting responses
 * @returns {Object} DeliveryResult
 */
function deliverQuestion(question) {
  // TODO: Replace with your channel delivery logic.
  // Examples:
  //   - Post to a Slack webhook using fetch()
  //   - Send a Teams Adaptive Card
  //   - Send a push notification

  console.error(`[my-channel] Delivering question: ${question.question}`);

  return {
    channel: "my-channel",
    delivery_id: `msg-${question.interaction_id || "unknown"}`,
    success: true,
    error: null,
  };
}

function main() {
  let input = "";

  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => {
    input += chunk;
  });

  process.stdin.on("end", () => {
    const line = input.trim().split("\n")[0];
    if (!line) {
      console.log(
        JSON.stringify({
          channel: "my-channel",
          delivery_id: "",
          success: false,
          error: "No input received on stdin",
        })
      );
      process.exit(1);
    }

    let question;
    try {
      question = JSON.parse(line);
    } catch (e) {
      console.log(
        JSON.stringify({
          channel: "my-channel",
          delivery_id: "",
          success: false,
          error: `Invalid JSON input: ${e.message}`,
        })
      );
      process.exit(1);
    }

    const result = deliverQuestion(question);
    console.log(JSON.stringify(result));
  });
}

main();
