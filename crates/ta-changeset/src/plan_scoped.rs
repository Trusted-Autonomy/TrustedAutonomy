//! Phase-scoped PLAN.md merge for `ta draft apply --phase`.
//!
//! The three-way merge in [`crate::plan_merge`] reconciles whole documents, which
//! is the wrong tool when a draft is applied for ONE plan phase: any line it
//! rewrites outside that phase is a change the draft never made. This module is
//! the narrow alternative. Starting from the current `PLAN.md` on disk (the
//! "source"), it changes exactly two kinds of lines, both inside the target
//! phase's own block:
//!
//! 1. an item checkbox (`1. [ ] ...` becomes `1. [x] ...`) when the draft's copy
//!    of the same item (matched by its text) is checked,
//! 2. an item's text, when the draft rewrote the item that has the same list
//!    number and the source item is still unchecked (a reviewer's reword), and
//! 3. nothing else inside the block. The `<!-- status: ... -->` marker is set
//!    separately by the status update step, which also only touches the target block.
//!
//! The one thing carried from outside the target block is a whole phase block
//! the draft added (a heading no phase in the source has). Changes the draft
//! makes to any other existing phase are refused.
//!
//! Everything else, including blank lines, code fences, line endings, trailing
//! spaces, other phases and every checkbox in them, is carried through
//! byte-for-byte. Items the draft left unchecked are never checked here; they
//! are reported so the caller can name them (Deferred Items Policy).
//!
//! Block boundaries are found with a fence-aware heading scan: a `###` inside a
//! fenced code block is not a phase heading, and a `- [ ]` inside one is not an
//! item. Items under a `Human Review` style sub-heading are never "own" items,
//! so a human sign-off checkbox cannot be flipped by an agent's draft.

/// A located phase block: `[heading_line, end_line)` indexes into the document's lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseBlock {
    pub heading_line: usize,
    pub end_line: usize,
    pub level: usize,
}

/// Why a phase block could not be located.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateError {
    /// No heading outside a code fence names this phase id.
    NotFound,
    /// More than one heading names this phase id (look-alike or duplicated heading).
    Ambiguous(usize),
}

impl std::fmt::Display for LocateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocateError::NotFound => write!(f, "no heading for this phase was found"),
            LocateError::Ambiguous(n) => {
                write!(
                    f,
                    "{n} headings match this phase id, so the block is ambiguous"
                )
            }
        }
    }
}

/// One checkbox item that belongs to a phase (not fenced, not under Human Review).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnItem {
    /// 0-based line index in the document.
    pub line: usize,
    /// The list number for `N. [ ]` items; `None` for `- [ ]` bullets.
    pub number: Option<u32>,
    pub checked: bool,
    /// Item text after the checkbox, whitespace-collapsed (used for matching).
    pub text: String,
}

/// An item whose text the draft rewrote and that was carried into the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewrittenItem {
    /// The item as the source had it.
    pub before: OwnItem,
    /// The item as carried (text from the draft; `checked` reflects the draft).
    pub after: OwnItem,
}

impl OwnItem {
    /// Short label for messages: `item 3 "Add the thing"` or `item "Add the thing"`.
    pub fn label(&self) -> String {
        let mut text: String = self.text.chars().take(80).collect();
        if self.text.chars().count() > 80 {
            text.push_str("...");
        }
        match self.number {
            Some(n) => format!("item {n} \"{text}\""),
            None => format!("item \"{text}\""),
        }
    }
}

/// Result of [`merge_phase_from_staging`].
#[derive(Debug, Clone)]
pub struct ScopedMerge {
    /// The new document text. Equal to the input source when nothing changed.
    pub content: String,
    /// Items checked because the draft checked them.
    pub newly_checked: Vec<OwnItem>,
    /// Items of the phase that are still unchecked after the merge.
    pub left_unchecked: Vec<OwnItem>,
    /// Items whose text was rewritten by the draft and carried (same list number,
    /// source item unchecked). A rewrite the draft also checked is in `newly_checked` too.
    pub rewritten: Vec<RewrittenItem>,
    /// Items the draft's copy has checked but that match nothing in the source
    /// (removed, renumbered, or reworded where the source item is already checked);
    /// never applied. The apply step fails when this is not empty.
    pub unmatched_in_draft: Vec<OwnItem>,
    /// Headings of phase blocks the draft added that were carried into the source.
    pub new_phases: Vec<String>,
    /// `(from, to)` when the draft advanced the phase marker from `pending` to
    /// `in_progress` and that was carried. `done` is never carried from a draft.
    pub marker_advanced: Option<(String, String)>,
    /// Set when the phase block could not be located in the source.
    pub source_problem: Option<LocateError>,
    /// Set when the phase block could not be located in the draft's copy.
    pub draft_problem: Option<LocateError>,
}

impl ScopedMerge {
    pub fn changed(&self, original: &str) -> bool {
        self.content != original
    }
}

