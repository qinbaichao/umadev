//! PR mode — turn a finished run's evidence into the most trustworthy PR on the
//! team.
//!
//! Most generated PRs hand a reviewer raw code and a one-line title. PR mode
//! flips that: it opens a pull request whose **body is the run's own evidence**
//! — the PR-ready review report (`review.rs`: contract / acceptance / coverage /
//! governance / security / runtime + rollback) followed by a proof-pack summary.
//! The reviewer reads a self-asserting, source-cited case for merge, not a diff
//! with no context.
//!
//! This module is **deterministic + fail-open + light-deps**. It never opens a
//! PR itself: it computes *readiness* (is this a git repo? are there uncommitted
//! changes? is there a GitHub remote? is `gh` on PATH and logged in?) and
//! *renders* the body. The binary (`umadev pr`) does the actual `git` / `gh`
//! shell-out and enforces the safety rails. The split keeps every decision here
//! a pure function the unit tests can assert on **without ever pushing or
//! opening a real PR**.
//!
//! Safety contract surfaced as data here, enforced by the caller:
//! - never commit directly on the default branch — branch first
//!   ([`PrPlan::needs_new_branch`]);
//! - never force-push, never rewrite the user's existing commits;
//! - any external probe that errors degrades to "not ready" with a manual hint,
//!   never a panic or a destructive fallback.

use std::path::Path;
use std::process::Command;

use crate::review::{build_review_report, render_review_md};

/// One readiness precondition for opening a PR automatically, and whether it
/// holds. Kept as data so the renderer + the manual-steps hint are pure
/// functions over a [`PrReadiness`] and the tests can assert on the structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessCheck {
    /// Short machine-ish id (e.g. `git-repo`, `gh-cli`).
    pub id: String,
    /// Human one-liner shown to the user.
    pub label: String,
    /// `true` iff the precondition is satisfied.
    pub ok: bool,
    /// What to do by hand when this check fails (empty when `ok`).
    pub remedy: String,
}

/// The full readiness picture: every precondition + the resolved branch facts.
/// `ready()` is `true` only when every *blocking* check passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrReadiness {
    /// Ordered preconditions.
    pub checks: Vec<ReadinessCheck>,
    /// The repo's default branch (e.g. `main`), best-effort; empty if unknown.
    pub default_branch: String,
    /// The branch currently checked out, best-effort; empty if unknown.
    pub current_branch: String,
    /// `true` when the working tree has uncommitted changes to commit.
    pub has_changes: bool,
}

impl PrReadiness {
    /// `true` iff every check passed — safe to drive `git` + `gh` end-to-end.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    /// The first failing check, if any (the headline reason we can't proceed).
    #[must_use]
    pub fn first_blocker(&self) -> Option<&ReadinessCheck> {
        self.checks.iter().find(|c| !c.ok)
    }
}

/// The concrete, safety-checked plan for opening the PR. Computed from a
/// [`PrReadiness`] so the caller's git/gh sequence is pure data it can print +
/// execute, and the tests can assert the safety rails without shelling out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrPlan {
    /// `true` when we must create a feature branch first because HEAD is on the
    /// default branch — **we never commit directly on the default branch**.
    pub needs_new_branch: bool,
    /// The branch we will commit + push to (an existing non-default branch, or
    /// the freshly-suggested feature branch name).
    pub head_branch: String,
    /// The base branch the PR targets (the repo default branch).
    pub base_branch: String,
}

