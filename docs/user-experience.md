# Trusted Autonomy v1.0 — User Experience Walkthrough

> Based on PLAN.md phases v0.1-v1.0. Written to stress-test the product plan from five real user perspectives. Each persona includes an honest comparison: what can you already do with Claude/Codex/Claude Desktop alone, and what does TA specifically add?
>
> See also: `docs/enterprise-state-intercept.md` for the deep containment model (network-level capture for enterprise/regulated environments).

---

## The Core Question: Why Not Just Use Claude Directly?

Claude Code, Codex, Claude Desktop, and Claude Teams are already powerful. A home user can ask Claude Desktop to categorize transactions from a CSV. A developer can use Codex to generate PRs. A business person can use Claude Teams to draft emails. **TA must earn its place on top of tools that already work.**

TA's value proposition is not "make AI possible" -- it's **"make AI autonomous."** The shift from synchronous conversation to asynchronous delegation:

| Mode | Agent alone (Claude/Codex/etc.) | TA-mediated |
|------|--------------------------------|-------------|
| **Active work** | You drive the conversation. ~100% attention. Agent does what you ask, one turn at a time. | `ta run` launches agent. Check back when draft ready. ~10-20% attention. |
| **Batch/overnight** | Session closes when you leave. You start over tomorrow. | Agent works in background. Review next morning. 0% during execution. |
| **Recurring tasks** | You re-prompt every time. Copy-paste results manually. | Workflow runs on triggers. Agent drafts, you review when notified. |
| **Multi-step external actions** | Agent calls APIs in real-time -- you approve each one synchronously, or trust blindly. | All external actions held as drafts. Review batch. Approve/reject selectively. |
| **Credentials** | You paste API keys into the conversation, or configure per-agent. | TA brokers all credentials. Agents never see raw secrets. |
| **Audit & compliance** | No record beyond chat history. | Hash-verified audit trail with provenance, auto-generated compliance evidence. |

**If you're happy with synchronous, one-off conversations -- you don't need TA.** TA is for when you want agents running autonomously on your behalf, with governance you can trust and escalate over time.

---

## 1. Software Engineer

**Who**: Full-stack dev, uses Claude Code / Codex / Cursor daily. Has a Rust+React monorepo. Wants AI to handle the tedious stuff safely.

### Day-to-day workflow at v1.0

```
# Morning: pick up where yesterday's agent left off
ta plan next                        # "v0.8.2 -- API pagination. Suggest: ta run ..."
ta run "Add cursor pagination" \
  --phase 0.8.2 --macro  # macro goal: agent stays in session

# Agent works in staging. I watch output stream in terminal (--interactive).
# Agent hits a design question -- I see it in real-time, type guidance.
# Agent builds sub-goal drafts as it goes. Each one pauses for my review.

ta draft view <id>                  # same format I'll see in the git commit
# Looks good -- approve inline from the session, or:
ta draft approve <id>

# Agent continues to next sub-goal. Repeat until done.
# Session ends. All approved changes applied. Git commit auto-created.

# Afternoon: overnight agent handled the refactor
ta draft list                       # 3 drafts auto-approved by supervisor
ta audit drift claude-code          # any unusual behavior? No drift detected.
# Supervisor auto-approved because: <10 files, risk score 8, all in src/
# I see the summary, trust the supervisor, move on.
```

### What they need to know

- `ta run` / `ta draft` / `ta plan` -- three commands cover 90% of use
- `.ta/workflow.toml` for project config (git integration, enforcement level)
- Agent YAML configs exist but defaults work -- auto-detection picks the right agent
- Review model: every change is a "draft" you approve/reject, like a PR but richer
- Zero-config path works: `ta run "fix the login bug"` needs nothing pre-configured

### What you already get without TA

| Capability | Claude Code | Codex | Cursor |
|-----------|------------|-------|--------|
| Edit files from natural language | Yes -- direct edits, you review in terminal | Yes -- creates git commits/PRs | Yes -- inline suggestions |
| Multi-file refactors | Yes -- but you watch the whole session | Yes -- headless, creates a PR | Limited |
| Code review of changes | Git diff after the fact | GitHub PR diff | Inline diff |
| Overnight/batch work | No -- session closes | Yes -- but all-or-nothing PR | No |
| Plan/roadmap tracking | No | No | No |
| Audit trail | Chat history (ephemeral) | Git history | None |
| Multi-agent coordination | Manual | N/A | N/A |

