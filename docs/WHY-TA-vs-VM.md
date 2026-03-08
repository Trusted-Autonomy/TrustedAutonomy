# Why TA? Staging-Based Mediation vs VM Sandboxing

> A critical comparison for anyone asking: "Why not just run agents in a VM?"

---

## The One-Liner

**VMs answer "what if the agent is malicious?" TA answers "what if the agent is wrong?"**

Most real-world agent failures aren't sandbox escapes — they're well-intentioned
agents making confident bad decisions. A VM contains the blast radius after the
fact. TA prevents the blast in the first place by requiring human approval before
any action reaches the real world.

---

## How Each Model Works

### VM Sandbox Approach

```
[Human] "Draft a reply to Alice"
    │
    ▼
[Supervisor] Spins up VM with network ACLs
    │
    ▼
[VM] Agent runs with full OS isolation
     - Has a copy of relevant data
     - Network filtered (e.g., no outbound SMTP)
     - Agent works freely inside the box
    │
    ▼
[Supervisor] Inspects VM state when agent exits
     - Diffs filesystem
     - Reviews network logs
     - Extracts artifacts
    │
    ▼
[Human] Reviews extracted output, decides what to use
```

### TA Staging Approach

```
[Human] "Draft a reply to Alice"
    │
    ▼
[TA] Creates staging workspace (lightweight copy)
    │
    ▼
[Agent] Works in staging with native tools
     - Reads/writes normally — TA is invisible
     - All changes captured automatically
    │
    ▼
[TA] Builds PR: semantic diff + agent rationale
     - "Here's what changed and WHY"
     - Per-file approve / reject / discuss
    │
    ▼
[Human] Reviews PR, approves selectively
    │
    ▼
[TA] Applies only approved changes to real world
```

---

## Honest Comparison

### Where VMs Genuinely Win

| Advantage | Why It Matters |
|---|---|
| **True kernel-level isolation** | Agent literally cannot escape. Decades of hardened hypervisor security. TA staging relies on tool-level mediation — a determined agent could theoretically bypass it. |
| **Untrusted code execution** | You can run arbitrary binaries from unknown sources. TA currently trusts the agent process itself. |
| **Network-level controls** | Mature, battle-tested. Block outbound SMTP at the network layer — no application-level bypass possible. |
| **Full state snapshots** | Snapshot before, rollback after. Complete undo of everything the agent did. |
| **Process visibility** | Monitor syscalls, file access, network traffic. Complete forensic record. |
| **Well-understood security model** | CISOs know VMs. The attack surface is well-studied. TA's mediation model is newer and less proven. |

**If your primary threat model is "the agent is actively trying to escape
containment," VMs are the right answer today.** No question.

### Where TA Wins

| Advantage | Why It Matters |
|---|---|
| **Review before action, not forensics after** | A VM lets the agent act, then you inspect the damage. TA shows you what *will* happen and asks permission first. The email doesn't get sent. The database doesn't get modified. The code doesn't get deployed. |
| **Semantic diffs, not filesystem deltas** | TA shows "modified `auth.rs`: added rate limiting to login endpoint" with the agent's rationale. A VM diff shows changed bytes in a filesystem image. For non-developers reviewing agent work, this is the difference between usable and useless. |
| **Selective approval** | Approve the code changes, reject the config changes, discuss the API modifications. VMs are all-or-nothing: accept the entire VM state or discard it. |
| **Zero infrastructure** | No hypervisor, no VM images, no boot time, no cloud compute. `ta run "task"` and you're working. A VM approach needs Firecracker/QEMU/cloud API, base images, networking setup, and ongoing maintenance. |
| **Native tool compatibility** | Claude Code, Codex, and any future agent work directly in TA staging using their normal tools. No need for VM-aware agents, special APIs, or agent-side VM integration. The agent doesn't even know TA exists. |
| **Multi-resource mediation** | VMs sandbox a filesystem + network. TA mediates across resource types: code, email drafts, database transactions, API calls, document edits — all through the same staging → review → approve → apply model. A VM can't meaningfully "stage" an email send or a Slack message. |
| **Dynamic multi-agent collaboration** | Multiple agents can share a staging workspace, see each other's work, build on it. VMs are isolated by design — sharing state between VMs requires explicit plumbing. |
| **Lightweight and fast** | TA staging is a file copy (milliseconds for small projects, seconds for large ones). VM boot is seconds to minutes. For interactive workflows where a human is waiting, this matters. |
| **Cost** | TA runs on your laptop. VMs need compute — either local (RAM/CPU per VM) or cloud ($0.01-0.10+ per VM-hour). For a virtual office running 10 roles checking email every 15 minutes, the VM cost adds up. |
| **Audit with intent** | TA's audit trail captures what the agent was trying to do (goal, rationale, dependencies). VM forensics capture what happened at the syscall level. The first is useful for reviewing agent judgment. The second is useful for security incident response. Different tools for different problems. |

