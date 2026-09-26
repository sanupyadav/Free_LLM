//! Roster injection: wraps the skill catalog (name + description) into a system prefix snippet.

/// Wraps the roster in `[freebuff-skills]` tags; returns an empty string when prefix is empty (caller can concatenate directly).
pub fn roster_block(prefix: &str) -> String {
    if prefix.trim().is_empty() {
        return String::new();
    }
    format!("\n\n[freebuff-skills]\n{prefix}\n[/freebuff-skills]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_prefix_yields_empty_block() {
        assert_eq!(roster_block(""), "");
        assert_eq!(roster_block("   \n"), "");
    }

    #[test]
    fn wraps_non_empty_prefix() {
        let block = roster_block("### Git 专家\n处理分支与 rebase");
        assert!(block.starts_with("\n\n[freebuff-skills]\n"));
        assert!(block.ends_with("\n[/freebuff-skills]"));
        assert!(block.contains("### Git 专家"));
    }
}
