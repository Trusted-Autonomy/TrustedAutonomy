//! terminal.rs — Terminal output adapter with configurable color support.
//!
//! Color is off by default. Enable with `TerminalAdapter::with_color()` or `--color` CLI flag.

use crate::artifact_kind::ArtifactKind;
use crate::error::ChangeSetError;
use crate::output_adapters::{
    default_summary, matches_file_filters, DetailLevel, OutputAdapter, RenderContext,
};
use crate::pr_package::{Artifact, ChangeType};

/// Format a byte count as a human-readable size string (e.g. "1.0 MB", "512 B").
fn format_byte_size(bytes: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = KB * 1_024;
    const GB: u64 = MB * 1_024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[derive(Default)]
pub struct TerminalAdapter {
    color: bool,
}

impl TerminalAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_color(color: bool) -> Self {
        Self { color }
    }

    /// Strip HTML tags from a string to prevent HTML-rendered content
    /// from leaking into terminal output.
    ///
    /// Only strips sequences that look like real HTML tags (contain attributes,
    /// slashes, or known tag names). Preserves angle-bracket text that looks
    /// like code placeholders: `<id>`, `<path>`, `<T>`, etc.
    fn strip_html(s: &str) -> std::borrow::Cow<'_, str> {
        if !s.contains('<') {
            return std::borrow::Cow::Borrowed(s);
        }
        // Only strip if it looks like actual HTML (contains known tags or attributes).
        // Pattern: <tag ...>, </tag>, <tag/>, or tags with class/style attributes.
        let has_html = s.contains("</")
            || s.contains("class=")
            || s.contains("style=")
            || s.contains("<span")
            || s.contains("<div")
            || s.contains("<br")
            || s.contains("<p>")
            || s.contains("<p ")
            || s.contains("<a ")
            || s.contains("<img");
        if !has_html {
            return std::borrow::Cow::Borrowed(s);
        }
        let mut out = String::with_capacity(s.len());
        let mut in_tag = false;
        for c in s.chars() {
            match c {
                '<' => in_tag = true,
                '>' if in_tag => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        std::borrow::Cow::Owned(out)
    }

    // -- ANSI helpers (return empty strings when color is off) --

    fn bold(&self) -> &str {
        if self.color {
            "\x1b[1m"
        } else {
            ""
        }
    }

    fn dim(&self) -> &str {
        if self.color {
            "\x1b[2m"
        } else {
            ""
        }
    }

    fn reset(&self) -> &str {
        if self.color {
            "\x1b[0m"
        } else {
            ""
        }
    }

    fn color_code<'a>(&self, code: &'a str) -> &'a str {
        if self.color {
            code
        } else {
            ""
        }
    }

    fn render_header(&self, ctx: &RenderContext) -> String {
        let pkg = ctx.package;
        let status_color = if self.color {
            match pkg.status {
                crate::pr_package::PRStatus::Draft => "\x1b[33m",
                crate::pr_package::PRStatus::PendingReview => "\x1b[36m",
                crate::pr_package::PRStatus::Approved { .. } => "\x1b[32m",
                crate::pr_package::PRStatus::Denied { .. } => "\x1b[31m",
                crate::pr_package::PRStatus::Applied { .. } => "\x1b[32m",
                crate::pr_package::PRStatus::Superseded { .. } => "\x1b[90m",
                crate::pr_package::PRStatus::Closed { .. } => "\x1b[90m",
            }
        } else {
            ""
        };
        let bold = self.bold();
        let reset = self.reset();

        // Build the draft identity string: prefer <shortref>/<seq> · <tag> (v0.14.7.3).
        let draft_identity = match (&pkg.goal_shortref, pkg.draft_seq, &pkg.tag) {
            (Some(shortref), seq, Some(tag)) if seq > 0 => {
                format!("{}/{} · {}", shortref, seq, tag)
            }
            (Some(shortref), seq, None) if seq > 0 => {
                format!("{}/{}", shortref, seq)
            }
            _ => pkg.package_id.to_string(),
        };

        format!(
            "{bold}Draft: {}{reset}\n\
            Status: {}{}{reset}\n\
            Goal: {}\n\
            Created: {}\n\n\
            {bold}Summary:{reset}\n\
            {}\n\n\
            {bold}Why:{reset}\n\
            {}\n\n\
            {bold}Impact:{reset}\n\
            {}\n\n",
            draft_identity,
            status_color,
            pkg.status,
            Self::strip_html(&pkg.goal.title),
            pkg.created_at.format("%Y-%m-%d %H:%M:%S"),
            Self::strip_html(&pkg.summary.what_changed),
            Self::strip_html(&pkg.summary.why),
            Self::strip_html(&pkg.summary.impact),
            bold = bold,
            reset = reset
        )
    }

    fn change_icon(&self, change_type: &ChangeType) -> String {
        if self.color {
            match change_type {
                ChangeType::Add => "\x1b[32m+\x1b[0m".to_string(),
                ChangeType::Modify => "\x1b[33m~\x1b[0m".to_string(),
                ChangeType::Delete => "\x1b[31m-\x1b[0m".to_string(),
                ChangeType::Rename => "\x1b[36m>\x1b[0m".to_string(),
            }
        } else {
            match change_type {
                ChangeType::Add => "+".to_string(),
                ChangeType::Modify => "~".to_string(),
                ChangeType::Delete => "-".to_string(),
                ChangeType::Rename => ">".to_string(),
            }
        }
    }

    fn render_artifact_top(&self, artifact: &Artifact) -> String {
        let icon = self.change_icon(&artifact.change_type);

        let disposition_badge = match artifact.disposition {
            crate::pr_package::ArtifactDisposition::Pending => "[pending]",
            crate::pr_package::ArtifactDisposition::Approved => "[approved]",
            crate::pr_package::ArtifactDisposition::Rejected => "[rejected]",
            crate::pr_package::ArtifactDisposition::Discuss => "[discuss]",
        };

        let summary_raw = artifact
            .explanation_tiers
            .as_ref()
            .map(|t| t.summary.as_str())
            .or(artifact.rationale.as_deref())
            .unwrap_or_else(|| default_summary(&artifact.resource_uri, &artifact.change_type));
        let summary = Self::strip_html(summary_raw);

        // File path on its own line, summary on next line indented to match.
        format!(
            "  {} {} {}\n    {}",
            icon, disposition_badge, artifact.resource_uri, summary
        )
    }

    fn render_artifact_medium(&self, artifact: &Artifact) -> String {
        let mut output = self.render_artifact_top(artifact);
        let dim = self.dim();
        let reset = self.reset();
        output.push('\n');

        if let Some(tiers) = &artifact.explanation_tiers {
            output.push_str(&format!(
                "\n    {dim}Explanation:{reset} {}\n",
                tiers.explanation
            ));

            if !tiers.tags.is_empty() {
                output.push_str(&format!(
                    "    {dim}Tags:{reset} {}\n",
                    tiers.tags.join(", ")
                ));
            }

            if !tiers.related_artifacts.is_empty() {
                output.push_str(&format!("    {dim}Related:{reset}\n"));
                for related in &tiers.related_artifacts {
                    output.push_str(&format!("      - {}\n", related));
                }
            }
        } else if let Some(rationale) = &artifact.rationale {
            output.push_str(&format!("\n    {dim}Rationale:{reset} {}\n", rationale));
        }

        if !artifact.dependencies.is_empty() {
            output.push_str(&format!("    {dim}Dependencies:{reset}\n"));
            for dep in &artifact.dependencies {
                output.push_str(&format!("      {:?}: {}\n", dep.kind, dep.target_uri));
            }
        }

        output
    }

    fn render_artifact_full(&self, artifact: &Artifact, ctx: &RenderContext) -> String {
        let mut output = self.render_artifact_medium(artifact);
        let bold = self.bold();
        let reset = self.reset();
        let dim = self.dim();

        // Image artifacts: suppress binary diff; show human-readable summary instead.
        if let Some(ArtifactKind::Image {
            width,
            height,
            format,
            frame_index,
        }) = &artifact.kind
        {
            output.push_str(&format!("\n    {bold}Image artifact:{reset}\n"));
            let fmt_str = format.as_deref().unwrap_or("unknown format");
            output.push_str(&format!("    {dim}Format:{reset} {}\n", fmt_str));
            if let (Some(w), Some(h)) = (width, height) {
                output.push_str(&format!("    {dim}Resolution:{reset} {}×{}\n", w, h));
            }
            if let Some(fi) = frame_index {
                output.push_str(&format!("    {dim}Frame index:{reset} {}\n", fi));
            }
            output.push_str(&format!(
                "    {dim}[Binary image — text diff suppressed]{reset}\n"
            ));
            return output;
        }

        // Memory summary artifacts: render entry list with [memory] tag prefix.
        if let Some(ArtifactKind::MemorySummary { entry_count, .. }) = &artifact.kind {
            output.push_str(&format!(
                "\n    {bold}[memory] Memory entries stored:{reset} {}\n",
                entry_count
            ));
            // The changeset holds the rendered entry list as text.
            if let Some(provider) = ctx.diff_provider {
                match provider.get_diff(&artifact.diff_ref) {
                    Ok(content) => {
                        for line in content.lines() {
                            output.push_str(&format!("    {dim}{}{reset}\n", line));
                        }
                    }
                    Err(e) => {
                        output.push_str(&format!(
                            "    {red}[Error loading memory summary: {}]{reset}\n",
                            e,
                            red = self.color_code("\x1b[31m"),
                            reset = reset
                        ));
                    }
                }
            }
            output.push_str(&format!(
                "    {dim}[Approve to keep entries · Deny to remove them from the store]{reset}\n"
            ));
            return output;
        }

        // Video artifacts: suppress binary diff; show metadata summary instead.
        if let Some(kind @ ArtifactKind::Video { .. }) = &artifact.kind {
            output.push_str(&format!("\n    {bold}Video artifact:{reset}\n"));
            let summary = kind.video_metadata_summary();
            output.push_str(&format!("    {dim}{summary}{reset}\n"));
            output.push_str(&format!(
                "    {dim}[Binary video — text diff suppressed]{reset}\n"
            ));
            return output;
        }

        // Binary artifacts: suppress diff; show size summary.
        if let Some(ArtifactKind::Binary {
            mime_type,
            byte_size,
        }) = &artifact.kind
        {
            output.push_str(&format!("\n    {bold}Binary artifact:{reset}\n"));
            if let Some(mime) = mime_type {
                output.push_str(&format!("    {dim}MIME type:{reset} {}\n", mime));
            }
            let size_str = byte_size
                .map(format_byte_size)
                .unwrap_or_else(|| "unknown size".to_string());
            output.push_str(&format!(
                "    {dim}[Binary file, {size_str} — text diff suppressed]{reset}\n"
            ));
            return output;
        }

        // Text artifacts: show kind label then render diff normally below.
        if let Some(ArtifactKind::Text {
            encoding,
            line_count,
        }) = &artifact.kind
        {
            output.push_str(&format!("\n    {bold}Text artifact:{reset}\n"));
            if let Some(enc) = encoding {
                output.push_str(&format!("    {dim}Encoding:{reset} {}\n", enc));
            }
            if let Some(lc) = line_count {
                output.push_str(&format!("    {dim}Lines:{reset} {}\n", lc));
            }
            // Fall through to diff rendering below.
        }

        // Fetch and display full diff if provider is available
        if let Some(provider) = ctx.diff_provider {
            match provider.get_diff(&artifact.diff_ref) {
                Ok(diff) => {
                    output.push_str(&format!("\n    {bold}Diff:{reset}\n"));
                    let green = self.color_code("\x1b[32m");
                    let red = self.color_code("\x1b[31m");
                    let cyan = self.color_code("\x1b[36m");
                    for line in diff.lines() {
                        if line.starts_with('+') && !line.starts_with("+++") {
                            output.push_str(&format!("    {green}{}{reset}\n", line));
                        } else if line.starts_with('-') && !line.starts_with("---") {
                            output.push_str(&format!("    {red}{}{reset}\n", line));
                        } else if line.starts_with("@@") {
                            output.push_str(&format!("    {cyan}{}{reset}\n", line));
                        } else {
                            output.push_str(&format!("    {}\n", line));
                        }
                    }
                }
                Err(e) => {
                    output.push_str(&format!(
                        "    {red}[Error loading diff: {}]{reset}\n",
                        e,
                        red = self.color_code("\x1b[31m"),
                        reset = reset
                    ));
                }
            }
        } else {
            output.push_str(&format!(
                "    {dim}[Diff available at: {}]{reset}\n",
                artifact.diff_ref
            ));
        }

        output
    }

    /// Build a human-readable summary for a set of image artifacts.
    ///
    /// Used by `ta draft view` to display a summary line like
    /// "42 PNG frames, 1024×1024, 380 MB" when a draft contains image artifacts.
    pub fn render_image_artifact_set_summary(artifacts: &[&Artifact]) -> String {
        let image_artifacts: Vec<_> = artifacts
            .iter()
            .filter(|a| a.kind.as_ref().map(|k| k.is_image()).unwrap_or(false))
            .collect();

        if image_artifacts.is_empty() {
            return String::new();
        }

        // Collect metadata from image kinds.
        let frame_count = image_artifacts.len();
        let format: Option<String> = image_artifacts.iter().find_map(|a| {
            if let Some(ArtifactKind::Image { format, .. }) = &a.kind {
                format.clone()
            } else {
                None
            }
        });
        let resolution: Option<(u32, u32)> = image_artifacts.iter().find_map(|a| {
            if let Some(ArtifactKind::Image {
                width: Some(w),
                height: Some(h),
                ..
            }) = &a.kind
            {
                Some((*w, *h))
            } else {
                None
            }
        });

        let fmt_str = format.as_deref().unwrap_or("image");
        let mut parts = vec![format!(
            "{} {} frame{}",
            frame_count,
            fmt_str,
            if frame_count == 1 { "" } else { "s" }
        )];
        if let Some((w, h)) = resolution {
            parts.push(format!("{}×{}", w, h));
        }
        parts.join(", ")
    }

    /// Build a human-readable summary for a set of binary artifacts.
    ///
    /// Returns a line like `"3 binary files (12.4 MB total)"` or an empty string
    /// if there are no `ArtifactKind::Binary` artifacts in the set.
    pub fn render_binary_artifact_set_summary(artifacts: &[&Artifact]) -> String {
        let binary_artifacts: Vec<_> = artifacts
            .iter()
            .filter(|a| a.kind.as_ref().map(|k| k.is_binary()).unwrap_or(false))
            .collect();

        if binary_artifacts.is_empty() {
            return String::new();
        }

        let count = binary_artifacts.len();
        let total_bytes: Option<u64> = binary_artifacts.iter().try_fold(0u64, |acc, a| {
            if let Some(ArtifactKind::Binary {
                byte_size: Some(b), ..
            }) = &a.kind
            {
                Some(acc + b)
            } else {
                None // any unknown size → can't compute total
            }
        });

        if let Some(total) = total_bytes {
            format!(
                "{} binary file{} ({} total)",
                count,
                if count == 1 { "" } else { "s" },
                format_byte_size(total)
            )
        } else {
            format!("{} binary file{}", count, if count == 1 { "" } else { "s" })
        }
    }

    /// Build a human-readable summary for a set of text artifacts.
    ///
    /// Returns a line like `"2 text files"` or an empty string if there are no
    /// `ArtifactKind::Text` artifacts in the set.
    pub fn render_text_artifact_set_summary(artifacts: &[&Artifact]) -> String {
        let count = artifacts
            .iter()
            .filter(|a| a.kind.as_ref().map(|k| k.is_text()).unwrap_or(false))
            .count();

        if count == 0 {
            return String::new();
        }

        format!("{} text file{}", count, if count == 1 { "" } else { "s" })
    }

    /// Build a human-readable summary for a set of video artifacts.
    ///
    /// Returns a line like `"2 MP4 video files, 1920×1080, 24fps"` or an empty string
    /// if there are no `ArtifactKind::Video` artifacts in the set.
    pub fn render_video_artifact_set_summary(artifacts: &[&Artifact]) -> String {
        let video_artifacts: Vec<_> = artifacts
            .iter()
            .filter(|a| a.kind.as_ref().map(|k| k.is_video()).unwrap_or(false))
            .collect();

        if video_artifacts.is_empty() {
            return String::new();
        }

        let count = video_artifacts.len();
        let format: Option<String> = video_artifacts.iter().find_map(|a| {
            if let Some(ArtifactKind::Video { format, .. }) = &a.kind {
                format.clone()
            } else {
                None
            }
        });
        let resolution: Option<(u32, u32)> = video_artifacts.iter().find_map(|a| {
            if let Some(ArtifactKind::Video {
                width: Some(w),
                height: Some(h),
                ..
            }) = &a.kind
            {
                Some((*w, *h))
            } else {
                None
            }
        });
        let fps: Option<f32> = video_artifacts.iter().find_map(|a| {
            if let Some(ArtifactKind::Video { fps, .. }) = &a.kind {
                *fps
            } else {
                None
            }
        });

        let label = match &format {
            Some(fmt) => format!("{} video", fmt),
            None => "video".to_string(),
        };
        let mut parts = vec![format!(
            "{} {} file{}",
            count,
            label,
            if count == 1 { "" } else { "s" }
        )];
        if let Some((w, h)) = resolution {
            parts.push(format!("{}×{}", w, h));
        }
        if let Some(f) = fps {
            parts.push(format!("{}fps", f));
        }
        parts.join(", ")
    }

    /// Group artifacts by module (top-level directory) for the "What Changed" section (v0.9.5).
    fn render_grouped_changes(&self, artifacts: &[&Artifact]) -> String {
        use std::collections::BTreeMap;
        let bold = self.bold();
        let reset = self.reset();
        let dim = self.dim();

        let mut output = format!("{bold}What Changed ({} files):{reset}\n", artifacts.len());

        // Group by module (first path segment after fs://workspace/).
        let mut groups: BTreeMap<String, Vec<&Artifact>> = BTreeMap::new();
        for artifact in artifacts {
            let path = artifact
                .resource_uri
                .strip_prefix("fs://workspace/")
                .unwrap_or(&artifact.resource_uri);
            let module = path.split('/').next().unwrap_or("root").to_string();
            groups.entry(module).or_default().push(artifact);
        }

        for (module, arts) in &groups {
            output.push_str(&format!("\n  {bold}{}/{reset}\n", module));
            for artifact in arts {
                let icon = self.change_icon(&artifact.change_type);
                let path = artifact
                    .resource_uri
                    .strip_prefix("fs://workspace/")
                    .unwrap_or(&artifact.resource_uri);
                let short_path = path.strip_prefix(&format!("{}/", module)).unwrap_or(path);

                let summary_raw = artifact
                    .explanation_tiers
                    .as_ref()
                    .map(|t| t.summary.as_str())
                    .or(artifact.rationale.as_deref())
                    .unwrap_or_else(|| {
                        default_summary(&artifact.resource_uri, &artifact.change_type)
                    });
                let summary = Self::strip_html(summary_raw);

                let dep_marker = if !artifact.dependencies.is_empty() {
                    let deps: Vec<&str> = artifact
                        .dependencies
                        .iter()
                        .map(|d| {
                            d.target_uri
                                .strip_prefix("fs://workspace/")
                                .unwrap_or(&d.target_uri)
                        })
                        .collect();
                    format!(" {dim}[deps: {}]{reset}", deps.join(", "))
                } else {
                    String::new()
                };

                output.push_str(&format!(
                    "    {} {} — {}{}\n",
                    icon, short_path, summary, dep_marker
                ));
            }
        }

        output
    }

    /// Render the "Implementation Plan" section from work-plan.json (v0.15.20).
    ///
    /// Shows the planner's decisions, implementation steps, and out-of-scope items
    /// before the file diff so reviewers see the reasoning context first.
    fn render_work_plan(&self, ctx: &RenderContext) -> String {
        use crate::draft_package::WorkPlanData;
        let work_plan = match ctx
            .package
            .work_plan
            .as_ref()
            .and_then(WorkPlanData::from_value)
        {
            Some(wp) => wp,
            None => return String::new(),
        };

        let bold = self.bold();
        let reset = self.reset();
        let dim = self.dim();

        let mut output = format!(
            "\n{bold}▸ Implementation Plan ({} decision(s), {} step(s)):{reset}\n",
            work_plan.decisions.len(),
            work_plan.implementation_plan.len(),
        );

        if !work_plan.decisions.is_empty() {
            output.push_str(&format!("  {bold}Decisions:{reset}\n"));
            for d in &work_plan.decisions {
                let conf = d
                    .confidence
                    .map(|c| format!(" {dim}[{:.0}%]{reset}", c * 100.0))
                    .unwrap_or_default();
                output.push_str(&format!("  ▸ {}{}{}\n", d.decision, conf, reset));
                output.push_str(&format!("      {dim}Rationale:{reset} {}\n", d.rationale));
                if !d.alternatives.is_empty() {
                    output.push_str(&format!(
                        "      {dim}Alternatives:{reset} {}\n",
                        d.alternatives.join(", ")
                    ));
                }
                if !d.files_affected.is_empty() {
                    output.push_str(&format!(
                        "      {dim}Files:{reset} {}\n",
                        d.files_affected.join(", ")
                    ));
                }
            }
        }

        if !work_plan.implementation_plan.is_empty() {
            output.push_str(&format!("  {bold}Steps:{reset}\n"));
            for step in &work_plan.implementation_plan {
                output.push_str(&format!(
                    "  {}. {} — {}\n",
                    step.step, step.file, step.action
                ));
                if !step.detail.is_empty() {
                    output.push_str(&format!("      {dim}{}{reset}\n", step.detail));
                }
            }
        }

        if !work_plan.out_of_scope.is_empty() {
            output.push_str(&format!("  {bold}Out of scope:{reset}\n"));
            for item in &work_plan.out_of_scope {
                output.push_str(&format!("  {dim}✗{reset} {}\n", item));
            }
        }

        output
    }

    /// Render the "Design Decisions" section from alternatives_considered (v0.9.5).
    fn render_design_decisions(&self, ctx: &RenderContext) -> String {
        let alts = &ctx.package.summary.alternatives_considered;
        if alts.is_empty() {
            return String::new();
        }

        let bold = self.bold();
        let reset = self.reset();
        let dim = self.dim();
        let green = self.color_code("\x1b[32m");

        let mut output = format!("\n{bold}Design Decisions:{reset}\n");
        for alt in alts {
            let marker = if alt.chosen {
                format!("{green}[chosen]{reset}")
            } else {
                format!("{dim}[considered]{reset}")
            };
            output.push_str(&format!("  {} {}\n", marker, alt.option));
            output.push_str(&format!("    {}\n", alt.rationale));
        }

        output
    }

    /// Render the "Agent Decision Log" section (v0.14.7).
    ///
    /// Shows decisions from `.ta-decisions.json` written by the agent,
    /// with indented alternatives and rationale.
    fn render_agent_decision_log(&self, ctx: &RenderContext) -> String {
        let decisions = &ctx.package.agent_decision_log;
        if decisions.is_empty() {
            return String::new();
        }

        let bold = self.bold();
        let reset = self.reset();
        let dim = self.dim();

        let mut output = format!(
            "\n{bold}▸ Agent Decision Log ({} decision(s)):{reset}\n",
            decisions.len()
        );

        for entry in decisions {
            let confidence_str = entry
                .confidence
                .map(|c| format!(" {dim}[{:.0}% confidence]{reset}", c * 100.0))
                .unwrap_or_default();

            // Header line: if context is set, use "[context] → decision"; otherwise just "decision".
            if let Some(ctx_str) = &entry.context {
                output.push_str(&format!(
                    "  ▸ {} → {}{}{}\n",
                    ctx_str, entry.decision, confidence_str, reset
                ));
            } else {
                output.push_str(&format!(
                    "  ▸ {}{}{}\n",
                    entry.decision, confidence_str, reset
                ));
            }

            // List alternatives if any.
            let alts: Vec<&str> = entry
                .alternatives
                .iter()
                .map(String::as_str)
                .chain(
                    entry
                        .alternatives_considered
                        .iter()
                        .map(|a| a.description.as_str()),
                )
                .collect();
            if !alts.is_empty() {
                output.push_str(&format!(
                    "      {dim}Alternatives:{reset} {}\n",
                    alts.join(", ")
                ));
            }

            output.push_str(&format!(
                "      {dim}Rationale:{reset} {}\n",
                entry.rationale
            ));
        }

        output
    }
}

