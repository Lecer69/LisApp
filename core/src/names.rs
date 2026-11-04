//! File names from the other device are never trusted as paths.

const MAX_NAME_BYTES: usize = 200;

/// A plain file name: no directories, hidden names or control characters.
pub fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim().trim_start_matches('.').trim();

    let mut end = clean.len().min(MAX_NAME_BYTES);
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    let clean = clean[..end].trim();

    if clean.is_empty() { "file".to_string() } else { clean.to_string() }
}

#[cfg(test)]
mod tests {
    use super::safe_file_name;

    #[test]
    fn strips_paths_and_hidden_names() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("..\\..\\windows\\x.txt"), "x.txt");
        assert_eq!(safe_file_name("/abs/path/photo.jpg"), "photo.jpg");
        assert_eq!(safe_file_name(".bashrc"), "bashrc");
        assert_eq!(safe_file_name(".."), "file");
        assert_eq!(safe_file_name(""), "file");
        assert_eq!(safe_file_name("a/"), "file");
        assert_eq!(safe_file_name("na\u{0}me\n.txt"), "name.txt");
        assert_eq!(safe_file_name("سلام.txt"), "سلام.txt");
    }

    #[test]
    fn limits_length_on_a_character_boundary() {
        let name = safe_file_name(&"س".repeat(300));
        assert!(name.len() <= 200);
        assert!(name.chars().all(|c| c == 'س'));
    }
}
