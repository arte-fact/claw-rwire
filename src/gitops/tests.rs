//! Real-git tests: each builds a scratch workspace in a tempdir and shells the actual `git`
//! binary — the jurisdiction rules (meta ignores projects, sovereign births, pass commits,
//! restart resets) are asserted against real repos, not mocks.

use std::path::{Path, PathBuf};

use super::requests::branch_exists;
use super::sweep::{META_REPO, ensure_project_repo};
use super::*;
use crate::db::{endpoints, open_in_memory, task_repos, tasks, workspaces};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("claw-gitops-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn log_message(dir: &Path, reference: &str) -> String {
    git(dir, &["log", "-1", "--format=%B", reference]).expect("log")
}

fn seeded_task(id: &str) -> rusqlite::Connection {
    let conn = open_in_memory().expect("db");
    endpoints::upsert(&conn, "e", "http://e/v1", None, None).expect("endpoint");
    workspaces::create(&conn, "ws", "Project").expect("workspace");
    tasks::create(
        &conn,
        &tasks::NewTask {
            id,
            workspace_id: "ws",
            agent: None,
            title: "t",
            spec: "do",
            schedule: None,
            origin_chat: None,
            max_retries: None,
        },
    )
    .expect("task");
    conn
}

#[test]
fn meta_repo_births_once_and_ignores_projects_wholesale() {
    let root = scratch("meta");
    write(&root, "AGENT.md", "# brief");
    ensure_meta_repo(&root).expect("init");
    ensure_meta_repo(&root).expect("idempotent");
    assert!(is_repo(&root));
    assert!(
        !is_dirty(&root).expect("status"),
        "existing material adopted"
    );

    // A project — even one that is itself a repo — must be invisible to the meta-repo.
    write(&root, "projects/site/index.html", "<html>");
    ensure_project_repo(&root.join("projects/site")).expect("project");
    assert!(!is_dirty(&root).expect("status"), "projects/ is ignored");
    let tracked = git(&root, &["ls-files"]).expect("ls-files");
    assert!(
        !tracked.contains("projects"),
        "no gitlink ever recorded: {tracked}"
    );
}

#[test]
fn meta_ignore_line_is_reasserted_when_an_agent_rewrites_gitignore() {
    let root = scratch("meta-ignore");
    ensure_meta_repo(&root).expect("init");
    std::fs::write(root.join(".gitignore"), "*.log\n").expect("clobber");
    ensure_meta_repo(&root).expect("re-ensure");
    let ignore = std::fs::read_to_string(root.join(".gitignore")).expect("read");
    assert!(ignore.contains("*.log") && ignore.contains("projects/"));
}

#[test]
fn project_birth_lands_on_develop_with_a_seed_only_initial_commit() {
    let root = scratch("birth");
    let dir = root.join("projects/app");
    write(&root, "projects/app/main.rs", "fn main() {}");
    ensure_project_repo(&dir).expect("birth");
    assert_eq!(current_branch(&dir).expect("branch"), "develop");
    let tracked = git(&dir, &["ls-files"]).expect("ls-files");
    assert_eq!(
        tracked, ".gitignore",
        "the initial commit is the seed only — pre-existing files await an adopt/pass commit"
    );
    assert!(is_dirty(&dir).expect("status"), "main.rs still uncommitted");
}

#[test]
fn repo_dir_rejects_an_escape() {
    let root = scratch("jail");
    assert!(matches!(repo_dir(&root, "../outside"), Err(GitError::Jail)));
}

