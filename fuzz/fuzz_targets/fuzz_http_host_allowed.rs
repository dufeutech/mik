//! Fuzz target for `is_http_host_allowed` - host header injection prevention.
//!
//! This fuzzer tests the host allowlist matching by:
//! 1. Testing various host header formats against patterns
//! 2. Verifying wildcard matching is secure
//! 3. Detecting potential host header injection attacks
//!
//! Key security properties tested:
//! - Empty patterns always deny
//! - Wildcard "*" only matches when explicitly specified
//! - Subdomain wildcards (*.example.com) don't match unrelated domains
//! - Case-insensitive matching per RFC 1035
//! - Injection attempts via special characters are handled
//!
//! Run with: `cargo +nightly fuzz run fuzz_http_host_allowed`

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mik::reliability::security::is_http_host_allowed;

/// Structured input for testing host allowlist matching.
#[derive(Arbitrary, Debug)]
struct HostAllowedInput {
    /// The hostname to check
    host: String,
    /// List of allowed patterns
    patterns: Vec<String>,
    /// Type of test scenario
    scenario: TestScenario,
}

/// Different test scenarios for comprehensive coverage.
#[derive(Arbitrary, Debug)]
enum TestScenario {
    /// Simple exact match testing
    ExactMatch,
    /// Wildcard subdomain testing (*.example.com)
    WildcardSubdomain,
    /// Global wildcard testing (*)
    GlobalWildcard,
    /// Test with adversarial patterns
    Adversarial(AdversarialPattern),
}

/// Adversarial patterns to test host header injection attacks.
#[derive(Arbitrary, Debug)]
enum AdversarialPattern {
    /// Empty host
    EmptyHost,
    /// Empty patterns list
    EmptyPatterns,
    /// Very long hostname
    VeryLongHost,
    /// Host with null byte injection
    NullByteInjection,
    /// Host with newline injection (HTTP header splitting)
    NewlineInjection,
    /// Host with port number
    HostWithPort,
    /// Host with username/password (user:pass@host)
    HostWithCredentials,
    /// Host with special characters
    SpecialCharacters,
    /// Host with IPv4 address
    IPv4Address,
    /// Host with IPv6 address
    IPv6Address,
    /// Host with trailing dot
    TrailingDot,
    /// Host with leading dot
    LeadingDot,
    /// Host with multiple dots
    MultipleDots,
    /// Unicode/IDN domain
    UnicodeDomain,
    /// Host with path component
    HostWithPath,
    /// Pattern injection via wildcard
    WildcardInjection,
    /// Case variation attack
    CaseVariation,
}

impl HostAllowedInput {
    /// Build the host and patterns based on the scenario.
    fn build(&self) -> (String, Vec<String>) {
        match &self.scenario {
            TestScenario::ExactMatch => (self.host.clone(), self.patterns.clone()),
            TestScenario::WildcardSubdomain => {
                let patterns = vec!["*.example.com".to_string()];
                (self.host.clone(), patterns)
            }
            TestScenario::GlobalWildcard => {
                let patterns = vec!["*".to_string()];
                (self.host.clone(), patterns)
            }
            TestScenario::Adversarial(pattern) => self.adversarial_input(pattern),
        }
    }

    /// Generate adversarial host/patterns based on pattern type.
    fn adversarial_input(&self, pattern: &AdversarialPattern) -> (String, Vec<String>) {
        match pattern {
            AdversarialPattern::EmptyHost => ("".to_string(), self.patterns.clone()),
            AdversarialPattern::EmptyPatterns => (self.host.clone(), vec![]),
            AdversarialPattern::VeryLongHost => {
                let long_host = "a".repeat(1000) + ".example.com";
                (long_host, self.patterns.clone())
            }
            AdversarialPattern::NullByteInjection => {
                // Null byte in host - potential C string termination attack
                let host = format!("evil.com\0.example.com");
                let patterns = vec!["*.example.com".to_string()];
                (host, patterns)
            }
            AdversarialPattern::NewlineInjection => {
                // HTTP header splitting attempt
                let host = "evil.com\r\nX-Injected: header".to_string();
                (host, self.patterns.clone())
            }
            AdversarialPattern::HostWithPort => {
                let host = format!("{}:8080", self.host);
                (host, self.patterns.clone())
            }
            AdversarialPattern::HostWithCredentials => {
                let host = format!("user:pass@{}", self.host);
                (host, self.patterns.clone())
            }
            AdversarialPattern::SpecialCharacters => {
                // Various special characters that might confuse parsing
                let host = format!("{}!@#$%^&*()[]", self.host);
                (host, self.patterns.clone())
            }
            AdversarialPattern::IPv4Address => {
                let host = "192.168.1.1".to_string();
                let patterns = vec!["*.168.1.1".to_string(), "192.168.1.1".to_string()];
                (host, patterns)
            }
            AdversarialPattern::IPv6Address => {
                let host = "[::1]".to_string();
                (host, self.patterns.clone())
            }
            AdversarialPattern::TrailingDot => {
                // DNS allows trailing dot (e.g., example.com.)
                let host = format!("{}.", self.host);
                (host, self.patterns.clone())
            }
            AdversarialPattern::LeadingDot => {
                // Invalid but should be handled
                let host = format!(".{}", self.host);
                (host, self.patterns.clone())
            }
            AdversarialPattern::MultipleDots => {
                let host = format!("...{}", self.host);
                (host, self.patterns.clone())
            }
            AdversarialPattern::UnicodeDomain => {
                // IDN domain - internationalized domain name
                let host = "\u{0430}\u{0440}\u{0440}\u{04cf}e.com".to_string(); // Cyrillic lookalikes
                let patterns = vec!["*.com".to_string(), "apple.com".to_string()];
                (host, patterns)
            }
            AdversarialPattern::HostWithPath => {
                // Host header with path (invalid but should be handled)
                let host = format!("{}/path/to/resource", self.host);
                (host, self.patterns.clone())
            }
            AdversarialPattern::WildcardInjection => {
                // Try to inject wildcards in the host
                let host = "*.example.com".to_string();
                let patterns = vec!["*.example.com".to_string()];
                (host, patterns)
            }
            AdversarialPattern::CaseVariation => {
                // Mixed case to test case-insensitive matching
                let host = "ApI.ExAmPlE.cOm".to_string();
                let patterns = vec!["api.example.com".to_string(), "*.EXAMPLE.COM".to_string()];
                (host, patterns)
            }
        }
    }
}

