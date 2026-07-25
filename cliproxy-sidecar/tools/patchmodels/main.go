// Command patchmodels inserts the Claude Opus 5 model into a CLIProxyAPI models.json registry
// file — upstream PR router-for-me/CLIProxyAPI#4547, which is unmerged, so the pinned release
// doesn't know opus-5's provider and rejects requests for it ("unknown provider for model
// claude-opus-5"). This is applied at build time to a WRITABLE copy of the pinned CLIProxyAPI
// module (see the Dockerfile go-build stage), which the sidecar build then points at via a
// filesystem `replace`, so the sidecar advertises opus-5 in /v1/models and routes it to the
// anthropic channel.
//
// The registry is keyed by provider: {"claude":[…],"gemini":[…],…}. We append the opus-5 entry to
// the "claude" list, leaving every other provider byte-identical (json.RawMessage passthrough).
// Idempotent: a no-op if claude-opus-5 is already present, so a future upstream that ships it
// natively needs no change here. Stdlib-only so it runs with just the Go toolchain.
package main

import (
	"encoding/json"
	"fmt"
	"os"
)

// The exact model definition from PR #4547.
const opus5Entry = `{
  "id": "claude-opus-5",
  "object": "model",
  "created": 1784038800,
  "owned_by": "anthropic",
  "type": "claude",
  "display_name": "Claude Opus 5",
  "description": "Latest premium model combining maximum intelligence with practical performance",
  "context_length": 1000000,
  "max_completion_tokens": 128000,
  "thinking": {
    "zero_allowed": true,
    "dynamic_allowed": true,
    "levels": ["low", "medium", "high", "xhigh", "max"]
  }
}`

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: patchmodels <path/to/models.json>")
		os.Exit(2)
	}
	path := os.Args[1]

	raw, err := os.ReadFile(path)
	if err != nil {
		fatal(err)
	}

	// Preserve every provider verbatim; only "claude" is rewritten.
	var doc map[string]json.RawMessage
	if err := json.Unmarshal(raw, &doc); err != nil {
		fatal(fmt.Errorf("parse %s: %w", path, err))
	}
	claudeRaw, ok := doc["claude"]
	if !ok {
		fatal(fmt.Errorf(`%s: no "claude" provider key`, path))
	}
	var claude []json.RawMessage
	if err := json.Unmarshal(claudeRaw, &claude); err != nil {
		fatal(fmt.Errorf(`%s: "claude" is not a list: %w`, path, err))
	}

	for _, m := range claude {
		var id struct {
			ID string `json:"id"`
		}
		_ = json.Unmarshal(m, &id)
		if id.ID == "claude-opus-5" {
			fmt.Println("patchmodels: claude-opus-5 already present; no-op")
			return
		}
	}

	var entry json.RawMessage
	if err := json.Unmarshal([]byte(opus5Entry), &entry); err != nil {
		fatal(fmt.Errorf("opus5 entry is invalid JSON: %w", err))
	}
	claude = append(claude, entry)

	newClaude, err := json.Marshal(claude)
	if err != nil {
		fatal(err)
	}
	doc["claude"] = newClaude

	out, err := json.MarshalIndent(doc, "", "  ")
	if err != nil {
		fatal(err)
	}
	out = append(out, '\n')
	if err := os.WriteFile(path, out, 0o644); err != nil {
		fatal(err)
	}
	fmt.Println("patchmodels: added claude-opus-5 to the claude registry")
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "patchmodels:", err)
	os.Exit(1)
}
