use super::*;
use time::macros::datetime;

fn release(day: u8) -> Release {
    let date = Date::from_calendar_date(2026, time::Month::January, day)
        .unwrap()
        .midnight()
        .assume_utc();
    Release {
        tag_name: format!("nightly-2026-01-{day:02}-abcdef123456"),
        body: Some(format!(
            "{MARKER}\nexpires_at={}",
            (date + Duration::days(DAYS)).format(&Rfc3339).unwrap()
        )),
        draft: false,
        prerelease: true,
        target_commitish: "a".repeat(40),
        published_at: Some(date),
    }
}
fn tags(releases: &[Release], now: OffsetDateTime) -> Vec<&str> {
    prune_candidates(releases, now)
        .into_iter()
        .map(|r| r.tag_name.as_str())
        .collect()
}
fn fixture(root: &Path, revision: &str) {
    let data = b"compressed build";
    fs::write(root.join("board.bundle.zip"), data).unwrap();
    write_json(&root.join("catalog.json"), &serde_json::json!({
        "format_version": FORMAT_VERSION, "revision": revision,
        "builds": [{"asset": "board.bundle.zip", "size": data.len(), "sha256": format!("{:x}", Sha256::digest(data)), "image_error": null}],
        "extra_catalog_metadata": "preserved",
    })).unwrap();
    fs::write(root.join("image-web.zip"), b"sdk").unwrap();
    fs::write(
        root.join("host-tools-linux-x86_64.tar.gz"),
        b"never execute downloaded tools",
    )
    .unwrap();
}

#[test]
fn retains_stable_and_latest_published_even_after_expiry() {
    let mut stable = release(3);
    stable.tag_name = "v1.0".into();
    stable.prerelease = false;
    assert_eq!(
        tags(
            &[release(1), release(2), stable],
            datetime!(2026-04-01 00:00 UTC)
        ),
        ["nightly-2026-01-01-abcdef123456"]
    );
}
#[test]
fn retains_promoted_nightlies() {
    let mut promoted = release(1);
    promoted.prerelease = false;
    assert!(tags(&[promoted, release(2)], datetime!(2026-04-01 00:00 UTC)).is_empty());
}
#[test]
fn retains_unexpired_and_unowned_releases() {
    let mut unowned = release(2);
    unowned.body = Some("manual release".into());
    assert!(
        tags(
            &[release(1), release(3), unowned],
            datetime!(2026-01-20 00:00 UTC)
        )
        .is_empty()
    );
}
#[test]
fn newer_drafts_do_not_protect_older_releases() {
    let mut draft = release(3);
    draft.draft = true;
    draft.published_at = None;
    assert_eq!(
        tags(
            &[release(1), release(2), draft],
            datetime!(2026-04-01 00:00 UTC)
        ),
        [
            "nightly-2026-01-01-abcdef123456",
            "nightly-2026-01-03-abcdef123456"
        ]
    );
}
#[test]
fn malformed_ownership_or_expiry_is_not_pruned() {
    for (tag, body) in [
        (
            "nightly-2026-99-01-abcdef123456",
            format!("{MARKER}\nexpires_at=2026-01-01T00:00:00Z"),
        ),
        (
            "nightly-2026-01-01-ABCDEF123456",
            format!("{MARKER}\nexpires_at=2026-01-01T00:00:00Z"),
        ),
        (
            "nightly-2026-01-01-abcdef123456",
            format!("not-{MARKER}\nexpires_at=2026-01-01T00:00:00Z"),
        ),
        (
            "nightly-2026-01-01-abcdef123456",
            format!("{MARKER}\nexpires_at=not-a-timestamp"),
        ),
    ] {
        let mut invalid = release(1);
        invalid.tag_name = tag.into();
        invalid.body = Some(body);
        assert!(tags(&[invalid, release(2)], datetime!(2026-04-01 00:00 UTC)).is_empty());
    }
}
#[test]
fn verifies_revision_version_and_exact_asset_identities() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fixture(root, "abc");
    let catalog = verify(root, "abc").unwrap();
    assert!(verify(root, "other").is_err());
    let mut old = catalog.clone();
    old["format_version"] = 1.into();
    write_json(&root.join("catalog.json"), &old).unwrap();
    assert!(verify(root, "abc").is_err());
    write_json(&root.join("catalog.json"), &catalog).unwrap();
    fs::write(root.join("vendor.rom"), b"do not publish").unwrap();
    assert!(verify(root, "abc").is_err());
    fs::remove_file(root.join("vendor.rom")).unwrap();
    fs::write(root.join("board.bundle.zip"), b"tampered").unwrap();
    assert!(verify(root, "abc").is_err());
    fixture(root, "abc");
    fs::remove_file(root.join("image-web.zip")).unwrap();
    assert!(verify(root, "abc").is_err());
}
#[test]
fn rejects_unsafe_duplicate_and_excessive_assets() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "abc");
    let catalog = verify(temp.path(), "abc").unwrap();
    for name in [
        "../escape.bundle.zip",
        "/escape.bundle.zip",
        "a#label.bundle.zip",
        "a\\b.bundle.zip",
    ] {
        let mut invalid = catalog.clone();
        invalid["builds"][0]["asset"] = name.into();
        write_json(&temp.path().join("catalog.json"), &invalid).unwrap();
        assert!(verify(temp.path(), "abc").is_err());
    }
    let mut duplicate = catalog.clone();
    duplicate["builds"]
        .as_array_mut()
        .unwrap()
        .push(catalog["builds"][0].clone());
    write_json(&temp.path().join("catalog.json"), &duplicate).unwrap();
    assert!(verify(temp.path(), "abc").is_err());
    let mut excessive = catalog.clone();
    excessive["builds"] = (0..996)
        .map(|i| {
            let mut build = catalog["builds"][0].clone();
            build["asset"] = format!("board-{i}.bundle.zip").into();
            build
        })
        .collect::<Vec<_>>()
        .into();
    write_json(&temp.path().join("catalog.json"), &excessive).unwrap();
    assert!(
        verify(temp.path(), "abc")
            .unwrap_err()
            .to_string()
            .contains("1000-asset")
    );
}
#[cfg(unix)]
#[test]
fn rejects_symlink_assets() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "abc");
    fs::remove_file(temp.path().join("image-web.zip")).unwrap();
    std::os::unix::fs::symlink("catalog.json", temp.path().join("image-web.zip")).unwrap();
    assert!(verify(temp.path(), "abc").is_err());
}

