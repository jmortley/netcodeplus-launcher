//! Integration tests for [`ncp_planner::plan`].
//!
//! Pure logic, no fixtures on disk. Each test builds its own minimal
//! manifest + local install in memory.

use std::collections::{HashMap, HashSet};

use chrono::{TimeZone, Utc};
use ncp_manifest::{Channel, Manifest, ManifestPak, Sha256Digest, SUPPORTED_SCHEMA_VERSION};
use ncp_planner::{
    plan, DownloadReason, LocalInstall, LocalPak, PlanError, RemoveReason, UpdatePlan,
};
use semver::{Version, VersionReq};

// ---------- helpers --------------------------------------------------

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}

fn v(s: &str) -> Version {
    Version::parse(s).expect("valid semver literal")
}

fn vr(s: &str) -> VersionReq {
    VersionReq::parse(s).expect("valid semver req literal")
}

/// Build a [`ManifestPak`] with sensible defaults so tests can supply
/// only the fields they care about.
fn make_pak(id: &str, version: &str, sha: u8, required: bool) -> ManifestPak {
    ManifestPak {
        version: v(version),
        pak_filename: format!("{id}.pak"),
        url: format!("https://example.test/{id}.pak"),
        sha256: digest(sha),
        size_bytes: 1_000_000,
        load_order: 100,
        required,
        depends_on: HashMap::new(),
    }
}

fn make_pak_with_deps(
    id: &str,
    version: &str,
    sha: u8,
    required: bool,
    deps: &[(&str, &str)],
) -> ManifestPak {
    let mut p = make_pak(id, version, sha, required);
    p.depends_on = deps
        .iter()
        .map(|(id, req)| ((*id).to_string(), vr(req)))
        .collect();
    p
}

fn manifest_with_channel(name: &str, paks: HashMap<String, ManifestPak>) -> Manifest {
    let mut channels = HashMap::new();
    channels.insert(name.to_string(), Channel { paks, plugin: None });
    Manifest {
        schema_version: SUPPORTED_SCHEMA_VERSION,
        generated_at: Utc.with_ymd_and_hms(2026, 4, 30, 0, 0, 0).unwrap(),
        expires_at: Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap(),
        sequence: 1,
        min_launcher_version: v("0.1.0"),
        channels,
    }
}

fn local_pak(version: &str, sha: u8, filename: &str) -> LocalPak {
    LocalPak {
        pak_filename: filename.to_string(),
        version: v(version),
        sha256: digest(sha),
    }
}

fn opted_out(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| (*s).to_string()).collect()
}

fn install(paks: &[(&str, LocalPak)]) -> LocalInstall {
    LocalInstall {
        paks: paks
            .iter()
            .map(|(id, p)| ((*id).to_string(), p.clone()))
            .collect(),
    }
}

// ---------- channel handling -----------------------------------------

#[test]
fn channel_not_found_is_an_error() {
    let manifest = manifest_with_channel("stable", HashMap::new());
    let err = plan(
        &manifest,
        "missing",
        &LocalInstall::default(),
        &HashSet::new(),
    )
    .unwrap_err();
    assert_eq!(err, PlanError::ChannelNotFound("missing".to_string()));
}

#[test]
fn empty_channel_with_empty_local_is_no_op() {
    let manifest = manifest_with_channel("stable", HashMap::new());
    let p = plan(
        &manifest,
        "stable",
        &LocalInstall::default(),
        &HashSet::new(),
    )
    .unwrap();
    assert!(p.is_no_op());
    assert_eq!(p, UpdatePlan::default());
}

// ---------- action categorisation ------------------------------------

#[test]
fn missing_pak_becomes_a_download_action() {
    let mut paks = HashMap::new();
    paks.insert("a".to_string(), make_pak("a", "1.0.0", 1, true));
    let manifest = manifest_with_channel("stable", paks);

    let p = plan(
        &manifest,
        "stable",
        &LocalInstall::default(),
        &HashSet::new(),
    )
    .unwrap();
    assert_eq!(p.to_download.len(), 1);
    assert_eq!(p.to_download[0].id, "a");
    assert_eq!(p.to_download[0].reason, DownloadReason::Missing);
    assert!(p.to_download[0].previous.is_none());
    assert!(p.to_keep.is_empty());
    assert!(p.to_remove.is_empty());
}

