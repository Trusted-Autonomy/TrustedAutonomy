# TA Studio Walkthrough: Building TaskFlow from Scratch

A complete end-to-end guide for product managers, team leads, and anyone who wants AI to build software — with full control over what gets shipped.

---

## 1. What Is TA Studio?

TA Studio is a web interface for Trusted Autonomy — a tool that lets an AI agent write code for your project while keeping you in control of every change. Think of it as a very capable contractor who does the work in a separate workspace, shows you exactly what they built, and waits for your sign-off before anything touches your real project.

The core idea is simple: the agent never writes directly to your codebase. It works in a private staging copy, and when it finishes, you get a draft — a complete summary of what changed and why, with a side-by-side view of every file. You can read through it, ask questions, approve it, or throw it away. Nothing gets applied until you click Approve.

The workflow loops like this: you describe a goal in plain English, the agent works on it (usually for a few minutes), you review the draft, and if everything looks good, you approve it. TA then applies the changes, creates a pull request in your version control system, and moves on to the next goal. Your job is to set direction and review outcomes — the agent handles the implementation.

---

## 2. Installing TA

Open a terminal and run:

```bash
ta install
```

That's it. The installer sets everything up and opens TA Studio in your browser. If you don't have a terminal handy, the TA download page includes a graphical installer for macOS and Windows — just double-click and follow the prompts.

> **Note:** If `ta` isn't recognized after installation, close and reopen your terminal, or restart your computer. The installer adds `ta` to your system path, but some shells need a fresh start to pick it up.

---

## 3. First-Run Setup Wizard

The first time TA Studio opens, it walks you through a five-step setup. This is a one-time process — once you're through it, you go straight to the home screen on future visits.

### Step 1: Agent System

The first thing TA needs is an AI model to do the work. The default option is Claude by Anthropic, which is what this guide uses.

You'll see a screen with a single field labeled **Anthropic API Key**. Paste your key there — it looks like `sk-ant-api03-...`. Click **Validate**.

After a moment, the field border turns green and a checkmark appears next to the word "Validated." You'll also see the model name confirmed: **Claude Sonnet** is selected by default, which is a good balance of speed and quality for most projects.

> **Note:** Don't have an API key? Visit console.anthropic.com, create a free account, and generate a key under API Keys. You'll need to add a payment method for usage beyond the free tier.

Click **Continue**.

### Step 2: Version Control

TA needs somewhere to put your code. This step connects it to GitHub so it can create branches and pull requests automatically.

The screen shows three options: GitHub, GitLab, and None (local only). Click **GitHub**.

You'll see two fields: **Repository URL** and **GitHub Token**.

You don't have a repo yet, so click the link that says **Create a new repository on GitHub**. This opens GitHub in a new tab. Create a new repository named `taskflow` — set it to Private if this is internal work, then copy the URL from your browser's address bar. It will look like `https://github.com/yourname/taskflow`.

Paste that URL into the Repository URL field.

For the GitHub Token: go to GitHub → Settings → Developer settings → Personal access tokens → Fine-grained tokens → Generate new token. Give it read/write access to your `taskflow` repository. Copy the token and paste it into the field.

Click **Connect**. The status updates to show a green connected badge: **"taskflow — connected"** with your GitHub username below it.

Click **Continue**.

### Step 3: Notifications

TA can notify you when a draft is ready or when a goal fails. This step is optional — you can skip it and add notifications later from Settings.

For this walkthrough, set up Discord. Click **Discord Webhook**. TA shows a short guide: in Discord, go to your server settings, pick the channel you want, click Integrations → Webhooks → New Webhook, and copy the webhook URL.

Paste the URL into the field. Click **Send Test Message**.

Within a few seconds, a message appears in your Discord channel:

> **Trusted Autonomy** — Test notification from TA Studio. If you see this, your webhook is working.

Click the **Looks good** button in Studio. A green checkmark confirms the connection.

Click **Continue**.

### Step 4: Create Your First Project

This is where you tell TA about your project.

Fill in the fields:

- **Project name:** `TaskFlow`
- **Description:** `Team task tracker`
- **First goal:** `Set up the project structure with React frontend and Node.js API`

Below the goal field, there's an **Approval Gate** setting. This controls when TA asks for your review. Leave it on **Always ask me** for now — this means every draft requires your explicit approval before anything is applied.

