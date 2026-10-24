//! First-party input minimizers at the one place TA itself hands command output to an
//! agent: the verification failure context re-injected when `[verify] on_failure = "agent"`
//! (v0.17.12.24 item 3).
//!
//! `[optimizers.input] use = "cargo" | "git" | "ta" | "first-party"` (or a replay's
//! `--set optimizers.input.use=...`) selects a minimizer from `ta-context-opt`. Each failing
//! command's output goes through `InputOptimizer::apply_for_command`, which stores the full
//! original under `.ta/compression/originals/` and returns the metrics record. Every
//! application is appended to `.ta/compression/input-metrics.jsonl` and summarised by
//! `ta compression status`. Nothing else changes: with no minimizer selected the output is
//! cut to the same line cap as before.

use std::io::Write;
use std::path::{Path, PathBuf};

use ta_changeset::draft_package::VerificationWarning;
use ta_context_opt::{
    InputOptimizer, OptMetrics, OptimizerKind, OptimizersConfig, OriginalStore, Registry, Slot,
};

/// File under `.ta/compression/` that records every application.
pub const METRICS_FILE: &str = "input-metrics.jsonl";

/// A selected first-party minimizer plus where its records go.
pub struct Minimizer {
    registry: Registry,
    name: String,
    store: OriginalStore,
    metrics_path: PathBuf,
    goal_id: uuid::Uuid,
}

impl Minimizer {
    /// The minimizer the input slot selects for this goal, or `None` when the slot is unset,
    /// `none`, or names an optimizer that does not transform text here (a hook such as `rtk`,
    /// or a proxy such as `headroom`, acts elsewhere).
    pub fn for_goal(
        project_root: &Path,
        goal_id: uuid::Uuid,
        overrides: Option<&serde_json::Value>,
    ) -> Option<Self> {
        let cfg = super::compression::load_optimizers(project_root).with_overrides(overrides);
        Self::from_config(project_root, goal_id, &cfg)
    }

    fn from_config(
        project_root: &Path,
        goal_id: uuid::Uuid,
        cfg: &OptimizersConfig,
    ) -> Option<Self> {
        let name = cfg.input.use_.clone()?;
        if name == "none" {
            return None;
        }
        let registry = match Registry::builtin().with_config(cfg) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[warn] optimizer: {e}. Verification output is sent unminimized.");
                return None;
            }
        };
        let in_process = registry
            .infos(Slot::Input)
            .into_iter()
            .any(|i| i.name == name && i.kind == OptimizerKind::InProcess);
        if !in_process {
            return None;
        }
        Some(Self {
            registry,
            name,
            store: OriginalStore::for_project(project_root),
            metrics_path: project_root
                .join(".ta")
                .join("compression")
                .join(METRICS_FILE),
            goal_id,
        })
    }

    fn optimizer(&self) -> Option<&dyn InputOptimizer> {
        self.registry.input(&self.name).ok()
    }

    /// Minimize `output` of `command`. On any failure the unminimized output is returned and
    /// the reason is printed, so a broken optimizer never costs the agent its failure text.
    pub fn minimize(&self, command: &str, output: &str) -> String {
        let Some(opt) = self.optimizer() else {
            return output.to_string();
        };
        match opt.apply_for_command(command, output, &self.store) {
            Ok((text, metrics)) => {
                self.record(command, &metrics);
                if metrics.bytes_out < metrics.bytes_in {
                    println!(
                        "[optimizer] {}: `{}` output {} -> {} bytes ({:.0}% smaller); original kept as {}",
                        self.name,
                        command,
                        metrics.bytes_in,
                        metrics.bytes_out,
                        metrics.reduction() * 100.0,
                        &metrics.original_ref[..12.min(metrics.original_ref.len())],
                    );
                }
                text
            }
            Err(e) => {
                eprintln!(
                    "[warn] optimizer {}: could not minimize `{command}` output: {e}. The \
                     unminimized output is sent instead.",
                    self.name
                );
                output.to_string()
            }
        }
    }

    fn record(&self, command: &str, metrics: &OptMetrics) {
        let line = serde_json::json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "goal_id": self.goal_id,
            "optimizer": self.name,
            "command": command,
            "metrics": metrics,
        });
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.metrics_path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.metrics_path)?;
            writeln!(f, "{line}")
        })();
        if let Err(e) = result {
            tracing::warn!(
                path = %self.metrics_path.display(),
                error = %e,
                "could not append an input optimizer metrics record"
            );
            eprintln!(
                "[warn] optimizer: could not write {}: {e}. Check the directory is writable; \
                 the minimized output is still used.",
                self.metrics_path.display()
            );
        }
    }
}