**Key insight**: Codex already provides headless execution + PR-based review. Claude Code already provides interactive editing. For a developer, TA's competition isn't "no AI" -- it's the native workflows these tools already provide.

### What TA adds on top

1. **Structured review of large changes**: Codex gives you a git diff. TA gives you per-file explanations, disposition tracking (approve this file, reject that one), and impact summaries. For a 40-file change, the diff is noise -- the structured review is signal.
2. **Selective approval**: Codex PRs are all-or-nothing (merge or close). TA lets you approve 38 files, reject 2, and ask the agent to revise just those 2. No re-running the whole job.
3. **Batch async with trust escalation**: Codex can run overnight, but you still review every PR manually. TA's supervisor agent auto-approves routine changes within constitutional bounds -- you only see escalations. This is the difference between reviewing 20 PRs/day vs 2.
4. **Agent-agnostic governance**: Same review workflow whether the agent is Claude Code, Codex, Cursor, or a custom script. Switch agents without switching governance.
5. **Plan-integrated execution**: `ta run --phase 0.8.2` ties work to a roadmap. Agent knows what to build next. No re-explaining context each session.
6. **Compliance as byproduct**: If you're in a regulated environment, TA's audit trail and provenance hashes are already ISO/EU AI Act compliant. Without TA, you'd build this yourself.

### When TA isn't worth it

- Quick one-off fixes: `claude "fix the typo in README"` is faster than `ta run`. Don't add governance overhead to trivial tasks.
- Solo developer on a small project with no compliance needs: git history + Codex PRs may be sufficient.
- Exploratory prototyping where you want the agent to just go: TA's review-before-apply model slows down "let it rip" development.

### Supervision frequency

- **Starting out**: Review every draft manually. ~15 min/day for a typical project.
- **After trust builds**: Set up constitutional auto-approval. Review only escalations and daily summary. ~5 min/day.
- **Mature usage**: Supervisor handles routine, human handles strategy. Drafts per week: 20+ agent-reviewed, 2-3 human-reviewed.
- **Comparison**: Codex requires reviewing every PR (~15 min/day indefinitely). Claude Code requires full attention during session (~hours/day). TA's supervision cost decreases over time as trust escalates.

### Remaining friction

- Must learn TA vocabulary (goals, drafts, phases, dispositions) on top of git -- needs to feel like a natural extension, not a second system
- Staging copy doubles disk for large repos (COW planned for future)
- `ta run "fix bug"` must feel as frictionless as `claude "fix bug"` -- if it doesn't, developers will skip TA for small tasks and lose the audit trail

---

## 2. Product / Business Person

**Who**: Manages a team shipping a SaaS product. Not a coder. Uses email, Slack, Google Docs daily. Wants AI assistants to handle routine comms and reporting -- but needs to trust what they send.

### Setup experience (v0.7.0 guided setup)

```
# Install TA (desktop installer bundles everything)
# First run launches setup assistant -- itself a TA-mediated agent:

ta setup --template email-assistant

# Agent asks questions in plain language:
#   "Which email account should I manage? (I'll open a Google sign-in)"
#   "What kinds of emails should I draft replies for?"
#   "Where should I notify you when drafts are ready? (Slack/email/web)"
#
# Agent proposes a config. TA shows it as a draft:
#   - workflow.toml with email role definition
#   - Gmail OAuth credentials (stored in vault, never shown)
#   - Constitutional bounds: "auto-draft for routine, escalate for new contacts"
#
# Review and approve. Config activates.
```

### Day-to-day workflow at v1.0

```
# Monday morning: notification arrives in Slack
# "3 drafts ready: Weekly Status Report (2 emails + 1 Slack message)"
# Click through to web review UI (localhost or LAN)

# See exactly what the agent wants to do:
#   - Send email to team@company.com: "Q1 Progress Update" (preview full body)
#   - Post to #general Slack: summary message (preview)
#   - Update Google Doc: status tracker (see diff)

# Approve the email, edit the Slack message, reject the doc update with note.
# Agent sees rejection, revises, resubmits. Approve the revision.

# Customer reply came in overnight. Agent drafted a response.
# Review in Slack thread (reply "approve" or type feedback).
# Approved. Agent sends via TA credential broker -- never touches the OAuth token.
```

### What they need to know