Click **Create Project**.

### Step 5: Ready

The final screen shows a summary of everything you've configured:

```
Project:     TaskFlow
Agent:       Claude Sonnet (validated)
Repository:  github.com/yourname/taskflow (connected)
Notify:      Discord — #dev-updates
Approval:    Always ask me
```

Click **Go to Studio →**

---

## 4. The Studio Home Screen

The home screen has three main areas.

On the left is the **sidebar**: navigation links for Goals, Drafts, Plan, Constitution, and Settings. There's also a quick-action button at the top labeled **+ New Goal**.

In the center is the **goal list** — currently empty, showing a placeholder: *"No goals yet. Start your first goal to see it here."*

On the right is the **draft queue** — also empty, with the message: *"Drafts appear here when a goal completes. Nothing to review yet."*

Take a moment to orient yourself. The main workflow lives between Goals and Drafts: you start a goal, the agent works, a draft shows up, you review it. That loop is most of what you'll do in Studio.

---

## 5. Creating the Initial Project Plan

Before running goals one by one, it helps to map out the whole project. TA Studio has a planning mode that turns a project brief into a structured set of phases.

Click **Plan** in the sidebar, then **New Plan** in the top right.

A text area appears with the prompt: *"Describe your project. What are you building and what does it need to do?"*

Type:

> "I want to build a task tracker web app for my team. It needs user accounts, the ability to create and assign tasks, due dates, and a simple dashboard showing what's overdue."

Click **Generate Plan**.

The planning agent takes about thirty seconds. When it finishes, you see a structured plan appear on screen:

```
TaskFlow — Project Plan

  v0.1.0  Project scaffold
          React (Vite) + Node.js (Express) + PostgreSQL setup
          Docker Compose for local development

  v0.2.0  User authentication
          Sign up, log in, log out
          JWT-based session management

  v0.3.0  Task CRUD API
          Create, read, update, delete tasks
          Assign tasks to users, set due dates

  v0.4.0  Frontend task list UI
          Display tasks, filter by status
          Mark tasks complete, drag to reorder

  v0.5.0  Dashboard & overdue tracking
          Summary view: overdue, due today, upcoming
          Email digest (daily summary)
```

Below each phase is a checkbox. You can accept, edit, or defer any phase.

Check **v0.1.0**, **v0.2.0**, and **v0.3.0**. These are approved and ready to run.

Click the edit icon next to **v0.4.0**. Change the description to:

> "Frontend task list UI — display tasks grouped by assignee, filter by status and due date, inline editing for task names and due dates."

Click the checkmark to save the edit.

For **v0.5.0**, click **Defer**. A label appears next to it: *"Deferred — will not run until re-enabled."* You can come back and turn this on after v0.4.0 ships and you've seen how the frontend feels.

Click **Save Plan**. The plan is now saved and visible from the Plan tab at any time.

---

## 6. Setting Up the Constitution with Agent Help

The Constitution is a set of rules that the agent must follow on every goal. Think of it as your team's coding standards, written in plain English, enforced automatically.

Click **Settings** in the sidebar, then **Constitution**.

The page is empty with one button: **Generate with Agent Help**. Click it.

A text area appears. Type:

> "This is a web app that real people will use. Security matters. Every API endpoint needs authentication. No hardcoded secrets. Tests are required."

Click **Generate Rules**.

The agent generates a set of rules and presents them as toggles, each one readable and specific:

```
✓  All API endpoints must require authentication middleware
✓  No secrets or API keys in source code (use environment variables)
✓  New functions must have at least one test
✓  Database queries must use parameterized statements (prevent SQL injection)
✓  Sensitive user data must not be logged
```

All five are toggled on by default. Review them — they look right. Click **Add These Rules**.

Now you want to add one more that the agent didn't think of. At the bottom of the constitution list, there's a text field: *"Add a rule..."*

Type: `All error messages must be user-friendly, not stack traces.`

Press **Enter**. The rule appears immediately at the bottom of the list with a toggle already enabled. The constitution now has six rules.

> **Note:** Constitution rules apply to every future goal in this project. The agent checks its output against these rules before marking a draft ready. If a rule would be violated, the agent tries to fix it before finishing.

Click **Save Constitution**.

---

## 7. Running the First Goal

