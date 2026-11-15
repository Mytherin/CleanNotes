//! Merging a note that was changed on two devices since they last synced.
//!
//! Notes are Markdown with one paragraph per line, so a line-based three-way
//! merge combines edits to different paragraphs. Edits to the same paragraph
//! can't be combined; then the caller keeps both versions.

/// Three-way merge of note text. `Err` if both sides changed the same lines.
pub fn merge_text(base: &str, ours: &str, theirs: &str) -> Result<String, ()> {
    if ours == theirs || theirs == base {
        return Ok(ours.to_owned());
    }
    if ours == base {
        return Ok(theirs.to_owned());
    }
    // A locked note's text is sealed; merging lines around the seal would
    // leave the other side's edit outside it, in the clear. Both are kept.
    if [base, ours, theirs].iter().any(|s| crate::lock::is_locked(s)) {
        return Err(());
    }
    // diffy works on lines; make sure every line ends in a newline so an edit
    // to the last line isn't seen as a change to the line ending as well.
    let norm = |s: &str| if s.is_empty() || s.ends_with('\n') { s.to_owned() } else { format!("{s}\n") };
    let (b, o, t) = (norm(base), norm(ours), norm(theirs));
    let merged = diffy::merge(&b, &o, &t).map_err(|_| ())?;
    // Keep the trailing newline the way ours had it.
    if !ours.ends_with('\n') && merged.ends_with('\n') && !theirs.ends_with('\n') {
        return Ok(merged.trim_end_matches('\n').to_owned());
    }
    Ok(merged)
}

/// Three-way merge of a single value: whichever side changed it wins; if
/// both did, ours.
pub fn merge_value<T: PartialEq + Clone>(base: &T, ours: &T, theirs: &T) -> T {
    if ours == base {
        theirs.clone()
    } else {
        ours.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_to_different_paragraphs_combine() {
        let base = "# List\neggs\nmilk\nbread\n";
        let ours = "# List\neggs (6)\nmilk\nbread\n";
        let theirs = "# List\neggs\nmilk\nbread\nbutter\n";
        assert_eq!(merge_text(base, ours, theirs), Ok("# List\neggs (6)\nmilk\nbread\nbutter\n".into()));
    }

    #[test]
    fn edits_to_the_same_paragraph_clash() {
        assert_eq!(merge_text("a\nb\n", "a\nB1\n", "a\nB2\n"), Err(()));
    }

    #[test]
    fn one_sided_and_identical_changes() {
        assert_eq!(merge_text("a", "a", "b"), Ok("b".into()));
        assert_eq!(merge_text("a", "b", "a"), Ok("b".into()));
        assert_eq!(merge_text("a", "c", "c"), Ok("c".into()));
    }

    #[test]
    fn last_lines_without_newline() {
        assert_eq!(merge_text("x\ny", "X\ny", "x\ny\nz"), Ok("X\ny\nz".into()));
    }

    #[test]
    fn values() {
        assert!(merge_value(&false, &true, &false));
        assert!(merge_value(&false, &false, &true));
        assert_eq!(merge_value(&1, &2, &3), 2);
    }
}
