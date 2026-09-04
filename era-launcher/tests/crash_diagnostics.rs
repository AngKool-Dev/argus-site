use era_launcher_lib::argus::state::DiagnosticHint;
use era_launcher_lib::crashes::diagnose;

#[test]
fn test_no_class_def_fired() {
    let text = "java.lang.NoClassDefFoundError: com/somebody/mod/Client\n";
    let hints = diagnose(text);
    assert!(hints
        .iter()
        .any(|h| matches!(h, DiagnosticHint::MissingMod(s) if s == "com/somebody/mod/Client")));
}

#[test]
fn test_out_of_memory_fired() {
    let text = "java.lang.OutOfMemoryError: Java heap space\n";
    let hints = diagnose(text);
    assert!(hints
        .iter()
        .any(|h| matches!(h, DiagnosticHint::InsufficientMemory)));
}

#[test]
fn test_class_file_major_version_fired() {
    let text = "Unsupported class file major version 65\n";
    let hints = diagnose(text);
    assert!(hints
        .iter()
        .any(|h| matches!(h, DiagnosticHint::JavaVersion(s) if s == "65")));
}

#[test]
fn test_unknown_falls_back() {
    let hints = diagnose("nothing recognizable here");
    assert!(matches!(hints[0], DiagnosticHint::Unknown));
}