impl OutputAdapter for TerminalAdapter {
    fn render(&self, ctx: &RenderContext) -> Result<String, ChangeSetError> {
        use crate::output_adapters::SectionFilter;

        let mut output = String::new();
        let bold = self.bold();
        let reset = self.reset();
        let dim = self.dim();

        // Filter artifacts
        let artifacts = &ctx.package.changes.artifacts;
        let filtered_artifacts: Vec<&Artifact> = artifacts
            .iter()
            .filter(|a| matches_file_filters(&a.resource_uri, &ctx.file_filters))
            .collect();

        if filtered_artifacts.is_empty() && !ctx.file_filters.is_empty() {
            return Err(ChangeSetError::InvalidData(format!(
                "No artifacts match filters: {}",
                ctx.file_filters.join(", ")
            )));
        }

        // ── Section filtering: emit only the requested section ──
        match ctx.section_filter {
            Some(SectionFilter::Summary) => {
                output.push_str(&self.render_header(ctx));
                return Ok(output);
            }
            Some(SectionFilter::Decisions) => {
                output.push_str(&self.render_work_plan(ctx));
                output.push_str(&self.render_agent_decision_log(ctx));
                output.push_str(&self.render_design_decisions(ctx));
                if output.is_empty() {
                    output.push_str(&format!(
                        "{dim}No decisions recorded for this draft.{reset}\n"
                    ));
                }
                return Ok(output);
            }
            Some(SectionFilter::Validation) => {
                // Validation log is rendered in the calling layer (draft.rs) after adapter output.
                // Return a hint so the user knows to look there.
                output.push_str(&format!(
                    "{dim}Validation output is shown after the main view.{reset}\n\
                     {dim}Run `ta draft view <id>` (without --section) to see it inline.{reset}\n"
                ));
                return Ok(output);
            }
            Some(SectionFilter::Files) => {
                output.push_str(&self.render_grouped_changes(&filtered_artifacts));
                if ctx.detail_level != DetailLevel::Top {
                    output.push_str(&format!(
                        "\n{bold}Artifacts ({}):{reset}\n",
                        filtered_artifacts.len()
                    ));
                    for artifact in &filtered_artifacts {
                        match ctx.detail_level {
                            DetailLevel::Top => unreachable!(),
                            DetailLevel::Medium => {
                                output.push_str(&self.render_artifact_medium(artifact));
                                output.push('\n');
                            }
                            DetailLevel::Full => {
                                output.push_str(&self.render_artifact_full(artifact, ctx));
                                output.push('\n');
                            }
                        }
                    }
                }
                return Ok(output);
            }
            None => {}
        }

        // ── Full hierarchical view ──

        // ── Summary ──
        output.push_str(&self.render_header(ctx));

        // ── Implementation Plan (v0.15.20) ──
        output.push_str(&self.render_work_plan(ctx));

        // ── Agent Decision Log (v0.14.7) ──
        output.push_str(&self.render_agent_decision_log(ctx));

        // ── Design Decisions (legacy alternatives_considered) ──
        output.push_str(&self.render_design_decisions(ctx));

        // ── What Changed (module-grouped file list) ──
        output.push_str(&self.render_grouped_changes(&filtered_artifacts));

        // ── Artifacts (detailed per-artifact view) ──
        if ctx.detail_level != DetailLevel::Top {
            output.push_str(&format!(
                "\n{bold}Artifacts ({}):{reset}\n",
                filtered_artifacts.len()
            ));

            for artifact in &filtered_artifacts {
                match ctx.detail_level {
                    DetailLevel::Top => unreachable!(),
                    DetailLevel::Medium => {
                        output.push_str(&self.render_artifact_medium(artifact));
                        output.push('\n');
                    }
                    DetailLevel::Full => {
                        output.push_str(&self.render_artifact_full(artifact, ctx));
                        output.push('\n');
                    }
                }
            }
        }

        // Footer with review guidance
        if ctx.detail_level == DetailLevel::Top || ctx.detail_level == DetailLevel::Medium {
            output.push_str(&format!(
                "\n{dim}Tip: Use --detail full to see diffs · --section <name> to filter · --section decisions for decision log{reset}\n"
            ));
        }

        Ok(output)
    }