- How to review and approve/reject (web UI or Slack/email)
- How to give feedback when agent gets something wrong (comment on draft)
- Setup wizard handles everything else -- no YAML, no CLI

### What you already get without TA

| Capability | Claude Desktop / Teams | Zapier / Make | Custom GPT |
|-----------|----------------------|--------------|------------|
| Draft emails from prompts | Yes -- paste context, get draft, copy to Gmail | Yes -- trigger-based, template-driven | Yes -- conversational |
| Weekly status reports | Yes -- paste data, get report each time | Yes -- automated but rigid templates | Yes -- but manual each time |
| Multi-channel (email + Slack + Docs) | One channel at a time, you orchestrate | Yes -- multi-step zaps | No |
| Review before sending | You read the output and copy-paste | No -- fires automatically or not at all | You copy-paste |
| Credential safety | Paste into conversation or connect via integration | OAuth per-zap, platform holds credentials | Via platform |
| Learns your preferences | Within conversation context window | No -- template-based | Somewhat via instructions |

**Key insight**: Claude Desktop/Teams already drafts great emails. The UX is polished. For a single-channel, one-at-a-time workflow, Claude Desktop is easier than TA. TA's competition here isn't "no AI" -- it's Claude's own consumer products, plus automation platforms like Zapier/Make.

### What TA adds on top

1. **Batch review, not one-at-a-time**: Claude Desktop drafts one email per conversation turn. TA's agent drafts 10 emails overnight, categorized and prioritized. You review the batch in one sitting. This is the difference between "AI as writing assistant" and "AI as delegation."
2. **Hold-before-send for external actions**: Zapier fires actions automatically -- you trust it or you don't. Claude Desktop gives you text to copy-paste -- safe but manual. TA holds every outbound action (email send, Slack post, doc update) in a draft queue. You see exactly what will happen, approve selectively, and TA executes. No copy-paste, no blind automation.
3. **Persistent workflows without programming**: Zapier requires building zaps with triggers and actions. TA's setup wizard is conversational -- describe what you want, agent proposes the config, you approve. The workflow persists and runs on schedule, not per-conversation.
4. **Trust escalation**: Start with manual review of everything. After the agent proves reliable for routine emails, set constitutional auto-approval for low-risk actions. You gradually reduce supervision without all-or-nothing trust. Neither Claude Desktop (always manual) nor Zapier (always automatic) offers this gradient.
5. **Credential isolation**: Claude Desktop conversations may see sensitive content you paste in. Zapier holds OAuth tokens on their servers. TA stores credentials locally in an encrypted vault -- the agent never sees raw tokens, and nothing leaves your machine.

### When TA isn't worth it

- Occasional email drafting: Opening Claude Desktop and saying "draft a reply to this" is faster for one-off tasks.
- Simple, predictable automations: If a Zapier zap does what you need and you trust it, the governance overhead of TA is unnecessary.
- Teams already using Claude Teams with built-in sharing: TA's web UI would need to match that polish, and it may not at v1.0.

### Supervision frequency

- **Week 1**: Review every draft. ~20 min/day. Learning what the agent does well.
- **Month 1**: Constitutional auto-approval for routine emails. Review only new contacts, complex threads. ~5 min/day.
- **Mature**: Daily summary notification. Handle 2-3 escalations per day. ~3 min/day.
- **Comparison**: Claude Desktop requires full attention per task (~minutes each, unbounded daily total). Zapier requires zero attention but offers zero review. TA starts like Claude Desktop (review everything) and trends toward Zapier (auto-approve routine) while maintaining the ability to intervene.

### Remaining friction

- Web review UI must be polished enough for non-technical users -- Claude Desktop sets the bar high
- MCP server health/connectivity issues need clear error messages, not stack traces
- Multi-user (team) workflows not addressed until v1.0 virtual office
- **Honest question**: Is a non-technical user going to install and run a localhost service? The distribution/onboarding story (v0.9) is critical for this persona.

---

## 3. Home User (Email & Social Media)

**Who**: Non-technical. Uses email, Instagram, X/Twitter. Wants an AI assistant to manage social presence, respond to routine emails, schedule posts.

### Setup experience