#[test]
fn commit_pass_commits_every_dirty_repo_with_author_and_trailers() {
    let root = scratch("pass");
    ensure_meta_repo(&root).expect("meta");
    let clean = root.join("projects/clean");
    ensure_project_repo(&clean).expect("clean project");
    let clean_head = head(&clean).expect("head");
    write(&root, "docs/note.md", "notes");
    write(&root, "projects/app/src/lib.rs", "pub fn f() {}");

    let (commits, errors) = commit_pass(&root, Some("forge"), "build: the app", "t1", "r1", None);
    assert_eq!(errors, Vec::<String>::new());
    // Meta (docs/note.md) + the newborn projects/app committed; the clean repo untouched.
    let repos_hit: Vec<&str> = commits.iter().map(|c| c.repo.as_str()).collect();
    assert_eq!(repos_hit, vec![META_REPO, "projects/app"]);
    assert_eq!(head(&clean).expect("head"), clean_head);

    let app = root.join("projects/app");
    let message = log_message(&app, "HEAD");
    assert!(message.starts_with("build: the app"), "{message}");
    assert!(message.contains("Task: t1") && message.contains("Run: r1"));
    let author = git(&app, &["log", "-1", "--format=%an <%ae>"]).expect("author");
    assert_eq!(author, "claw <forge@claw>");
    // The newborn's pre-pass base is its seed commit — one commit behind the pass.
    let seed = git(&app, &["rev-parse", "HEAD^"]).expect("parent");
    assert_eq!(commits[1].base_before.as_deref(), Some(seed.as_str()));
    assert_eq!(current_branch(&app).expect("branch"), "develop");
}

#[test]
fn snapshot_adopts_a_dirty_tree_only_on_first_launch() {
    let root = scratch("snapshot");
    let conn = seeded_task("t1");
    write(&root, "docs/pre-existing.md", "before the task");

    let errors = snapshot_task_bases(&conn, &root, "t1");
    assert_eq!(errors, Vec::<String>::new());
    assert!(!is_dirty(&root).expect("status"), "pre-task state adopted");
    let bases = task_repos::bases_for(&conn, "t1").expect("bases");
    assert_eq!(bases.len(), 1);
    assert_eq!(bases[0].base_commit, head(&root).expect("head"));

    // A later launch (retry after a failed pass) must not adopt the deliberate dirt.
    write(&root, "docs/failed-pass-leftover.md", "dirty");
    let errors = snapshot_task_bases(&conn, &root, "t1");
    assert_eq!(errors, Vec::<String>::new());
    assert!(
        is_dirty(&root).expect("status"),
        "a failed pass's tree stays dirty across the retry re-claim"
    );
    assert_eq!(
        task_repos::bases_for(&conn, "t1").expect("bases"),
        bases,
        "the base never moves"
    );
}

#[test]
fn restart_reset_returns_the_tree_to_the_pre_task_state() {
    // The dirty-tree inheritance regression: pass commits + a failed pass's leftovers must
    // all vanish on restart, and the pre-task content must come back byte-identical.
    let root = scratch("reset");
    let conn = seeded_task("t1");
    write(&root, "docs/keep.md", "pre-task content");
    write(&root, "projects/app/src/lib.rs", "pub fn keep() {}");
    let errors = snapshot_task_bases(&conn, &root, "t1");
    assert_eq!(errors, Vec::<String>::new());

    // The task works: a successful pass commits, then a failed pass leaves dirt everywhere.
    write(&root, "docs/keep.md", "clobbered by the task");
    write(&root, "projects/app/src/lib.rs", "pub fn broken() {}");
    let (commits, errors) = commit_pass(&root, Some("forge"), "step: t", "t1", "r1", None);
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(commits.len(), 2);
    write(&root, "docs/untracked-leftover.md", "from the failed pass");
    write(&root, "projects/app/junk.tmp", "half-written");

    let errors = reset_task_tree(&conn, &root, "t1");
    assert_eq!(errors, Vec::<String>::new());
    let keep = std::fs::read_to_string(root.join("docs/keep.md")).expect("read");
    assert_eq!(keep, "pre-task content");
    let lib = std::fs::read_to_string(root.join("projects/app/src/lib.rs")).expect("read");
    assert_eq!(lib, "pub fn keep() {}");
    assert!(!root.join("docs/untracked-leftover.md").exists());
    assert!(!root.join("projects/app/junk.tmp").exists());
    assert!(!is_dirty(&root).expect("meta status"));
    assert!(!is_dirty(&root.join("projects/app")).expect("app status"));
}

