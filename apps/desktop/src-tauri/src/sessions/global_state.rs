pub(super) fn normalize_workspace_path(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with(r"\\?\unc\") {
        return Some(format!(r"\\{}", trimmed[8..].replace('/', r"\")));
    }
    if let Some(stripped) = trimmed.strip_prefix(r"\\?\") {
        return Some(stripped.replace('\\', "/"));
    }
    Some(trimmed.to_string())
}