```
# Install TA from desktop installer (Mac/Windows/Linux)
# Opens setup wizard in browser (localhost web UI)

# Conversational flow:
#   "What would you like help with?"
#   > "Managing my Instagram and handling routine emails"
#
#   "Let's connect your accounts." (OAuth popups for Gmail, Instagram)
#   "How often should I post to Instagram?"
#   > "3 times a week from my photo library"
#
#   "Here's what I've set up. Review before activating:"
#   [Shows proposed config as a simple checklist, not YAML]
#   - Email: draft replies to routine emails, escalate business inquiries
#   - Instagram: 3 posts/week, you review captions before posting
#   - Notifications: email digest at 9am with pending drafts
#
#   [Approve] -> Config activates.
```

### Day-to-day workflow at v1.0

```
# Morning: open TA app (web UI), see dashboard
#   - 2 Instagram posts drafted (preview images + captions)
#   - 4 email replies drafted
#   - 1 flagged: "Business inquiry -- needs your input"

# Tap each draft. Swipe to approve/reject.
# Edit caption on Instagram post before approving.
# Type guidance for the flagged email: "Tell them I'm available next week"
# Agent revises and resubmits. Approve.

# Weekly: check summary
#   15 posts published, 28 emails handled
#   0 alerts, agent staying on track
#   Cost: $3.40 in API tokens this week
```

### What they need to know

- How to review and approve (tap/swipe in web UI)
- How to give feedback when the agent gets it wrong
- Where to adjust preferences ("don't post before 10am")

### What you already get without TA

| Capability | Claude Desktop / ChatGPT | Buffer / Hootsuite | IFTTT / Shortcuts |
|-----------|-------------------------|-------------------|------------------|
| Draft social posts | Yes -- describe what you want, get captions | No -- you write, they schedule | No |
| Draft email replies | Yes -- paste email, get reply | N/A | N/A |
| Schedule posts | No -- you copy-paste to the platform | Yes -- core feature, calendar UI | Limited triggers |
| Manage across platforms | One at a time, you orchestrate | Yes -- multi-platform dashboard | Trigger-based |
| Review before posting | You read the output | Yes -- queue with preview | No -- fires automatically |
| Learns your voice/style | Within context window | No | No |
| Privacy | Data goes to Anthropic/OpenAI servers | Data on their servers | Local (Shortcuts) or cloud (IFTTT) |

**Key insight**: For social media, the real competitors are Buffer/Hootsuite (scheduling) and Claude Desktop (drafting). A home user already has good options. For email, Claude Desktop + manual copy-paste works. TA needs to be significantly easier than the combination of these existing tools.

### What TA adds on top

1. **Unified AI + execution in one place**: Today you'd use Claude Desktop to draft, then copy to Buffer to schedule, then manually check Gmail for replies. TA's agent drafts posts, schedules them, and handles email replies -- all in one workflow with one review queue. The value is eliminating the orchestration between 3-4 separate tools.
2. **Review queue, not copy-paste**: Claude Desktop gives you text to copy somewhere. TA shows you "here's what I want to post to Instagram and here's the email I want to send" -- approve and it happens. No clipboard, no switching apps.
3. **Persistent automation without Zapier complexity**: IFTTT/Zapier can automate posting but require building trigger-action chains. TA's setup wizard lets you say "post 3 times a week from my photos" in plain language. The agent proposes a workflow; you approve.
4. **Privacy (local-first)**: Claude Desktop sends your data to Anthropic's servers. Buffer/Hootsuite store your content on their servers. TA runs locally -- your photos, emails, and social content stay on your machine. The agent calls APIs through TA's credential broker, but your data isn't stored in someone else's cloud.
5. **Trust escalation**: Start reviewing every post. After a month, auto-approve routine posts that match your style guide. Neither Claude Desktop (always manual) nor Buffer (no AI review) offers this middle ground.
6. **Cost visibility**: See exactly what AI costs per week. Claude Desktop Pro is $20/month flat. TA's per-token cost may be lower for light usage or higher for heavy usage -- transparent either way.

### When TA isn't worth it

- Casual social media use: If you post 2-3 times a week manually, the setup cost of TA exceeds the time saved.
- Already happy with Buffer + ChatGPT: If the copy-paste workflow works for you, TA adds complexity for marginal improvement.
- Privacy isn't a concern: If you're fine with cloud services, the local-first advantage doesn't matter.
- **Honest assessment**: This is TA's hardest sell. A non-technical home user is being asked to install a localhost service, understand "drafts" and "approvals," and manage API costs -- when Claude Desktop's chat interface "just works" for one-off tasks. TA's value only kicks in when the user wants persistent automation, not just occasional AI help.

