//! Crash diagnostics — turn JVM `hs_err_pid*.log` text into actionable hints.
//!
//! The `diagnose` function runs a few regex-shaped substring matches against
//! the raw crash text and returns the hints in order of likelihood. Hints are
//! rendered in the CRASHES widget alongside each report.

use crate::argus::state::DiagnosticHint;

/// Run the diagnostic rule set against `text` and return every matched
/// hint. Multiple hints can fire on the same report (e.g. a missing mod
/// that ALSO triggers a NoClassDefFoundError).
pub fn diagnose(text: &str) -> Vec<DiagnosticHint> {
    let mut out = Vec::new();

    if let Some(cls) = first_match(text, "java.lang.NoClassDefFoundError: ") {
        out.push(DiagnosticHint::MissingMod(cls));
    }
    if let Some(cls) = first_match(text, "java.lang.ClassNotFoundException: ") {
        out.push(DiagnosticHint::NoClassDefFound(cls));
    }
    if text.contains("Could not init class net.fabricmc.loader.impl.launch.knot.KnotClient ")
        || text.contains("net.fabricmc.loader.impl.launch.knot.KnotClient")
    {
        out.push(DiagnosticHint::ModVersionMismatch {
            mod_id: "fabric loader".to_string(),
            expected: "loader compatible".to_string(),
            got: "mismatched".to_string(),
        });
    }
    if text.contains("OutOfMemoryError")
        || text.contains("GC overhead limit exceeded")
        || text.contains("Java heap space")
    {
        out.push(DiagnosticHint::InsufficientMemory);
    }
    if let Some(ver) = first_match(text, "Unsupported class file major version ") {
        out.push(DiagnosticHint::JavaVersion(ver));
    }

    if out.is_empty() {
        out.push(DiagnosticHint::Unknown);
    }
    out
}

/// Helper: find the first quoted substring after `needle`, stopping at the
/// next whitespace, newline, or comma.
fn first_match(text: &str, needle: &str) -> Option<String> {
    let idx = text.find(needle)?;
    let rest = &text[idx + needle.len()..];
    let end = rest
        .find(|c: char| {
            c.is_whitespace() || c == ',' || c == '\n' || c == '\r' || c == '(' || c == ')'
        })
        .unwrap_or(rest.len());
    let s = rest[..end].trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnose_no_class_def() {
        let text = "java.lang.NoClassDefFoundError: com/somebody/mod/Client";
        let hints = diagnose(text);
        assert!(matches!(&hints[0], DiagnosticHint::MissingMod(s) if s == "com/somebody/mod/Client"));
    }

    #[test]
    fn test_diagnose_out_of_memory() {
        let text = "java.lang.OutOfMemoryError: Java heap space";
        let hints = diagnose(text);
        assert!(hints
            .iter()
            .any(|h| matches!(h, DiagnosticHint::InsufficientMemory)));
    }

    #[test]
    fn test_diagnose_java_version_mismatch() {
        let text = "Unsupported class file major version 65";
        let hints = diagnose(text);
        assert!(matches!(&hints[0], DiagnosticHint::JavaVersion(s) if s == "65"));
    }

    #[test]
    fn test_diagnose_mod_version_mismatch() {
        let text = "Could not init class net.fabricmc.loader.impl.launch.knot.KnotClient";
        let hints = diagnose(text);
        assert!(hints
            .iter()
            .any(|h| matches!(h, DiagnosticHint::ModVersionMismatch { .. })));
    }

    #[test]
    fn test_diagnose_unknown_falls_back() {
        let hints = diagnose("nothing recognizable here");
        assert!(matches!(hints[0], DiagnosticHint::Unknown));
    }
}