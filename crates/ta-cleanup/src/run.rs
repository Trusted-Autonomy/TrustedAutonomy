//! Scan, decide, act, ask. One entry point for the CLI, doctor and the daemon.

use std::collections::HashSet;

use crate::model::*;
use crate::scan::{human_bytes, scan};
use crate::{act, state};

const MAX_PASSES: usize = 3;

pub fn scan_findings(e: &Engine, only: Option<&[Category]>) -> anyhow::Result<Vec<Finding>> {
    let mut f = scan(e, only)?;
    annotate(e, &mut f);
    Ok(f)
}

/// Attach remembered answers and (optional) model suggestions to `Ask` findings.
fn annotate(e: &Engine, findings: &mut [Finding]) {
    for f in findings.iter_mut().filter(|f| f.verdict == Verdict::Ask) {
        if let Some(r) = state::lookup_decision(&e.paths, f.kind, &f.signature) {
            if f.options.iter().any(|o| o.key == r.answer) {
                f.recommended = Some(r.answer.clone());
                f.remembered = Some(r);
            }
        }
        if e.settings.model_assist && f.remembered.is_none() {
            if let Some(a) = e.assistant {
                // The reply is data: it only counts if it names an existing option.
                if let Some(s) = a
                    .suggest(f)
                    .filter(|s| f.options.iter().any(|o| o.key == s.option))
                {
                    f.recommended = Some(s.option.clone());
                    f.suggestion = Some(s);
                }
            }
        }
    }
}

pub fn run(
    e: &Engine,
    opts: &RunOptions,
    mut prompter: Option<&mut dyn Prompter>,
) -> anyhow::Result<Report> {
    let mut report = Report::default();
    let mut tried: HashSet<String> = HashSet::new();
    for pass in 0..MAX_PASSES {
        let findings = scan_findings(e, opts.only.as_deref())?;
        report.proposals_open.clear();
        report.questions_open.clear();
        let mut progressed = false;
        let mut propose: Vec<&Finding> = Vec::new();
        let mut ask: Vec<&Finding> = Vec::new();
        for f in &findings {
            if !tried.insert(format!("{}:{}", f.id, f.signature)) && f.verdict != Verdict::Skip {
                continue;
            }
            match f.verdict {
                Verdict::Skip => {}
                Verdict::AutoSafe => {
                    if !opts.dry_run {
                        progressed |= apply(e, &mut report, f, &f.action, Decider::Rule);
                    }
                }
                Verdict::Propose => propose.push(f),
                Verdict::Ask => ask.push(f),
            }
        }

        if !propose.is_empty() && !opts.auto_safe_only {
            let accepted = if opts.dry_run {
                false
            } else if opts.yes {
                true
            } else if let Some(p) = prompter.as_deref_mut().filter(|p| p.is_interactive()) {
                let mut msg = format!("{} proposal(s):\n", propose.len());
                for f in &propose {
                    msg.push_str(&format!("  - {}: {}\n", f.title, f.reason));
                }
                msg.push_str("Apply all?");
                p.confirm(&msg)
            } else {
                false
            };
            for f in propose {
                if accepted {
                    progressed |= apply(e, &mut report, f, &f.action, Decider::Human);
                } else {
                    report.proposals_open.push(f.id.clone());
                }
            }
        } else {
            report
                .proposals_open
                .extend(propose.iter().map(|f| f.id.clone()));
        }

        for f in ask {
            report.questions_open.push(f.id.clone());
            if opts.auto_safe_only || opts.dry_run {
                continue;
            }
            let interactive = prompter.as_deref().is_some_and(|p| p.is_interactive());
            if interactive {
                let p = prompter
                    .as_deref_mut()
                    .expect("interactive prompter present");
                if let Some(key) = p.choose(f) {
                    if let Some(opt) = f.options.iter().find(|o| o.key == key) {
                        let decider = if f.remembered.as_ref().is_some_and(|r| r.answer == key) {
                            Decider::HumanRemembered
                        } else if f.suggestion.as_ref().is_some_and(|s| s.option == key) {
                            Decider::ModelSuggestedHuman
                        } else {
                            Decider::Human
                        };
                        if let Err(err) = state::record_decision(&e.paths, f, &key) {
                            tracing::warn!(finding = %f.id, error = %err, "cleanup: could not record decision");
                        }
                        if let Some(s) = &f.suggestion {
                            let _ = state::record_suggestion(&e.paths, f, s);
                        }
                        report.questions_open.retain(|i| i != &f.id);
                        progressed |= apply(e, &mut report, f, &opt.action, decider);
                    }
                }
            } else {
                match state::queue_question(&e.paths, f) {
                    Ok(new) => report.questions_file_written |= new,
                    Err(err) => report
                        .failed
                        .push((f.id.clone(), format!("could not queue question: {err}"))),
                }
                if let Some(s) = &f.suggestion {
                    let _ = state::record_suggestion(&e.paths, f, s);
                }
            }
        }

        if pass == 0 {
            report.findings = findings;
        }
        if !progressed {
            break;
        }
    }
    Ok(report)
}

