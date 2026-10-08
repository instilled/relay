//! What a step leaves for the steps after it, checked before it is kept: a value's key and size,
//! a file's name and size, the `key=value` lines a step appends to `$RELAY_OUT`, and the variable
//! a command reads a value from. The daemon keeps what passes; what does not is refused, saying why.

/// The most a value may be, in bytes.
pub const VALUE_MAX: usize = 4 << 10;
/// The most a file written through `rl flow state write` may be, in bytes.
pub const FILE_MAX: usize = 10 << 20;
/// The most a step's `$RELAY_OUT` may be, in bytes: it is read whole at the step's end.
pub const OUT_FILE_MAX: usize = 1 << 20;
/// What Relay itself says of a step, so no value may be called it.
pub const RESERVED: &[&str] = &["out", "choice", "note"];

/// A value's key: `[a-z][a-z0-9_]*`, and none of [`RESERVED`]. It is a placeholder's
/// (`{probe.signature}`) and part of a variable's name (`$RELAY_V_probe_signature`), and nothing
/// else fits both.
pub fn key(k: &str) -> Result<(), String> {
    let mut chars = k.chars();
    let word = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !word {
        return Err(format!("`{k}` is not a value's key: a-z, 0-9 and _, from a letter"));
    }
    if RESERVED.contains(&k) {
        return Err(format!("`{k}` is Relay's to say of a step, not a value's key"));
    }
    Ok(())
}

/// A value as it may be kept: its key good, and 4 KB at most.
pub fn value(k: &str, v: &str) -> Result<(), String> {
    key(k)?;
    if v.len() > VALUE_MAX {
        return Err(format!(
            "`{k}` is {} bytes; a value is 4 KB at most — write a file instead",
            v.len()
        ));
    }
    Ok(())
}

/// A file's name in a firing's folder: one name — no `/`, no `..`, not hidden (a name from a dot
/// is Relay's own), 255 bytes at most — and never a step's out (`probe.out` is what probe printed).
/// `steps` are the flow's step names.
pub fn file_name(name: &str, steps: &[&str]) -> Result<(), String> {
    let plain = !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && !name.contains(['/', '\0'])
        && !name.contains("..");
    if !plain {
        return Err(format!(
            "`{name}` is not a file's name here: one name, no / or .., not starting with a dot"
        ));
    }
    if let Some(step) = out_of(name, steps) {
        return Err(format!("`{name}` is what {step} printed, not a file to write"));
    }
    Ok(())
}

/// A file a step writes through `rl flow state write`: its name as [`file_name`] has it, and
/// 10 MB at most.
pub fn file(name: &str, len: usize, steps: &[&str]) -> Result<(), String> {
    file_name(name, steps)?;
    if len > FILE_MAX {
        return Err(format!("`{name}` is {len} bytes; a file is 10 MB at most"));
    }
    Ok(())
}

/// The step whose out `name` is: `probe.out` is probe's, when probe is one of `steps`.
pub fn out_of<'a>(name: &str, steps: &[&'a str]) -> Option<&'a str> {
    let step = name.strip_suffix(".out")?;
    steps.iter().copied().find(|s| *s == step)
}

/// The values in what a step appended to `$RELAY_OUT`: a `key=value` a line, the value all after
/// the first `=`, `\r\n` read as `\n`, blank lines skipped, a key said twice its last. Anything
/// else is refused, at its line: a step that meant to say something is not let go on without it.
pub fn out_lines(text: &str) -> Result<Vec<(String, String)>, String> {
    if text.len() > OUT_FILE_MAX {
        return Err(format!("$RELAY_OUT is {} bytes; 1 MB at most", text.len()));
    }
    let mut kept: Vec<(String, String)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("$RELAY_OUT line {}: `{line}` is not key=value", i + 1));
        };
        value(k, v).map_err(|e| format!("$RELAY_OUT line {}: {e}", i + 1))?;
        kept.retain(|(had, _)| had != k);
        kept.push((k.to_string(), v.to_string()));
    }
    Ok(kept)
}