fn split_lines(content: &str) -> Vec<&str> {
    content.split_inclusive('\n').collect()
}

/// A line without its line terminator.
fn bare(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}

/// If `line` opens or closes a code fence, return `(marker char, run length, rest after run)`.
fn fence_run(line: &str) -> Option<(char, usize, &str)> {
    let s = bare(line).trim_start_matches(' ');
    let ch = s.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let run = s.chars().take_while(|&c| c == ch).count();
    if run < 3 {
        return None;
    }
    Some((ch, run, &s[run..]))
}

/// For each line, whether it is a fence delimiter or inside a fenced block.
fn fenced_lines(lines: &[&str]) -> Vec<bool> {
    let mut out = vec![false; lines.len()];
    let mut open: Option<(char, usize)> = None;
    for (i, line) in lines.iter().enumerate() {
        match (open, fence_run(line)) {
            (None, Some((ch, run, _))) => {
                open = Some((ch, run));
                out[i] = true;
            }
            (Some((och, orun)), Some((ch, run, rest))) => {
                out[i] = true;
                if ch == och && run >= orun && rest.trim().is_empty() {
                    open = None;
                }
            }
            (Some(_), None) => out[i] = true,
            (None, None) => {}
        }
    }
    out
}

/// `(level, text)` for an ATX heading line (`### Title`).
fn heading(line: &str) -> Option<(usize, &str)> {
    let s = bare(line);
    let level = s.chars().take_while(|&c| c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &s[level..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some((level, rest.trim()))
}

fn norm_id(id: &str) -> String {
    id.trim()
        .trim_end_matches([':', ',', '.'])
        .to_ascii_lowercase()
        .trim_start_matches('v')
        .to_string()
}

/// True when the heading text names `phase_id`: its first word (or the word after a
/// leading `Phase`) equals the id, ignoring a leading `v` and case. Exact token
/// comparison, so `v0.17.11.2` never matches `v0.17.11.29`.
fn heading_names_phase(text: &str, phase_id: &str) -> bool {
    let mut words = text.split_whitespace();
    let Some(mut first) = words.next() else {
        return false;
    };
    if first.eq_ignore_ascii_case("phase") {
        match words.next() {
            Some(w) => first = w,
            None => return false,
        }
    }
    norm_id(first) == norm_id(phase_id)
}

/// Find the block of `phase_id`: its heading line through the line before the next
/// heading of the same or a shallower level (fenced lines never count as headings).
pub fn locate_phase_block(content: &str, phase_id: &str) -> Result<PhaseBlock, LocateError> {
    let lines = split_lines(content);
    locate_in_lines(&lines, &fenced_lines(&lines), phase_id)
}

fn locate_in_lines(
    lines: &[&str],
    fenced: &[bool],
    phase_id: &str,
) -> Result<PhaseBlock, LocateError> {
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if fenced[i] {
            continue;
        }
        if let Some((level, text)) = heading(line) {
            if level >= 2 && heading_names_phase(text, phase_id) {
                hits.push((i, level));
            }
        }
    }
    match hits.as_slice() {
        [] => Err(LocateError::NotFound),
        [(start, level)] => {
            let mut end = lines.len();
            for (j, line) in lines.iter().enumerate().skip(start + 1) {
                if fenced[j] {
                    continue;
                }
                if let Some((l, _)) = heading(line) {
                    if l <= *level {
                        end = j;
                        break;
                    }
                }
            }
            Ok(PhaseBlock {
                heading_line: *start,
                end_line: end,
                level: *level,
            })
        }
        many => Err(LocateError::Ambiguous(many.len())),
    }
}

/// Parse `1. [ ] text`, `- [x] text` (column 0 only). Returns
/// `(number, checked, byte index of the box character, text)`.
fn parse_item(line: &str) -> Option<(Option<u32>, bool, usize, String)> {
    let s = bare(line);
    let bytes = s.as_bytes();
    let (number, mut i) = if bytes.first() == Some(&b'-') {
        (None, 1)
    } else {
        let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || bytes.get(digits) != Some(&b'.') {
            return None;
        }
        (s[..digits].parse::<u32>().ok(), digits + 1)
    };
    if bytes.get(i) != Some(&b' ')
        || bytes.get(i + 1) != Some(&b'[')
        || bytes.get(i + 3) != Some(&b']')
    {
        return None;
    }
    i += 2;
    let checked = match bytes.get(i) {
        Some(b' ') => false,
        Some(b'x') | Some(b'X') => true,
        _ => return None,
    };
    let rest = &s[i + 2..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let text = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    Some((number, checked, i, text))
}

fn is_human_review_heading(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("human review") || t.contains("human gate") || t.contains("human task")
}

fn own_items_in(lines: &[&str], fenced: &[bool], block: &PhaseBlock) -> Vec<OwnItem> {
    let mut items = Vec::new();
    let mut in_human_review = false;
    for i in (block.heading_line + 1)..block.end_line {
        if fenced[i] {
            continue;
        }
        if let Some((_, text)) = heading(lines[i]) {
            in_human_review = is_human_review_heading(text);
            continue;
        }
        if in_human_review {
            continue;
        }
        if let Some((number, checked, _, text)) = parse_item(lines[i]) {
            items.push(OwnItem {
                line: i,
                number,
                checked,
                text,
            });
        }
    }
    items
}

/// The checkbox items that belong to `phase_id` (empty when the block is not found).
pub fn own_items(content: &str, phase_id: &str) -> Vec<OwnItem> {
    let lines = split_lines(content);
    let fenced = fenced_lines(&lines);
    match locate_in_lines(&lines, &fenced, phase_id) {
        Ok(block) => own_items_in(&lines, &fenced, &block),
        Err(_) => Vec::new(),
    }
}

/// Own items of `phase_id` that are still unchecked.
pub fn phase_unchecked_own_items(content: &str, phase_id: &str) -> Vec<OwnItem> {
    own_items(content, phase_id)
        .into_iter()
        .filter(|i| !i.checked)
        .collect()
}

/// The status word (`pending`, `in_progress`, `done`, ...) in the phase's own
/// `<!-- status: ... -->` marker, if the block and marker exist.
pub fn phase_status_word(content: &str, phase_id: &str) -> Option<String> {
    let lines = split_lines(content);
    let fenced = fenced_lines(&lines);
    let block = locate_in_lines(&lines, &fenced, phase_id).ok()?;
    for i in (block.heading_line + 1)..block.end_line {
        if fenced[i] {
            continue;
        }
        let t = bare(lines[i]).trim();
        if let Some(rest) = t.strip_prefix("<!--") {
            if let Some(rest) = rest.trim_start().strip_prefix("status:") {
                if let Some(word) = rest.strip_suffix("-->") {
                    return Some(word.trim().to_string());
                }
            }
        }
    }
    None
}

/// The `[x]` flip of a single unchecked item line, preserving every other byte.
fn check_line(line: &str) -> Option<String> {
    let (_, checked, box_idx, _) = parse_item(line)?;
    if checked {
        return None;
    }
    let mut out = String::with_capacity(line.len());
    out.push_str(&line[..box_idx]);
    out.push('x');
    out.push_str(&line[box_idx + 1..]);
    Some(out)
}

/// Matching passes, tried in order so the closest pairing wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchPass {
    ExactSameNumber,
    Exact,
    NoteAppended,
}

/// Whether the draft's item `draft` is the source item `src`.
///
/// Exact (whitespace-collapsed) text always matches. In the `NoteAppended` pass, one text may be the
/// other plus a trailing note: the shorter must be a prefix of the longer, ending on a word
/// boundary, and the two must carry the same list number. Unnumbered bullets have no number
/// to confirm the pairing, so they need a prefix of at least 12 characters.
fn items_match(src: &OwnItem, draft: &OwnItem, pass: MatchPass) -> bool {
    if src.text == draft.text {
        return pass != MatchPass::ExactSameNumber || src.number == draft.number;
    }
    if pass != MatchPass::NoteAppended || src.number != draft.number {
        return false;
    }
    let (short, long) = if src.text.len() <= draft.text.len() {
        (&src.text, &draft.text)
    } else {
        (&draft.text, &src.text)
    };
    if short.is_empty() || (src.number.is_none() && short.chars().count() < 12) {
        return false;
    }
    match long.strip_prefix(short.as_str()) {
        Some(rest) => rest.chars().next().is_none_or(|c| !c.is_alphanumeric()),
        None => false,
    }
}

/// Carry the draft's item checkmarks for `phase_id` onto `source`, touching nothing else.
///
/// `staging` is the draft's copy of `PLAN.md`. Items are matched by text, so
/// renumbering or reordering inside the phase is harmless. A checkmark is only ever
/// added: an item the draft unchecked stays as the source has it.
pub fn merge_phase_from_staging(source: &str, staging: &str, phase_id: &str) -> ScopedMerge {
    let src_lines = split_lines(source);
    let src_fenced = fenced_lines(&src_lines);
    let stg_lines = split_lines(staging);
    let stg_fenced = fenced_lines(&stg_lines);

    let mut result = ScopedMerge {
        content: source.to_string(),
        newly_checked: Vec::new(),
        left_unchecked: Vec::new(),
        rewritten: Vec::new(),
        unmatched_in_draft: Vec::new(),
        new_phases: Vec::new(),
        marker_advanced: None,
        source_problem: None,
        draft_problem: None,
    };

    let src_block = match locate_in_lines(&src_lines, &src_fenced, phase_id) {
        Ok(b) => b,
        Err(e) => {
            result.source_problem = Some(e);
            return result;
        }
    };
    let src_items = own_items_in(&src_lines, &src_fenced, &src_block);

    let stg_items = match locate_in_lines(&stg_lines, &stg_fenced, phase_id) {
        Ok(b) => own_items_in(&stg_lines, &stg_fenced, &b),
        Err(e) => {
            result.draft_problem = Some(e);
            Vec::new()
        }
    };

    // Pair each source item with a checked draft item. Exact text wins first; a second pass
    // pairs an item the agent reworded only by appending (or dropping) a trailing note, which
    // agents do when they check a box (seen in v0.17.11.27). `used` marks draft items that
    // have been paired, so duplicate item texts pair up in order.
    let mut used: Vec<bool> = vec![false; stg_items.len()];
    let mut paired: Vec<bool> = vec![false; src_items.len()];
    for pass in [
        MatchPass::ExactSameNumber,
        MatchPass::Exact,
        MatchPass::NoteAppended,
    ] {
        for (idx, item) in src_items.iter().enumerate() {
            if paired[idx] {
                continue;
            }
            let slot = stg_items
                .iter()
                .enumerate()
                .find(|(di, d)| d.checked && !used[*di] && items_match(item, d, pass));
            if let Some((di, _)) = slot {
                used[di] = true;
                paired[idx] = true;
            }
        }
    }

    // Rewrites: an unchecked source item still unpaired, and a draft item with the same list
    // number whose text matches no source item, is a reviewer's reword of that item. The
    // draft item's text replaces the source text (and its checkmark is carried).
    let mut rewrite_of: Vec<Option<usize>> = vec![None; src_items.len()];
    for (idx, item) in src_items.iter().enumerate() {
        let Some(number) = item.number else { continue };
        if paired[idx] || item.checked {
            continue;
        }
        // An item whose text still appears elsewhere in the draft moved (the draft inserted
        // or reordered items); it was not reworded, so never overwrite it with a neighbour.
        if stg_items.iter().any(|d| d.text == item.text) {
            continue;
        }
        let slot = stg_items.iter().enumerate().find(|(di, d)| {
            !used[*di]
                && d.number == Some(number)
                && d.text != item.text
                && !src_items.iter().any(|s| s.text == d.text)
        });
        if let Some((di, _)) = slot {
            used[di] = true;
            rewrite_of[idx] = Some(di);
        }
    }

    let mut new_lines: Vec<String> = src_lines.iter().map(|l| l.to_string()).collect();
    for (idx, item) in src_items.iter().enumerate() {
        if let Some(di) = rewrite_of[idx] {
            let draft = &stg_items[di];
            let line = rewrite_line(src_lines[item.line], stg_lines[draft.line], draft.checked);
            if let Some(line) = line {
                new_lines[item.line] = line;
                let after = OwnItem {
                    line: item.line,
                    number: item.number,
                    checked: draft.checked,
                    text: draft.text.clone(),
                };
                if after.checked {
                    result.newly_checked.push(after.clone());
                } else {
                    result.left_unchecked.push(after.clone());
                }
                result.rewritten.push(RewrittenItem {
                    before: item.clone(),
                    after,
                });
                continue;
            }
        }
        // An item already checked in the source consumes one matching draft checkmark.
        if paired[idx] && !item.checked {
            if let Some(flipped) = check_line(src_lines[item.line]) {
                new_lines[item.line] = flipped;
                let mut done = item.clone();
                done.checked = true;
                result.newly_checked.push(done);
                continue;
            }
        }
        if !item.checked {
            result.left_unchecked.push(item.clone());
        }
    }
    result.unmatched_in_draft = stg_items
        .iter()
        .enumerate()
        .filter(|(di, d)| d.checked && !used[*di])
        .map(|(_, d)| d.clone())
        .collect();
    // The draft's own pending -> in_progress marker is honored. `done` is decided by the
    // status step (only when every own item is checked), never copied from a draft.
    if phase_status_word(staging, phase_id).as_deref() == Some("in_progress")
        && phase_status_word(source, phase_id).as_deref() == Some("pending")
    {
        for i in (src_block.heading_line + 1)..src_block.end_line {
            if src_fenced[i] {
                continue;
            }
            let t = bare(src_lines[i]).trim();
            if t.starts_with("<!--") && t.contains("status:") && t.ends_with("-->") {
                let eol = &src_lines[i][bare(src_lines[i]).len()..];
                new_lines[i] = format!("<!-- status: in_progress -->{eol}");
                result.marker_advanced = Some(("pending".to_string(), "in_progress".to_string()));
                break;
            }
        }
    }
    result.content = new_lines.concat();
    if result.draft_problem.is_none() {
        let (content, added) = carry_new_phases(&result.content, staging, src_block.level);
        result.content = content;
        result.new_phases = added;
    }
    result
}

/// `src_line` with its item text replaced by the draft line's text, keeping the source's
/// list prefix and line ending. The box is checked when `checked`.
fn rewrite_line(src_line: &str, draft_line: &str, checked: bool) -> Option<String> {
    let (_, _, src_box, _) = parse_item(src_line)?;
    let (_, _, draft_box, _) = parse_item(draft_line)?;
    let eol = &src_line[bare(src_line).len()..];
    let mut out = String::with_capacity(draft_line.len());
    out.push_str(&src_line[..src_box]);
    out.push(if checked { 'x' } else { ' ' });
    out.push_str(&bare(draft_line)[draft_box + 1..]);
    out.push_str(eol);
    Some(out)
}

/// True when `text` (a heading's text) starts with something shaped like a version id
/// (`v0.17.12.25`, `0.1.0`), optionally after a leading `Phase`.
fn heading_has_phase_id(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let Some(mut first) = words.next() else {
        return false;
    };
    if first.eq_ignore_ascii_case("phase") {
        match words.next() {
            Some(w) => first = w,
            None => return false,
        }
    }
    let id = norm_id(first);
    id.contains('.')
        && id.split('.').all(|part| !part.is_empty())
        && id
            .split('.')
            .all(|part| part.chars().all(|c| c.is_ascii_digit()))
}

/// The id word of a phase heading (the token [`heading_names_phase`] compares).
fn heading_phase_id(text: &str) -> Option<String> {
    let mut words = text.split_whitespace();
    let mut first = words.next()?;
    if first.eq_ignore_ascii_case("phase") {
        first = words.next()?;
    }
    Some(norm_id(first))
}

/// Add whole phase blocks that exist in `staging` (at heading `level`) but whose id is in no
/// heading of `source`. Each is placed after the block that precedes it in the draft when
/// that block exists in the source, else before the block that follows it, else at the
/// end. Blocks for ids that already exist in `source` are never touched here. Returns the
/// new content and the carried headings.
fn carry_new_phases(source: &str, staging: &str, level: usize) -> (String, Vec<String>) {
    let stg_lines = split_lines(staging);
    let stg_fenced = fenced_lines(&stg_lines);

    // Phase blocks of the draft at this level, in order: (id, heading, start, end).
    let mut stg_blocks: Vec<(String, String, usize, usize)> = Vec::new();
    for (i, line) in stg_lines.iter().enumerate() {
        if stg_fenced[i] {
            continue;
        }
        let Some((l, text)) = heading(line) else {
            continue;
        };
        if l != level || !heading_has_phase_id(text) {
            continue;
        }
        let Some(id) = heading_phase_id(text) else {
            continue;
        };
        let mut end = stg_lines.len();
        for (j, next) in stg_lines.iter().enumerate().skip(i + 1) {
            if stg_fenced[j] {
                continue;
            }
            if let Some((l2, _)) = heading(next) {
                if l2 <= level {
                    end = j;
                    break;
                }
            }
        }
        stg_blocks.push((id, text.to_string(), i, end));
    }

    let mut content = source.to_string();
    let mut added = Vec::new();
    for (n, (id, text, start, end)) in stg_blocks.iter().enumerate() {
        // A duplicated id in the draft is ambiguous: do not guess which block is meant.
        if stg_blocks.iter().filter(|b| &b.0 == id).count() != 1 {
            continue;
        }
        let lines = split_lines(&content);
        let fenced = fenced_lines(&lines);
        if !matches!(
            locate_in_lines(&lines, &fenced, id),
            Err(LocateError::NotFound)
        ) {
            continue;
        }
        let mut block_text: String = stg_lines[*start..*end].concat();
        if !block_text.ends_with('\n') {
            block_text.push('\n');
        }
        // Insertion line: after the previous draft block, else before the next one.
        let after_prev = stg_blocks[..n].iter().rev().find_map(|(pid, ..)| {
            locate_in_lines(&lines, &fenced, pid)
                .ok()
                .map(|b| b.end_line)
        });
        let before_next = || {
            stg_blocks[n + 1..].iter().find_map(|(nid, ..)| {
                locate_in_lines(&lines, &fenced, nid)
                    .ok()
                    .map(|b| b.heading_line)
            })
        };
        let at = after_prev.or_else(before_next).unwrap_or(lines.len());
        let mut out = String::with_capacity(content.len() + block_text.len() + 1);
        out.push_str(&lines[..at].concat());
        if at > 0 && !lines[at - 1].ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&block_text);
        out.push_str(&lines[at..].concat());
        content = out;
        added.push(text.clone());
    }
    (content, added)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\
# Plan\n\
\n\
## Human Tasks\n\
- [ ] Sign the contract\n\
\n\
### v0.1.0 - First\n\
<!-- status: in_progress -->\n\
\n\
1. [ ] Alpha thing\n\
2. [ ] Beta thing\n\
3. [x] Gamma thing\n\
\n\
```\n\
### v0.1.1 - Not a heading\n\
1. [ ] Not an item\n\
```\n\
\n\
#### Human Review\n\
1. [ ] Maintainer signs off\n\
\n\
### v0.1.1 - Second\n\
<!-- status: pending -->\n\
1. [ ] Alpha thing\n\
2. [ ] Beta thing\n\
\n\
### v0.1.10 - Look alike\n\
<!-- status: pending -->\n\
1. [ ] Alpha thing\n";

    fn all_checked(doc: &str) -> String {
        doc.replace("[ ]", "[x]")
    }

    #[test]
    fn locates_the_exact_phase_and_ignores_fenced_headings() {
        let b = locate_phase_block(DOC, "v0.1.0").unwrap();
        let lines = split_lines(DOC);
        assert!(lines[b.heading_line].contains("v0.1.0 - First"));
        assert!(lines[b.end_line].contains("v0.1.1 - Second"));
        let b2 = locate_phase_block(DOC, "0.1.1").unwrap();
        assert!(split_lines(DOC)[b2.heading_line].contains("Second"));
        assert_eq!(
            locate_phase_block(DOC, "v0.1.2"),
            Err(LocateError::NotFound)
        );
    }

    #[test]
    fn duplicate_headings_are_reported_ambiguous() {
        let doc = "### v1.0.0 - A\n1. [ ] x\n### v1.0.0 - B\n1. [ ] y\n";
        assert_eq!(
            locate_phase_block(doc, "v1.0.0"),
            Err(LocateError::Ambiguous(2))
        );
        let m = merge_phase_from_staging(doc, &all_checked(doc), "v1.0.0");
        assert_eq!(m.content, doc);
        assert!(m.source_problem.is_some());
    }

    #[test]
    fn only_the_targets_own_items_are_checked() {
        let staging = all_checked(DOC);
        let m = merge_phase_from_staging(DOC, &staging, "v0.1.0");
        let expected = DOC.replace(
            "1. [ ] Alpha thing\n2. [ ] Beta thing\n3. [x]",
            "1. [x] Alpha thing\n2. [x] Beta thing\n3. [x]",
        );
        // Only the first occurrence (v0.1.0's items) changes; v0.1.1 and v0.1.10 keep theirs.
        let idx = DOC.find("1. [ ] Alpha thing").unwrap();
        let mut want = DOC.to_string();
        want.replace_range(idx..idx + "1. [ ] Alpha thing".len(), "1. [x] Alpha thing");
        let idx2 = want.find("2. [ ] Beta thing").unwrap();
        want.replace_range(idx2..idx2 + "2. [ ] Beta thing".len(), "2. [x] Beta thing");
        assert_ne!(expected, DOC);
        assert_eq!(m.content, want);
        assert_eq!(m.newly_checked.len(), 2);
        assert!(m.left_unchecked.is_empty());
    }

    #[test]
    fn human_review_and_fenced_items_are_never_own_items() {
        let items = own_items(DOC, "v0.1.0");
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|i| !i.text.contains("Maintainer")));
        assert!(items.iter().all(|i| !i.text.contains("Not an item")));
        let m = merge_phase_from_staging(DOC, &all_checked(DOC), "v0.1.0");
        assert!(m.content.contains("1. [ ] Maintainer signs off"));
        assert!(m.content.contains("- [ ] Sign the contract"));
        assert!(m.content.contains("1. [ ] Not an item"));
    }

    #[test]
    fn items_the_draft_left_unchecked_stay_unchecked_and_are_named() {
        let mut staging = DOC.to_string();
        staging = staging.replacen("1. [ ] Alpha thing", "1. [x] Alpha thing", 1);
        let m = merge_phase_from_staging(DOC, &staging, "v0.1.0");
        assert_eq!(m.newly_checked.len(), 1);
        assert_eq!(m.left_unchecked.len(), 1);
        assert_eq!(m.left_unchecked[0].number, Some(2));
        assert!(m.left_unchecked[0].label().contains("Beta thing"));
    }

    #[test]
    fn a_draft_never_unchecks_a_source_checkmark() {
        let staging = DOC.replace("3. [x] Gamma thing", "3. [ ] Gamma thing");
        let m = merge_phase_from_staging(DOC, &staging, "v0.1.0");
        assert_eq!(m.content, DOC);
    }

    #[test]
    fn crlf_blank_lines_and_trailing_spaces_survive() {
        let doc = "### v2.0.0 - X\r\n<!-- status: pending -->\r\n\r\n1. [ ] one  \r\n\r\n\r\n2. [ ] two\r\n";
        let staging = doc.replace("1. [ ]", "1. [x]");
        let m = merge_phase_from_staging(doc, &staging, "v2.0.0");
        assert_eq!(
            m.content,
            "### v2.0.0 - X\r\n<!-- status: pending -->\r\n\r\n1. [x] one  \r\n\r\n\r\n2. [ ] two\r\n"
        );
    }

    #[test]
    fn missing_phase_in_draft_changes_nothing_and_says_so() {
        let m = merge_phase_from_staging(DOC, "# nothing here\n", "v0.1.0");
        assert_eq!(m.content, DOC);
        assert_eq!(m.draft_problem, Some(LocateError::NotFound));
        assert_eq!(m.left_unchecked.len(), 2);
    }

    #[test]
    fn reworded_checked_item_with_the_same_number_is_carried() {
        // The reviewer rewrote item 1's own words and the draft checked it.
        let staging = DOC.replace("1. [ ] Alpha thing\n2.", "1. [x] Alpha widget\n2.");
        let m = merge_phase_from_staging(DOC, &staging, "v0.1.0");
        assert!(m.content.contains("1. [x] Alpha widget\n2. [ ] Beta thing"));
        assert!(!m
            .content
            .contains("1. [ ] Alpha thing\n2. [ ] Beta thing\n3. [x]"));
        assert!(m.unmatched_in_draft.is_empty(), "{m:?}");
        assert_eq!(m.rewritten.len(), 1);
        assert_eq!(m.rewritten[0].before.text, "Alpha thing");
        assert_eq!(m.rewritten[0].after.text, "Alpha widget");
        assert_eq!(m.newly_checked.len(), 1);
    }

    #[test]
    fn reworded_unchecked_item_is_carried_and_stays_unchecked() {
        let staging = DOC.replace("2. [ ] Beta thing\n3.", "2. [ ] Beta, reworded\n3.");
        let m = merge_phase_from_staging(DOC, &staging, "v0.1.0");
        assert!(m.content.contains("2. [ ] Beta, reworded\n3. [x]"));
        assert_eq!(m.rewritten.len(), 1);
        assert!(m.newly_checked.is_empty());
        assert!(m.left_unchecked.iter().any(|i| i.text == "Beta, reworded"));
    }

    #[test]
    fn rewrite_is_not_carried_over_an_already_checked_source_item() {
        let staging = DOC.replace("3. [x] Gamma thing", "3. [x] Gamma, reworded");
        let m = merge_phase_from_staging(DOC, &staging, "v0.1.0");
        assert_eq!(m.content, DOC);
        assert_eq!(m.unmatched_in_draft.len(), 1);
        assert!(m.rewritten.is_empty());
    }

    #[test]
    fn checked_item_with_no_counterpart_is_unmatched() {
        // A new item number the source never had: nothing to attach it to.
        let staging = DOC.replace(
            "3. [x] Gamma thing\n",
            "3. [x] Gamma thing\n4. [x] Brand new thing\n",
        );
        let m = merge_phase_from_staging(DOC, &staging, "v0.1.0");
        assert_eq!(m.content, DOC);
        assert_eq!(m.unmatched_in_draft.len(), 1);
        assert_eq!(m.unmatched_in_draft[0].number, Some(4));
    }

    #[test]
    fn renumbered_items_are_not_mistaken_for_rewrites() {
        // The draft inserted a new item 1; old 1 and 2 moved to 2 and 3 unchecked. Nothing is
        // lost: A and B stay as they were (an inserted item is not carried by this merge).
        let src = "### v1.0.0 - X\n<!-- status: pending -->\n1. [ ] A\n2. [ ] B\n";
        let stg = "### v1.0.0 - X\n<!-- status: pending -->\n1. [ ] New\n2. [ ] A\n3. [ ] B\n";
        let m = merge_phase_from_staging(src, stg, "v1.0.0");
        assert_eq!(m.content, src);
        assert!(m.rewritten.is_empty());
        assert!(m.unmatched_in_draft.is_empty());
    }

    const TWO_PHASES: &str = "\
### v1.0.0 - First\n\
<!-- status: in_progress -->\n\
1. [ ] One\n\
\n\
---\n\
\n\
### v1.1.0 - Last\n\
<!-- status: pending -->\n\
1. [ ] Tail\n";

    #[test]
    fn a_new_phase_block_added_by_the_draft_is_carried_in_place() {
        let stg = TWO_PHASES.replace(
            "### v1.1.0 - Last",
            "### v1.0.5 - Added\n<!-- status: pending -->\n1. [ ] Fresh\n\n---\n\n### v1.1.0 - Last",
        );
        let m = merge_phase_from_staging(TWO_PHASES, &stg, "v1.0.0");
        assert_eq!(m.new_phases, vec!["v1.0.5 - Added".to_string()]);
        let want = TWO_PHASES.replace(
            "### v1.1.0 - Last",
            "### v1.0.5 - Added\n<!-- status: pending -->\n1. [ ] Fresh\n\n---\n\n### v1.1.0 - Last",
        );
        assert_eq!(m.content, want);
        // Applying the same draft again adds nothing.
        let again = merge_phase_from_staging(&m.content, &stg, "v1.0.0");
        assert_eq!(again.content, m.content);
        assert!(again.new_phases.is_empty());
    }

    #[test]
    fn a_new_phase_at_the_end_is_appended_with_a_line_break() {
        let src = "### v1.0.0 - First\n<!-- status: in_progress -->\n1. [ ] One";
        let stg = format!("{src}\n\n### v1.2.0 - Next\n<!-- status: pending -->\n1. [ ] Z\n");
        let m = merge_phase_from_staging(src, &stg, "v1.0.0");
        assert_eq!(m.new_phases.len(), 1);
        assert_eq!(
            m.content,
            "### v1.0.0 - First\n<!-- status: in_progress -->\n1. [ ] One\n### v1.2.0 - Next\n<!-- status: pending -->\n1. [ ] Z\n"
        );
    }

    #[test]
    fn a_change_to_a_different_existing_phase_is_still_refused() {
        let stg = TWO_PHASES
            .replace("1. [ ] Tail", "1. [x] Tail, reworded")
            .replace("<!-- status: pending -->", "<!-- status: done -->");
        let m = merge_phase_from_staging(TWO_PHASES, &stg, "v1.0.0");
        assert_eq!(m.content, TWO_PHASES);
        assert!(m.new_phases.is_empty());
        assert!(m.rewritten.is_empty());
    }

    #[test]
    fn rewrite_and_new_phase_land_together() {
        let stg = TWO_PHASES
            .replace("1. [ ] One", "1. [x] Uno, reworded by the reviewer")
            .replace(
                "### v1.1.0 - Last",
                "### v1.0.5 - Added\n<!-- status: pending -->\n1. [ ] Fresh\n\n---\n\n### v1.1.0 - Last",
            );
        let m = merge_phase_from_staging(TWO_PHASES, &stg, "v1.0.0");
        assert!(m.content.contains("1. [x] Uno, reworded by the reviewer"));
        assert!(m.content.contains("### v1.0.5 - Added"));
        assert!(m.unmatched_in_draft.is_empty());
        assert!(m.content.contains("1. [ ] Tail"));
    }

    #[test]
    fn draft_in_progress_marker_is_carried_but_done_is_not() {
        let src = "### v4.0.0 - X\n<!-- status: pending -->\n1. [ ] a\n";
        let m = merge_phase_from_staging(src, &src.replace("pending", "in_progress"), "v4.0.0");
        assert_eq!(m.content, src.replace("pending", "in_progress"));
        assert!(m.marker_advanced.is_some());
        let m = merge_phase_from_staging(src, &src.replace("pending", "done"), "v4.0.0");
        assert_eq!(m.content, src);
    }

    #[test]
    fn bullet_items_are_own_items_too() {
        let doc = "### v3.0.0 - Old style\n<!-- status: pending -->\n- [ ] a\n- [ ] b\n";
        let m = merge_phase_from_staging(doc, &doc.replace("- [ ] a", "- [x] a"), "v3.0.0");
        assert_eq!(
            m.content,
            "### v3.0.0 - Old style\n<!-- status: pending -->\n- [x] a\n- [ ] b\n"
        );
    }
    #[test]
    fn checkmark_with_an_appended_implementation_note_still_counts() {
        let source =
            "### v1.0.0 - X\n<!-- status: in_progress -->\n1. [ ] Add the thing\n2. [ ] Other\n";
        let staging = "### v1.0.0 - X\n<!-- status: in_progress -->\n1. [x] Add the thing (done in apply.rs, 3 tests)\n2. [ ] Other\n";
        let m = merge_phase_from_staging(source, staging, "v1.0.0");
        assert_eq!(m.newly_checked.len(), 1, "{m:?}");
        assert!(m.unmatched_in_draft.is_empty(), "{m:?}");
        assert_eq!(
            m.content,
            "### v1.0.0 - X\n<!-- status: in_progress -->\n1. [x] Add the thing\n2. [ ] Other\n",
            "only the box flips; the agent's note is not copied into the source"
        );
    }

    #[test]
    fn appended_note_does_not_match_a_different_item_number_or_word() {
        // Same prefix but a different number: not the same item.
        let source = "### v1.0.0 - X\n<!-- status: in_progress -->\n1. [ ] Add the thing\n";
        let staging =
            "### v1.0.0 - X\n<!-- status: in_progress -->\n2. [x] Add the thing, extended\n";
        let m = merge_phase_from_staging(source, staging, "v1.0.0");
        assert!(m.newly_checked.is_empty(), "{m:?}");
        assert_eq!(m.unmatched_in_draft.len(), 1);
        // A longer word is not a note: it is a reword of item 1, carried as a rewrite.
        let staging = "### v1.0.0 - X\n<!-- status: in_progress -->\n1. [x] Add the things\n";
        let m = merge_phase_from_staging(source, staging, "v1.0.0");
        assert_eq!(m.rewritten.len(), 1, "{m:?}");
        assert!(m.unmatched_in_draft.is_empty(), "{m:?}");
    }

    #[test]
    fn exact_match_is_preferred_over_a_prefix_match() {
        let source = "### v1.0.0 - X\n<!-- status: in_progress -->\n1. [ ] Add\n2. [ ] Add\n";
        let staging = "### v1.0.0 - X\n<!-- status: in_progress -->\n1. [ ] Add\n2. [x] Add\n";
        let m = merge_phase_from_staging(source, staging, "v1.0.0");
        assert_eq!(m.newly_checked.len(), 1);
        assert!(m.content.contains("2. [x] Add"), "{}", m.content);
        assert!(m.content.contains("1. [ ] Add"), "{}", m.content);
    }
}