### Supervision frequency

- **Week 1**: Review everything. ~15 min/day.
- **Month 1**: Auto-approve routine email replies (low risk, familiar contacts). ~5 min/day.
- **Mature**: Glance at daily digest. Handle 1-2 items. ~2 min/day.
- **Comparison**: Claude Desktop requires active engagement per task (~5 min each, as needed). Buffer/Hootsuite require queue management (~10 min/day). TA's supervision cost starts higher but decreases as trust escalates -- break-even vs. manual tools at ~month 2 if you have enough volume.

### Remaining friction

- No mobile native app (PWA must work well on phone)
- Onboarding must be conversational, never show YAML or terminal
- Cost must be predictable -- "this month will cost ~$15" not surprises
- **Distribution gap**: This user won't `cargo install` or run a CLI. Desktop installer + browser-based setup is minimum bar. Even that may be too much -- compare to downloading the Claude Desktop app.

---

## 4. Home Finance Manager

**Who**: Manages household budget, tracks investments, pays bills. Wants dashboards and reports without spreadsheet drudgery. May also be a family office administrator managing multiple accounts.

### Setup experience

```
ta setup --template home-finance

# Agent guides through:
#   "Let's connect your bank accounts." (Plaid OAuth flow -- TA stores credentials)
#   "Which accounts? Checking, savings, credit card, brokerage?"
#   "How should I categorize transactions? (I'll learn from your corrections)"
#   "Monthly report format: dashboard HTML, email summary, or both?"
#
# Proposed config as draft:
#   - Plaid connection: Chase checking, Fidelity brokerage (read-only)
#   - Weekly categorization review: agent categorizes, you review mistakes
#   - Monthly dashboard: spending by category, budget vs actual, investment performance
#   - Bill monitoring: flag anomalies (unexpected charges, price increases)
#   - Constitutional bounds: read-only access -- agent cannot initiate transfers
#
# [Approve] -> Activates.
```

### Day-to-day workflow at v1.0

```
# Weekly: notification "Transaction review ready"
# Open web UI: 47 transactions this week, agent categorized all
#   - 3 flagged as uncertain: "Costco -- Groceries or Household?"
#   - 1 anomaly: "Netflix increased from $15.49 to $22.99"
# Correct the 3 categories. Agent learns for next time.
# Acknowledge the Netflix increase.

# Monthly: "Monthly finance report ready"
# Open HTML dashboard (rendered by TA's output adapter system):
#   - Spending by category with month-over-month trends
#   - Budget vs actual with variance alerts
#   - Investment portfolio performance (daily/monthly/YTD)
#   - Upcoming bills with estimated dates
# Agent also drafted a summary email to spouse. Review and approve sending.

# Tax season: "Tax document prep ready"
# Agent collected all deductible transactions, organized by category
# Generated summary PDF for accountant. Review draft before sharing.
```

### Family office extension

```
# Setup adds multi-account, multi-person structure:
ta setup --template family-office

# Tiered access (enforced by TA credential broker):
#   Principal: sees all accounts, all reports, can approve transfers
#   Advisor: sees portfolio data, generates reports, no transaction detail
#   Accountant: sees tax-relevant transactions only, during tax season only
#
# Each person authenticates via SSO/OAuth -- gets their scoped view
# Agent runs reports for each tier separately -- same data, different views
# All access logged in audit trail
```

### What you already get without TA

| Capability | Claude Desktop / ChatGPT | Mint / YNAB / Copilot | Excel + Claude |
|-----------|-------------------------|----------------------|----------------|
| Categorize transactions | Yes -- paste CSV, get categories | Yes -- auto-categorize from bank feed | Claude categorizes, you update sheet |
| Monthly dashboard | Yes -- ask for charts from data | Built-in dashboards | Claude generates charts from sheet |
| Budget tracking | Yes -- if you provide the data each time | Yes -- core feature, auto-updated | Manual + Claude analysis |
| Bill monitoring | No -- no persistent access | Some -- alerts for unusual charges | No |
| Investment tracking | Limited -- no real-time data | Copilot does some, Mint limited | Manual data entry + Claude analysis |
| Tax prep | Yes -- paste transactions, get summaries | Limited export features | Claude organizes, you verify |
| Bank connection | No -- you export CSV manually | Yes -- Plaid/bank feeds built-in | No |
| Privacy | Data goes to AI provider's servers | Data on their servers | Local (Excel) + AI servers (Claude) |
| Multi-user/family | Within conversation | Yes -- shared accounts | Shared file |