#[test]
fn matching_local_pak_becomes_a_keep_action() {
    let mut paks = HashMap::new();
    paks.insert("a".to_string(), make_pak("a", "1.0.0", 7, true));
    let manifest = manifest_with_channel("stable", paks);

    let local = install(&[("a", local_pak("1.0.0", 7, "a.pak"))]);

    let p = plan(&manifest, "stable", &local, &HashSet::new()).unwrap();
    assert!(p.is_no_op());
    assert_eq!(p.to_keep.len(), 1);
    assert_eq!(p.to_keep[0].id, "a");
    assert_eq!(p.to_keep[0].local_pak.sha256, digest(7));
    assert_eq!(p.to_keep[0].manifest_pak.sha256, digest(7));
}

#[test]
fn hash_mismatch_becomes_a_download_with_previous() {
    let mut paks = HashMap::new();
    paks.insert("a".to_string(), make_pak("a", "2.0.0", 9, true));
    let manifest = manifest_with_channel("stable", paks);

    let local = install(&[("a", local_pak("1.0.0", 7, "a.pak"))]);

    let p = plan(&manifest, "stable", &local, &HashSet::new()).unwrap();
    assert_eq!(p.to_download.len(), 1);
    assert_eq!(p.to_download[0].reason, DownloadReason::HashMismatch);
    let prev = p.to_download[0].previous.as_ref().expect("has previous");
    assert_eq!(prev.sha256, digest(7));
    assert_eq!(prev.version, v("1.0.0"));
}

#[test]
fn local_pak_not_in_channel_becomes_a_remove_with_not_in_channel_reason() {
    // Channel has nothing; local has a leftover from a previous channel.
    let manifest = manifest_with_channel("stable", HashMap::new());
    let local = install(&[("legacy", local_pak("0.5.0", 3, "legacy.pak"))]);

    let p = plan(&manifest, "stable", &local, &HashSet::new()).unwrap();
    assert_eq!(p.to_remove.len(), 1);
    assert_eq!(p.to_remove[0].id, "legacy");
    assert_eq!(p.to_remove[0].reason, RemoveReason::NotInChannel);
}

#[test]
fn opted_out_pak_present_locally_becomes_a_remove_with_opted_out_reason() {
    let mut paks = HashMap::new();
    paks.insert(
        "optional".to_string(),
        make_pak("optional", "1.0.0", 5, false),
    );
    let manifest = manifest_with_channel("stable", paks);

    let local = install(&[("optional", local_pak("1.0.0", 5, "optional.pak"))]);
    let opt = opted_out(&["optional"]);

    let p = plan(&manifest, "stable", &local, &opt).unwrap();
    assert_eq!(p.to_remove.len(), 1);
    assert_eq!(p.to_remove[0].id, "optional");
    assert_eq!(p.to_remove[0].reason, RemoveReason::OptedOut);
    assert!(p.to_keep.is_empty());
    assert!(p.to_download.is_empty());
}

#[test]
fn opted_out_pak_absent_locally_produces_no_action_at_all() {
    let mut paks = HashMap::new();
    paks.insert(
        "optional".to_string(),
        make_pak("optional", "1.0.0", 5, false),
    );
    let manifest = manifest_with_channel("stable", paks);

    let opt = opted_out(&["optional"]);
    let p = plan(&manifest, "stable", &LocalInstall::default(), &opt).unwrap();
    assert!(p.is_no_op());
    assert!(p.to_keep.is_empty());
}

// ---------- opt-out validation ---------------------------------------

#[test]
fn opt_out_of_unknown_pak_is_an_error() {
    let manifest = manifest_with_channel("stable", HashMap::new());
    let opt = opted_out(&["does-not-exist"]);
    let err = plan(&manifest, "stable", &LocalInstall::default(), &opt).unwrap_err();
    assert_eq!(
        err,
        PlanError::OptOutOfUnknownPak {
            id: "does-not-exist".to_string(),
            channel: "stable".to_string(),
        }
    );
}

#[test]
fn opt_out_of_required_pak_is_an_error() {
    let mut paks = HashMap::new();
    paks.insert("core".to_string(), make_pak("core", "1.0.0", 1, true));
    let manifest = manifest_with_channel("stable", paks);

    let opt = opted_out(&["core"]);
    let err = plan(&manifest, "stable", &LocalInstall::default(), &opt).unwrap_err();
    assert_eq!(
        err,
        PlanError::CannotOptOutOfRequired {
            id: "core".to_string()
        }
    );
}

// ---------- dependency resolution ------------------------------------