fn apply(e: &Engine, report: &mut Report, f: &Finding, action: &Action, decider: Decider) -> bool {
    if matches!(action, Action::None) {
        return false;
    }
    match act::execute(e, action) {
        Ok((summary, bytes)) => {
            state::audit(&e.paths, f, decider, &summary);
            report.bytes_reclaimed += bytes;
            report.done.push(ActionResult {
                finding_id: f.id.clone(),
                summary,
                bytes_reclaimed: bytes,
                decided_by: decider,
            });
            true
        }
        Err(err) => {
            tracing::warn!(finding = %f.id, error = %err, "cleanup: action failed");
            report.failed.push((f.id.clone(), err.to_string()));
            false
        }
    }
}

/// Full reasoning for one finding (`--explain`), matched by id or id prefix.
pub fn explain(e: &Engine, id: &str) -> anyhow::Result<String> {
    let all = scan_findings(e, None)?;
    let hits: Vec<&Finding> = all
        .iter()
        .filter(|f| f.id == id || f.id.ends_with(&format!(":{id}")) || f.id.contains(id))
        .collect();
    match hits.as_slice() {
        [] => anyhow::bail!(
            "no cleanup finding matches '{id}'. Run `ta cleanup --dry-run` to list findings and their ids (like draft:025f676e)."
        ),
        hs => Ok(hs.iter().map(|f| render_finding(f, true)).collect::<Vec<_>>().join("\n")),
    }
}

pub fn render_finding(f: &Finding, full: bool) -> String {
    let mut s = format!("[{}] {} ({})\n  {}\n", f.verdict, f.title, f.id, f.reason);
    if full || f.verdict != Verdict::Skip {
        for ev in &f.evidence {
            s.push_str(&format!("    - {}: {}\n", ev.source, ev.fact));
        }
    }
    if full {
        for o in &f.options {
            let mark = if f.recommended.as_deref() == Some(&o.key) {
                " (recommended)"
            } else {
                ""
            };
            s.push_str(&format!("    option {}: {}{}\n", o.key, o.label, mark));
        }
        if let Some(r) = &f.remembered {
            s.push_str(&format!(
                "    you answered '{}' for the same evidence on {}\n",
                r.answer, r.at
            ));
        }
        if let Some(sg) = &f.suggestion {
            s.push_str(&format!(
                "    model suggestion (not executed): {} ({:.0}%) {}\n",
                sg.option,
                sg.confidence * 100.0,
                sg.note
            ));
        }
        s.push_str(&format!("    signature: {}\n", f.signature));
    }
    if let Some(c) = &f.manual_command {
        s.push_str(&format!("  do it yourself: {c}\n"));
    }
    s
}

pub fn render_report(r: &Report, dry_run: bool) -> String {
    let mut s = String::new();
    let acting: Vec<&Finding> = r
        .findings
        .iter()
        .filter(|f| f.verdict != Verdict::Skip)
        .collect();
    if dry_run {
        s.push_str(&format!(
            "Dry run: {} finding(s) would be acted on or asked about; nothing was changed.\n",
            acting.len()
        ));
        for f in &acting {
            s.push_str(&render_finding(f, false));
        }
    } else {
        for d in &r.done {
            s.push_str(&format!(
                "  done [{}] {}: {}\n",
                d.decided_by.as_str(),
                d.finding_id,
                d.summary
            ));
        }
        for (id, err) in &r.failed {
            s.push_str(&format!("  FAILED {id}: {err}\n"));
        }
        for id in &r.proposals_open {
            if let Some(f) = r.findings.iter().find(|f| &f.id == id) {
                s.push_str(&format!("  proposal not applied: {}\n    {}\n    run `ta cleanup --yes` to accept all\n", f.title, f.reason));
            }
        }
        for id in &r.questions_open {
            if let Some(f) = r.findings.iter().find(|f| &f.id == id) {
                s.push_str(&format!("  needs a human: {}\n    {}\n", f.title, f.reason));
            }
        }
        if r.questions_file_written {
            s.push_str("  Questions saved to .ta/cleanup/questions.jsonl and .ta/interactions/pending/. Run `ta cleanup` at a terminal to answer them.\n");
        }
        s.push_str(&format!(
            "Cleanup finished: {} action(s), {} failed, {} proposal(s) open, {} question(s) open, {} reclaimed.\n",
            r.done.len(), r.failed.len(), r.proposals_open.len(), r.questions_open.len(), human_bytes(r.bytes_reclaimed)
        ));
    }
    s
}