fuzz_target!(|data: HostAllowedInput| {
    let (host, patterns) = data.build();

    // The function must never panic
    let result = is_http_host_allowed(&host, &patterns);

    // Verify security invariants based on the result

    if result {
        // INVARIANT 1: If allowed, patterns must not be empty
        assert!(
            !patterns.is_empty(),
            "Host '{}' was allowed but patterns list is empty!",
            host
        );

        // INVARIANT 2: If allowed, host must match at least one pattern
        // We verify by checking the matching logic manually
        let host_lower = host.to_ascii_lowercase();
        let has_match = patterns.iter().any(|p| {
            if p == "*" {
                return true;
            }
            let pattern_lower = p.to_ascii_lowercase();
            if let Some(suffix) = pattern_lower.strip_prefix("*.") {
                let dot_suffix = format!(".{}", suffix);
                host_lower.ends_with(&dot_suffix) || host_lower == suffix
            } else {
                pattern_lower == host_lower
            }
        });
        assert!(
            has_match,
            "Host '{}' was allowed but doesn't match any pattern in {:?}",
            host, patterns
        );
    } else {
        // INVARIANT 3: If denied, either patterns is empty OR host doesn't match
        if !patterns.is_empty() {
            // Verify the host truly doesn't match
            let host_lower = host.to_ascii_lowercase();
            let has_match = patterns.iter().any(|p| {
                if p == "*" {
                    return true;
                }
                let pattern_lower = p.to_ascii_lowercase();
                if let Some(suffix) = pattern_lower.strip_prefix("*.") {
                    let dot_suffix = format!(".{}", suffix);
                    host_lower.ends_with(&dot_suffix) || host_lower == suffix
                } else {
                    pattern_lower == host_lower
                }
            });
            assert!(
                !has_match,
                "Host '{}' was denied but matches pattern in {:?}",
                host, patterns
            );
        }
    }

    // Additional security checks

    // INVARIANT 4: Empty patterns should always deny
    if patterns.is_empty() {
        assert!(
            !result,
            "Empty patterns list allowed host '{}' - this is a security issue!",
            host
        );
    }

    // INVARIANT 5: Global wildcard "*" should always allow
    if patterns.contains(&"*".to_string()) {
        assert!(
            result,
            "Global wildcard '*' in patterns but host '{}' was denied!",
            host
        );
    }

    // INVARIANT 6: Case-insensitive matching
    // If host matches a pattern, uppercase/lowercase variants should also match
    if result && !host.is_empty() {
        let upper_result = is_http_host_allowed(&host.to_uppercase(), &patterns);
        let lower_result = is_http_host_allowed(&host.to_lowercase(), &patterns);
        // Note: This may not always hold due to unicode normalization differences
        // between to_ascii_lowercase and to_uppercase/to_lowercase
        // So we only check for pure ASCII hosts
        if host.is_ascii() {
            assert!(
                upper_result == result && lower_result == result,
                "Case-insensitive matching inconsistency for host '{}': upper={}, lower={}, original={}",
                host, upper_result, lower_result, result
            );
        }
    }

    // INVARIANT 7: Wildcard pattern should not match if it's not actually a subdomain
    // E.g., "*.example.com" should NOT match "notexample.com"
    for pattern in &patterns {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            let suffix_lower = suffix.to_ascii_lowercase();
            let host_lower = host.to_ascii_lowercase();

            // If this specific pattern matched, verify it's a valid match
            let dot_suffix = format!(".{}", suffix_lower);
            let pattern_matches =
                host_lower.ends_with(&dot_suffix) || host_lower == suffix_lower;

            if result && pattern_matches {
                // Make sure we're not matching something like "notexample.com"
                // against "*.example.com"
                if host_lower.ends_with(&dot_suffix) {
                    // Valid subdomain match
                    assert!(
                        host_lower.len() > dot_suffix.len(),
                        "Empty subdomain matched for pattern '{}' against host '{}'",
                        pattern, host
                    );
                }
            }
        }
    }
});

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that the fuzzer's invariant checking logic is correct.
    #[test]
    fn test_basic_invariants() {
        // Empty patterns = denied
        assert!(!is_http_host_allowed("example.com", &[]));

        // Global wildcard = allowed
        assert!(is_http_host_allowed("anything.com", &["*".to_string()]));

        // Exact match
        assert!(is_http_host_allowed(
            "api.example.com",
            &["api.example.com".to_string()]
        ));
        assert!(!is_http_host_allowed(
            "other.example.com",
            &["api.example.com".to_string()]
        ));

        // Subdomain wildcard
        assert!(is_http_host_allowed(
            "api.example.com",
            &["*.example.com".to_string()]
        ));
        assert!(is_http_host_allowed(
            "example.com",
            &["*.example.com".to_string()]
        ));
        assert!(!is_http_host_allowed(
            "example.org",
            &["*.example.com".to_string()]
        ));
    }
}
