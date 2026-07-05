# What Is TA, In Plain Terms

> A no-jargon description of what Trusted Autonomy actually is and does, meant to be read as a sanity check before committing to technical plan work. If this doesn't match the product you're trying to build, that's worth catching *before* the plan, not after. The deep technical version of everything below lives in [`docs/design/ta-concepts-and-architecture.md`](../design/ta-concepts-and-architecture.md); this document is the plain-language companion, the same relationship [why-constitution.md](why-constitution.md) has to [`TA-CONSTITUTION.md`](../TA-CONSTITUTION.md).

## The one-sentence version

TA lets AI "workers" actually do real work — writing code, updating a database, posting content, shipping a release — without ever giving them the power to make something permanent on their own. Every irreversible step goes through one narrow, careful checkpoint: either a person, or a rule you've explicitly trusted in advance.

## The core idea

Think of TA's AI agents like a new contractor on your team: talented, fast, works around the clock — but never gets the keys to the building. They go into a private, disposable copy of your project and do whatever they think the job requires. When they're done, they hand over a plain-language writeup: here's what I did, here's why. A second, independent reviewer — also AI, always on, never tired, never skips a step — checks that writeup and flags anything concerning. Only then does a human, or a rule you set up ahead of time for routine cases, say "yes, make it real." Nothing the contractor does ever touches the real thing before that moment.

## What TA is made of

- **A worker (the "agent")** — the AI that does the actual task.
- **A task ("goal")** — what you asked for: add this feature, fix this bug, prep this release.
- **A practice copy ("staging")** — an exact, disposable copy of your project where the worker does everything. Nothing here is real yet.
- **A writeup ("draft")** — when the worker's done, you get a plain-language summary of everything that changed and why, not a wall of raw output.
- **A built-in reviewer ("supervisor")** — an AI that automatically double-checks the writeup before a human even sees it, like a QA pass that never gets tired.
- **The rulebook ("constitution")** — the standing rules for what's always fine, what always needs a person's sign-off, and what's off-limits entirely. Written once; applies to everything after.
- **The "make it real" moment ("commit" / "apply")** — the one narrow gate where a change actually becomes permanent: code gets saved, a database updates, a post goes live, a release ships. This is the *only* place anything becomes real, and it is always the last step — never something the worker does directly.
- **An assistant you talk to ("advisor")** — a plain-language chat partner that explains what's going on, answers questions, and can queue up new work.
- **Different specialists (teams, roles, "personas")** — you can configure different AI workers for different kinds of jobs: a careful reviewer, a fast implementer, someone specifically trusted with releases — each with its own level of trust.
- **Plug-ins for the outside world ("adapters," "connectors")** — how TA reaches real systems: your code repository, a database, social media, email, chat tools. Some come built in; others can be added later, including ones the community builds and shares.

## How a piece of work actually flows

1. Something needs doing — you say so, or (eventually) the system pulls the next item from an agreed backlog on its own.
2. TA gives an AI worker a private practice copy to work in. Nothing outside that copy can be touched.
3. The worker does the job.
4. Before anything becomes real, TA's own reviewer checks the work and writes a plain-language summary: what changed, why, anything that looks risky.
5. That summary goes to you for a final look — unless the work is routine and low-risk enough that you've already said "things like this can go through automatically," in which case it's approved on the spot, with the reasoning recorded so you can always see why later.
6. Only now does the change become real, through the one careful gate.
7. If it's rejected, the worker tries again with your feedback, or the idea is dropped. Either way nothing was ever at risk, because nothing outside the practice copy was ever touched.
8. Every step is logged. You can always answer "what happened, and why" for anything TA has ever done.

## How you'd actually use it, day to day

- Talk to it as a command-line tool if you're technical, or through a web dashboard ("Studio") if you want something more visual. Both are getting simpler as part of the current cleanup — today both have more buttons and screens than they should, which is exactly the kind of thing the plan work fixes.
- Set up your rules and your team once: who does what, what needs your OK, what doesn't.
- After that, your day-to-day involvement should mostly be: hand it work (or let it pull from an agreed backlog), answer the occasional question it can't resolve alone, glance at a dashboard, and weigh in on the handful of things that genuinely need your judgment. Most of the repetitive, grinding work should happen without anyone babysitting every step — that's the actual point of calling this a "team" instead of a tool you have to keep operating by hand.
- The same trust model applies no matter what the "real thing" is — a codebase, a database, a social account, a game or content release. Same shape, different destination.

## Why this matters before the plan work starts

Everything in the deeper technical write-up — one consistent plug-in model instead of a dozen overlapping ones, one review-and-approve pipeline instead of several independent copies of the same logic, a much smaller set of commands, a redesigned dashboard — exists to make the description above actually true, reliably, end to end, without a person having to personally stand in for the missing pieces. (That's a description of what actually happened producing these documents, not a hypothetical risk.) If this description matches the product you're building, the technical plan is ready to execute against it.
