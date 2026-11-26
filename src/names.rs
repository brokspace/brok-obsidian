//! Names that come from the server decide where files are written, so every
//! one is checked before it touches the disk.

/// Longest single path part, in bytes.
const MAX_PART_BYTES: usize = 200;

/// A listing entry as a safe single name and whether it is a folder (it ends
/// with `/`). `None` for anything that could leave the mirror, hide, or be
/// refused by a file system: `..`, separators, drive letters, control
/// characters, hidden names, reserved Windows names, trailing dots or spaces.
pub fn safe_entry(entry: &str) -> Option<(&str, bool)> {
    let (name, folder) = match entry.strip_suffix('/') {
        Some(name) => (name, true),
        None => (entry, false),
    };
    if name.is_empty()
        || name.len() > MAX_PART_BYTES
        || name.starts_with('.')
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| c.is_control() || r#"/\:*?"<>|"#.contains(c))
    {
        return None;
    }
    let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let numbered = stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.ends_with(|c: char| c.is_ascii_digit());
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered {
        return None;
    }
    Some((name, folder))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_pass_including_spaces_and_unicode() {
        assert_eq!(safe_entry("Reunião de produto.md"), Some(("Reunião de produto.md", false)));
        assert_eq!(safe_entry("Projetos 2026/"), Some(("Projetos 2026", true)));
    }

    #[test]
    fn hostile_and_unportable_names_are_refused() {
        for bad in [
            "",
            "/",
            "..",
            "../",
            ".",
            ".obsidian/",
            ".env",
            "a/b.md",
            "/etc/passwd",
            "a\\b.md",
            "C:x.md",
            "x\0.md",
            "x\n.md",
            "CON",
            "nul.md",
            "com1.md",
            "name. ",
            "name.",
            "what?.md",
        ] {
            assert_eq!(safe_entry(bad), None, "{bad:?}");
        }
        assert_eq!(safe_entry(&"a".repeat(300)), None);
    }
}