Now for the main event. Click **Goals** in the sidebar, then **+ New Goal**.

The form pre-fills based on your plan:

- **Title:** `Set up the project structure with React frontend and Node.js API`
- **Agent:** Claude Sonnet
- **Phase:** v0.1.0

Everything looks correct. Click **Start Goal**.

The goal view opens. You can see a live status panel:

```
Goal: Set up the project structure...
Phase: v0.1.0
Status: Running
Elapsed: 0:23

Stage: Creating project directory structure
```

Below that, an activity log scrolls as the agent works:

```
[0:08]  Reading plan context for v0.1.0
[0:15]  Creating React project with Vite
[0:34]  Setting up Express API skeleton
[0:51]  Writing Docker Compose configuration for PostgreSQL
[1:12]  Adding environment variable templates (.env.example)
[1:29]  Writing initial README
[1:44]  Running tests — 4 passed
[1:58]  Checking constitution rules — 6/6 passed
[2:03]  Draft ready
```

The status badge changes from blue (Running) to green (Draft Ready).

At the same moment, your Discord channel pings:

> **Trusted Autonomy** — Draft ready for review: "Set up the project structure with React frontend and Node.js API" — [Open in Studio]

---

## 8. Reviewing a Draft

Click the notification or click **Drafts** in the sidebar. Your draft is at the top of the queue.

Click it. The Draft Review screen has four panels.

**Summary** (top):

```
Draft 6ebf85ab/1
Goal: Set up the project structure with React frontend and Node.js API
Phase: v0.1.0
Agent: Claude Sonnet

23 files changed (23 new, 0 modified, 0 deleted)

Created a React/Vite frontend, Express API, and Docker Compose configuration
for PostgreSQL. Includes environment variable templates, a basic README, and
an initial test suite. All constitution rules passed.
```

**Files Changed** (left panel):

A list of all 23 files, each with a green "New" badge. Click any file to see its contents.

Click `api/src/index.js`. The right panel shows the file diff — in this case, it's all green (new file). You can read the Express server setup. It's clean: no hardcoded values, just references to `process.env.PORT` and `process.env.DATABASE_URL`.

Scroll down and click `docker-compose.yml`. You can see the PostgreSQL service definition and the API service, both pulling configuration from environment variables. Looks good.

**Agent Rationale** (middle):

The agent left notes explaining key decisions:

> "Used Vite instead of Create React App because CRA is no longer maintained and Vite has significantly faster build times. Docker Compose makes local PostgreSQL setup reproducible without requiring a local install."

This is useful context. You don't need to ask why — the agent explains its choices automatically.

**Review Panel** (right):

```
Constitution Check
  ✓  Authentication middleware — N/A (scaffold only, no endpoints yet)
  ✓  No secrets in source code
  ✓  Tests present (4 tests)
  ✓  Parameterized queries — N/A (no queries yet)
  ✓  No sensitive data logged
  ✓  User-friendly error messages — N/A (scaffold only)

Reviewer Agent Verdict
  approve (confidence 94%)

  Findings:
  - Project structure follows React/Node.js best practices
  - Environment variable usage is consistent
  - README includes setup instructions
  - No issues detected
```

You've read through the key files, the constitution is clean, and the reviewer is confident. Click **Approve**.

---

## 9. The Apply & PR Flow

After you click Approve, TA works for a few seconds. The draft status updates to "Applying..." then "Applied."

A **Next Steps** panel appears:

```
Draft applied successfully.

  Branch created:   feature/v0.1.0-project-scaffold
  Pull request:     #1 — Set up project structure (React + Node.js + PostgreSQL)
  PR URL:           https://github.com/yourname/taskflow/pull/1

  Open PR on GitHub →
```

Click **Open PR on GitHub**. The GitHub pull request opens in a new tab. You can see all 23 files, the commit message, and TA's description of what changed. Everything is already there — you don't need to write anything.

Review it one more time if you want, then click **Merge pull request** on GitHub.

Back in Studio, the goal status updates automatically: the badge changes from "Applied" to **"Complete"** with a small checkmark. The plan view shows v0.1.0 marked done.

---

## 10. Iterating — Running More Goals

Click **+ New Goal** and select v0.2.0: User authentication. Start the goal.

This one runs a bit longer — about four minutes. When the draft appears, click through to review it.

