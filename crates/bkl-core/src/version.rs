//! Bazel version parsing and the supported minimum.

/// The first release with `--inject_repository`.
pub const MIN_BAZEL: BazelVersion = BazelVersion {
    major: 8,
    minor: 0,
    patch: 0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BazelVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for BazelVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Parses `bazel --version` output such as `bazel 8.0.0` or `bazel 9.1.0rc2`. `None` for development builds.
pub fn parse_bazel_version(out: &str) -> Option<BazelVersion> {
    let version = out.split_whitespace().nth(1)?;
    let mut parts = version.splitn(3, '.');
    let number = |s: &str| -> Option<u32> {
        let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    };
    Some(BazelVersion {
        major: number(parts.next()?)?,
        minor: number(parts.next()?)?,
        patch: number(parts.next()?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(
            parse_bazel_version("bazel 8.0.0\n"),
            Some(BazelVersion {
                major: 8,
                minor: 0,
                patch: 0
            })
        );
        assert_eq!(
            parse_bazel_version("bazel 9.1.0rc2"),
            Some(BazelVersion {
                major: 9,
                minor: 1,
                patch: 0
            })
        );
        assert_eq!(parse_bazel_version("bazel no_version"), None);
        assert!(parse_bazel_version("bazel 7.6.0").unwrap() < MIN_BAZEL);
        assert!(parse_bazel_version("bazel 8.0.0").unwrap() >= MIN_BAZEL);
    }
}
