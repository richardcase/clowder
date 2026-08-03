//! Curated matcher: does a rendered line look like a program blocking on interactive input?
//! Conservative — unknown prompts simply don't match (no false alarm), and bare shell prompts
//! are deliberately excluded so an idle shell never reads as "needs input".

/// True when `line` (a rendered screen line) looks like a blocking interactive prompt.
pub fn is_blocking_prompt(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();

    // yes/no prompts, optionally followed by ? or :
    const YN: &[&str] = &["(y/n)", "(yes/no)", "[y/n]", "[yes/no]"];
    if YN.iter().any(|p| {
        lower.ends_with(p)
            || lower.ends_with(&format!("{p}?"))
            || lower.ends_with(&format!("{p}:"))
    }) {
        return true;
    }

    // password / passphrase
    if lower.ends_with("password:")
        || lower.ends_with("passphrase:")
        || (lower.contains("password for") && lower.ends_with(':'))
        || (lower.contains("passphrase for") && lower.ends_with(':'))
    {
        return true;
    }

    // press <key> to continue
    if lower.contains("press enter") || lower.contains("press return") || lower.contains("press any key") {
        return true;
    }

    // pagers
    if t.contains("--More--") || lower.ends_with("(end)") {
        return true;
    }

    // inquirer-style question
    if t.starts_with("? ") {
        return true;
    }

    // REPLs
    if lower.ends_with(">>>") {
        return true;
    }
    if lower.starts_with("in [") && lower.ends_with("]:") {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_interactive_prompts() {
        for line in [
            "Continue? (y/n)",
            "Overwrite? [Y/n] ",
            "Proceed (yes/no)?",
            "Delete everything? [y/N]:",
            "Password:",
            "Enter passphrase for key '/x':",
            "[sudo] password for alice:",
            "Press ENTER to continue",
            "Press any key to continue . . .",
            "--More--",
            "lines 1-10 (END)",
            "? Select an option",
            ">>> ",
            "In [12]:",
        ] {
            assert!(is_blocking_prompt(line), "should match: {line:?}");
        }
    }

    #[test]
    fn rejects_shell_prompts_and_text() {
        for line in [
            "$ ",
            "user@host:~/proj$ ",
            "% ",
            "# ",
            "❯ ",
            "> ",
            "building project...",
            "error: something failed",
            "",
        ] {
            assert!(!is_blocking_prompt(line), "should NOT match: {line:?}");
        }
    }
}