/// The failure context re-injected into the agent: one section per failing command. With a
/// minimizer the output is reduced first and may use twice `line_cap` lines, since the same
/// budget now carries more signal; without one it is cut to `line_cap` lines.
pub fn render_failures(
    warnings: &[VerificationWarning],
    line_cap: usize,
    minimizer: Option<&Minimizer>,
) -> String {
    let mut out = String::new();
    out.push_str("\n## Verification Failures (auto-injected)\n\n");
    out.push_str(
        "The following verification commands failed. Fix these issues and ensure all checks pass.\n\n",
    );
    for w in warnings {
        out.push_str(&format!("### `{}`\n", w.command));
        out.push_str(&format!(
            "Exit code: {}\n",
            w.exit_code.map_or("N/A".to_string(), |c| c.to_string())
        ));
        if !w.output.is_empty() {
            let (text, cap) = match minimizer {
                Some(m) => (m.minimize(&w.command, &w.output), line_cap * 2),
                None => (w.output.clone(), line_cap),
            };
            out.push_str("```\n");
            for line in text.lines().take(cap) {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("```\n");
        }
        out.push('\n');
    }
    out
}

/// Totals over `.ta/compression/input-metrics.jsonl`: applications, bytes in, bytes out.
pub fn totals(project_root: &Path) -> Option<(usize, usize, usize)> {
    let raw = std::fs::read_to_string(
        project_root
            .join(".ta")
            .join("compression")
            .join(METRICS_FILE),
    )
    .ok()?;
    let (mut n, mut bin, mut bout) = (0, 0, 0);
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let m = &v["metrics"];
        n += 1;
        bin += m["bytes_in"].as_u64().unwrap_or(0) as usize;
        bout += m["bytes_out"].as_u64().unwrap_or(0) as usize;
    }
    (n > 0).then_some((n, bin, bout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(use_: Option<&str>) -> OptimizersConfig {
        let mut c = OptimizersConfig::default();
        c.input.use_ = use_.map(str::to_string);
        c
    }

    fn warning(command: &str, output: &str) -> VerificationWarning {
        VerificationWarning {
            command: command.into(),
            exit_code: Some(101),
            output: output.into(),
        }
    }

    fn noisy_cargo() -> String {
        let mut s = String::new();
        for i in 0..40 {
            s.push_str(&format!("   Compiling dep{i} v1.0.{i}\n"));
        }
        s.push_str("error[E0308]: mismatched types\n --> src/lib.rs:1:1\n\n");
        s.push_str("error: could not compile `x` (lib) due to 1 previous error\n");
        s
    }

    #[test]
    fn only_in_process_minimizers_are_selected() {
        let dir = tempfile::tempdir().unwrap();
        let g = uuid::Uuid::new_v4();
        for name in ["cargo", "git", "ta", "first-party"] {
            assert!(Minimizer::from_config(dir.path(), g, &cfg(Some(name))).is_some());
        }
        for name in [
            None,
            Some("none"),
            Some("rtk"),
            Some("headroom"),
            Some("nope"),
        ] {
            assert!(
                Minimizer::from_config(dir.path(), g, &cfg(name)).is_none(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_replay_override_selects_the_minimizer_over_daemon_toml() {
        let dir = tempfile::tempdir().unwrap();
        let ov = serde_json::json!({"optimizers.input.use": "cargo"});
        let m = Minimizer::for_goal(dir.path(), uuid::Uuid::new_v4(), Some(&ov));
        assert!(m.is_some());
        let off = serde_json::json!({"optimizers.input.use": "none"});
        assert!(Minimizer::for_goal(dir.path(), uuid::Uuid::new_v4(), Some(&off)).is_none());
    }

    #[test]
    fn failure_context_is_minimized_stored_and_recorded_through_apply() {
        let dir = tempfile::tempdir().unwrap();
        let g = uuid::Uuid::new_v4();
        let m = Minimizer::from_config(dir.path(), g, &cfg(Some("first-party"))).unwrap();
        let raw = noisy_cargo();
        let ctx = render_failures(&[warning("cargo build --workspace", &raw)], 30, Some(&m));
        assert!(ctx.contains("error[E0308]: mismatched types"), "{ctx}");
        assert!(!ctx.contains("Compiling dep0 "), "{ctx}");
        assert!(ctx.contains("Exit code: 101"));

        let (n, bin, bout) = totals(dir.path()).expect("a metrics record was written");
        assert_eq!(n, 1);
        assert_eq!(bin, raw.len());
        assert!(bout < bin);

        let line =
            std::fs::read_to_string(dir.path().join(".ta/compression").join(METRICS_FILE)).unwrap();
        let rec: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
        assert_eq!(rec["optimizer"], "first-party");
        assert_eq!(rec["goal_id"], g.to_string());
        assert_eq!(rec["command"], "cargo build --workspace");
        let reference = rec["metrics"]["original_ref"].as_str().unwrap();
        assert_eq!(
            OriginalStore::for_project(dir.path())
                .recover(reference)
                .unwrap(),
            raw
        );
    }

    #[test]
    fn without_a_minimizer_output_is_cut_to_the_cap_exactly_as_before() {
        let raw: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let ctx = render_failures(&[warning("make", &raw)], 30, None);
        assert!(
            ctx.contains("line 29\n") && !ctx.contains("line 30\n"),
            "{ctx}"
        );
    }

    #[test]
    fn a_single_kind_minimizer_does_not_touch_another_commands_output() {
        let dir = tempfile::tempdir().unwrap();
        let m =
            Minimizer::from_config(dir.path(), uuid::Uuid::new_v4(), &cfg(Some("git"))).unwrap();
        let raw = noisy_cargo();
        let ctx = render_failures(&[warning("cargo build", &raw)], 100, Some(&m));
        assert!(
            ctx.contains("Compiling dep0 "),
            "cargo output untouched by `git`"
        );
    }
}