**Key insight**: Dedicated finance apps (Mint, YNAB, Monarch, Copilot) already solve categorization, dashboards, and bank connectivity. Claude Desktop can analyze any financial data you paste in. The combination of "finance app for data + Claude for analysis" covers most needs. TA's competition is this existing stack, not "doing it manually."

### What TA adds on top

1. **AI-powered analysis with persistent bank access**: Mint/YNAB categorize but don't analyze or explain. Claude analyzes but you re-paste data every time. TA connects to your bank via Plaid (once), and the agent analyzes weekly -- automatically, with persistent context. "Your grocery spending is up 22% this month, mostly from Whole Foods" without you lifting a finger.
2. **Local-first with real bank data**: Claude Desktop requires pasting sensitive financial data into a cloud conversation. Mint/Copilot store your financial data on their servers (and Mint was shut down). TA runs locally -- bank credentials in an encrypted vault, transaction data on your machine. The agent accesses data through TA's credential broker with read-only scope.
3. **Review before any action**: YNAB/Copilot update automatically -- you trust or you don't. Claude Desktop gives you text to copy. TA holds every output (dashboard, email to spouse, tax summary for accountant) as a draft. You review before anything is shared or published.
4. **Custom reports beyond templates**: Mint/YNAB give you their dashboards. Claude can build custom reports but you re-prompt each time. TA's agent builds the reports you actually want (because you described them in setup), regenerates them on schedule, and you just review.
5. **Family office tiered access**: No consumer finance app handles "principal sees everything, advisor sees portfolio only, accountant sees tax-relevant transactions during tax season." TA's credential broker + constitutional configs make this possible without building a custom application.
6. **Audit trail for financial data access**: Every time the agent reads your transactions, it's logged -- who (which agent), what (which accounts), when. Required for family office governance, useful for personal peace of mind.

### When TA isn't worth it

- Simple budgeting: YNAB or Monarch already does 90% of what you need with zero setup complexity.
- No bank API needs: If you're happy exporting CSV and pasting into Claude Desktop monthly, the Plaid integration overhead isn't worth it.
- Single person, simple finances: TA's tiered access, audit trails, and constitutional configs are overkill. Use Claude Desktop + a spreadsheet.
- **Honest assessment**: For household budgets, the primary value is "persistent automated analysis with local privacy." For family office, it's "tiered access + audit trail without building custom software." The household use case is a nice-to-have; the family office use case is where TA solves a real gap.

### Supervision frequency

- **Weekly**: 5-10 min reviewing categorizations and flagged items
- **Monthly**: 10 min reviewing dashboard and report before sharing
- **Tax season**: 30 min reviewing collected documents before sending to accountant
- **Comparison**: Mint/YNAB require ~5 min/week to verify categories (similar). Claude Desktop requires ~15 min/month to re-paste data and re-prompt for reports (TA eliminates this). Family office: without TA, you'd spend hours managing separate logins and manually restricting what each person sees.

---

## 5. Areas to Examine Before Going Too Far

### Where TA's value is strongest vs weakest (honest summary)

| Persona | Without TA baseline | TA's incremental value | Strength |
|---------|--------------------|-----------------------|----------|
| **SW Engineer** | Codex PRs + Claude Code interactive | Selective approval, trust escalation, multi-agent governance, compliance | **Strong** -- real gaps in batch review + trust escalation |
| **Product Person** | Claude Teams/Desktop + Zapier | Unified review queue, hold-before-send, credential isolation | **Medium** -- competing against polished UIs; TA's value is delegation + governance |
| **Home User** | Claude Desktop + Buffer/Hootsuite | Unified automation, local privacy, trust escalation | **Weak** -- hardest onboarding, most competition from consumer tools |
| **Home Finance** | YNAB/Mint + Claude Desktop | Persistent analysis, local privacy, family office tiered access | **Medium** (household) / **Strong** (family office) -- consumer apps cover basics |

**Takeaway**: TA's strongest differentiation is for users who want **autonomous, persistent, governed agent workflows** -- not one-off AI conversations. The developer persona is the clearest fit. Non-technical personas require significant UX investment to compete with existing consumer AI products.