#[test]
fn harness_git_never_runs_repo_hooks() {
    // `.git/hooks` is inside the workspace jail, so an agent can plant one; the harness
    // commit sweep must neither execute it nor let it veto the commit.
    use std::os::unix::fs::PermissionsExt;
    let root = scratch("hooks");
    ensure_meta_repo(&root).expect("init");
    let hook = root.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().expect("hooks dir")).expect("mkdir");
    std::fs::write(&hook, "#!/bin/sh\ntouch hook-ran\nexit 1\n").expect("hook");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    write(&root, "docs/x.md", "x");
    commit_all(&root, None, "pass despite hostile hook").expect("commit succeeds");
    assert!(
        !root.join("hook-ran").exists(),
        "the planted hook must never execute under harness git"
    );
    assert!(!is_dirty(&root).expect("status"), "the commit landed");
}

#[test]
fn request_branch_is_cut_once_and_passes_land_on_it() {
    // G1: the first request-bound pass cuts req/… from develop's tip (reporting the branch
    // point); later passes land on the same branch with the Request: trailer.
    let root = scratch("req-branch");
    ensure_meta_repo(&root).expect("meta");
    let develop_tip = head(&root).expect("head");
    write(&root, "docs/step1.md", "one");

    let request = Some(RequestRef {
        id: "req-1",
        branch: "req/req1-thing",
    });
    let (commits, errors) = commit_pass(&root, Some("forge"), "s1: t", "t1", "r1", request);
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(
        commits[0].request_base.as_deref(),
        Some(develop_tip.as_str()),
        "first touch reports the branch point"
    );
    assert_eq!(current_branch(&root).expect("branch"), "req/req1-thing");
    assert!(log_message(&root, "HEAD").contains("Request: req-1"));

    // Second pass: same branch, no new cut.
    write(&root, "docs/step2.md", "two");
    let (commits, errors) = commit_pass(&root, Some("forge"), "s2: t", "t1", "r2", request);
    assert_eq!(errors, Vec::<String>::new());
    assert!(commits[0].request_base.is_none(), "already on the branch");
    // develop never moved — the request's work is isolated on its branch.
    let develop_now = git(&root, &["rev-parse", "develop"]).expect("develop");
    assert_eq!(develop_now, develop_tip);
}

#[test]
fn delivery_merges_no_ff_into_develop_and_totals_the_diff() {
    let root = scratch("deliver");
    ensure_meta_repo(&root).expect("meta");
    write(&root, "docs/feature.md", "line one\nline two\n");
    let request = Some(RequestRef {
        id: "req-2",
        branch: "req/req2-feat",
    });
    let (commits, errors) = commit_pass(&root, Some("forge"), "build: f", "t1", "r1", request);
    assert_eq!(errors, Vec::<String>::new());
    let base = commits[0].request_base.clone().expect("cut");

    let merge =
        deliver_repo(&root, "req/req2-feat", "Deliver: feat\n\nRequest: req-2").expect("deliver");
    assert_eq!(current_branch(&root).expect("branch"), "develop");
    // A real --no-ff merge commit: two parents, carrying the Request trailer.
    let parents = git(&root, &["rev-list", "--parents", "-1", &merge]).expect("parents");
    assert_eq!(parents.split_whitespace().count(), 3, "{parents}");
    assert!(log_message(&root, &merge).contains("Request: req-2"));
    // The delivered diff totals the request's change.
    let (files, adds, dels) = diff_numstat(&root, &format!("{base}..{merge}")).expect("numstat");
    assert_eq!((files, adds, dels), (1, 2, 0));
    // Delivering over a dirty tree is refused.
    write(&root, "docs/dirt.md", "x");
    assert!(matches!(
        deliver_repo(&root, "req/req2-feat", "again"),
        Err(GitError::DirtyTree)
    ));
}

