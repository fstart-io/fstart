use crate::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io,
    path::Path,
};
use time::{
    Date, Duration, OffsetDateTime, format_description::well_known::Rfc3339,
    macros::format_description,
};

const MARKER: &str = "fstart-nightly-v1";
/// The catalog format, `fstart_image_build::bundle::VERSION`.
const FORMAT_VERSION: u32 = 3;
const DAYS: i64 = 30;
type Github<'a> = dyn FnMut(&[&str]) -> Result<String> + 'a;

#[cfg(test)]
#[path = "nightly_tests.rs"]
mod tests;

#[derive(Debug, Serialize, Deserialize)]
struct Release {
    /// The release's REST id; a draft is deleted by it.
    #[serde(default)]
    id: u64,
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    target_commitish: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    published_at: Option<OffsetDateTime>,
}
impl Release {
    fn owned(&self) -> bool {
        let Some((date, hash)) = self
            .tag_name
            .strip_prefix("nightly-")
            .and_then(|tag| tag.rsplit_once('-'))
        else {
            return false;
        };
        date.len() == 10
            && Date::parse(date, format_description!("[year]-[month]-[day]")).is_ok()
            && hex(hash, 12)
            && self
                .body
                .as_deref()
                .is_some_and(|body| body.lines().any(|line| line == MARKER))
    }
    fn expiration(&self) -> Option<OffsetDateTime> {
        self.body
            .as_deref()?
            .lines()
            .find_map(|line| line.strip_prefix("expires_at="))
            .and_then(|stamp| OffsetDateTime::parse(stamp, &Rfc3339).ok())
    }
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn prune_candidates(releases: &[Release], now: OffsetDateTime) -> Vec<&Release> {
    let managed = || {
        releases
            .iter()
            .filter(|r| r.owned() && (r.prerelease || r.draft))
    };
    // Promoting to stable opts out of expiry. A newer draft cannot displace
    // the latest published nightly, even when that nightly has expired.
    let newest = managed()
        .filter(|r| !r.draft)
        .max_by_key(|r| r.published_at);
    managed()
        .filter(|r| {
            !newest.is_some_and(|latest| std::ptr::eq(*r, latest))
                && (r.draft || r.published_at.is_some())
                && r.expiration().is_some_and(|expiration| expiration < now)
        })
        .collect()
}

#[derive(Deserialize)]
struct Catalog {
    format_version: u32,
    revision: String,
    builds: Vec<Build>,
}
#[derive(Deserialize)]
struct Build {
    asset: String,
    size: u64,
    sha256: String,
}
fn identity(path: &Path) -> Result<(u64, String)> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err("release assets must be regular files".into());
    }
    let mut digest = Sha256::new();
    let size = io::copy(&mut File::open(path)?, &mut digest)?;
    Ok((size, format!("{:x}", digest.finalize())))
}
fn verify(directory: &Path, revision: &str) -> Result<Value> {
    if !fs::symlink_metadata(directory.join("catalog.json"))?
        .file_type()
        .is_file()
    {
        return Err("catalog must be a regular file".into());
    }
    let value: Value = serde_json::from_slice(&fs::read(directory.join("catalog.json"))?)?;
    let catalog: Catalog = serde_json::from_value(value.clone())?;
    if catalog.format_version != FORMAT_VERSION
        || catalog.revision != revision
        || catalog.builds.is_empty()
    {
        return Err("artifact catalog does not match successful CI revision".into());
    }
    let mut expected: BTreeSet<String> = [
        "catalog.json",
        "image-web.zip",
        "host-tools-linux-x86_64.tar.gz",
    ]
    .map(String::from)
    .into();
    for build in &catalog.builds {
        let name = &build.asset;
        if !name.ends_with(".bundle.zip")
            || name.len() > 192
            || name.contains("..")
            || !name
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || !expected.insert(name.clone())
        {
            return Err("invalid or duplicate bundle asset name".into());
        }
    }
    if expected.len() + 2 > 1000 {
        return Err("release exceeds GitHub's 1000-asset limit".into());
    }
    let mut found = BTreeSet::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Err("release assets must be regular files".into());
        }
        found.insert(
            entry
                .file_name()
                .into_string()
                .map_err(|_| "invalid asset name")?,
        );
    }
    if found != expected {
        return Err("unexpected or missing distribution assets".into());
    }
    for build in &catalog.builds {
        let (size, sha256) = identity(&directory.join(&build.asset))?;
        if size != build.size || sha256 != build.sha256 {
            return Err(format!("bundle identity mismatch: {}", build.asset).into());
        }
    }
    Ok(value)
}
fn json<T: DeserializeOwned>(gh: &mut Github<'_>, args: &[&str]) -> Result<T> {
    Ok(serde_json::from_str(&gh(args)?)?)
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}
#[derive(Deserialize)]
struct Runs {
    workflow_runs: Vec<Run>,
}
#[derive(Deserialize)]
struct Run {
    head_sha: String,
    head_branch: String,
    event: String,
    conclusion: String,
}
#[derive(Serialize)]
struct Metadata<'a> {
    format_version: u32,
    revision: &'a str,
    created_at: String,
    expires_at: String,
    source_run_id: u64,
}

fn releases(repo: &str, gh: &mut Github<'_>) -> Result<Vec<Release>> {
    // Follow all release pages: stable releases must not hide old nightlies.
    let pages: Vec<Vec<Release>> = json(
        gh,
        &[
            "api",
            "--paginate",
            "--slurp",
            &format!("repos/{repo}/releases?per_page=100"),
        ],
    )?;
    Ok(pages.into_iter().flatten().collect())
}