### Critical path validation (do these spikes before committing)

1. **MCP interception spike**: Intercept one MCP tool call, hold it, replay it. Confirm the pattern works. This is the biggest architecture bet -- if it doesn't work cleanly, v0.5+ needs redesign.
2. **Credential broker spike**: OAuth flow -> encrypted storage -> scoped session token -> MCP server call. End-to-end. If this is clunky, every non-filesystem use case suffers.
3. **Web UI spike**: Serve one HTML page from `ta daemon`, render one draft, approve it. If the daemon architecture doesn't support this cleanly, v0.5.2 is harder than expected.
4. **Plaid integration spike**: Connect one bank account, fetch transactions, display in `ta draft view`. Validates the finance use case isn't blocked by API complexity.
5. **Auto-approval spike**: Supervisor agent evaluates one draft against a constitutional config. If the LLM-based verification is unreliable, v0.6.0 needs a different approach (rule-based only).
6. **Onboarding comparison spike**: Install TA, set up one workflow, complete one task -- time it. Compare to the same task in Claude Desktop. If TA takes 3x longer to first value, the non-dev personas won't convert.

### Workflow openness checklist

| Area | Open or fixed? | Status |
|------|---------------|--------|
| Agent framework | Open -- any CLI agent (Claude, Codex, custom) via YAML config | Good |
| External services | Open -- any MCP server, no built-in service clients | Good |
| Plan format | Open after v0.3.1.1 -- schema-driven parsing | Good |
| Review channel | Open after v0.3.1.2 -- SessionChannel trait, any adapter | Good |
| Auth/identity | Open after v0.5.0 -- OAuth, API key, SSO, custom | Good |
| Output format | Open -- terminal, HTML, JSON, markdown adapters | Good |
| Workflow logic | **Risk**: Constitutional configs (v0.6.0) could become rigid. Ensure they're composable, not monolithic. | Watch |
| Trigger system | **Risk**: v1.0 trigger system (cron, webhook, event) may not cover all cases. Ensure extensible. | Watch |
| Finance integrations | **Risk**: Plaid is US-centric. International users need Open Banking, Yodlee, or manual CSV import. | Watch |

### What NOT to build (leave to the ecosystem)

- Custom MCP servers for specific services -- use existing ones, contribute patches upstream
- A mobile native app -- PWA is sufficient for v1.0
- An LLM model -- use Claude/GPT/local models via agent framework
- A cloud hosting platform -- let users self-host; cloud can be a wrapper later
- Accounting software -- TA generates reports, doesn't replace QuickBooks
- A consumer chat UI to compete with Claude Desktop -- TA's UI is for review/governance, not conversation
- Social media scheduling features -- defer to MCP servers wrapping existing platforms

### Suggested priority order

The current plan phases are mostly in the right order. Key reorderings already made:
1. **Credential broker** moved to v0.5.0 (prerequisite for all external actions)
2. **Web review UI** moved to v0.5.2 (unblocks non-dev users early)
3. **Notifications** moved to v0.5.3 (needed immediately when external actions land)
4. **Supervisor/auto-approval** becomes v0.6 (unlocks async supervision model)
5. **Network intercept** extracted to enterprise doc (not blocking v1.0)
6. **Native Windows support** at v0.9.1 (when non-engineers arrive via guided setup)
7. **Sandbox** becomes optional hardening at v0.9.2 (not a prerequisite)
8. **Setup wizard + templates** becomes v0.7 (enables non-dev onboarding)

### Strategic positioning

TA is not a replacement for Claude Desktop, Codex, or any specific AI tool. It's a **governance and orchestration layer** that wraps whatever agents the user already prefers. The pitch:

- **To developers**: "Keep using Claude Code / Codex / Cursor. TA adds the review, trust, and compliance layer you'd otherwise build yourself."
- **To business users**: "Keep using Claude for drafting. TA makes it persistent, automated, and safe to delegate -- with a review queue instead of copy-paste."
- **To home users**: "TA turns AI assistants into AI staff -- they work while you sleep, and you review in the morning."
- **To enterprises**: "TA provides the audit, compliance, and containment evidence your security and legal teams require."

The developer persona should drive v0.3-v0.6. Non-dev personas unlock at v0.7+ (setup wizard, web UI, templates). Don't optimize for non-dev UX before the governance model is proven with developers.