/// Decide the branch plan from readiness. Pure: no side effects.
///
/// Rule: if we're on the default branch (or the current branch is unknown but a
/// default exists), create a fresh feature branch derived from `slug`; otherwise
/// keep the user's already-checked-out feature branch as the PR head. We never
/// return a plan that would commit onto the default branch.
#[must_use]
pub fn plan_branches(readiness: &PrReadiness, slug: &str) -> PrPlan {
    let base = if readiness.default_branch.is_empty() {
        "main".to_string()
    } else {
        readiness.default_branch.clone()
    };
    let on_default = readiness.current_branch == base || readiness.current_branch.is_empty();
    if on_default {
        PrPlan {
            needs_new_branch: true,
            head_branch: feature_branch_name(slug),
            base_branch: base,
        }
    } else {
        PrPlan {
            needs_new_branch: false,
            head_branch: readiness.current_branch.clone(),
            base_branch: base,
        }
    }
}

/// A safe, deterministic feature-branch name for a slug. Sanitised to the subset
/// git + GitHub accept everywhere (lowercase alnum + `-`), prefixed `umadev/`.
#[must_use]
pub fn feature_branch_name(slug: &str) -> String {
    let cleaned: String = slug
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    let stem = if trimmed.is_empty() {
        "change"
    } else {
        trimmed
    };
    format!("umadev/{stem}")
}

// =====================================================================
// readiness probes (fail-open shell-out to git / gh)
// =====================================================================

/// Probe the workspace and the environment for PR readiness. Every probe is
/// fail-open: a spawn error / non-zero exit is read as "precondition not met"
/// and surfaced as a failing [`ReadinessCheck`] with a manual remedy — never a
/// panic, never a destructive assumption.
#[must_use]
pub fn assess_readiness(project_root: &Path) -> PrReadiness {
    let is_repo = git_is_repo(project_root);
    let default_branch = if is_repo {
        git_default_branch(project_root)
    } else {
        String::new()
    };
    let current_branch = if is_repo {
        git_current_branch(project_root)
    } else {
        String::new()
    };
    let has_changes = is_repo && git_has_changes(project_root);
    let has_remote = is_repo && git_has_github_remote(project_root);
    let gh_present = gh_on_path();
    let gh_authed = gh_present && gh_logged_in();

    let checks = vec![
        ReadinessCheck {
            id: "git-repo".to_string(),
            label: "Inside a git repository".to_string(),
            ok: is_repo,
            remedy: if is_repo {
                String::new()
            } else {
                "Run `git init` (and commit a baseline) before opening a PR.".to_string()
            },
        },
        ReadinessCheck {
            id: "has-changes".to_string(),
            label: "There are changes to put in the PR".to_string(),
            ok: has_changes,
            remedy: if has_changes {
                String::new()
            } else {
                "Nothing to commit — run the pipeline so it writes artifacts/code first."
                    .to_string()
            },
        },
        ReadinessCheck {
            id: "github-remote".to_string(),
            label: "A GitHub remote is configured".to_string(),
            ok: has_remote,
            remedy: if has_remote {
                String::new()
            } else {
                "Add a GitHub remote: `git remote add origin <github-url>`.".to_string()
            },
        },
        ReadinessCheck {
            id: "gh-cli".to_string(),
            label: "GitHub CLI (`gh`) is installed".to_string(),
            ok: gh_present,
            remedy: if gh_present {
                String::new()
            } else {
                "Install GitHub CLI from https://cli.github.com/ to open the PR automatically."
                    .to_string()
            },
        },
        ReadinessCheck {
            id: "gh-auth".to_string(),
            label: "GitHub CLI is logged in".to_string(),
            ok: gh_authed,
            remedy: if gh_authed {
                String::new()
            } else {
                "Authenticate once with `gh auth login`, then re-run `umadev pr`.".to_string()
            },
        },
    ];

    PrReadiness {
        checks,
        default_branch,
        current_branch,
        has_changes,
    }
}