#[test]
fn satisfied_dependency_is_accepted() {
    let mut paks = HashMap::new();
    paks.insert("base".to_string(), make_pak("base", "1.0.0", 1, true));
    paks.insert(
        "ext".to_string(),
        make_pak_with_deps("ext", "1.0.0", 2, true, &[("base", ">=1.0.0")]),
    );
    let manifest = manifest_with_channel("stable", paks);

    plan(
        &manifest,
        "stable",
        &LocalInstall::default(),
        &HashSet::new(),
    )
    .expect("satisfied dep should plan ok");
}

#[test]
fn unknown_dependency_id_is_an_error() {
    let mut paks = HashMap::new();
    paks.insert(
        "ext".to_string(),
        make_pak_with_deps("ext", "1.0.0", 2, true, &[("ghost", ">=1.0.0")]),
    );
    let manifest = manifest_with_channel("stable", paks);

    let err = plan(
        &manifest,
        "stable",
        &LocalInstall::default(),
        &HashSet::new(),
    )
    .unwrap_err();
    assert_eq!(
        err,
        PlanError::UnknownDependency {
            pak: "ext".to_string(),
            dep: "ghost".to_string(),
            channel: "stable".to_string(),
        }
    );
}

#[test]
fn dependency_with_unsatisfied_version_is_an_error() {
    let mut paks = HashMap::new();
    paks.insert("base".to_string(), make_pak("base", "1.0.0", 1, true));
    paks.insert(
        "ext".to_string(),
        make_pak_with_deps("ext", "1.0.0", 2, true, &[("base", ">=2.0.0")]),
    );
    let manifest = manifest_with_channel("stable", paks);

    let err = plan(
        &manifest,
        "stable",
        &LocalInstall::default(),
        &HashSet::new(),
    )
    .unwrap_err();
    assert_eq!(
        err,
        PlanError::DependencyVersionMismatch {
            pak: "ext".to_string(),
            dep: "base".to_string(),
            req: vr(">=2.0.0"),
            got: v("1.0.0"),
        }
    );
}

#[test]
fn opt_out_breaking_a_dependency_is_an_error() {
    let mut paks = HashMap::new();
    paks.insert("base".to_string(), make_pak("base", "1.0.0", 1, false)); // optional
    paks.insert(
        "ext".to_string(),
        make_pak_with_deps("ext", "1.0.0", 2, true, &[("base", ">=1.0.0")]),
    );
    let manifest = manifest_with_channel("stable", paks);

    let opt = opted_out(&["base"]);
    let err = plan(&manifest, "stable", &LocalInstall::default(), &opt).unwrap_err();
    assert_eq!(
        err,
        PlanError::OptOutBreaksDependency {
            pak: "ext".to_string(),
            dep: "base".to_string(),
        }
    );
}

#[test]
fn opt_out_of_dependency_is_fine_when_dependent_is_also_opted_out() {
    let mut paks = HashMap::new();
    paks.insert("base".to_string(), make_pak("base", "1.0.0", 1, false));
    paks.insert(
        "ext".to_string(),
        make_pak_with_deps("ext", "1.0.0", 2, false, &[("base", ">=1.0.0")]),
    );
    let manifest = manifest_with_channel("stable", paks);

    let opt = opted_out(&["base", "ext"]);
    plan(&manifest, "stable", &LocalInstall::default(), &opt)
        .expect("opting out both dep and dependent should be fine");
}

