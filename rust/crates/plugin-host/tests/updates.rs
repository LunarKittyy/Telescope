//! The update check for added plugins: versions, GitHub's answers, and staying quiet offline or rate limited (test_plugin_updates.py).

use serde_json::json;
use std::collections::VecDeque;
use std::sync::Mutex;
use telescope_core::testing::scratch_dir;
use telescope_plugin_host::updates::{self, Asset, Cache, Fetch, Release, Reply, CHECK_INTERVAL_S, WAIT_MAX_S};

const REPO: &str = "someone/their_plugin";
const NOW: f64 = 1_800_000_000.0;

/// One answer GitHub gives: a status with headers and a body, or no answer at all.
enum Answer {
    Http(u16, Vec<(&'static str, String)>, Vec<u8>),
    Offline(&'static str),
}

fn ok(body: Vec<u8>) -> Answer {
    Answer::Http(200, vec![], body)
}

fn status(code: u16, headers: &[(&'static str, String)]) -> Answer {
    Answer::Http(code, headers.to_vec(), vec![])
}

/// Stands in for the network: answers each request with the next answer, and records what was asked.
struct GitHub {
    answers: Mutex<VecDeque<Answer>>,
    asked: Mutex<Vec<(String, Vec<(String, String)>)>>,
}

impl GitHub {
    fn new(answers: Vec<Answer>) -> GitHub {
        GitHub { answers: Mutex::new(answers.into()), asked: Mutex::new(Vec::new()) }
    }

    fn asked(&self) -> Vec<(String, Vec<(String, String)>)> {
        self.asked.lock().unwrap().clone()
    }
}

impl Fetch for GitHub {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Reply, String> {
        self.asked.lock().unwrap().push((url.into(), headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()));
        match self.answers.lock().unwrap().pop_front().expect("asked more than expected") {
            Answer::Offline(why) => Err(why.into()),
            Answer::Http(code, headers, body) => Ok(Reply {
                status: code,
                headers: headers.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
                body: Box::new(std::io::Cursor::new(body)),
            }),
        }
    }
}

fn release_json(tag: &str, assets: Option<serde_json::Value>) -> Vec<u8> {
    let assets = assets.unwrap_or_else(|| json!([
        {"name": "their_plugin.zip", "size": 1000, "digest": format!("sha256:{}", "a".repeat(64)),
         "browser_download_url": format!("https://github.com/{REPO}/releases/download/{tag}/their_plugin.zip")},
        {"name": "notes.txt", "size": 10, "browser_download_url": format!("https://github.com/{REPO}/releases/download/{tag}/notes.txt")},
    ]));
    json!({"tag_name": tag, "html_url": format!("https://github.com/{REPO}/releases/tag/{tag}"), "assets": assets}).to_string().into_bytes()
}

fn cache() -> Cache {
    Cache::open(scratch_dir("updates").join("plugin-updates.json"))
}

fn repos(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn versions_compare_by_their_numbers() {
    for (tag, installed, newer) in [("v1.3", "1.2", true), ("1.10", "1.9", true), ("v2", "1.9.9", true), ("1.2", "1.2", false), ("v1.2.0", "1.2", false),
                                    ("1.1", "1.2", false), ("1.2", "1.2-beta", true), ("1.2-beta", "1.2", false), ("1.3-beta", "1.2", true),
                                    ("latest", "1.2", false), ("1.3", "", false), ("1.3", "first", false)] {
        assert_eq!(updates::is_newer(tag, installed), newer, "{tag} over {installed}");
    }
}

#[test]
fn only_github_repos_are_followed() {
    for (url, repo) in [("https://github.com/someone/their_plugin", Some(REPO)), ("https://github.com/someone/their_plugin/", Some(REPO)),
                        ("https://github.com/someone/their_plugin.git", Some(REPO)), ("https://github.com/someone/their_plugin/tree/main", None),
                        ("https://gitlab.com/someone/their_plugin", None), ("https://github.com.evil.org/someone/their_plugin", None), ("", None)] {
        assert_eq!(updates::github_repo(url).as_deref(), repo, "{url}");
    }
}

#[test]
fn a_release_is_found_kept_and_not_asked_for_again_the_same_day() {
    let mut c = cache();
    let github = GitHub::new(vec![Answer::Http(200, vec![("ETag", "\"abc\"".into())], release_json("v1.3", None))]);
    updates::check(&repos(&[REPO]), &mut c, NOW, &github);
    assert_eq!(github.asked()[0].0, format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let release = Cache::open(&c.path).release(REPO).expect("saved to the file");
    assert_eq!(release.tag, "v1.3");
    assert_eq!(release.assets.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["their_plugin.zip"]);
    assert_eq!(release.zip_for("their_plugin").unwrap().sha256, "a".repeat(64));
    updates::check(&repos(&[REPO]), &mut Cache::open(&c.path), NOW + 3600.0, &github);
    assert_eq!(github.asked().len(), 1);
}

#[test]
fn asking_again_sends_the_etag_and_a_304_keeps_what_was_found() {
    let mut c = cache();
    let github = GitHub::new(vec![Answer::Http(200, vec![("ETag", "\"abc\"".into())], release_json("v1.3", None)), status(304, &[])]);
    updates::check(&repos(&[REPO]), &mut c, NOW, &github);
    let mut c = Cache::open(&c.path);
    updates::check(&repos(&[REPO]), &mut c, NOW + CHECK_INTERVAL_S, &github);
    assert!(github.asked()[1].1.contains(&("If-None-Match".into(), "\"abc\"".into())));
    assert_eq!(c.release(REPO).unwrap().tag, "v1.3");
    assert!(!c.due(REPO, NOW + CHECK_INTERVAL_S + 60.0));
}

#[test]
fn a_repo_without_releases_is_nothing_to_say() {
    let mut c = cache();
    updates::check(&repos(&[REPO]), &mut c, NOW, &GitHub::new(vec![status(404, &[])]));
    assert!(c.release(REPO).is_none() && c.wait_until == 0.0 && !c.due(REPO, NOW + 60.0));
}

#[test]
fn offline_it_waits_longer_each_time_and_never_raises() {
    let github = GitHub::new(vec![Answer::Offline("no network"), Answer::Offline("no network"), Answer::Offline("no network"), ok(release_json("v1.3", None))]);
    let mut c = cache();
    updates::check(&repos(&[REPO]), &mut c, NOW, &github);
    assert!(c.wait_until == NOW + 3600.0 && c.release(REPO).is_none());
    updates::check(&repos(&[REPO]), &mut c, NOW + 60.0, &github);
    assert_eq!(github.asked().len(), 1);
    updates::check(&repos(&[REPO]), &mut c, NOW + 3600.0, &github);
    assert_eq!(c.wait_until, NOW + 3600.0 + 7200.0);
    let mut c = Cache::open(&c.path);
    updates::check(&repos(&[REPO]), &mut c, NOW + 3600.0 + 7200.0, &github);
    assert!(c.failures == 3 && c.wait_until == NOW + 3600.0 + 7200.0 + 4.0 * 3600.0, "kept in the file");
    let at = c.wait_until;
    updates::check(&repos(&[REPO]), &mut c, at, &github);
    assert!(c.release(REPO).unwrap().tag == "v1.3" && c.failures == 0 && c.wait_until == 0.0);
}

#[test]
fn the_wait_stops_at_a_day() {
    let mut c = cache();
    c.failures = 20;
    updates::check(&repos(&[REPO]), &mut c, NOW, &GitHub::new(vec![Answer::Offline("timed out")]));
    assert_eq!(c.wait_until, NOW + WAIT_MAX_S);
}

#[test]
fn rate_limited_it_waits_until_github_says_and_stops_asking() {
    let github = GitHub::new(vec![status(403, &[("X-RateLimit-Remaining", "0".into()), ("X-RateLimit-Reset", format!("{}", NOW as i64 + 5 * 3600))])]);
    let mut c = cache();
    updates::check(&repos(&[REPO, "other/repo"]), &mut c, NOW, &github);
    assert!(github.asked().len() == 1 && c.wait_until == NOW + 5.0 * 3600.0, "the second repo wasn't asked either");
    updates::check(&repos(&[REPO]), &mut c, NOW + 4.0 * 3600.0, &github);
    assert_eq!(github.asked().len(), 1);
}

#[test]
fn a_429_waits_as_long_as_it_says_within_reason() {
    let cases = [(("Retry-After", "120".to_string()), 3600.0), (("Retry-After", "7200".to_string()), 7200.0),
                 (("X-RateLimit-Reset", format!("{}", NOW as i64 + 10 * 24 * 3600)), 24.0 * 3600.0), (("Retry-After", "soon".to_string()), 3600.0)];
    for (header, wait) in cases {
        let mut c = cache();
        updates::check(&repos(&[REPO]), &mut c, NOW, &GitHub::new(vec![status(429, &[header.clone()])]));
        assert_eq!(c.wait_until, NOW + wait, "{header:?}");
    }
}

#[test]
fn a_clock_set_back_doesnt_leave_it_waiting_for_days() {
    let mut c = cache();
    c.wait_until = NOW + 30.0 * 24.0 * 3600.0;
    c.repos.insert(REPO.into(), json!({"checked": NOW + 30.0 * 24.0 * 3600.0}));
    let github = GitHub::new(vec![ok(release_json("v1.3", None))]);
    updates::check(&repos(&[REPO]), &mut c, NOW, &github);
    assert!(github.asked().len() == 1 && c.release(REPO).unwrap().tag == "v1.3");
}

#[test]
fn a_reply_that_isnt_a_release_is_ignored() {
    for raw in [b"<html>".to_vec(), b"[]".to_vec(), json!({"tag_name": "v1"}).to_string().into_bytes(),
                json!({"tag_name": "v1", "html_url": "https://evil.org/x"}).to_string().into_bytes()] {
        let mut c = cache();
        updates::check(&repos(&[REPO]), &mut c, NOW, &GitHub::new(vec![ok(raw.clone())]));
        assert!(c.release(REPO).is_none() && c.wait_until == 0.0, "{}", String::from_utf8_lossy(&raw));
    }
}

#[test]
fn assets_that_arent_plugin_zips_from_github_are_left_out() {
    let release = updates::parse_release(&release_json("v1", Some(json!([
        {"name": "a.zip", "size": 10, "browser_download_url": "https://evil.org/a.zip"},
        {"name": "b.zip", "size": 1_000_000_000u64, "browser_download_url": "https://github.com/x/y/b.zip"},
        {"name": "c.zip", "size": 10, "browser_download_url": "https://github.com/x/y/c.zip", "digest": "md5:x"},
    ])))).unwrap();
    assert_eq!(release.assets.iter().map(|a| (a.name.as_str(), a.sha256.as_str())).collect::<Vec<_>>(), [("c.zip", "")]);
}

#[test]
fn a_broken_cache_file_is_no_checks_yet() {
    let path = scratch_dir("broken-cache").join("plugin-updates.json");
    std::fs::write(&path, "{nope").unwrap();
    assert!(Cache::open(&path).repos.is_empty());
    std::fs::write(&path, json!({"repos": {REPO: {"release": {"tag": 5}}}, "wait_until": "x"}).to_string()).unwrap();
    let c = Cache::open(&path);
    assert!(c.wait_until == 0.0 && c.release(REPO).is_none());
}

#[test]
fn the_right_zip_is_picked_for_the_plugin() {
    for (names, want) in [(vec!["their_plugin.zip", "other.zip"], Some("their_plugin.zip")), (vec!["their_plugin-1.3.zip", "other.zip"], Some("their_plugin-1.3.zip")),
                          (vec!["build.zip"], Some("build.zip")), (vec!["a.zip", "b.zip"], None)] {
        let release = Release { tag: "v1".into(), page: "https://github.com/x/y".into(),
                                assets: names.iter().map(|n| Asset { name: n.to_string(), url: format!("https://github.com/{n}"), size: 1, sha256: String::new() }).collect() };
        assert_eq!(release.zip_for("their_plugin").map(|a| a.name.as_str()), want, "{names:?}");
    }
}

#[test]
fn a_download_is_checked_against_its_size_and_digest() {
    let dir = scratch_dir("download");
    let data: Vec<u8> = [b"PK".as_slice(), &[b'x'; 98]].concat();
    let sha = telescope_plugin_host::package::sha256_hex(&data);
    let good = Asset { name: "p.zip".into(), url: "https://github.com/p.zip".into(), size: data.len() as u64, sha256: sha };
    let serve = || GitHub::new(vec![ok(data.clone())]);
    assert_eq!(std::fs::read(updates::download(&good, &dir, &serve()).unwrap()).unwrap(), data);
    let with = |size: u64, sha: &str| Asset { size, sha256: sha.into(), ..good.clone() };
    assert!(updates::download(&with(100, &"0".repeat(64)), &dir, &serve()).unwrap_err().0.contains("checksum"));
    assert!(updates::download(&with(10, ""), &dir, &serve()).unwrap_err().0.contains("bigger"));
    assert!(updates::download(&with(1000, ""), &dir, &serve()).unwrap_err().0.contains("cut short"));
    assert!(updates::download(&good, &dir, &GitHub::new(vec![Answer::Offline("offline")])).unwrap_err().0.contains("couldn't be downloaded"));
}

#[test]
fn a_repo_github_refuses_only_skips_that_repo() {
    for code in [403, 410, 451] {
        let github = GitHub::new(vec![status(code, &[]), ok(release_json("v1.3", None))]);
        let mut c = cache();
        updates::check(&repos(&["aaa/blocked", REPO]), &mut c, NOW, &github);
        assert!(github.asked().len() == 2 && c.wait_until == 0.0, "{code}");
        assert!(c.release("aaa/blocked").is_none() && !c.due("aaa/blocked", NOW + 60.0));
        assert_eq!(c.release(REPO).unwrap().tag, "v1.3");
    }
}

#[test]
fn a_403_with_retry_after_is_a_rate_limit() {
    let github = GitHub::new(vec![status(403, &[("Retry-After", "7200".into())])]);
    let mut c = cache();
    updates::check(&repos(&["aaa/first", REPO]), &mut c, NOW, &github);
    assert!(github.asked().len() == 1 && c.wait_until == NOW + 7200.0);
}

#[test]
fn a_server_error_waits_for_every_repo() {
    let github = GitHub::new(vec![status(502, &[])]);
    let mut c = cache();
    updates::check(&repos(&["aaa/first", REPO]), &mut c, NOW, &github);
    assert!(github.asked().len() == 1 && c.wait_until == NOW + 3600.0 && c.due("aaa/first", NOW + 3600.0));
}