#[test]
fn rollback_reverts_the_delivery_merge_on_develop() {
    let root = scratch("revert");
    ensure_meta_repo(&root).expect("meta");
    write(&root, "docs/f.md", "v1\n");
    let request = Some(RequestRef {
        id: "req-3",
        branch: "req/req3-f",
    });
    let (_, errors) = commit_pass(&root, None, "b: f", "t1", "r1", request);
    assert_eq!(errors, Vec::<String>::new());
    let merge = deliver_repo(&root, "req/req3-f", "Deliver: f\n\nRequest: req-3").expect("deliver");
    assert!(root.join("docs/f.md").exists());

    let revert = revert_merge(&root, &merge).expect("revert");
    assert_eq!(current_branch(&root).expect("branch"), "develop");
    assert!(
        !root.join("docs/f.md").exists(),
        "the delivered change is undone by a new commit"
    );
    let message = log_message(&root, &revert);
    assert!(message.contains("Revert"), "{message}");
}

#[test]
fn abandon_drops_the_branch_and_returns_to_develop() {
    let root = scratch("abandon");
    ensure_meta_repo(&root).expect("meta");
    let develop_tip = head(&root).expect("head");
    write(&root, "docs/half-done.md", "wip\n");
    let request = Some(RequestRef {
        id: "req-4",
        branch: "req/req4-drop",
    });
    let (_, errors) = commit_pass(&root, None, "b: wip", "t1", "r1", request);
    assert_eq!(errors, Vec::<String>::new());

    abandon_branch(&root, "req/req4-drop").expect("abandon");
    assert_eq!(current_branch(&root).expect("branch"), "develop");
    assert_eq!(
        head(&root).expect("head"),
        develop_tip,
        "develop never moved"
    );
    assert!(!branch_exists(&root, "req/req4-drop"));
    assert!(!root.join("docs/half-done.md").exists());
}

#[test]
fn push_lands_develop_and_the_request_branch_on_a_bare_remote() {
    let root = scratch("push");
    let remote = scratch("push-remote");
    ensure_meta_repo(&root).expect("meta");
    git(&remote, &["init", "--bare"]).expect("bare");
    git(
        &root,
        &["remote", "add", "origin", remote.to_str().expect("utf8")],
    )
    .expect("remote");
    assert!(has_remote(&root));
    write(&root, "docs/pushed.md", "out\n");
    let request = Some(RequestRef {
        id: "req-5",
        branch: "req/req5-push",
    });
    let (_, errors) = commit_pass(&root, None, "b: out", "t1", "r1", request);
    assert_eq!(errors, Vec::<String>::new());
    deliver_repo(&root, "req/req5-push", "Deliver: out\n\nRequest: req-5").expect("deliver");

    push_refs(&root, "req/req5-push").expect("push");
    let remote_refs = git(&remote, &["branch"]).expect("branches");
    assert!(remote_refs.contains("develop"), "{remote_refs}");
    assert!(remote_refs.contains("req/req5-push"), "{remote_refs}");
}

#[test]
fn restore_file_returns_one_path_to_its_pre_pass_state() {
    let root = scratch("restore");
    ensure_meta_repo(&root).expect("meta");
    write(&root, "docs/keep.md", "original\n");
    commit_all(&root, None, "seed").expect("seed");
    write(&root, "docs/keep.md", "clobbered by a pass\n");
    let (commits, _) = commit_pass(&root, None, "b: clobber", "t1", "r1", None);
    let pass_sha = commits[0].commit.clone();

    restore_file(&root, &pass_sha, "docs/keep.md").expect("restore");
    let content = std::fs::read_to_string(root.join("docs/keep.md")).expect("read");
    assert_eq!(content, "original\n");
    assert!(
        is_dirty(&root).expect("status"),
        "the restore awaits the next pass commit"
    );
    // A file born in that pass has no pre-state — refused, not silently emptied.
    write(&root, "docs/newborn.md", "x");
    let (commits, _) = commit_pass(&root, None, "b: newborn", "t2", "r2", None);
    assert!(restore_file(&root, &commits[0].commit, "docs/newborn.md").is_err());
}