    fn name(&self) -> &str {
        "terminal"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_package::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn test_package() -> PRPackage {
        PRPackage {
            package_version: "1.0.0".to_string(),
            package_id: Uuid::new_v4(),
            created_at: Utc::now(),
            goal: Goal {
                goal_id: "goal-1".to_string(),
                title: "Test Goal".to_string(),
                objective: "Test objective".to_string(),
                success_criteria: vec![],
                constraints: vec![],
                parent_goal_title: None,
            },
            iteration: Iteration {
                iteration_id: "iter-1".to_string(),
                sequence: 1,
                workspace_ref: WorkspaceRef {
                    ref_type: "staging".to_string(),
                    ref_name: "staging/1".to_string(),
                    base_ref: None,
                },
            },
            agent_identity: AgentIdentity {
                agent_id: "agent-1".to_string(),
                agent_type: "coder".to_string(),
                constitution_id: "default".to_string(),
                capability_manifest_hash: "hash123".to_string(),
                orchestrator_run_id: None,
            },
            summary: Summary {
                what_changed: "Updated auth system".to_string(),
                why: "To improve security".to_string(),
                impact: "All users must re-login".to_string(),
                rollback_plan: "Revert commit".to_string(),
                open_questions: vec![],
                alternatives_considered: vec![],
            },
            plan: Plan {
                completed_steps: vec![],
                next_steps: vec![],
                decision_log: vec![],
            },
            changes: Changes {
                artifacts: vec![Artifact {
                    resource_uri: "fs://workspace/src/auth.rs".to_string(),
                    change_type: ChangeType::Modify,
                    diff_ref: "changeset:0".to_string(),
                    tests_run: vec![],
                    disposition: ArtifactDisposition::Pending,
                    rationale: Some("JWT migration".to_string()),
                    dependencies: vec![],
                    explanation_tiers: Some(ExplanationTiers {
                        summary: "Migrated to JWT auth".to_string(),
                        explanation: "Full JWT implementation with validation".to_string(),
                        tags: vec!["security".to_string()],
                        related_artifacts: vec![],
                    }),
                    comments: None,
                    amendment: None,
                    kind: None,
                }],
                patch_sets: vec![],
                pending_actions: vec![],
            },
            risk: Risk {
                risk_score: 10,
                findings: vec![],
                policy_decisions: vec![],
            },
            provenance: Provenance {
                inputs: vec![],
                tool_trace_hash: "trace123".to_string(),
            },
            review_requests: ReviewRequests {
                requested_actions: vec![],
                reviewers: vec![],
                required_approvals: 1,
                notes_to_reviewer: None,
            },
            signatures: Signatures {
                package_hash: "hash123".to_string(),
                agent_signature: "sig123".to_string(),
                gateway_attestation: None,
            },
            status: PRStatus::PendingReview,
            verification_warnings: vec![],
            validation_log: vec![],
            display_id: None,
            tag: None,
            vcs_status: None,
            parent_draft_id: None,
            pending_approvals: vec![],
            supervisor_review: None,
            ignored_artifacts: vec![],
            baseline_artifacts: vec![],
            agent_decision_log: vec![],
            work_plan: None,
            goal_shortref: None,
            draft_seq: 0,
            plan_phase: None,
            plan_md_base: None,
        }
    }

    #[test]
    fn render_top_level() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };

        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Draft"));
        assert!(output.contains("pending_review"));
        assert!(output.contains("src/"));
        assert!(output.contains("auth.rs"));
        assert!(output.contains("Migrated to JWT auth"));
        // Default (no color) should not contain ANSI escape codes.
        assert!(!output.contains("\x1b["));
    }

    #[test]
    fn render_with_color() {
        let adapter = TerminalAdapter::with_color(true);
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };

        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Draft"));
        // Color mode should contain ANSI escape codes.
        assert!(output.contains("\x1b["));
    }

    #[test]
    fn render_medium_level() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Medium,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };

        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Full JWT implementation"));
        assert!(output.contains("security"));
    }

    #[test]
    fn file_filter_works() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec!["auth.rs".to_string()],
            diff_provider: None,
            section_filter: None,
        };

        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("auth.rs"));
    }

    #[test]
    fn file_filter_no_match_returns_error() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec!["nonexistent.rs".to_string()],
            diff_provider: None,
            section_filter: None,
        };

        let result = adapter.render(&ctx);
        assert!(result.is_err());
    }

    #[test]
    fn terminal_output_contains_no_html_tags() {
        // Regression test for the garbled HTML bug (ÆpendingÅ in terminal output).
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Medium,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(
            !output.contains("<span"),
            "HTML span tags must not appear in terminal output"
        );
        assert!(
            !output.contains("</span>"),
            "HTML closing tags must not appear in terminal output"
        );
        assert!(
            output.contains("[pending]"),
            "Disposition badge must use bracket notation"
        );
    }

    #[test]
    fn strip_html_removes_tags() {
        assert_eq!(
            TerminalAdapter::strip_html(r#"<span class="status">pending</span>"#).as_ref(),
            "pending"
        );
        assert_eq!(
            TerminalAdapter::strip_html("no tags here").as_ref(),
            "no tags here"
        );
        assert_eq!(TerminalAdapter::strip_html("").as_ref(), "");
    }

    #[test]
    fn strip_html_preserves_code_placeholders() {
        // Angle brackets in code-style text (e.g. <id>, <path>, <T>) should be preserved.
        assert_eq!(
            TerminalAdapter::strip_html("ta session show <id>").as_ref(),
            "ta session show <id>"
        );
        assert_eq!(
            TerminalAdapter::strip_html("Vec<String>").as_ref(),
            "Vec<String>"
        );
        assert_eq!(
            TerminalAdapter::strip_html("list [--all] and show <id>").as_ref(),
            "list [--all] and show <id>"
        );
        // But actual HTML is still stripped.
        assert_eq!(
            TerminalAdapter::strip_html(r#"text <span class="x">inner</span> more"#).as_ref(),
            "text inner more"
        );
    }

    #[test]
    fn strip_html_sanitizes_summary_fields() {
        // Simulate a package where the summary contains HTML (as if data was corrupted).
        let mut package = test_package();
        package.summary.what_changed =
            r#"Updated <span class="bold">auth</span> system"#.to_string();

        let adapter = TerminalAdapter::new();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(
            output.contains("Updated auth system"),
            "HTML should be stripped from summary"
        );
        assert!(!output.contains("<span"), "No HTML tags in terminal output");
    }

    // ── v0.9.5 Structured view tests ──

    #[test]
    fn render_grouped_changes_by_module() {
        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.changes.artifacts.push(Artifact {
            resource_uri: "fs://workspace/tests/auth_test.rs".to_string(),
            change_type: ChangeType::Add,
            diff_ref: "changeset:1".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Pending,
            rationale: Some("Added auth tests".to_string()),
            dependencies: vec![],
            explanation_tiers: None,
            comments: None,
            amendment: None,
            kind: None,
        });
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("What Changed (2 files):"));
        assert!(output.contains("src/"));
        assert!(output.contains("tests/"));
    }

    #[test]
    fn render_design_decisions() {
        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.summary.alternatives_considered = vec![
            DesignAlternative {
                option: "Use HashMap for lookup".to_string(),
                rationale: "Best performance".to_string(),
                chosen: true,
            },
            DesignAlternative {
                option: "Use BTreeMap".to_string(),
                rationale: "Ordered but slower".to_string(),
                chosen: false,
            },
        ];
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Design Decisions:"));
        assert!(output.contains("[chosen]"));
        assert!(output.contains("[considered]"));
        assert!(output.contains("Use HashMap for lookup"));
        assert!(output.contains("Use BTreeMap"));
    }

    #[test]
    fn render_no_design_decisions_when_empty() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(!output.contains("Design Decisions:"));
    }

    #[test]
    fn render_medium_shows_artifacts_section() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Medium,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        // Medium shows both grouped summary and detailed artifacts
        assert!(output.contains("What Changed"));
        assert!(output.contains("Artifacts (1):"));
    }

    // ── v0.14.7 Agent Decision Log tests ──

    #[test]
    fn render_agent_decision_log() {
        use crate::draft_package::DecisionLogEntry;

        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.agent_decision_log = vec![DecisionLogEntry {
            decision: "Used Ed25519 instead of RSA".to_string(),
            rationale: "Ed25519 is faster and smaller keys".to_string(),
            alternatives: vec!["RSA-2048".to_string(), "ECDSA P-256".to_string()],
            alternatives_considered: vec![],
            confidence: Some(0.9),
            context: None,
        }];
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Agent Decision Log"));
        assert!(output.contains("Used Ed25519 instead of RSA"));
        assert!(output.contains("RSA-2048"));
        assert!(output.contains("ECDSA P-256"));
        assert!(output.contains("Ed25519 is faster"));
        // Confidence shown as percentage
        assert!(output.contains("90%"));
    }

    #[test]
    fn render_agent_decision_log_empty() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(!output.contains("Agent Decision Log"));
    }

    #[test]
    fn section_filter_decisions() {
        use crate::draft_package::DecisionLogEntry;
        use crate::output_adapters::SectionFilter;

        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.agent_decision_log = vec![DecisionLogEntry {
            decision: "Chose async over sync".to_string(),
            rationale: "Better throughput".to_string(),
            alternatives: vec!["sync".to_string()],
            alternatives_considered: vec![],
            confidence: None,
            context: None,
        }];
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: Some(SectionFilter::Decisions),
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Chose async over sync"));
        // Summary should not appear when section=decisions
        assert!(!output.contains("Status:"));
    }

    #[test]
    fn section_filter_summary() {
        use crate::output_adapters::SectionFilter;

        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: Some(SectionFilter::Summary),
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Summary:"));
        // Files section should not appear
        assert!(!output.contains("What Changed"));
    }

    #[test]
    fn section_filter_files() {
        use crate::output_adapters::SectionFilter;

        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: Some(SectionFilter::Files),
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("What Changed"));
        // Header not present in files-only view
        assert!(!output.contains("Status:"));
    }

    #[test]
    fn render_agent_decision_log_with_context() {
        // Verify context is shown as "▸ [context] → [decision]" (v0.14.9.2).
        use crate::draft_package::DecisionLogEntry;

        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.agent_decision_log = vec![DecisionLogEntry {
            decision: "Use Ed25519 keys".to_string(),
            rationale: "Smaller and faster than RSA".to_string(),
            alternatives: vec![],
            alternatives_considered: vec![],
            confidence: None,
            context: Some("Ollama thinking-mode config".to_string()),
        }];
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Ollama thinking-mode config"));
        assert!(output.contains("Use Ed25519 keys"));
        // Should show the "→" separator between context and decision
        assert!(output.contains("→"));
    }

    #[test]
    fn file_filter_glob_match() {
        // Create a package with 2 artifacts, filter with "src/*.rs",
        // verify only the matching src-level file appears (v0.14.9.2).
        use crate::pr_package::*;

        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        // The default test_package has src/auth.rs — add a file in a different directory.
        package.changes.artifacts.push(Artifact {
            resource_uri: "fs://workspace/docs/README.md".to_string(),
            change_type: ChangeType::Modify,
            diff_ref: "changeset:1".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Pending,
            rationale: Some("Documentation".to_string()),
            dependencies: vec![],
            explanation_tiers: None,
            comments: None,
            amendment: None,
            kind: None,
        });
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec!["src/*.rs".to_string()],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        // auth.rs should appear (matches glob src/*.rs)
        assert!(output.contains("auth.rs"), "auth.rs should be shown");
        // README.md should not appear (doesn't match glob)
        assert!(
            !output.contains("README.md"),
            "README.md should be filtered out"
        );
    }

    #[test]
    fn file_filter_unmatched_returns_error() {
        // Filter with non-matching pattern should return an error (v0.14.9.2).
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filters: vec!["totally/nonexistent/path.rs".to_string()],
            diff_provider: None,
            section_filter: None,
        };
        let result = adapter.render(&ctx);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("No artifacts match filters"));
    }

    // ── v0.14.15 Image artifact rendering tests ──

    fn image_artifact(uri: &str, frame_index: u32) -> Artifact {
        Artifact {
            resource_uri: uri.to_string(),
            change_type: ChangeType::Add,
            diff_ref: format!("changeset:{}", frame_index),
            tests_run: vec![],
            disposition: ArtifactDisposition::Pending,
            rationale: Some("Rendered frame".to_string()),
            dependencies: vec![],
            explanation_tiers: None,
            comments: None,
            amendment: None,
            kind: Some(crate::artifact_kind::ArtifactKind::Image {
                width: Some(1024),
                height: Some(1024),
                format: Some("PNG".to_string()),
                frame_index: Some(frame_index),
            }),
        }
    }

    #[test]
    fn image_artifact_full_view_suppresses_diff() {
        // An image artifact in full detail should show image metadata,
        // not attempt to render a binary text diff.
        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.changes.artifacts = vec![image_artifact(
            "fs://workspace/render_output/day/beauty/frame_0000.png",
            0,
        )];

        struct AlwaysPanic;
        impl crate::output_adapters::DiffProvider for AlwaysPanic {
            fn get_diff(&self, _: &str) -> Result<String, ChangeSetError> {
                panic!("get_diff must not be called for image artifacts");
            }
        }

        let provider = AlwaysPanic;
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Full,
            file_filters: vec![],
            diff_provider: Some(&provider),
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(
            output.contains("Image artifact"),
            "should show 'Image artifact' header; got: {}",
            output
        );
        assert!(
            output.contains("Binary image — text diff suppressed"),
            "should indicate binary diff suppression; got: {}",
            output
        );
        assert!(
            output.contains("PNG"),
            "should show format; got: {}",
            output
        );
        assert!(
            output.contains("1024"),
            "should show resolution; got: {}",
            output
        );
    }

    #[test]
    fn image_artifact_set_summary_multiple_frames() {
        let artifacts: Vec<Artifact> = (0..42)
            .map(|i| image_artifact(&format!("fs://workspace/render/frame_{:04}.png", i), i))
            .collect();
        let refs: Vec<&Artifact> = artifacts.iter().collect();
        let summary = TerminalAdapter::render_image_artifact_set_summary(&refs);
        assert!(
            summary.contains("42"),
            "should contain frame count; got: {}",
            summary
        );
        assert!(
            summary.contains("PNG"),
            "should contain format; got: {}",
            summary
        );
        assert!(
            summary.contains("1024"),
            "should contain resolution; got: {}",
            summary
        );
    }

    #[test]
    fn image_artifact_set_summary_single_frame() {
        let artifacts = [image_artifact("fs://workspace/render/frame_0000.png", 0)];
        let refs: Vec<&Artifact> = artifacts.iter().collect();
        let summary = TerminalAdapter::render_image_artifact_set_summary(&refs);
        assert!(
            summary.contains("1 PNG frame"),
            "singular 'frame' for single image; got: {}",
            summary
        );
    }

    #[test]
    fn image_artifact_set_summary_empty() {
        // A set of non-image artifacts returns empty string.
        let mut package = test_package();
        package.changes.artifacts[0].kind = None;
        let refs: Vec<&Artifact> = package.changes.artifacts.iter().collect();
        let summary = TerminalAdapter::render_image_artifact_set_summary(&refs);
        assert_eq!(summary, "", "no images → empty summary");
    }

    // ── v0.15.0 Binary artifact rendering tests ──

    fn binary_artifact(uri: &str, mime: Option<&str>, byte_size: Option<u64>) -> Artifact {
        Artifact {
            resource_uri: uri.to_string(),
            change_type: ChangeType::Add,
            diff_ref: "changeset:bin0".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Pending,
            rationale: Some("Binary asset".to_string()),
            dependencies: vec![],
            explanation_tiers: None,
            comments: None,
            amendment: None,
            kind: Some(crate::artifact_kind::ArtifactKind::Binary {
                mime_type: mime.map(|s| s.to_string()),
                byte_size,
            }),
        }
    }

    fn text_artifact(uri: &str, encoding: Option<&str>, line_count: Option<u64>) -> Artifact {
        Artifact {
            resource_uri: uri.to_string(),
            change_type: ChangeType::Add,
            diff_ref: "changeset:txt0".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Pending,
            rationale: Some("Generated text".to_string()),
            dependencies: vec![],
            explanation_tiers: None,
            comments: None,
            amendment: None,
            kind: Some(crate::artifact_kind::ArtifactKind::Text {
                encoding: encoding.map(|s| s.to_string()),
                line_count,
            }),
        }
    }

    #[test]
    fn binary_artifact_full_view_suppresses_diff() {
        // Binary artifact in full detail should show size info, not call diff provider.
        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.changes.artifacts = vec![binary_artifact(
            "fs://workspace/output/model.bin",
            Some("application/octet-stream"),
            Some(1_048_576),
        )];

        struct AlwaysPanic;
        impl crate::output_adapters::DiffProvider for AlwaysPanic {
            fn get_diff(&self, _: &str) -> Result<String, ChangeSetError> {
                panic!("get_diff must not be called for binary artifacts");
            }
        }

        let provider = AlwaysPanic;
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Full,
            file_filters: vec![],
            diff_provider: Some(&provider),
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(
            output.contains("Binary artifact"),
            "should show 'Binary artifact' header; got: {}",
            output
        );
        assert!(
            output.contains("Binary file") || output.contains("binary file"),
            "should indicate diff suppression; got: {}",
            output
        );
        assert!(
            output.contains("1.0 MB"),
            "should show size; got: {}",
            output
        );
    }

    #[test]
    fn binary_artifact_full_view_shows_mime() {
        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.changes.artifacts = vec![binary_artifact(
            "fs://workspace/output/archive.zip",
            Some("application/zip"),
            Some(512),
        )];

        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Full,
            file_filters: vec![],
            diff_provider: None,
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(
            output.contains("application/zip"),
            "should show MIME type; got: {}",
            output
        );
        assert!(
            output.contains("512 B"),
            "should show size in bytes; got: {}",
            output
        );
    }

    #[test]
    fn binary_artifact_set_summary_with_sizes() {
        let artifacts = [
            binary_artifact("fs://workspace/a.bin", None, Some(1_024)),
            binary_artifact("fs://workspace/b.bin", None, Some(2_048)),
            binary_artifact("fs://workspace/c.bin", None, Some(1_024)),
        ];
        let refs: Vec<&Artifact> = artifacts.iter().collect();
        let summary = TerminalAdapter::render_binary_artifact_set_summary(&refs);
        assert!(
            summary.contains("3 binary files"),
            "should say '3 binary files'; got: {}",
            summary
        );
        assert!(
            summary.contains("4.0 KB"),
            "should show total size; got: {}",
            summary
        );
    }

    #[test]
    fn binary_artifact_set_summary_unknown_size() {
        // When byte_size is absent for any artifact, total is omitted.
        let artifacts = [
            binary_artifact("fs://workspace/a.bin", None, Some(1_024)),
            binary_artifact("fs://workspace/b.bin", None, None),
        ];
        let refs: Vec<&Artifact> = artifacts.iter().collect();
        let summary = TerminalAdapter::render_binary_artifact_set_summary(&refs);
        assert!(
            summary.contains("2 binary files"),
            "should say '2 binary files'; got: {}",
            summary
        );
        // No total should appear because b.bin has unknown size.
        assert!(
            !summary.contains("total"),
            "should not show total when size unknown; got: {}",
            summary
        );
    }

    #[test]
    fn binary_artifact_set_summary_single() {
        let artifacts = [binary_artifact("fs://workspace/x.bin", None, Some(256))];
        let refs: Vec<&Artifact> = artifacts.iter().collect();
        let summary = TerminalAdapter::render_binary_artifact_set_summary(&refs);
        assert!(
            summary.contains("1 binary file"),
            "singular form; got: {}",
            summary
        );
        assert!(
            !summary.contains("1 binary files"),
            "no plural 's'; got: {}",
            summary
        );
    }

    #[test]
    fn binary_artifact_set_summary_empty() {
        let refs: Vec<&Artifact> = vec![];
        let summary = TerminalAdapter::render_binary_artifact_set_summary(&refs);
        assert_eq!(summary, "", "no binaries → empty summary");
    }

    // ── v0.15.0 Text artifact rendering tests ──

    #[test]
    fn text_artifact_full_view_renders_diff() {
        // Text artifact should fall through to diff rendering.
        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.changes.artifacts = vec![text_artifact(
            "fs://workspace/scripts/setup.sh",
            Some("utf-8"),
            Some(42),
        )];

        struct FixedDiff;
        impl crate::output_adapters::DiffProvider for FixedDiff {
            fn get_diff(&self, _: &str) -> Result<String, ChangeSetError> {
                Ok("+#!/bin/bash\n+echo hello\n".to_string())
            }
        }

        let provider = FixedDiff;
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Full,
            file_filters: vec![],
            diff_provider: Some(&provider),
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(
            output.contains("Text artifact"),
            "should show 'Text artifact' header; got: {}",
            output
        );
        assert!(
            output.contains("utf-8"),
            "should show encoding; got: {}",
            output
        );
        assert!(
            output.contains("42"),
            "should show line count; got: {}",
            output
        );
        // diff should be rendered (not suppressed)
        assert!(
            output.contains("echo hello"),
            "should render diff content; got: {}",
            output
        );
    }

    #[test]
    fn text_artifact_set_summary_multiple() {
        let artifacts = [
            text_artifact("fs://workspace/a.sh", None, None),
            text_artifact("fs://workspace/b.sh", None, None),
        ];
        let refs: Vec<&Artifact> = artifacts.iter().collect();
        let summary = TerminalAdapter::render_text_artifact_set_summary(&refs);
        assert_eq!(summary, "2 text files");
    }

    #[test]
    fn text_artifact_set_summary_single() {
        let artifacts = [text_artifact("fs://workspace/a.conf", None, None)];
        let refs: Vec<&Artifact> = artifacts.iter().collect();
        let summary = TerminalAdapter::render_text_artifact_set_summary(&refs);
        assert_eq!(summary, "1 text file");
    }

    #[test]
    fn text_artifact_set_summary_empty() {
        let refs: Vec<&Artifact> = vec![];
        let summary = TerminalAdapter::render_text_artifact_set_summary(&refs);
        assert_eq!(summary, "");
    }

    // ── format_byte_size helper tests ──

    #[test]
    fn format_byte_size_bytes() {
        assert_eq!(super::format_byte_size(0), "0 B");
        assert_eq!(super::format_byte_size(512), "512 B");
        assert_eq!(super::format_byte_size(1023), "1023 B");
    }

    #[test]
    fn format_byte_size_kb() {
        assert_eq!(super::format_byte_size(1024), "1.0 KB");
        assert_eq!(super::format_byte_size(1536), "1.5 KB");
    }

    #[test]
    fn format_byte_size_mb() {
        assert_eq!(super::format_byte_size(1_048_576), "1.0 MB");
        assert_eq!(super::format_byte_size(5 * 1_048_576), "5.0 MB");
    }

    #[test]
    fn format_byte_size_gb() {
        assert_eq!(super::format_byte_size(1_073_741_824), "1.0 GB");
    }

    // ── v0.15.1 Video artifact rendering tests ──

    fn video_artifact(
        uri: &str,
        width: Option<u32>,
        height: Option<u32>,
        fps: Option<f32>,
        duration_secs: Option<f32>,
        format: Option<&str>,
    ) -> Artifact {
        Artifact {
            resource_uri: uri.to_string(),
            change_type: ChangeType::Add,
            diff_ref: "changeset:vid0".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Pending,
            rationale: Some("Rendered video".to_string()),
            dependencies: vec![],
            explanation_tiers: None,
            comments: None,
            amendment: None,
            kind: Some(crate::artifact_kind::ArtifactKind::Video {
                width,
                height,
                fps,
                duration_secs,
                format: format.map(|s| s.to_string()),
                frame_count: None,
            }),
        }
    }

    #[test]
    fn video_artifact_full_view_suppresses_diff() {
        // Video artifact in full detail should show metadata, not attempt to render a text diff.
        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.changes.artifacts = vec![video_artifact(
            "fs://workspace/output/clip.mp4",
            Some(1920),
            Some(1080),
            Some(24.0),
            Some(6.2),
            Some("MP4"),
        )];

        struct AlwaysPanic;
        impl crate::output_adapters::DiffProvider for AlwaysPanic {
            fn get_diff(&self, _: &str) -> Result<String, ChangeSetError> {
                panic!("get_diff must not be called for video artifacts");
            }
        }

        let provider = AlwaysPanic;
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Full,
            file_filters: vec![],
            diff_provider: Some(&provider),
            section_filter: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(
            output.contains("Video artifact"),
            "should show 'Video artifact' header; got: {}",
            output
        );
        assert!(
            output.contains("Binary video — text diff suppressed"),
            "should indicate diff suppression; got: {}",
            output
        );
        assert!(
            output.contains("1920×1080"),
            "should show resolution; got: {}",
            output
        );
        assert!(
            output.contains("MP4"),
            "should show format; got: {}",
            output
        );
        assert!(
            output.contains("6.2s"),
            "should show duration; got: {}",
            output
        );
    }

    #[test]
    fn video_artifact_set_summary_multiple() {
        let artifacts = [
            video_artifact(
                "fs://workspace/output/clip_a.mp4",
                Some(1920),
                Some(1080),
                Some(24.0),
                None,
                Some("MP4"),
            ),
            video_artifact(
                "fs://workspace/output/clip_b.mp4",
                Some(1920),
                Some(1080),
                Some(24.0),
                None,
                Some("MP4"),
            ),
        ];
        let refs: Vec<&Artifact> = artifacts.iter().collect();
        let summary = TerminalAdapter::render_video_artifact_set_summary(&refs);
        assert!(
            summary.contains("2 MP4 video files"),
            "should say '2 MP4 video files'; got: {}",
            summary
        );
        assert!(
            summary.contains("1920×1080"),
            "should contain resolution; got: {}",
            summary
        );
        assert!(
            summary.contains("24fps") || summary.contains("24"),
            "should contain fps; got: {}",
            summary
        );
    }

    #[test]
    fn video_artifact_set_summary_single() {
        let artifacts = [video_artifact(
            "fs://workspace/output/clip.mov",
            None,
            None,
            None,
            Some(10.0),
            Some("MOV"),
        )];
        let refs: Vec<&Artifact> = artifacts.iter().collect();
        let summary = TerminalAdapter::render_video_artifact_set_summary(&refs);
        assert!(
            summary.contains("1 MOV video file"),
            "singular form; got: {}",
            summary
        );
        assert!(
            !summary.contains("1 MOV video files"),
            "no plural 's'; got: {}",
            summary
        );
    }

    #[test]
    fn video_artifact_set_summary_empty() {
        let refs: Vec<&Artifact> = vec![];
        let summary = TerminalAdapter::render_video_artifact_set_summary(&refs);
        assert_eq!(summary, "", "no videos → empty summary");
    }

    #[test]
    fn video_artifact_set_summary_no_metadata() {
        let artifacts = [video_artifact(
            "fs://workspace/output/clip.webm",
            None,
            None,
            None,
            None,
            None,
        )];
        let refs: Vec<&Artifact> = artifacts.iter().collect();
        let summary = TerminalAdapter::render_video_artifact_set_summary(&refs);
        assert!(
            summary.contains("1 video file"),
            "should say '1 video file' without format; got: {}",
            summary
        );
    }
}
