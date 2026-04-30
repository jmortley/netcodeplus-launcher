//! Error type for [`crate::plan`].

use semver::{Version, VersionReq};
use thiserror::Error;

/// Result alias used by the planner.
pub type Result<T> = std::result::Result<T, PlanError>;

/// Failure modes for [`crate::plan`].
///
/// Every variant is a *user-actionable* misconfiguration: a typo in
/// an opt-out id, a missing dependency, an opt-out that would break a
/// kept pak, etc. All errors are clonable and equatable so they can
/// be cheaply attached to UI state.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PlanError {
    /// The requested channel name is not present in the manifest's
    /// `channels` map.
    #[error("channel '{0}' not found in manifest")]
    ChannelNotFound(String),

    /// The opt-out set contains an id that does not appear in the
    /// requested channel. Typically a typo in user-supplied input.
    #[error("opt-out '{id}' does not exist in channel '{channel}'")]
    OptOutOfUnknownPak {
        /// The unknown id.
        id: String,
        /// The channel that was searched.
        channel: String,
    },

    /// The opt-out set contains a pak whose `required` flag is `true`.
    /// Required paks cannot be opted out of by design.
    #[error("pak '{id}' is required and cannot be opted out of")]
    CannotOptOutOfRequired {
        /// Id of the required pak the user attempted to opt out of.
        id: String,
    },

    /// A pak's `depends_on` entry references an id that does not
    /// appear in the channel at all (typo in the published manifest).
    #[error("pak '{pak}' depends on '{dep}', which is not present in channel '{channel}'")]
    UnknownDependency {
        /// The dependent pak.
        pak: String,
        /// The id it referred to.
        dep: String,
        /// The channel that was searched.
        channel: String,
    },

    /// A pak's `depends_on` constraint is not satisfied by the
    /// channel's published version of the dep.
    #[error("pak '{pak}' requires '{dep}' {req}, but channel has version {got}")]
    DependencyVersionMismatch {
        /// The dependent pak.
        pak: String,
        /// The dep that did not match.
        dep: String,
        /// The constraint declared by the dependent pak.
        req: VersionReq,
        /// The version that was actually published in the channel.
        got: Version,
    },

    /// A pak the user wants to keep depends on a pak the user has
    /// opted out of. Either drop the opt-out or also opt out of the
    /// dependent pak.
    #[error("pak '{pak}' requires '{dep}', which the user has opted out of")]
    OptOutBreaksDependency {
        /// The dependent pak that would be broken.
        pak: String,
        /// The opted-out dep.
        dep: String,
    },
}
