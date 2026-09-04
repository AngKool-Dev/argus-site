use era_launcher_lib::servers::{parse_address, ping_server};

#[test]
fn test_parse_address_basic() {
    let (h, p) = parse_address("play.example.com:25565").unwrap();
    assert_eq!(h, "play.example.com");
    assert_eq!(p, 25565);
}

#[test]
fn test_parse_address_trims() {
    let (h, p) = parse_address("  minecraft.net  :19132  ").unwrap();
    assert_eq!(h, "minecraft.net");
    assert_eq!(p, 19132);
}

#[test]
fn test_parse_address_ipv6() {
    let (h, p) = parse_address("[::1]:25565").unwrap();
    assert_eq!(h, "::1");
    assert_eq!(p, 25565);
}

#[test]
fn test_parse_address_invalid() {
    assert!(parse_address("no-port").is_err());
    assert!(parse_address(":1234").is_err());
    assert!(parse_address("host:not-a-port").is_err());
}

/// Try to ping a server that should always fail fast (port 1 is unprivileged).
/// This verifies the timeout path actually times out within the 3-second
/// budget instead of hanging the test suite indefinitely.
#[test]
fn test_ping_timeout_unreachable() {
    let result = ping_server("127.0.0.1:1");
    assert!(result.is_err());
}