---

## The Threat Model Difference

### VM: "Defense against a hostile actor"

The implicit assumption is that the agent might be adversarial. This makes sense
when:
- Running untrusted third-party code
- The agent has access to credentials or secrets
- You need provable isolation for compliance
- The failure mode is data exfiltration or destruction

### TA: "Oversight of a capable but fallible collaborator"

The implicit assumption is that the agent is trying to help but might be wrong.
This makes sense when:
- The agent is a known, trusted model (Claude, GPT, etc.)
- The failure mode is "did the wrong thing" not "tried to escape"
- The human needs to understand and approve the work, not just contain it
- The workflow involves judgment calls, not just execution

**Most real-world agent deployments today are the second case.** Your Claude Code
session isn't trying to exfiltrate data. It's trying to write good code and
sometimes gets it wrong. You need to catch the "wrong," not contain the
"malicious."

---

## The Non-Developer Argument

This is where TA's advantage is sharpest. Consider the email assistant scenario:

**VM approach for a non-technical user:**
"Your agent ran in an isolated environment. Here's a filesystem diff of the VM
state. The file `/tmp/draft-reply-7a3f.eml` was created with 2,847 bytes. Network
logs show DNS lookup for smtp.gmail.com was blocked by policy. Do you want to
extract and send?"

**TA approach for a non-technical user:**
"Your agent drafted a reply to Alice. Here's what it wrote: [full text]. It also
updated your contacts note for Alice with the Q3 budget reference. Approve both,
or approve just the reply?"

The second version works for anyone. The first requires a sysadmin.

For codeless automation — where non-developers create agent workflows for
business tasks — the VM model is a non-starter from a UX perspective. TA's
semantic review model is the only one that scales to non-technical users.

---

## They're Not Mutually Exclusive

TA's PLAN.md already includes Phase 7 — Sandbox Runner (OCI/gVisor). The best
architecture is layered:

```
┌──────────────────────────────────────┐
│        TA Semantic Mediation         │  ← Human reviews intent
│  Staging → PR → Approve → Apply     │
├──────────────────────────────────────┤
│     Sandbox / Container Isolation    │  ← Process-level containment
│  OCI │ gVisor │ Firecracker │ VM    │
├──────────────────────────────────────┤
│         OS / Network Controls        │  ← Infrastructure-level
│  Seccomp │ AppArmor │ Network ACLs  │
└──────────────────────────────────────┘
```

Each layer catches different failure modes:
- **OS/Network**: Catches escape attempts, unauthorized network access
- **Sandbox**: Catches process-level misbehavior, resource abuse
- **TA**: Catches wrong decisions, bad judgment, unintended consequences

**A VM without TA** contains damage but doesn't help you evaluate agent judgment.
**TA without a sandbox** evaluates judgment but doesn't protect against escape.
**Both together** give you defense-in-depth: containment AND oversight.

But if you have to pick one to build first — and you're working with trusted
models on non-adversarial tasks — TA's review layer gives you more practical
safety per unit of engineering effort than a VM sandbox.

---

## Why Not Just a VM? — Summary

| Question | VM Answer | TA Answer |
|---|---|---|
| Can I see what the agent will do before it happens? | No — inspect after | Yes — review before |
| Can I approve some changes and reject others? | Yes - manual intervention | Yes — per-artifact |
| Can a non-developer review and approve? | Yes with addtional tools and learning | Yes — semantic diffs with rationale |
| Does it work with email, Slack, APIs, databases? | Poorly — VMs sandbox files + network | Yes — unified staging model for any resource |
| Can multiple agents collaborate? | Yes — Using a system like Claude Flow or manual setup | Easy — shared staging |
| What infrastructure do I need? | Hypervisor + images + compute | None — runs on your laptop |
| Does it protect against a malicious agent? | Yes (strong) | Yes, but not hardened or inescapable (sandbox layer needed) |
| Does it protect against a wrong agent? | Only with human best efforts review (damage already done inside VM) | Yes (nothing happens without approval) |
| How fast is it? | Seconds (boot) | Milliseconds (file copy) |
| What does it cost to run? | free if you know what you are doing | Free (file copies) |

---

## The Pitch

> Trusted Autonomy is a local-first safety layer for AI agents.
> Instead of containing agents in VMs after the fact, TA mediates every action
> before it reaches the real world. Agents work in staging copies. Humans review
> semantic diffs — not filesystem deltas — and approve selectively. Nothing
> touches production, your inbox, or your database until you say so.
>
> It's the difference between a security camera and a locked door.
> A VM records what happened. TA prevents what shouldn't happen.
