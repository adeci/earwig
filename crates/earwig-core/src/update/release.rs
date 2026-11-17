//! What GitHub's API says about earwig's releases, and which one, if any, a
//! build updates to.

use semver::Version;
use serde::Deserialize;

/// A release as GitHub's API lists it; only what earwig reads.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Asset {
    pub name: String,
    pub size: u64,
    /// `sha256:<hex>`, which GitHub computes as the file is uploaded.
    #[serde(default)]
    pub digest: Option<String>,
    /// `uploaded` once the file is complete.
    pub state: String,
    pub browser_download_url: String,
}

/// A newer release and this platform's file of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub version: Version,
    pub(crate) url: String,
    pub(crate) size: u64,
    /// Lowercase hex.
    pub(crate) sha256: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    /// Nothing newer than this build.
    UpToDate,
    Offer(Offer),
    /// The newest release has no file this build can trust (yet).
    Unusable {
        version: Version,
        why: String,
    },
}

/// The version of a tag as earwig's are made: `v` and SemVer, no build
/// metadata, nothing a parser would have to tidy up.
fn tag_version(tag: &str) -> Option<Version> {
    let text = tag.strip_prefix('v')?;
    let version = Version::parse(text).ok()?;
    (version.build.is_empty() && version.to_string() == text).then_some(version)
}

/// The release a build of `running` updates to. A stable build only takes
/// stable releases; a pre-release build takes the newest of all. The file is
/// `asset(version)`, at `<downloads>/v<version>/<name>`, uploaded, with a
/// SHA-256 digest.
pub(crate) fn pick(
    running: &Version,
    releases: &[Release],
    downloads: &str,
    asset: impl Fn(&Version) -> String,
) -> Pick {
    let stable = running.pre.is_empty();
    let newest = releases
        .iter()
        .filter(|release| !release.draft)
        .filter_map(|release| Some((tag_version(&release.tag_name)?, release)))
        .filter(|(version, release)| !stable || (version.pre.is_empty() && !release.prerelease))
        .max_by(|(a, _), (b, _)| a.cmp(b));
    let Some((version, release)) = newest.filter(|(version, _)| version > running) else {
        return Pick::UpToDate;
    };
    let name = asset(&version);
    let unusable = |why: String| Pick::Unusable {
        version: version.clone(),
        why,
    };
    let Some(file) = release.assets.iter().find(|file| file.name == name) else {
        return unusable(format!("it has no {name}"));
    };
    if file.state != "uploaded" {
        return unusable(format!("{name} is still uploading"));
    }
    let Some(sha256) = file.digest.as_deref().and_then(sha256_hex) else {
        return unusable(format!("{name} has no SHA-256 digest"));
    };
    let url = format!("{downloads}/v{version}/{name}");
    if file.browser_download_url != url {
        return unusable(format!(
            "{name} is at {}, not {url}",
            shown(&file.browser_download_url)
        ));
    }
    if file.size == 0 {
        return unusable(format!("{name} is empty"));
    }
    Pick::Offer(Offer {
        version,
        url,
        size: file.size,
        sha256,
    })
}

/// The hex of a `sha256:<64 hex digits>` digest, lowercased.
fn sha256_hex(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_lowercase())
}

