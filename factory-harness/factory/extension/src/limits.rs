pub(crate) const MAX_WORK_UNITS: usize = 32;
pub(crate) const MAX_FINDINGS: usize = 32;
pub(crate) const MAX_DEPENDENCIES: usize = 32;
pub(crate) const MAX_IDENTIFIER_CHARS: usize = 128;
pub(crate) const MAX_TITLE_CHARS: usize = 512;
pub(crate) const MAX_DETAIL_CHARS: usize = 4_096;
pub(crate) const MAX_SUMMARY_CHARS: usize = 4_096;
pub(crate) const MAX_CONTEXT_CHARS: usize = 32_768;
pub(crate) const MAX_CONTEXT_FIELD_CHARS: usize = 2_000;

pub(crate) fn require_bounded_text(
    field: &str,
    value: &str,
    max_chars: usize,
) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    let actual = value.chars().count();
    if actual > max_chars {
        return Err(format!(
            "{field} must contain at most {max_chars} characters, found {actual}"
        ));
    }
    Ok(())
}

pub(crate) fn truncate_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_none() {
        return prefix;
    }
    let suffix = "...";
    let retained = limit.saturating_sub(suffix.len());
    let mut truncated = value.chars().take(retained).collect::<String>();
    truncated.push_str(&suffix[..suffix.len().min(limit)]);
    truncated
}
