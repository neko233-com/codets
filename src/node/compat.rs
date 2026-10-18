//! Node.js release compatibility.
//!
//! `codets` speaks the JavaScript of one specific Node.js release. That release
//! is selected with `--node-version` (or by the version manager's `current`
//! pointer) and is what `process.version` reports, so a package that gates on
//! `process.versions.node` takes the same branch it would on real Node.
//!
//! Two things are version dependent and therefore live here:
//!
//! * **Language/library features** — [`Feature`] enumerates the standard-library
//!   additions whose availability moved between the Node 18, 20, 22 and 24 lines.
//!   A feature is installed into the realm only when [`NodeCompat::supports`]
//!   says the selected release has it, which is what makes `Object.groupBy` exist
//!   on 22+ and be absent on 18.
//! * **Reported versions** — `process.version`, `process.versions.node` and the
//!   LTS codename.
//!
//! [`Feature`] lists only features `codets` actually implements; advertising a
//! feature in `codets features` that the realm does not install would be a lie.

use std::fmt;

/// The Node.js release line `codets` defaults to.
pub const DEFAULT_MAJOR: u32 = 24;

/// A Node.js semantic version, to the precision compatibility cares about.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Version {
        Version { major, minor, patch }
    }

    /// Parse `"24"`, `"24.0"`, `"24.0.0"` or `"v22.11.0"`.
    pub fn parse(text: &str) -> Option<Version> {
        let trimmed = text.trim();
        let digits = trimmed.strip_prefix('v').unwrap_or(trimmed);
        let mut parts = digits.split('.');
        let major = parts.next()?.parse::<u32>().ok()?;
        let minor = match parts.next() {
            Some(part) => part.parse::<u32>().ok()?,
            None => 0,
        };
        let patch = match parts.next() {
            Some(part) => part.parse::<u32>().ok()?,
            None => 0,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Version::new(major, minor, patch))
    }

    /// The first version of a major line.
    pub fn from_major(major: u32) -> Version {
        Version::new(major, 0, 0)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A standard-library feature whose availability depends on the Node.js release.
///
/// `minimum_major` is the Node major that shipped the feature unflagged, which is
/// the number a compatibility check has to use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feature {
    /// `Object.hasOwn` — Node 16.9.
    ObjectHasOwn,
    /// `structuredClone` — Node 17.0.
    StructuredClone,
    /// `Array.prototype.toSorted` / `toReversed` / `toSpliced` / `with`, plus
    /// `String.prototype.isWellFormed` / `toWellFormed` — Node 20.0.
    ChangeByCopy,
    /// `Object.groupBy` — Node 21.0.
    ObjectGroupBy,
    /// `Promise.withResolvers` — Node 22.0.
    PromiseWithResolvers,
    /// Set methods: `union`, `intersection`, `difference`,
    /// `symmetricDifference`, `isSubsetOf`, `isSupersetOf`, `isDisjointFrom` —
    /// Node 22.0.
    SetMethods,
    /// `Error.isError` — Node 24.0.
    ErrorIsError,
    /// `RegExp.escape` — Node 24.0.
    RegExpEscape,
    /// `Math.sumPrecise` — Node 24.0.
    MathSumPrecise,
}

impl Feature {
    /// Every feature, oldest first.
    pub const ALL: [Feature; 9] = [
        Feature::ObjectHasOwn,
        Feature::StructuredClone,
        Feature::ChangeByCopy,
        Feature::ObjectGroupBy,
        Feature::PromiseWithResolvers,
        Feature::SetMethods,
        Feature::ErrorIsError,
        Feature::RegExpEscape,
        Feature::MathSumPrecise,
    ];

    /// The Node major that shipped this feature unflagged.
    pub fn minimum_major(self) -> u32 {
        match self {
            Feature::ObjectHasOwn => 16,
            Feature::StructuredClone => 17,
            Feature::ChangeByCopy => 20,
            Feature::ObjectGroupBy => 21,
            Feature::PromiseWithResolvers => 22,
            Feature::SetMethods => 22,
            Feature::ErrorIsError => 24,
            Feature::RegExpEscape => 24,
            Feature::MathSumPrecise => 24,
        }
    }

    /// The spelling used in `codets features` output and the docs.
    pub fn name(self) -> &'static str {
        match self {
            Feature::ObjectHasOwn => "Object.hasOwn",
            Feature::StructuredClone => "structuredClone",
            Feature::ChangeByCopy => "Array.prototype.toSorted/toReversed/toSpliced/with",
            Feature::ObjectGroupBy => "Object.groupBy",
            Feature::PromiseWithResolvers => "Promise.withResolvers",
            Feature::SetMethods => "Set.prototype.union and friends",
            Feature::ErrorIsError => "Error.isError",
            Feature::RegExpEscape => "RegExp.escape",
            Feature::MathSumPrecise => "Math.sumPrecise",
        }
    }
}

/// The Node.js release the realm claims to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NodeCompat {
    version: Version,
}

impl Default for NodeCompat {
    fn default() -> Self {
        NodeCompat::for_major(DEFAULT_MAJOR)
    }
}