/// A string from the feed as it may go in a log line: at most 200
/// characters, quoted, control characters escaped.
fn shown(remote: &str) -> String {
    let short: String = remote.chars().take(200).collect();
    format!("{short:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOWNLOADS: &str = "https://github.com/adeci/earwig/releases/download";
    const SHA: &str = "1f8a0a4f5e0c4b2c9a7d1e3f5b6c7d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8";

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn asset(version: &Version) -> String {
        format!("earwig-{version}-windows-x64-setup.exe")
    }

    /// GitHub's JSON for a release of `tag` with its installer.
    fn release(tag: &str, prerelease: bool) -> serde_json::Value {
        let version = tag.trim_start_matches('v');
        let name = format!("earwig-{version}-windows-x64-setup.exe");
        serde_json::json!({
            "tag_name": tag,
            "name": format!("earwig {tag}"),
            "draft": false,
            "prerelease": prerelease,
            "html_url": format!("https://github.com/adeci/earwig/releases/tag/{tag}"),
            "assets": [
                {
                    "name": format!("earwig-{version}-windows-x64.zip"),
                    "size": 1000,
                    "digest": format!("sha256:{}", "0".repeat(64)),
                    "state": "uploaded",
                    "browser_download_url": format!("{DOWNLOADS}/{tag}/earwig-{version}-windows-x64.zip"),
                },
                {
                    "name": name,
                    "size": 31_337,
                    "digest": format!("sha256:{SHA}"),
                    "state": "uploaded",
                    "content_type": "application/x-msdownload",
                    "browser_download_url": format!("{DOWNLOADS}/{tag}/{name}"),
                },
            ],
        })
    }

    fn parse(json: &serde_json::Value) -> Vec<Release> {
        serde_json::from_value(json.clone()).unwrap()
    }

    fn picked(running: &str, json: &serde_json::Value) -> Pick {
        pick(&v(running), &parse(json), DOWNLOADS, asset)
    }

    fn offered(running: &str, json: &serde_json::Value) -> Option<String> {
        match picked(running, json) {
            Pick::Offer(offer) => Some(offer.version.to_string()),
            Pick::UpToDate => None,
            Pick::Unusable { version, why } => panic!("{version}: {why}"),
        }
    }

    #[test]
    fn tags_are_v_and_exact_semver() {
        assert_eq!(tag_version("v0.1.0-rc.2"), Some(v("0.1.0-rc.2")));
        assert_eq!(tag_version("v1.2.3"), Some(v("1.2.3")));
        for tag in [
            "0.1.0",
            "v0.1",
            "v01.2.3",
            "v1.2.3+build.5",
            "v1.2.3 ",
            "release-1",
        ] {
            assert_eq!(tag_version(tag), None, "{tag}");
        }
    }

    #[test]
    fn a_newer_release_with_its_file_and_digest_is_offered() {
        let json = serde_json::json!([release("v0.1.1", false)]);
        let Pick::Offer(offer) = picked("0.1.0", &json) else {
            panic!("no offer");
        };
        assert_eq!(
            offer,
            Offer {
                version: v("0.1.1"),
                url: format!("{DOWNLOADS}/v0.1.1/earwig-0.1.1-windows-x64-setup.exe"),
                size: 31_337,
                sha256: SHA.into(),
            }
        );
    }

    #[test]
    fn the_same_or_an_older_release_is_no_update() {
        for running in ["0.1.1", "0.1.2", "0.2.0-rc.1"] {
            let json = serde_json::json!([release("v0.1.1", false)]);
            assert_eq!(picked(running, &json), Pick::UpToDate, "{running}");
        }
        assert_eq!(picked("0.1.0", &serde_json::json!([])), Pick::UpToDate);
    }

    #[test]
    fn a_stable_build_ignores_pre_releases() {
        let json = serde_json::json!([
            release("v0.2.0-rc.1", true),
            release("v0.1.1", false),
            // Marked as a pre-release on GitHub, though its version isn't one.
            release("v0.1.5", true),
        ]);
        assert_eq!(offered("0.1.0", &json).as_deref(), Some("0.1.1"));
    }

    #[test]
    fn a_pre_release_build_takes_the_newest_of_all() {
        let candidates = serde_json::json!([
            release("v0.1.0-rc.2", true),
            release("v0.1.0-rc.10", true),
            release("v0.0.9", false),
        ]);
        assert_eq!(
            offered("0.1.0-rc.1", &candidates).as_deref(),
            Some("0.1.0-rc.10")
        );
        let with_final =
            serde_json::json!([release("v0.1.0-rc.2", true), release("v0.1.0", false),]);
        assert_eq!(offered("0.1.0-rc.2", &with_final).as_deref(), Some("0.1.0"));
        assert_eq!(offered("0.1.0", &with_final), None);
    }

    #[test]
    fn drafts_and_odd_tags_are_skipped() {
        let mut draft = release("v0.3.0", false);
        draft["draft"] = true.into();
        let json = serde_json::json!([draft, release("nightly", true), release("v0.2.0", false)]);
        assert_eq!(offered("0.1.0", &json).as_deref(), Some("0.2.0"));
    }

    /// The newest release, with its installer changed by `edit`.
    fn with_installer(edit: impl FnOnce(&mut serde_json::Value)) -> Pick {
        let mut json = release("v0.1.1", false);
        edit(&mut json["assets"][1]);
        picked(
            "0.1.0",
            &serde_json::json!([json, release("v0.1.0", false)]),
        )
    }

    fn why(pick: &Pick) -> &str {
        match pick {
            Pick::Unusable { version, why } => {
                assert_eq!(*version, v("0.1.1"));
                why
            }
            other => panic!("expected unusable, got {other:?}"),
        }
    }

    #[test]
    fn a_release_without_this_platforms_file_is_no_update() {
        let missing = with_installer(|file| file["name"] = "earwig-0.1.1-macos-arm64.dmg".into());
        assert!(why(&missing).contains("has no earwig-0.1.1-windows-x64-setup.exe"));
        // Named after another version.
        let renamed =
            with_installer(|file| file["name"] = "earwig-0.1.0-windows-x64-setup.exe".into());
        assert!(why(&renamed).contains("has no"));
    }

    #[test]
    fn a_file_without_a_sha256_digest_is_no_update() {
        for digest in [
            serde_json::Value::Null,
            format!("sha1:{}", "a".repeat(40)).into(),
            "sha256:".into(),
            format!("sha256:{}", "g".repeat(64)).into(),
        ] {
            let pick = with_installer(|file| file["digest"] = digest.clone());
            assert!(why(&pick).contains("no SHA-256"), "{digest}");
        }
        let gone = with_installer(|file| {
            file.as_object_mut().unwrap().remove("digest");
        });
        assert!(why(&gone).contains("no SHA-256"));
    }

    #[test]
    fn a_file_elsewhere_or_unfinished_is_no_update() {
        let elsewhere = with_installer(|file| {
            file["browser_download_url"] = "https://example.com/earwig-setup.exe".into();
        });
        assert!(why(&elsewhere).contains("example.com"));
        let hostile = with_installer(|file| {
            file["browser_download_url"] = format!("https://x/\n{}", "a".repeat(5000)).into();
        });
        assert!(!why(&hostile).contains('\n'), "control characters escaped");
        assert!(why(&hostile).len() < 400, "cut short");
        let uploading = with_installer(|file| file["state"] = "open".into());
        assert!(why(&uploading).contains("still uploading"));
        let empty = with_installer(|file| file["size"] = 0.into());
        assert!(why(&empty).contains("empty"));
    }

    #[test]
    fn digests_are_taken_in_any_case() {
        let upper =
            with_installer(|file| file["digest"] = format!("sha256:{}", SHA.to_uppercase()).into());
        let Pick::Offer(offer) = upper else {
            panic!("{upper:?}");
        };
        assert_eq!(offer.sha256, SHA);
    }
}