fn api_response(args: &[&str], releases: &Value) -> Result<String> {
    let value = if args.contains(&"--slurp") {
        releases.clone()
    } else if args.last().unwrap().contains("actions/workflows/") {
        serde_json::json!({"workflow_runs": [{"id":42,"head_sha":"a".repeat(40),"head_branch":"master","event":"push","conclusion":"success"}]})
    } else {
        serde_json::json!({"artifacts":[{"name":"distribution","expired":false}]})
    };
    Ok(serde_json::to_string(&value)?)
}
#[test]
fn publishes_verified_assets_as_a_draft_before_exposing_them() {
    let mut calls = Vec::new();
    let mut published = Value::Null;
    publish(
        "fstart-io/fstart",
        datetime!(2026-04-01 12:00 UTC),
        &mut |args| {
            calls.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            if args[0] == "api" {
                return api_response(args, &serde_json::json!([[]]));
            }
            if args[..2] == ["run", "download"] {
                fixture(
                    Path::new(args[args.iter().position(|s| *s == "--dir").unwrap() + 1]),
                    &"a".repeat(40),
                );
            }
            if args[..2] == ["release", "create"] {
                assert!(args.contains(&"--draft"));
                let path = Path::new(args.iter().find(|s| s.ends_with("/catalog.json")).unwrap());
                published = serde_json::from_slice(&fs::read(path)?)?;
                let sums = fs::read_to_string(path.parent().unwrap().join("SHA256SUMS"))?;
                assert_eq!(sums.lines().count(), 5);
                assert!(sums.contains("  release.json\n"));
            }
            Ok(String::new())
        },
    )
    .unwrap();
    let create = calls
        .iter()
        .position(|a| a[..2] == ["release", "create"])
        .unwrap();
    let edit = calls
        .iter()
        .position(|a| a[..2] == ["release", "edit"])
        .unwrap();
    assert!(create < edit);
    assert!(calls[edit].iter().any(|a| a == "--draft=false"));
    assert_eq!(published["revision"], "a".repeat(40));
    assert_eq!(published["source_run_id"], 42);
    assert_eq!(published["expires_at"], "2026-05-01T12:00:00Z");
    assert_eq!(published["extra_catalog_metadata"], "preserved");
}
#[test]
fn upload_failure_never_publishes_or_prunes() {
    let mut calls = Vec::new();
    let mut staging = String::new();
    let mut old = release(1);
    old.target_commitish = "b".repeat(40);
    let result = publish(
        "fstart-io/fstart",
        datetime!(2026-04-01 00:00 UTC),
        &mut |args| {
            calls.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            if args[0] == "api" {
                return api_response(args, &serde_json::json!([[old]]));
            }
            if args[..2] == ["run", "download"] {
                staging = args[args.iter().position(|s| *s == "--dir").unwrap() + 1].into();
                fixture(Path::new(&staging), &"a".repeat(40));
            }
            if args[..2] == ["release", "create"] {
                return Err("upload failed".into());
            }
            Ok(String::new())
        },
    );
    assert!(result.unwrap_err().to_string().contains("upload failed"));
    assert!(
        !calls
            .iter()
            .any(|a| a[..2] == ["release", "edit"] || a[..2] == ["release", "delete"])
    );
    assert!(!Path::new(&staging).exists());
}
#[test]
fn existing_revision_is_not_republished_and_all_release_pages_are_pruned() {
    let mut calls = Vec::new();
    let releases = serde_json::json!([[release(2)], [release(1)]]);
    publish(
        "fstart-io/fstart",
        datetime!(2026-04-01 00:00 UTC),
        &mut |args| {
            calls.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            if args[0] == "api" {
                return api_response(args, &releases);
            }
            assert_eq!(args[..2], ["release", "delete"]);
            assert_eq!(args[2], "nightly-2026-01-01-abcdef123456");
            Ok(String::new())
        },
    )
    .unwrap();
    assert_eq!(calls.len(), 3);
}
#[test]
fn invalid_revision_or_missing_successful_run_fails_closed() {
    for runs in [
        serde_json::json!([]),
        serde_json::json!([{"id":42,"head_sha":"invalid","head_branch":"master","event":"push","conclusion":"success"}]),
    ] {
        assert!(
            publish(
                "fstart-io/fstart",
                datetime!(2026-04-01 00:00 UTC),
                &mut |args| {
                    assert_eq!(args[0], "api");
                    if args.contains(&"--slurp") {
                        Ok("[[]]".into())
                    } else {
                        Ok(serde_json::json!({"workflow_runs": runs}).to_string())
                    }
                }
            )
            .is_err()
        );
    }
}