The constitution check passes, but there's a flag from the reviewer agent:

```
Reviewer Agent Verdict
  flag (confidence 71%)

  Findings:
  - Authentication endpoints implemented: /auth/register, /auth/login, /auth/logout
  - JWT session management looks correct
  - Passwords hashed with bcrypt

  Issues requiring human review:
  - No password reset flow is implemented. Users who forget their
    password will have no recovery path. This may be intentional
    (can be added later) but should be confirmed.
```

The screen shows a yellow banner:

> **Human review required.** The reviewer flagged an issue: no password reset flow. You can apply this draft anyway, or deny it and ask the agent to add the missing feature.

You have two buttons: **Apply Anyway** and **Deny and Revise**.

You click **Deny and Revise**. A text field appears asking for your feedback:

> "Please add a password reset flow — email-based, with a time-limited token."

Click **Send for Revision**.

The goal re-enters the Running state. The agent picks up your feedback and adds the password reset endpoints. About two minutes later, a new draft arrives. This one clears the reviewer with confidence 91% and no flags.

You click **Approve**, TA applies it, and PR #2 goes up on GitHub.

---

### Denying Part of a Draft

Sometimes an agent makes the right change to most files but gets one wrong. Instead of denying the whole draft and rerunning everything, you can deny a single artifact:

```
ta draft deny abc123 --file src/auth/session.rs --reason "Should use Ed25519, not RSA"
```

After denying, `ta` asks if you'd like to understand why the agent made this choice:

```
Denied artifact fs://workspace/src/auth/session.rs: Should use Ed25519, not RSA

Ask the agent why it made this choice? [y/N] y

[Interrogation] Agent's rationale for src/auth/session.rs:
  RSA-2048 was chosen for compatibility with the existing JWT library.

Options:
  r) Re-approve this artifact
  c) Provide a correction    (ta draft amend abc123 <uri> --file <corrected-file>)
  Enter) Leave it denied
```

To replace the artifact with a corrected version:

```
ta draft amend abc123 fs://workspace/src/auth/session.rs --file src/auth/session.rs --reason "Replaced RSA with Ed25519"
```

### Examining a Specific File in a Draft

Large drafts can be hard to navigate. Use `--file` to focus on specific files:

```
# Show diff for a specific file
ta draft view abc123 --file src/auth/middleware.rs

# Show all Rust files in the auth module
ta draft view abc123 --file "src/auth/*.rs"

# Multiple patterns
ta draft view abc123 --file PLAN.md --file "src/commands/*.rs"
```

When no `--file` is given, the default summary view shows all changed files. The `--file` flag is especially useful when you've already reviewed most of a draft and want to re-examine one area after making corrections.

---

## 11. Modifying the Plan

After v0.2.0 ships, you realize you want to add email notifications — users should get notified when a task is assigned to them. This wasn't in the original plan.

Click **Plan** in the sidebar. Your current plan shows v0.1.0 and v0.2.0 checked off, v0.3.0 and v0.4.0 pending, v0.5.0 deferred.

Click **Add Phase** at the bottom. A new row appears:

```
v0.6.0  [ enter description ]
```

Type: `Email notifications — notify users by email when a task is assigned to them or when a due date is approaching.`

Press **Enter**. The phase is added at the bottom. You want it to run after v0.4.0 but before the deferred v0.5.0, so drag the v0.6.0 row up until it sits between v0.4.0 and v0.5.0. The order updates.

Click **Save Plan**.

While you're here, you want to refine v0.4.0 now that v0.3.0 is almost done and you have a better sense of what the API returns. Click the edit icon next to v0.4.0.

Change the description to:

> "Frontend task list UI — display tasks grouped by assignee, with inline editing for names and due dates. Include a filter bar for status (todo/in-progress/done) and assignee. Keyboard shortcuts for common actions."

Save it. The plan updates immediately. The next time you start the v0.4.0 goal, the agent will use this updated description.

---

## 12. Updating the Constitution

While reviewing the v0.3.0 draft (Task CRUD API), you notice the agent built the POST /tasks endpoint without any rate limiting. Someone could hammer that endpoint with thousands of requests. The constitution didn't catch it because you hadn't thought to include it.

Go to **Settings → Constitution**.

Find the *"Add a rule..."* field at the bottom. Type:

`All public API endpoints must have rate limiting.`