/// Render the manual fallback steps for when automation isn't ready. Pure over
/// [`PrReadiness`] + the rendered body, so the user can always finish the PR by
/// hand. Lists exactly the failing preconditions and their remedies, then the
/// generic git/gh recipe, and points at the body we *would* have used.
#[must_use]
pub fn manual_steps(readiness: &PrReadiness, slug: &str, body_path_rel: &str) -> String {
    let plan = plan_branches(readiness, slug);
    let mut out = String::from("Could not open the PR automatically. Resolve these, then retry:\n");
    for c in readiness.checks.iter().filter(|c| !c.ok) {
        out.push_str(&format!("  - {} — {}\n", c.label, c.remedy));
    }
    out.push_str("\nOr open it by hand (UmaDev never force-pushes or rewrites your commits):\n");
    if plan.needs_new_branch {
        out.push_str(&format!(
            "  git switch -c {head}            # never commit on `{base}` directly\n",
            head = plan.head_branch,
            base = plan.base_branch
        ));
    }
    out.push_str(&format!(
        "  git add -A && git commit -m \"{slug}: UmaDev pipeline output\"\n  \
         git push -u origin {head}\n  \
         gh pr create --base {base} --head {head} --title \"{slug}\" --body-file {body}\n",
        slug = slug,
        head = plan.head_branch,
        base = plan.base_branch,
        body = body_path_rel,
    ));
    out
}

// =====================================================================
// PR body rendering (reuse the review-report + proof-pack summary)
// =====================================================================

/// Workspace-relative path of the rendered PR body (so the manual recipe and the
/// `gh --body-file` path agree).
#[must_use]
pub fn pr_body_rel_path(slug: &str) -> String {
    format!("output/{slug}-pr-body.md")
}

/// Build the full PR body markdown: the PR-ready review report (verbatim from
/// `review.rs`) followed by a proof-pack summary. Pure assembly over artifacts
/// UmaDev already produced — fail-open: missing artifacts degrade to honest
/// "not available" lines inside the review report, and an absent proof-pack
/// renders a one-line note rather than an error.
#[must_use]
pub fn render_pr_body(project_root: &Path, slug: &str) -> String {
    let report = build_review_report(project_root, slug);
    let review_md = render_review_md(&report);

    let mut out = String::new();
    out.push_str(&review_md);
    out.push_str("\n---\n\n## Proof pack\n\n");
    out.push_str(&proof_pack_summary(project_root, slug));
    out.push_str(
        "\n_This PR body was generated by UmaDev from the run's own evidence. \
         Every claim above cites the file or number it derives from._\n",
    );
    out
}

/// Summarise the delivery proof-pack(s) in `release/` for the PR body. Names the
/// latest zip + its size, or an honest note when none exists yet. Pure read.
#[must_use]
pub fn proof_pack_summary(project_root: &Path, _slug: &str) -> String {
    match latest_proof_pack(project_root) {
        Some((path, size_bytes)) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!(
                "Full evidence bundle: `release/{name}` ({} KiB) — docs + quality gate + \
                 compliance mapping + a runnable scorecard, retained for post-merge audit.\n",
                size_bytes / 1024
            )
        }
        None => "No proof-pack zip yet (it is produced at the `delivery` phase). \
             The review checklist above still reflects the current evidence.\n"
            .to_string(),
    }
}

/// The newest `release/proof-pack-*.zip` and its size in bytes, or `None`.
#[must_use]
pub fn latest_proof_pack(project_root: &Path) -> Option<(std::path::PathBuf, u64)> {
    let release = project_root.join("release");
    let mut packs: Vec<std::path::PathBuf> = std::fs::read_dir(&release)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let is_zip = p
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
            let named = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|s| s.starts_with("proof-pack-"));
            is_zip && named
        })
        .collect();
    packs.sort();
    let latest = packs.pop()?;
    let size = std::fs::metadata(&latest).map_or(0, |m| m.len());
    Some((latest, size))
}

// =====================================================================
// git / gh helpers — each fail-open
// =====================================================================

