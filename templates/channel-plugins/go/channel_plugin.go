// TA channel plugin skeleton — reads ChannelQuestion JSON from stdin,
// delivers it, and writes DeliveryResult JSON to stdout.
//
// Usage:
//
//	1. Copy this directory to .ta/plugins/channels/my-channel/
//	2. Edit channel.toml with your plugin name and command
//	3. Implement deliverQuestion() below
//	4. Test: echo '{"interaction_id":"...","question":"test"}' | go run channel_plugin.go
//
// Protocol:
//   - TA writes one JSON line to stdin: a ChannelQuestion object
//   - Plugin writes one JSON line to stdout: a DeliveryResult object
//   - Human responses go back to TA via HTTP:
//     POST {callback_url}/api/interactions/{interaction_id}/respond
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
)

// ChannelQuestion from TA.
type ChannelQuestion struct {
	InteractionID string   `json:"interaction_id"`
	GoalID        string   `json:"goal_id"`
	Question      string   `json:"question"`
	Context       *string  `json:"context"`
	ResponseHint  string   `json:"response_hint"`
	Choices       []string `json:"choices"`
	Turn          int      `json:"turn"`
	CallbackURL   string   `json:"callback_url"`
}

// DeliveryResult to return to TA.
type DeliveryResult struct {
	Channel    string  `json:"channel"`
	DeliveryID string  `json:"delivery_id"`
	Success    bool    `json:"success"`
	Error      *string `json:"error"`
}

func deliverQuestion(q ChannelQuestion) DeliveryResult {
	// TODO: Replace with your channel delivery logic.
	// Examples:
	//   - Post to a Slack webhook using net/http
	//   - Send a Teams Adaptive Card
	//   - Send a push notification

	fmt.Fprintf(os.Stderr, "[my-channel] Delivering question: %s\n", q.Question)

	return DeliveryResult{
		Channel:    "my-channel",
		DeliveryID: fmt.Sprintf("msg-%s", q.InteractionID),
		Success:    true,
		Error:      nil,
	}
}

func main() {
	scanner := bufio.NewScanner(os.Stdin)
	if !scanner.Scan() {
		errMsg := "No input received on stdin"
		result := DeliveryResult{
			Channel:    "my-channel",
			DeliveryID: "",
			Success:    false,
			Error:      &errMsg,
		}
		json.NewEncoder(os.Stdout).Encode(result)
		os.Exit(1)
	}

	line := scanner.Text()
	var question ChannelQuestion
	if err := json.Unmarshal([]byte(line), &question); err != nil {
		errMsg := fmt.Sprintf("Invalid JSON input: %v", err)
		result := DeliveryResult{
			Channel:    "my-channel",
			DeliveryID: "",
			Success:    false,
			Error:      &errMsg,
		}
		json.NewEncoder(os.Stdout).Encode(result)
		os.Exit(1)
	}

	result := deliverQuestion(question)
	json.NewEncoder(os.Stdout).Encode(result)
}