Press **Enter**. The rule appears and is toggled on immediately.

Click **Save Constitution**.

From now on, every goal that creates or modifies API endpoints will be required to add rate limiting. If the agent forgets, the constitution check will fail and the agent will fix it before the draft is marked ready.

> **Note:** Updating the constitution doesn't change any already-applied code. It only affects future goals. If you want the existing endpoints to have rate limiting, you can start a new goal: "Add rate limiting to all existing API endpoints."

---

## 13. Modifying Settings

### Changing the Approval Gate

After a few successful goals, you feel confident in the process and want to let high-confidence drafts go through automatically.

Go to **Settings → Workflow**.

Find the **Approval Gate** dropdown — currently set to "Always ask me." Click it and select:

> **Auto-approve when reviewer confidence > 90%**

A description appears below: *"Drafts where the reviewer agent is 90%+ confident will be applied and submitted as PRs automatically. Drafts below 90% confidence, or those with flags, will still require your review."*

Click **Save**. From this point on, clean drafts from straightforward goals will flow through without interrupting you. Anything the reviewer is uncertain about still lands in your queue.

### Adding Slack Notifications

Your team uses both Discord and Slack. You want Studio to notify the Slack channel too.

Go to **Settings → Notifications**.

Discord is listed with a green connected badge. Below it, Slack shows as "Not configured." Click **Enable** next to Slack.

A field appears: **Slack Webhook URL**. Go to your Slack workspace → Apps → Incoming Webhooks → Add to Slack, pick your channel, copy the webhook URL, and paste it in.

Click **Test**. Within a few seconds, a message appears in your Slack channel:

> *Trusted Autonomy — Test notification from TA Studio.*

Click **Looks good**, then **Save**. Both Discord and Slack now show green connected badges. Future notifications go to both.

---

## 14. What Engineers See

If your team has engineers who prefer the command line, everything in Studio has a CLI equivalent. The `ta` command gives access to the same goals, drafts, plan, and settings — nothing is Studio-only.

For example, everything in this walkthrough is also doable as:

```bash
ta goal start "Set up the project structure" --phase v0.1.0
ta draft list
ta draft view <draft-id>
ta draft approve <draft-id>
```

Engineers can also open **Settings → Advanced** in Studio to edit the raw configuration YAML directly — useful for power users who want to configure things Studio's UI doesn't expose yet.

The CLI and Studio stay in sync automatically. If an engineer starts a goal from the terminal, it shows up in Studio's goal list. If you approve a draft in Studio, the CLI reflects it. There's no conflict and no separate state to manage.

For a full CLI reference, see [USAGE.md](USAGE.md).

---

## 15. Tips & Common Questions

**"The agent made a mistake — how do I undo it?"**

Click **Deny** on the draft. Nothing was applied to your project. The staging copy is thrown away, and your codebase is exactly as it was before you started the goal. You can revise and resubmit, or just start a new goal with better instructions.

---

**"Can I run multiple goals at the same time?"**

Yes. Goals run independently in separate staging copies. You can have v0.3.0 running while you review the v0.2.0 draft. Completed drafts queue up in the Drafts panel — review them in whatever order makes sense. Just be aware that if two goals touch the same files, you'll need to apply them in order to avoid conflicts.

---

**"What if I don't have a GitHub account?"**

During setup (or in Settings → Version Control), choose **None — local only**. TA still tracks every goal, draft, and approval in its local database. You won't get automatic PRs or branches, but you can still review and apply drafts. Everything is stored in your project's `.ta/` folder.

---

**"How do I invite a teammate to review drafts?"**

Multi-user access with shared review is a Secure Autonomy (SA) feature. With SA, your team members can log into the Studio from their own machines, see the same draft queue, and leave review comments. Contact the TA team or visit the pricing page for SA options.

---

**"The agent keeps failing — what do I do?"**

Run the diagnostics tool. In the terminal:

```bash
ta doctor
```

Or in Studio: **Settings → Diagnostics → Run Check**.

The diagnostics tool checks your API key, GitHub connection, network access, and local configuration. It reports exactly what's wrong and how to fix it. Common causes are an expired API key, a revoked GitHub token, or a misconfigured project path.

---

*Questions not covered here? See [USAGE.md](USAGE.md) for the full command reference, or run `ta help` in your terminal.*