impl NodeCompat {
    /// The compat level for a Node major, with the patch level `codets` targets.
    ///
    /// The patch numbers are a `codets` baseline for each supported line; they are
    /// not a claim about upstream's newest patch release.
    pub fn for_major(major: u32) -> NodeCompat {
        let version = match major {
            18 => Version::new(18, 20, 4),
            20 => Version::new(20, 19, 0),
            21 => Version::new(21, 7, 3),
            22 => Version::new(22, 12, 0),
            23 => Version::new(23, 11, 1),
            _ => Version::new(major.max(DEFAULT_MAJOR), 0, 0),
        };
        NodeCompat { version }
    }

    /// Build from an explicit version string such as `"22.11.0"`.
    pub fn parse(text: &str) -> Option<NodeCompat> {
        Version::parse(text).map(|version| NodeCompat { version })
    }

    /// Interpret a version *request* from a command line.
    ///
    /// A bare major means that release line's baseline (`18` → `18.20.4`), while
    /// a full version is taken exactly as written. This is the difference between
    /// what `codets use 22` should install and `--node-version=22.11.0`, which
    /// asks to masquerade as one specific release.
    pub fn request(text: &str) -> Option<NodeCompat> {
        let trimmed = text.trim();
        let digits = trimmed.strip_prefix('v').unwrap_or(trimmed);
        let mut parts = digits.split('.');
        let major = parts.next()?.parse::<u32>().ok()?;
        if parts.next().is_none() {
            return Some(NodeCompat::for_major(major));
        }
        NodeCompat::parse(trimmed)
    }

    pub fn version(self) -> Version {
        self.version
    }

    pub fn major(self) -> u32 {
        self.version.major
    }

    /// `"24.0.0"` — what `process.versions.node` holds.
    pub fn version_string(self) -> String {
        self.version.to_string()
    }

    /// `"v24.0.0"` — what `process.version` holds.
    pub fn v_string(self) -> String {
        format!("v{}", self.version)
    }

    /// The LTS codename for the supported lines.
    pub fn lts_name(self) -> Option<&'static str> {
        match self.version.major {
            18 => Some("Hydrogen"),
            20 => Some("Iron"),
            22 => Some("Jod"),
            24 => Some("Krypton"),
            _ => None,
        }
    }

    /// The Node majors `codets` accepts on the command line.
    pub fn supported_majors() -> &'static [u32] {
        &[18, 20, 21, 22, 23, 24]
    }

    /// True when the selected release has `feature`.
    pub fn supports(self, feature: Feature) -> bool {
        self.version.major >= feature.minimum_major()
    }

    /// Every feature the selected release has, for `codets features`.
    pub fn enabled_features(self) -> Vec<Feature> {
        Feature::ALL
            .into_iter()
            .filter(|feature| self.supports(*feature))
            .collect()
    }

    /// Features this release has that `previous` did not, for `codets versions`.
    pub fn features_since(self, previous: NodeCompat) -> Vec<Feature> {
        self.enabled_features()
            .into_iter()
            .filter(|feature| !previous.supports(*feature))
            .collect()
    }

    /// A one-line human description, used by `--version` and `codets versions`.
    pub fn describe(self) -> String {
        match self.lts_name() {
            Some(name) => format!("Node {} LTS ({name})", self.version.major),
            None => format!("Node {} (current)", self.version.major),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_forms() {
        assert_eq!(Version::parse("24"), Some(Version::new(24, 0, 0)));
        assert_eq!(Version::parse("22.11"), Some(Version::new(22, 11, 0)));
        assert_eq!(Version::parse("v22.11.0"), Some(Version::new(22, 11, 0)));
        assert_eq!(Version::parse("nope"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
    }

    #[test]
    fn feature_gates_follow_the_node_release() {
        let node18 = NodeCompat::for_major(18);
        let node22 = NodeCompat::for_major(22);
        let node24 = NodeCompat::for_major(24);

        assert!(!node18.supports(Feature::ObjectGroupBy));
        assert!(!node18.supports(Feature::PromiseWithResolvers));
        assert!(!node18.supports(Feature::SetMethods));
        assert!(!node18.supports(Feature::ErrorIsError));

        assert!(node22.supports(Feature::ObjectGroupBy));
        assert!(node22.supports(Feature::PromiseWithResolvers));
        assert!(node22.supports(Feature::SetMethods));
        assert!(!node22.supports(Feature::ErrorIsError));

        assert!(node24.supports(Feature::ErrorIsError));
        assert!(node24.supports(Feature::RegExpEscape));
        assert!(node24.supports(Feature::MathSumPrecise));
    }

    #[test]
    fn reports_the_selected_release() {
        let compat = NodeCompat::for_major(22);
        assert_eq!(compat.version_string(), "22.12.0");
        assert_eq!(compat.v_string(), "v22.12.0");
        assert_eq!(compat.lts_name(), Some("Jod"));
        assert_eq!(NodeCompat::for_major(24).describe(), "Node 24 LTS (Krypton)");
    }

    #[test]
    fn defaults_to_the_newest_lts_line() {
        assert_eq!(NodeCompat::default().major(), DEFAULT_MAJOR);
        assert_eq!(NodeCompat::default().lts_name(), Some("Krypton"));
    }
}