fn published(releases: &[Release], revision: &str) -> bool {
    releases
        .iter()
        .any(|r| r.owned() && !r.draft && r.target_commitish == revision)
}

/// The revision the next nightly is built from: the newest master push
/// whose CI passed, unless a nightly of it exists already.
pub(super) fn pending(repo: &str, gh: &mut Github<'_>) -> Result<Option<String>> {
    let runs: Runs = json(
        gh,
        &[
            "api",
            &format!(
                "repos/{repo}/actions/workflows/ci.yml/runs?branch=master&event=push&status=success&per_page=100"
            ),
        ],
    )?;
    let run = runs
        .workflow_runs
        .into_iter()
        .find(|r| r.head_branch == "master" && r.event == "push" && r.conclusion == "success")
        .ok_or("no successful master CI run is available")?;
    if !hex(&run.head_sha, 40) {
        return Err("invalid CI revision".into());
    }
    Ok((!published(&releases(repo, gh)?, &run.head_sha)).then_some(run.head_sha))
}

/// What the nightly workflow built: `fbuild bundle`'s output of
/// `revision` in `directory`, by the workflow run `run_id`.
pub(super) struct Built<'a> {
    pub directory: &'a Path,
    pub revision: &'a str,
    pub run_id: u64,
}

/// Publish `built` as a nightly (unless that revision has one), then
/// delete the expired nightlies.
pub(super) fn publish(
    repo: &str,
    now: OffsetDateTime,
    gh: &mut Github<'_>,
    built: Option<Built<'_>>,
) -> Result<()> {
    let now = now.replace_nanosecond(0)?;
    let mut releases = releases(repo, gh)?;
    if let Some(built) = built
        && !published(&releases, built.revision)
    {
        let revision = built.revision;
        if !hex(revision, 40) {
            return Err("invalid revision".into());
        }
        let date = now.date().to_string();
        let tag = format!("nightly-{date}-{}", &revision[..12]);
        if releases.iter().any(|r| r.tag_name == tag) {
            return Err(format!(
                "{tag} already exists; inspect/remove the incomplete draft before retrying"
            )
            .into());
        }
        // The build's files are data: they are checked and staged, never run.
        let temp = tempfile::tempdir()?;
        let directory = temp.path();
        for entry in fs::read_dir(built.directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                return Err("release assets must be regular files".into());
            }
            fs::copy(entry.path(), directory.join(entry.file_name()))?;
        }
        let mut catalog = verify(directory, revision)?;
        let metadata = Metadata {
            format_version: FORMAT_VERSION,
            revision,
            created_at: now.format(&Rfc3339)?,
            expires_at: (now + Duration::days(DAYS)).format(&Rfc3339)?,
            source_run_id: built.run_id,
        };
        write_json(&directory.join("release.json"), &metadata)?;
        catalog
            .as_object_mut()
            .ok_or("catalog is not an object")?
            .extend(
                serde_json::to_value(&metadata)?
                    .as_object()
                    .ok_or("invalid release metadata")?
                    .clone(),
            );
        write_json(&directory.join("catalog.json"), &catalog)?;
        let mut paths = fs::read_dir(directory)?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<io::Result<Vec<_>>>()?;
        paths.sort();
        let mut sums = String::new();
        for path in &paths {
            sums.push_str(&format!(
                "{}  {}\n",
                identity(path)?.1,
                path.file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("invalid asset name")?
            ));
        }
        fs::write(directory.join("SHA256SUMS"), sums)?;
        paths.push(directory.join("SHA256SUMS"));
        let assets = paths
            .iter()
            .map(|p| p.to_str().ok_or("invalid asset path"))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let notes = format!(
            "{MARKER}\nrevision={revision}\nexpires_at={}\n\nAll declared boards, variants, payload selections and unit binaries. Vendor firmware is not included. See catalog.json for required local inputs. Nightlies expire after 30 days; the latest successful nightly is retained. Locally assembled images use an ephemeral signing key unless you provide your own.\n",
            metadata.expires_at
        );
        let title = format!("Nightly {date}");
        let mut create = vec![
            "release",
            "create",
            &tag,
            "--repo",
            repo,
            "--target",
            revision,
            "--title",
            &title,
            "--notes",
            &notes,
            "--prerelease",
            "--latest=false",
            "--draft",
        ];
        create.extend(assets);
        gh(&create)?;
        // The release remains a draft if any upload fails.
        gh(&["release", "edit", &tag, "--repo", repo, "--draft=false"])?;
        releases.push(Release {
            // Published, so never deleted as a draft by id.
            id: 0,
            tag_name: tag,
            body: Some(notes),
            draft: false,
            prerelease: true,
            target_commitish: revision.into(),
            published_at: Some(now),
        });
    }
    for release in prune_candidates(&releases, now) {
        let args = delete_args(repo, release);
        gh(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
    }
    Ok(())
}

/// The `gh` arguments that delete `release`. A draft has no git tag until
/// it is published, and its tag name is only the intended one (a published
/// release may have it): delete a draft by its id through the REST API,
/// which leaves tags alone, and only a published release with its tag.
fn delete_args(repo: &str, release: &Release) -> Vec<String> {
    if release.draft {
        vec![
            "api".into(),
            "--method".into(),
            "DELETE".into(),
            format!("repos/{repo}/releases/{}", release.id),
        ]
    } else {
        [
            "release",
            "delete",
            &release.tag_name,
            "--repo",
            repo,
            "--cleanup-tag",
            "--yes",
        ]
        .map(String::from)
        .to_vec()
    }
}