/// The variable a command reads an earlier step's value from: `RELAY_V_<step>_<key>`, a `-` in
/// the step's name made `_` so a shell can name it (`new-errors` → `RELAY_V_new_errors_found`).
pub fn env_name(step: &str, key: &str) -> String {
    format!("RELAY_V_{}_{key}", step.replace('-', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_a_word_a_placeholder_and_a_variable_can_both_hold() {
        for k in ["found", "signature", "error_count", "a1"] {
            assert!(key(k).is_ok(), "{k}");
        }
        for k in ["", "Found", "1st", "_x", "a-b", "a.b", "a b", "ä"] {
            assert!(key(k).unwrap_err().contains("not a value's key"), "{k}");
        }
        for k in RESERVED {
            assert!(key(k).unwrap_err().contains("Relay's"), "{k}");
        }
    }

    #[test]
    fn a_value_is_four_kb_at_most() {
        assert!(value("found", &"x".repeat(VALUE_MAX)).is_ok());
        let e = value("found", &"x".repeat(VALUE_MAX + 1)).unwrap_err();
        assert!(e.contains("4 KB"), "{e}");
        assert!(value("Found", "3").is_err(), "the key is checked too");
    }

    #[test]
    fn a_file_is_one_plain_name_and_ten_mb_at_most() {
        let steps = ["probe", "triage"];
        for n in ["report.md", "errors.json", "x", "a.b.c"] {
            assert!(file_name(n, &steps).is_ok(), "{n}");
        }
        for n in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "/etc/passwd",
            ".relay",
            ".hidden",
            "a..b",
            "x\0y",
        ] {
            assert!(file_name(n, &steps).unwrap_err().contains("not a file's name"), "{n:?}");
        }
        assert!(file_name(&"a".repeat(256), &steps).is_err());
        let e = file_name("probe.out", &steps).unwrap_err();
        assert!(e.contains("what probe printed"), "{e}");
        assert!(
            file_name("other.out", &steps).is_ok(),
            "only a step's own name is its out"
        );
        assert!(file("report.md", FILE_MAX, &steps).is_ok());
        assert!(file("report.md", FILE_MAX + 1, &steps).unwrap_err().contains("10 MB"));
    }

    #[test]
    fn a_steps_out_is_named_for_it() {
        let steps = ["probe", "new-errors"];
        assert_eq!(out_of("probe.out", &steps), Some("probe"));
        assert_eq!(out_of("new-errors.out", &steps), Some("new-errors"));
        assert_eq!(out_of("probe.txt", &steps), None);
        assert_eq!(out_of("nope.out", &steps), None);
    }

    #[test]
    fn out_lines_read_as_a_shell_writes_them() {
        let got = out_lines("found=2\r\n\nsignature=a=b\nfound=3").unwrap();
        assert_eq!(
            got,
            [
                ("signature".to_string(), "a=b".to_string()),
                ("found".to_string(), "3".to_string())
            ],
            "\\r\\n and no final newline read alike; the value is all after the first =; the last said wins"
        );
        assert_eq!(out_lines("").unwrap(), []);
        assert_eq!(out_lines("empty=\n").unwrap(), [("empty".to_string(), String::new())]);
        let e = out_lines("found=3\nnot a value\n").unwrap_err();
        assert!(e.contains("line 2") && e.contains("key=value"), "{e}");
        let e = out_lines("Found=3\n").unwrap_err();
        assert!(e.contains("line 1") && e.contains("not a value's key"), "{e}");
        let e = out_lines(&format!("big={}\n", "x".repeat(VALUE_MAX + 1))).unwrap_err();
        assert!(e.contains("4 KB"), "{e}");
        let e = out_lines(&"x".repeat(OUT_FILE_MAX + 1)).unwrap_err();
        assert!(e.contains("1 MB"), "{e}");
    }

    #[test]
    fn a_variable_names_its_step_with_underscores() {
        assert_eq!(env_name("probe", "signature"), "RELAY_V_probe_signature");
        assert_eq!(env_name("new-errors", "found"), "RELAY_V_new_errors_found");
    }
}