#[test]
fn transitive_dependency_chain_is_accepted() {
    // a -> b -> c; nothing installed locally.
    let mut paks = HashMap::new();
    paks.insert("c".to_string(), make_pak("c", "1.0.0", 1, true));
    paks.insert(
        "b".to_string(),
        make_pak_with_deps("b", "1.0.0", 2, true, &[("c", ">=1.0.0")]),
    );
    paks.insert(
        "a".to_string(),
        make_pak_with_deps("a", "1.0.0", 3, true, &[("b", "=1.0.0")]),
    );
    let manifest = manifest_with_channel("stable", paks);

    let p = plan(
        &manifest,
        "stable",
        &LocalInstall::default(),
        &HashSet::new(),
    )
    .unwrap();
    assert_eq!(p.to_download.len(), 3);
    assert_eq!(
        p.to_download
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
}

#[test]
fn circular_dependency_with_satisfying_versions_is_accepted() {
    // a -> b, b -> a — both at 1.0.0, both require >=1.0.0. The
    // planner doesn't model install order; cycles only matter to the
    // installer (Phase 4+).
    let mut paks = HashMap::new();
    paks.insert(
        "a".to_string(),
        make_pak_with_deps("a", "1.0.0", 1, true, &[("b", ">=1.0.0")]),
    );
    paks.insert(
        "b".to_string(),
        make_pak_with_deps("b", "1.0.0", 2, true, &[("a", ">=1.0.0")]),
    );
    let manifest = manifest_with_channel("stable", paks);

    plan(
        &manifest,
        "stable",
        &LocalInstall::default(),
        &HashSet::new(),
    )
    .expect("circular dep should validate (no install order modelled)");
}

// ---------- whole-plan shapes ---------------------------------------

#[test]
fn full_round_trip_mix_of_keep_download_remove() {
    // Manifest: required core + optional extra
    let mut paks = HashMap::new();
    paks.insert("core".to_string(), make_pak("core", "2.0.0", 22, true));
    paks.insert("extra".to_string(), make_pak("extra", "1.5.0", 15, false));
    let manifest = manifest_with_channel("stable", paks);

    // Local: stale core + matching extra + leftover from old channel
    let local = install(&[
        ("core", local_pak("1.0.0", 11, "core.pak")),   // stale
        ("extra", local_pak("1.5.0", 15, "extra.pak")), // matches
        ("legacy", local_pak("0.1.0", 99, "legacy.pak")), // leftover
    ]);

    let p = plan(&manifest, "stable", &local, &HashSet::new()).unwrap();

    assert_eq!(p.to_download.len(), 1);
    assert_eq!(p.to_download[0].id, "core");
    assert_eq!(p.to_download[0].reason, DownloadReason::HashMismatch);

    assert_eq!(p.to_keep.len(), 1);
    assert_eq!(p.to_keep[0].id, "extra");

    assert_eq!(p.to_remove.len(), 1);
    assert_eq!(p.to_remove[0].id, "legacy");
    assert_eq!(p.to_remove[0].reason, RemoveReason::NotInChannel);
}

#[test]
fn opt_out_replaces_keep_with_remove_for_already_installed_optional() {
    // Optional pak already installed and matching. User now opts out:
    // it should move from to_keep to to_remove.
    let mut paks = HashMap::new();
    paks.insert(
        "optional".to_string(),
        make_pak("optional", "1.0.0", 5, false),
    );
    let manifest = manifest_with_channel("stable", paks);
    let local = install(&[("optional", local_pak("1.0.0", 5, "optional.pak"))]);

    let without_opt = plan(&manifest, "stable", &local, &HashSet::new()).unwrap();
    let with_opt = plan(&manifest, "stable", &local, &opted_out(&["optional"])).unwrap();

    assert_eq!(without_opt.to_keep.len(), 1);
    assert!(without_opt.to_remove.is_empty());

    assert!(with_opt.to_keep.is_empty());
    assert_eq!(with_opt.to_remove.len(), 1);
    assert_eq!(with_opt.to_remove[0].reason, RemoveReason::OptedOut);
}

// ---------- determinism -----------------------------------------------

#[test]
fn output_is_sorted_by_id() {
    // Insert ids in non-alphabetical order; confirm output is sorted.
    let mut paks = HashMap::new();
    for id in ["zeta", "alpha", "mu", "beta"] {
        paks.insert(id.to_string(), make_pak(id, "1.0.0", 1, true));
    }
    let manifest = manifest_with_channel("stable", paks);

    let p = plan(
        &manifest,
        "stable",
        &LocalInstall::default(),
        &HashSet::new(),
    )
    .unwrap();
    let ids: Vec<&str> = p.to_download.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["alpha", "beta", "mu", "zeta"]);
}

// ---------- UpdatePlan helper methods --------------------------------

#[test]
fn is_no_op_only_true_when_no_downloads_or_removes() {
    let p = UpdatePlan::default();
    assert!(p.is_no_op());
}

#[test]
fn total_download_bytes_sums_across_actions() {
    let mut paks = HashMap::new();
    let mut a = make_pak("a", "1.0.0", 1, true);
    a.size_bytes = 200;
    let mut b = make_pak("b", "1.0.0", 2, true);
    b.size_bytes = 800;
    paks.insert("a".to_string(), a);
    paks.insert("b".to_string(), b);
    let manifest = manifest_with_channel("stable", paks);

    let p = plan(
        &manifest,
        "stable",
        &LocalInstall::default(),
        &HashSet::new(),
    )
    .unwrap();
    assert_eq!(p.total_download_bytes(), 1000);
}