/// `true` iff `project_root` is inside a git work tree.
fn git_is_repo(project_root: &Path) -> bool {
    run_git(project_root, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

/// The repo's default branch. Tries the remote HEAD symref first
/// (`origin/HEAD` → e.g. `main`), then falls back to the current branch, then
/// to `main`. Best-effort: a failure returns an empty string (caller treats
/// empty as "unknown" and still avoids committing on a guessed default).
fn git_default_branch(project_root: &Path) -> String {
    if let Some(out) = run_git(
        project_root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        // e.g. `origin/main` → `main`
        if let Some(name) = out.trim().rsplit('/').next() {
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    // Fall back to the configured init default, else the common `main`.
    run_git(project_root, &["config", "--get", "init.defaultBranch"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "main".to_string())
}

/// The branch currently checked out, or empty (detached HEAD / failure).
fn git_current_branch(project_root: &Path) -> String {
    run_git(project_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "HEAD")
        .unwrap_or_default()
}

/// `true` iff `git status --porcelain` reports any change (staged, unstaged, or
/// untracked) — i.e. there is something to commit into the PR.
fn git_has_changes(project_root: &Path) -> bool {
    run_git(project_root, &["status", "--porcelain"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// `true` iff any configured remote URL points at github.com.
fn git_has_github_remote(project_root: &Path) -> bool {
    run_git(project_root, &["remote", "-v"])
        .map(|s| s.to_ascii_lowercase().contains("github.com"))
        .unwrap_or(false)
}

/// `true` iff `gh` resolves on PATH (a `gh --version` succeeds).
fn gh_on_path() -> bool {
    Command::new("gh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `true` iff `gh auth status` reports a logged-in account.
fn gh_logged_in() -> bool {
    Command::new("gh")
        .args(["auth", "status"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a git subcommand in `project_root`, returning stdout on success. Any
/// spawn error / non-zero exit → `None` (fail-open).
fn run_git(project_root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(project_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn readiness(default: &str, current: &str, all_ok: bool) -> PrReadiness {
        let mk = |id: &str| ReadinessCheck {
            id: id.to_string(),
            label: id.to_string(),
            ok: all_ok,
            remedy: if all_ok {
                String::new()
            } else {
                format!("fix {id}")
            },
        };
        PrReadiness {
            checks: vec![
                mk("git-repo"),
                mk("has-changes"),
                mk("github-remote"),
                mk("gh-cli"),
                mk("gh-auth"),
            ],
            default_branch: default.to_string(),
            current_branch: current.to_string(),
            has_changes: all_ok,
        }
    }

    #[test]
    fn ready_only_when_every_check_passes() {
        assert!(readiness("main", "feature/x", true).ready());
        let mut r = readiness("main", "feature/x", true);
        r.checks[2].ok = false;
        assert!(!r.ready());
        assert_eq!(r.first_blocker().unwrap().id, "github-remote");
    }

    #[test]
    fn on_default_branch_forces_a_new_feature_branch() {
        // SAFETY: HEAD on the default branch must never be committed onto.
        let r = readiness("main", "main", true);
        let plan = plan_branches(&r, "my-app");
        assert!(plan.needs_new_branch);
        assert_eq!(plan.head_branch, "umadev/my-app");
        assert_eq!(plan.base_branch, "main");
    }

    #[test]
    fn unknown_current_branch_also_forces_a_new_branch() {
        // Detached HEAD / unknown current branch → never reuse, branch fresh.
        let r = readiness("main", "", true);
        let plan = plan_branches(&r, "app");
        assert!(plan.needs_new_branch);
        assert_eq!(plan.head_branch, "umadev/app");
    }

    #[test]
    fn existing_feature_branch_is_reused_as_head() {
        let r = readiness("main", "feat/login", true);
        let plan = plan_branches(&r, "app");
        assert!(!plan.needs_new_branch);
        assert_eq!(plan.head_branch, "feat/login");
        assert_eq!(plan.base_branch, "main");
    }

    #[test]
    fn unknown_default_branch_defaults_base_to_main() {
        let r = readiness("", "", true);
        let plan = plan_branches(&r, "x");
        assert_eq!(plan.base_branch, "main");
        assert!(plan.needs_new_branch); // current empty == treated as on-default
    }

    #[test]
    fn feature_branch_name_is_sanitised() {
        assert_eq!(feature_branch_name("My App!"), "umadev/my-app");
        assert_eq!(feature_branch_name("  ---  "), "umadev/change");
        assert_eq!(feature_branch_name(""), "umadev/change");
        assert_eq!(feature_branch_name("clean-slug-1"), "umadev/clean-slug-1");
    }

    #[test]
    fn manual_steps_list_failing_checks_and_safe_recipe() {
        let r = readiness("main", "main", false);
        let steps = manual_steps(&r, "demo", "output/demo-pr-body.md");
        // Every failing check is surfaced with its remedy.
        assert!(steps.contains("fix github-remote"));
        // On the default branch the recipe MUST branch first (no direct commit).
        assert!(steps.contains("git switch -c umadev/demo"));
        assert!(steps.contains("never commit on `main` directly"));
        // Never instructs a force-push: the recipe must use a plain `push`,
        // never `--force` / `-f` / `+ref` (the safety promise in the header text
        // may *mention* "force-push", but no actual force command is emitted).
        assert!(steps.contains("git push -u origin"));
        assert!(!steps.contains("--force"));
        assert!(!steps.contains("push -f"));
        assert!(steps.contains("--body-file output/demo-pr-body.md"));
    }

    #[test]
    fn manual_steps_skip_branch_line_on_feature_branch() {
        let r = readiness("main", "feat/x", false);
        let steps = manual_steps(&r, "demo", "output/demo-pr-body.md");
        assert!(!steps.contains("git switch -c"));
        assert!(steps.contains("gh pr create --base main --head feat/x"));
    }

    #[test]
    fn body_embeds_review_report_and_proof_pack_section() {
        // Bare workspace: review report degrades to fail-open claims, proof-pack
        // section reports "no zip yet" — nothing panics, body is well-formed.
        let tmp = TempDir::new().unwrap();
        let body = render_pr_body(tmp.path(), "demo");
        assert!(body.contains("# Review report — demo"));
        assert!(body.contains("## Proof pack"));
        assert!(body.contains("No proof-pack zip yet"));
        assert!(body.contains("generated by UmaDev"));
    }

    #[test]
    fn proof_pack_summary_names_latest_zip() {
        let tmp = TempDir::new().unwrap();
        let release = tmp.path().join("release");
        fs::create_dir_all(&release).unwrap();
        fs::write(release.join("proof-pack-demo-001.zip"), vec![0u8; 2048]).unwrap();
        fs::write(release.join("proof-pack-demo-002.zip"), vec![0u8; 4096]).unwrap();
        // Non-pack files are ignored.
        fs::write(release.join("notes.txt"), "x").unwrap();
        let summary = proof_pack_summary(tmp.path(), "demo");
        // Latest by sort order is ...-002.
        assert!(summary.contains("proof-pack-demo-002.zip"));
        assert!(summary.contains("4 KiB"));
    }

    #[test]
    fn assess_on_non_repo_is_fail_open_not_ready() {
        // A plain temp dir is not a git repo → not ready, but no panic, and the
        // first blocker is the git-repo check.
        let tmp = TempDir::new().unwrap();
        let r = assess_readiness(tmp.path());
        assert_eq!(r.checks.len(), 5);
        assert!(!r.ready());
        assert_eq!(r.first_blocker().unwrap().id, "git-repo");
        // Rendering the manual steps over a not-ready assessment never panics.
        let steps = manual_steps(&r, "demo", &pr_body_rel_path("demo"));
        assert!(steps.contains("git init"));
    }

    #[test]
    fn body_rel_path_is_stable() {
        assert_eq!(pr_body_rel_path("app"), "output/app-pr-body.md");
    }
}
