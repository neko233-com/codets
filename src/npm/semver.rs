//! The semver subset a package installer needs: versions, ordering, and ranges.
//!
//! Ranges are evaluated the way npm evaluates them: a range is a union (`||`) of
//! comparators, and a comparator set is an intersection of space-separated
//! constraints with `^`, `~`, `x`, and the plain operators. Without this,
//! `"^4.17.0"` cannot be turned into a concrete version to download.

use std::cmp::Ordering;
use std::fmt;

/// A published version, with the pre-release part kept as text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Semver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// `1.0.0-alpha.2` keeps `alpha.2` here; empty for a release.
    pub pre: String,
}

impl Semver {
    pub fn new(major: u64, minor: u64, patch: u64) -> Semver {
        Semver {
            major,
            minor,
            patch,
            pre: String::new(),
        }
    }

    /// Parse `1.2.3`, `v1.2.3` or `1.2.3-beta.1`.
    pub fn parse(text: &str) -> Option<Semver> {
        let trimmed = text.trim();
        let digits = trimmed.strip_prefix('v').unwrap_or(trimmed);
        let (core, pre) = match digits.split_once('-') {
            Some((core, pre)) => (core, pre.split('+').next().unwrap_or("").to_string()),
            None => (digits.split('+').next().unwrap_or(digits), String::new()),
        };
        let mut parts = core.split('.');
        let major = parts.next()?.parse::<u64>().ok()?;
        let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
        let patch = match parts.next() {
            Some(patch) => patch.parse::<u64>().ok()?,
            None => 0,
        };
        Some(Semver {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// True when this is a pre-release, which a range must ask for explicitly.
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    fn core(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

impl fmt::Display for Semver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

impl Ord for Semver {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.core().cmp(&other.core()) {
            Ordering::Equal => match (self.pre.is_empty(), other.pre.is_empty()) {
                // A release outranks its pre-releases.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                _ => compare_prerelease(&self.pre, &other.pre),
            },
            other => other,
        }
    }
}

impl PartialOrd for Semver {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_prerelease(left: &str, right: &str) -> Ordering {
    let left_parts: Vec<&str> = left.split('.').collect();
    let right_parts: Vec<&str> = right.split('.').collect();
    for (index, left) in left_parts.iter().enumerate() {
        let Some(right) = right_parts.get(index) else {
            return Ordering::Greater;
        };
        let ordering = match (left.parse::<u64>(), right.parse::<u64>()) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => left.cmp(right),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    match left_parts.len().cmp(&right_parts.len()) {
        Ordering::Greater => Ordering::Less,
        Ordering::Less => Ordering::Greater,
        Ordering::Equal => Ordering::Equal,
    }
}

/// One constraint, such as `>=1.2.0` or `^4.17.0`.
#[derive(Clone, Debug, PartialEq)]
enum Comparator {
    Any,
    Exact(Semver),
    /// `>=`, `>`, `<=`, `<` against a version.
    Bound(Op, Semver),
    /// `^1.2.3`: compatible within the leftmost non-zero component.
    Caret(Semver),
    /// `~1.2.3`: patch-level changes allowed.
    Tilde(Semver),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Gte,
    Gt,
    Lte,
    Lt,
}

/// A parsed range: a union of comparator sets.
#[derive(Clone, Debug, PartialEq)]
pub struct Range {
    alternatives: Vec<Vec<Comparator>>,
}

impl Range {
    /// Parse a range such as `^1.2.3`, `>=1 <2`, `1.x` or `1 || 2`.
    pub fn parse(text: &str) -> Option<Range> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Some(Range {
                alternatives: vec![vec![Comparator::Any]],
            });
        }
        // npm treats `*`, `latest` and a git/file URL as "any" for our purposes.
        if matches!(trimmed, "*" | "x" | "latest") || trimmed.contains('/') || trimmed.contains(':') {
            return Some(Range {
                alternatives: vec![vec![Comparator::Any]],
            });
        }
        let mut alternatives = Vec::new();
        for alternative in trimmed.split("||") {
            let mut set = Vec::new();
            for token in alternative.split_whitespace() {
                set.push(parse_comparator(token)?);
            }
            if set.is_empty() {
                set.push(Comparator::Any);
            }
            alternatives.push(set);
        }
        Some(Range { alternatives })
    }

    /// True when `version` satisfies the range.
    ///
    /// A pre-release only satisfies a range whose bound mentions a pre-release,
    /// which is the rule npm uses to keep `^1.0.0` from selecting `2.0.0-beta`.
    pub fn matches(&self, version: &Semver) -> bool {
        for set in &self.alternatives {
            let mentions_prerelease = set.iter().any(|comparator| match comparator {
                Comparator::Exact(v) | Comparator::Caret(v) | Comparator::Tilde(v) => v.is_prerelease(),
                Comparator::Bound(_, v) => v.is_prerelease(),
                Comparator::Any => false,
            });
            if version.is_prerelease() && !mentions_prerelease {
                continue;
            }
            if set.iter().all(|comparator| satisfies(comparator, version)) {
                return true;
            }
        }
        false
    }

    /// The highest version in `candidates` that satisfies the range.
    pub fn best<'a>(&self, candidates: impl IntoIterator<Item = &'a Semver>) -> Option<Semver> {
        candidates
            .into_iter()
            .filter(|version| self.matches(version))
            .max()
            .cloned()
    }
}

fn satisfies(comparator: &Comparator, version: &Semver) -> bool {
    match comparator {
        Comparator::Any => true,
        Comparator::Exact(target) => version == target,
        Comparator::Bound(op, target) => match op {
            Op::Gte => version >= target,
            Op::Gt => version > target,
            Op::Lte => version <= target,
            Op::Lt => version < target,
        },
        Comparator::Caret(target) => {
            if version < target {
                return false;
            }
            let upper = if target.major > 0 {
                Semver::new(target.major + 1, 0, 0)
            } else if target.minor > 0 {
                Semver::new(0, target.minor + 1, 0)
            } else {
                Semver::new(0, 0, target.patch + 1)
            };
            version < &upper
        }
        Comparator::Tilde(target) => {
            if version < target {
                return false;
            }
            let upper = if target.major > 0 || target.minor > 0 {
                Semver::new(target.major, target.minor + 1, 0)
            } else {
                Semver::new(0, 0, target.patch + 1)
            };
            version < &upper
        }
    }
}

fn parse_comparator(token: &str) -> Option<Comparator> {
    let token = token.trim();
    if token.is_empty() || matches!(token, "*" | "x" | "X") {
        return Some(Comparator::Any);
    }
    if let Some(rest) = token.strip_prefix('^') {
        return Some(Comparator::Caret(parse_partial(rest)?));
    }
    if let Some(rest) = token.strip_prefix('~') {
        return Some(Comparator::Tilde(parse_partial(rest)?));
    }
    for (prefix, op) in [
        (">=", Op::Gte),
        ("<=", Op::Lte),
        (">", Op::Gt),
        ("<", Op::Lt),
    ] {
        if let Some(rest) = token.strip_prefix(prefix) {
            return Some(Comparator::Bound(op, parse_partial(rest)?));
        }
    }
    if let Some(rest) = token.strip_prefix('=') {
        return Some(Comparator::Exact(parse_partial(rest)?));
    }
    // A part with an `x` is a wildcard range, which npm reads as a caret-ish
    // lower bound on the specified components.
    if token.split(['.', '-']).any(|part| matches!(part, "x" | "X" | "*")) {
        let mut parts = token.split('.');
        let major: u64 = parts.next().unwrap_or("0").parse().ok()?;
        let minor_text = parts.next().unwrap_or("0");
        if matches!(minor_text, "x" | "X" | "*") {
            return Some(Comparator::Caret(Semver::new(major, 0, 0)));
        }
        let minor: u64 = minor_text.parse().ok()?;
        let patch_text = parts.next().unwrap_or("0");
        if matches!(patch_text, "x" | "X" | "*") {
            return Some(Comparator::Caret(Semver::new(major, minor, 0)));
        }
        return Some(Comparator::Exact(Semver::new(
            major,
            minor,
            patch_text.parse().ok()?,
        )));
    }
    Some(Comparator::Exact(parse_partial(token)?))
}

/// Parse `1`, `1.2`, `1.2.3` — npm fills the missing parts with zeros.
fn parse_partial(text: &str) -> Option<Semver> {
    let trimmed = text.trim().trim_start_matches('v');
    if trimmed.is_empty() {
        return Some(Semver::new(0, 0, 0));
    }
    let cleaned = trimmed.trim_end_matches('.').to_string();
    if let Some(parsed) = Semver::parse(&cleaned) {
        return Some(parsed);
    }
    let mut parts = cleaned.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().unwrap_or(0);
    Some(Semver::new(major, minor, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Semver {
        Semver::parse(text).expect("parse")
    }

    #[test]
    fn orders_versions_and_prereleases() {
        assert!(version("1.2.3") < version("1.2.4"));
        assert!(version("1.2.3") < version("1.3.0"));
        assert!(version("1.10.0") > version("1.9.9"));
        assert!(version("2.0.0") > version("1.99.99"));
        // A pre-release sorts below its release.
        assert!(version("1.0.0-alpha") < version("1.0.0"));
        assert!(version("1.0.0-alpha.1") < version("1.0.0-alpha.2"));
        assert!(version("1.0.0-alpha.2") < version("1.0.0-beta"));
    }

    #[test]
    fn caret_stays_inside_the_major() {
        let range = Range::parse("^1.2.3").expect("range");
        assert!(range.matches(&version("1.2.3")));
        assert!(range.matches(&version("1.9.0")));
        assert!(!range.matches(&version("2.0.0")));
        assert!(!range.matches(&version("1.2.2")));
        // `^0.x` is special: only the patch may move.
        let zero = Range::parse("^0.2.3").expect("range");
        assert!(zero.matches(&version("0.2.9")));
        assert!(!zero.matches(&version("0.3.0")));
    }

    #[test]
    fn tilde_stays_inside_the_minor() {
        let range = Range::parse("~1.2.3").expect("range");
        assert!(range.matches(&version("1.2.9")));
        assert!(!range.matches(&version("1.3.0")));
    }

    #[test]
    fn picks_the_highest_match() {
        let range = Range::parse("^4.0.0").expect("range");
        let published = [
            version("4.17.20"),
            version("4.17.21"),
            version("5.0.0"),
            version("3.10.1"),
        ];
        assert_eq!(range.best(published.iter()), Some(version("4.17.21")));
    }

    #[test]
    fn ranges_that_are_not_versions_are_any() {
        for text in ["*", "latest", "file:../local", "git+https://example.com/x.git"] {
            let range = Range::parse(text).expect("range");
            assert!(range.matches(&version("9.9.9")), "{text}");
        }
    }

    #[test]
    fn unions_and_intersections_work() {
        let union = Range::parse("^1.0.0 || ^3.0.0").expect("range");
        assert!(union.matches(&version("1.5.0")));
        assert!(union.matches(&version("3.2.1")));
        assert!(!union.matches(&version("2.0.0")));

        let intersection = Range::parse(">=1.2.0 <2.0.0").expect("range");
        assert!(intersection.matches(&version("1.9.9")));
        assert!(!intersection.matches(&version("2.0.0")));
    }
}
