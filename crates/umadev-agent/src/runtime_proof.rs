//! Runtime proof — the engine's "does the app actually RUN?" capability.
//!
//! Plain [`verify`](crate::verify) proves the project *compiles and its tests
//! pass*. That is necessary but not sufficient evidence of a working delivery:
//! a build can be green while the app fails to boot, the dev server crashes on
//! start, or the documented routes 500. This module closes that gap by
//! producing **runtime evidence** — it boots the detected dev server, waits for
//! it to answer, and probes the real routes over HTTP.
//!
//! The flow (all fail-open — any failure degrades to a recorded reason, never a
//! panic or a blocked host):
//!
//! 1. **Detect** the dev-server command via
//!    [`crate::verify::detect_dev_server`] (Vite / Next / Astro / CRA / generic
//!    `dev` script / static-file server).
//! 2. **Spawn** that command as a child process in the workspace.
//! 3. **Poll readiness** by shelling out to `curl` against the base URL until it
//!    answers or a budget elapses. `curl` is used deliberately — it is
//!    near-universal and needs no new crate dependency or model endpoint.
//! 4. **Probe routes**: read `.umadev/contracts/openapi.json` (written by the
//!    contract/adopt stage) and `curl` each documented path, recording
//!    `{path, status, ms}`. With no contract, at least the root path is probed.
//! 5. **Optional e2e**: if a Playwright/Cypress config or a `test:e2e` script is
//!    present, run it once and capture the outcome.
//! 6. **Tear down**: the child is killed (`kill_on_drop`) so no dev server is
//!    left running.
//!
//! The structured [`RuntimeProof`] is serialized to
//! `.umadev/audit/runtime-proof.json` and folded into the delivery proof-pack
//! (see `phases::build_and_zip_proof_pack`). User-facing prose lives in the
//! binary (which owns the i18n catalog); this crate stays dependency-light and
//! emits machine-readable data plus a neutral one-line summary.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::verify::detect_dev_server;

/// Cap captured e2e / probe-body output so a chatty run can't bloat the JSON.
const CAPTURE_CAP: usize = 8 * 1024;

/// How long (seconds) we wait for the dev server to answer its base URL before
/// giving up. A cold `npm run dev` (install already done) usually answers in a
/// few seconds; we allow generous headroom for slower machines / first builds.
const READY_TIMEOUT_SECS: u64 = 60;

/// Poll interval (milliseconds) while waiting for readiness.
const READY_POLL_MS: u64 = 500;

/// Per-route `curl` timeout (seconds). A live route answers fast; a hang here
/// means the route is effectively down, which is itself a finding.
const PROBE_TIMEOUT_SECS: u64 = 10;

/// Budget (seconds) for the optional e2e step.
const E2E_TIMEOUT_SECS: u64 = 600;

/// Whether the runtime check ran end-to-end or degraded (and why). This is the
/// top-level verdict the proof-pack and the CLI surface.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "reason")]
pub enum RuntimeStatus {
    /// The dev server booted, answered its base URL, and routes were probed.
    Verified,
    /// The check could not complete; the payload is a short machine reason
    /// (e.g. `"no dev server detected"`, `"curl not found"`,
    /// `"server did not become ready within 60s"`). Fail-open: this is a
    /// neutral "not verified", never an error.
    NotVerified(String),
}

impl RuntimeStatus {
    /// `true` iff the runtime was actually exercised end-to-end.
    #[must_use]
    pub fn is_verified(&self) -> bool {
        matches!(self, RuntimeStatus::Verified)
    }

    /// Stable label for audit rows / display switches.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeStatus::Verified => "verified",
            RuntimeStatus::NotVerified(_) => "not_verified",
        }
    }
}

/// One route probe result: the path we hit, the HTTP status we got, and how
/// long it took. `status` is `0` when `curl` could not get any response at all
/// (connection refused / timeout) — distinct from a real `5xx`.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RouteProbe {
    /// Path probed, relative to the base URL (e.g. `/` or `/api/users`).
    pub path: String,
    /// HTTP status code; `0` means "no response received".
    pub status: u16,
    /// Round-trip wall-clock duration, milliseconds.
    pub ms: u64,
    /// `true` when the status is a non-error response (`< 400`). A `2xx`/`3xx`
    /// proves the route is wired; `4xx` on a contract route (e.g. missing auth)
    /// still proves the server is *up* but is flagged for the reader.
    pub ok: bool,
}

/// The full runtime-proof record. Serialized to
/// `.umadev/audit/runtime-proof.json` and embedded in the proof-pack.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeProof {
    /// ISO-8601 timestamp the check ran.
    pub timestamp: String,
    /// Top-level verdict.
    pub status: RuntimeStatus,
    /// Human label of the dev server we tried (e.g. "Vite dev server"), if one
    /// was detected.
    pub dev_server: Option<String>,
    /// The exact command we spawned, if any.
    pub command: Option<String>,
    /// Base URL we polled / probed against.
    pub base_url: Option<String>,
    /// Milliseconds from spawn until the base URL first answered. `None` when
    /// the server never became ready.
    pub ready_ms: Option<u64>,
    /// Per-route probe results.
    pub routes: Vec<RouteProbe>,
    /// Optional e2e step outcome (`None` when no e2e suite was detected).
    pub e2e: Option<E2eResult>,
}

/// Outcome of the optional e2e suite (Playwright / Cypress / `test:e2e`).
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct E2eResult {
    /// The command we ran.
    pub command: String,
    /// `true` iff the suite exited 0.
    pub passed: bool,
    /// Duration, milliseconds.
    pub ms: u64,
    /// Truncated combined output (last words, capped).
    pub output: String,
}

impl RuntimeProof {
    /// Build a "not verified" record carrying only the reason — used on every
    /// fail-open early return so the artifact is still produced.
    fn not_verified(reason: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            status: RuntimeStatus::NotVerified(reason.into()),
            dev_server: None,
            command: None,
            base_url: None,
            ready_ms: None,
            routes: Vec::new(),
            e2e: None,
        }
    }

    /// A neutral, language-agnostic one-line summary (the binary localizes the
    /// real user message; this is for logs / the proof-pack summary file).
    #[must_use]
    pub fn summary_line(&self) -> String {
        match &self.status {
            RuntimeStatus::Verified => {
                let ok = self.routes.iter().filter(|r| r.ok).count();
                let total = self.routes.len();
                let base = self.base_url.as_deref().unwrap_or("(unknown)");
                format!("runtime verified: {base} ready, {ok}/{total} route(s) answered")
            }
            RuntimeStatus::NotVerified(reason) => {
                format!("runtime not verified: {reason}")
            }
        }
    }
}

/// Run the full runtime-proof flow against `workspace`. Always returns a
/// [`RuntimeProof`] — on any failure it degrades to
/// [`RuntimeStatus::NotVerified`] with a reason, never an `Err`/panic. This is
/// the single entry point the CLI / runner call.
pub async fn run_runtime_proof(workspace: &Path) -> RuntimeProof {
    // 0. `curl` is the readiness/probe transport. No curl → cannot verify.
    if !has_curl() {
        return RuntimeProof::not_verified("curl not found on PATH");
    }

    // 1. Detect the dev server command. None → nothing to boot.
    let Some(dev) = detect_dev_server(workspace) else {
        return RuntimeProof::not_verified("no dev server detected");
    };
    let base_url = dev.default_url.to_string();

    // 2. Spawn the dev server as a child process.
    let (program, args) = split_command(dev.command);
    let (vprog, vlead) = spawn_parts(&program);
    let spawn = Command::new(vprog)
        .args(&vlead)
        .args(&args)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawn {
        Ok(c) => c,
        Err(e) => {
            let mut proof = RuntimeProof::not_verified(format!("failed to start dev server: {e}"));
            proof.dev_server = Some(dev.label.to_string());
            proof.command = Some(dev.command.to_string());
            proof.base_url = Some(base_url);
            return proof;
        }
    };

    // 3. Poll readiness against the base URL.
    let started = Instant::now();
    let ready = wait_until_ready(&base_url, READY_TIMEOUT_SECS).await;
    let ready_ms = ready.map(|d| d.as_millis().try_into().unwrap_or(u64::MAX));

    let mut proof = RuntimeProof {
        timestamp: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        status: RuntimeStatus::Verified,
        dev_server: Some(dev.label.to_string()),
        command: Some(dev.command.to_string()),
        base_url: Some(base_url.clone()),
        ready_ms,
        routes: Vec::new(),
        e2e: None,
    };

    if ready_ms.is_none() {
        // Never came up: kill the child and report a neutral non-verification.
        let _ = child.start_kill();
        proof.status = RuntimeStatus::NotVerified(format!(
            "server did not become ready within {READY_TIMEOUT_SECS}s"
        ));
        let _ = started; // (kept for clarity; ready measured inside wait_until_ready)
        return proof;
    }

    // 4. Probe the documented routes (from the contract), else just the root.
    let paths = contract_route_paths(workspace);
    let probe_paths = if paths.is_empty() {
        vec!["/".to_string()]
    } else {
        paths
    };
    for path in &probe_paths {
        proof.routes.push(probe_route(&base_url, path).await);
    }

    // 5. Optional e2e suite.
    proof.e2e = run_e2e_if_present(workspace).await;

    // 6. Tear down — explicit kill so no dev server is left running. (drop also
    //    kills via kill_on_drop, but we do it eagerly so the port frees now.)
    let _ = child.start_kill();
    let _ = child.wait().await;

    proof
}

/// Persist the proof to `.umadev/audit/runtime-proof.json`. Returns the path on
/// success; fail-open (`Err`) is swallowed by callers — a write failure must
/// not block delivery.
pub fn write_runtime_proof(workspace: &Path, proof: &RuntimeProof) -> std::io::Result<PathBuf> {
    let audit_dir = workspace.join(".umadev/audit");
    std::fs::create_dir_all(&audit_dir)?;
    let path = audit_dir.join("runtime-proof.json");
    let body = serde_json::to_string_pretty(proof).unwrap_or_else(|_| "{}".into());
    std::fs::write(&path, body)?;
    Ok(path)
}

/// The canonical location of the runtime-proof artifact relative to the
/// workspace root. Used by the proof-pack assembler so it stays in sync.
#[must_use]
pub fn runtime_proof_rel_path() -> &'static str {
    ".umadev/audit/runtime-proof.json"
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

/// Whether `curl` is on PATH.
fn has_curl() -> bool {
    which("curl")
}

/// Split a shell-ish command string ("npm run dev") into (program, args).
/// Intentionally simple whitespace split — the dev-server commands we generate
/// in [`detect_dev_server`] never contain quotes or shell operators.
fn split_command(cmd: &str) -> (String, Vec<String>) {
    let mut parts = cmd.split_whitespace().map(str::to_string);
    let program = parts.next().unwrap_or_default();
    (program, parts.collect())
}

/// Poll `base_url` with `curl` every [`READY_POLL_MS`] until it answers (any
/// HTTP status counts as "up") or `budget_secs` elapses. Returns the elapsed
/// time on success, `None` on timeout.
async fn wait_until_ready(base_url: &str, budget_secs: u64) -> Option<Duration> {
    let started = Instant::now();
    let deadline = Duration::from_secs(budget_secs);
    while started.elapsed() < deadline {
        if curl_status(base_url, 3).await.is_some() {
            return Some(started.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(READY_POLL_MS)).await;
    }
    None
}

/// Probe one route: `curl` `base + path`, recording status + duration.
async fn probe_route(base_url: &str, path: &str) -> RouteProbe {
    let url = join_url(base_url, path);
    let started = Instant::now();
    let status = curl_status(&url, PROBE_TIMEOUT_SECS).await.unwrap_or(0);
    let ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    RouteProbe {
        path: path.to_string(),
        status,
        ms,
        ok: status != 0 && status < 400,
    }
}

/// Run `curl -s -o /dev/null -w "%{http_code}" --max-time <secs> <url>` and
/// parse the printed status code. Returns `None` when curl can't connect (exit
/// non-zero, or a `000` status — curl's "no response" sentinel).
async fn curl_status(url: &str, max_time_secs: u64) -> Option<u16> {
    let null_sink = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let out = Command::new("curl")
        .arg("-s")
        .arg("-o")
        .arg(null_sink)
        .arg("-w")
        .arg("%{http_code}")
        .arg("--max-time")
        .arg(max_time_secs.to_string())
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let code = String::from_utf8_lossy(&out.stdout);
    parse_http_code(code.trim())
}

/// Parse curl's `%{http_code}` output. `000` is curl's "no response" sentinel →
/// `None`. A real `2xx`–`5xx` → `Some(code)`.
fn parse_http_code(s: &str) -> Option<u16> {
    let code: u16 = s.parse().ok()?;
    if code == 0 {
        None
    } else {
        Some(code)
    }
}

/// Join a base URL and a path without doubling or dropping the `/`.
fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.is_empty() {
        return base.to_string();
    }
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// Read the route paths from the adopt/contract-stage `openapi.json` in
/// `.umadev/contracts/`. Returns a de-duplicated, ordered list of paths. An
/// absent / malformed contract yields an empty list (caller falls back to `/`).
///
/// Templated path segments (`{id}`, `:id`) are substituted with a placeholder
/// so the probe hits a concrete URL rather than a literal `{id}` that would
/// always 404.
fn contract_route_paths(workspace: &Path) -> Vec<String> {
    let openapi = workspace.join(".umadev/contracts/openapi.json");
    let Ok(body) = std::fs::read_to_string(&openapi) else {
        return Vec::new();
    };
    parse_openapi_paths(&body)
}

/// Pure parse of an OpenAPI JSON document's `paths` keys. Split out from disk
/// I/O so it's unit-testable. Only `GET`-safe probing is intended, but here we
/// just collect the path keys; the prober treats every path as a read.
fn parse_openapi_paths(body: &str) -> Vec<String> {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(paths) = doc.get("paths").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for key in paths.keys() {
        let concrete = concretize_path(key);
        if !out.contains(&concrete) {
            out.push(concrete);
        }
    }
    out
}

/// Replace templated path params with a concrete placeholder so a probe lands
/// on a real handler instead of a literal `{id}` / `:id` (which 404s).
fn concretize_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for seg in path.split('/') {
        if seg.is_empty() {
            continue;
        }
        out.push('/');
        if (seg.starts_with('{') && seg.ends_with('}')) || seg.starts_with(':') {
            out.push('1');
        } else {
            out.push_str(seg);
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// Detect + run an e2e suite once. Returns `None` when no suite is present.
/// Detection order: a `test:e2e` npm script → Playwright config → Cypress
/// config. Fail-open: a missing runner binary records a `passed:false` outcome
/// rather than erroring.
async fn run_e2e_if_present(workspace: &Path) -> Option<E2eResult> {
    let cmd = detect_e2e_command(workspace)?;
    let (program, args) = split_command(&cmd);
    let (vprog, vlead) = spawn_parts(&program);
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(E2E_TIMEOUT_SECS),
        Command::new(vprog)
            .args(&vlead)
            .args(&args)
            .current_dir(workspace)
            .stdin(Stdio::null())
            .output(),
    )
    .await;
    let ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    match result {
        Ok(Ok(out)) => {
            let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&out.stderr));
            truncate(&mut combined, CAPTURE_CAP);
            Some(E2eResult {
                command: cmd,
                passed: out.status.success(),
                ms,
                output: combined,
            })
        }
        Ok(Err(e)) => Some(E2eResult {
            command: cmd,
            passed: false,
            ms,
            output: format!("failed to spawn e2e runner: {e}"),
        }),
        Err(_) => Some(E2eResult {
            command: cmd,
            passed: false,
            ms,
            output: format!("e2e timed out after {E2E_TIMEOUT_SECS}s"),
        }),
    }
}

/// Decide the e2e command for `workspace`, or `None` if no e2e setup is found.
/// Pure-ish (only reads files) so it's testable via a temp dir.
fn detect_e2e_command(workspace: &Path) -> Option<String> {
    // 1. An explicit `test:e2e` script wins — it's what the project author meant.
    if package_json_has_script(workspace, "test:e2e") {
        return Some("npm run test:e2e".to_string());
    }
    // 2. Playwright config (any of the conventional names).
    for name in [
        "playwright.config.ts",
        "playwright.config.js",
        "playwright.config.mjs",
    ] {
        if workspace.join(name).is_file() {
            return Some("npx playwright test".to_string());
        }
    }
    // 3. Cypress config.
    for name in ["cypress.config.ts", "cypress.config.js"] {
        if workspace.join(name).is_file() {
            return Some("npx cypress run".to_string());
        }
    }
    None
}

/// Whether `package.json` declares a given script. Local copy (the verify
/// module's is private) — kept tiny on purpose.
fn package_json_has_script(workspace: &Path, script: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(workspace.join("package.json")) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    json.get("scripts").and_then(|s| s.get(script)).is_some()
}

/// Truncate a captured buffer at a char boundary, appending a marker.
fn truncate(s: &mut String, cap: usize) {
    if s.len() > cap {
        let mut idx = cap;
        while !s.is_char_boundary(idx) {
            idx -= 1;
        }
        s.truncate(idx);
        s.push_str("\n...[truncated]");
    }
}

/// Whether a PATH-resolvable binary exists. Mirrors the verify module's helper
/// (kept local so this module doesn't widen verify's surface). Honours
/// `PATHEXT` on Windows.
fn which(bin: &str) -> bool {
    let Ok(path_var) = std::env::var("PATH") else {
        return false;
    };
    let separator = if cfg!(windows) { ';' } else { ':' };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.BAT;.CMD;.COM".to_string())
            .split(';')
            .map(str::to_string)
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in path_var.split(separator) {
        if dir.is_empty() {
            continue;
        }
        for ext in &exts {
            let candidate = Path::new(dir).join(format!("{bin}{ext}"));
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

/// Resolve a bare program name to a spawnable path on Windows (npm shims are
/// `.cmd`/`.bat` that `Command::new` won't find), routing `.cmd`/`.bat` through
/// `cmd /c`. No-op off Windows. Returns `(program, leading_args)`. Mirrors the
/// verify module's private helper so dev-server spawn behaves the same.
fn spawn_parts(program: &str) -> (String, Vec<String>) {
    if !cfg!(windows) || program.contains(std::path::is_separator) {
        return (program.to_string(), Vec::new());
    }
    let Ok(path_var) = std::env::var("PATH") else {
        return (program.to_string(), Vec::new());
    };
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    for dir in path_var.split(';') {
        if dir.is_empty() {
            continue;
        }
        for ext in std::iter::once("").chain(pathext.split(';')) {
            let candidate = Path::new(dir).join(format!("{program}{ext}"));
            if candidate.is_file() {
                let resolved = candidate.to_string_lossy().into_owned();
                let lower_ext = ext.to_ascii_lowercase();
                if lower_ext == ".cmd" || lower_ext == ".bat" {
                    return ("cmd".to_string(), vec!["/c".to_string(), resolved]);
                }
                return (resolved, Vec::new());
            }
        }
    }
    (program.to_string(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_http_code_treats_000_as_no_response() {
        assert_eq!(parse_http_code("000"), None);
        assert_eq!(parse_http_code("0"), None);
        assert_eq!(parse_http_code("200"), Some(200));
        assert_eq!(parse_http_code("404"), Some(404));
        assert_eq!(parse_http_code("503"), Some(503));
        assert_eq!(parse_http_code(""), None);
        assert_eq!(parse_http_code("not-a-number"), None);
    }

    #[test]
    fn join_url_handles_slashes() {
        assert_eq!(join_url("http://x:3000", "/"), "http://x:3000/");
        assert_eq!(join_url("http://x:3000/", "/api"), "http://x:3000/api");
        assert_eq!(join_url("http://x:3000", "api"), "http://x:3000/api");
        assert_eq!(join_url("http://x:3000/", ""), "http://x:3000");
        assert_eq!(
            join_url("http://x:3000", "/api/users"),
            "http://x:3000/api/users"
        );
    }

    #[test]
    fn split_command_splits_program_and_args() {
        assert_eq!(
            split_command("npm run dev"),
            (
                "npm".to_string(),
                vec!["run".to_string(), "dev".to_string()]
            )
        );
        assert_eq!(
            split_command("python3 -m http.server 8000"),
            (
                "python3".to_string(),
                vec!["-m".into(), "http.server".into(), "8000".into()]
            )
        );
        assert_eq!(split_command(""), (String::new(), Vec::new()));
    }

    #[test]
    fn concretize_path_substitutes_templates() {
        assert_eq!(concretize_path("/"), "/");
        assert_eq!(concretize_path("/api/users"), "/api/users");
        assert_eq!(concretize_path("/api/users/{id}"), "/api/users/1");
        assert_eq!(concretize_path("/api/users/:id"), "/api/users/1");
        assert_eq!(concretize_path("/api/{org}/repos/:repo"), "/api/1/repos/1");
    }

    #[test]
    fn parse_openapi_paths_extracts_and_dedups() {
        let doc = r#"{
            "openapi": "3.1.0",
            "paths": {
                "/api/users": { "get": {} },
                "/api/users/{id}": { "get": {} },
                "/health": { "get": {} }
            }
        }"#;
        let paths = parse_openapi_paths(doc);
        assert!(paths.contains(&"/api/users".to_string()));
        assert!(paths.contains(&"/api/users/1".to_string()));
        assert!(paths.contains(&"/health".to_string()));
        assert_eq!(paths.len(), 3);
    }

    #[test]
    fn parse_openapi_paths_handles_garbage_and_missing() {
        assert!(parse_openapi_paths("not json").is_empty());
        assert!(parse_openapi_paths("{}").is_empty());
        assert!(parse_openapi_paths(r#"{"paths": "not-an-object"}"#).is_empty());
        assert!(parse_openapi_paths(r#"{"paths": {}}"#).is_empty());
    }

    #[test]
    fn contract_route_paths_reads_from_disk() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".umadev/contracts");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("openapi.json"),
            r#"{"paths":{"/api/ping":{"get":{}}}}"#,
        )
        .unwrap();
        let paths = contract_route_paths(tmp.path());
        assert_eq!(paths, vec!["/api/ping".to_string()]);
    }

    #[test]
    fn contract_route_paths_empty_when_no_contract() {
        let tmp = TempDir::new().unwrap();
        assert!(contract_route_paths(tmp.path()).is_empty());
    }

    #[test]
    fn detect_e2e_prefers_test_e2e_script() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"scripts":{"test:e2e":"playwright test"}}"#,
        )
        .unwrap();
        assert_eq!(
            detect_e2e_command(tmp.path()),
            Some("npm run test:e2e".to_string())
        );
    }

    #[test]
    fn detect_e2e_finds_playwright_config() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("playwright.config.ts"), "export default {}").unwrap();
        assert_eq!(
            detect_e2e_command(tmp.path()),
            Some("npx playwright test".to_string())
        );
    }

    #[test]
    fn detect_e2e_finds_cypress_config() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("cypress.config.js"), "module.exports = {}").unwrap();
        assert_eq!(
            detect_e2e_command(tmp.path()),
            Some("npx cypress run".to_string())
        );
    }

    #[test]
    fn detect_e2e_none_when_absent() {
        let tmp = TempDir::new().unwrap();
        assert!(detect_e2e_command(tmp.path()).is_none());
    }

    #[test]
    fn status_helpers() {
        assert!(RuntimeStatus::Verified.is_verified());
        assert!(!RuntimeStatus::NotVerified("x".into()).is_verified());
        assert_eq!(RuntimeStatus::Verified.as_str(), "verified");
        assert_eq!(
            RuntimeStatus::NotVerified("x".into()).as_str(),
            "not_verified"
        );
    }

    #[test]
    fn not_verified_summary_includes_reason() {
        let p = RuntimeProof::not_verified("no dev server detected");
        assert!(p.summary_line().contains("no dev server detected"));
        assert!(!p.status.is_verified());
        assert!(p.routes.is_empty());
        assert!(p.ready_ms.is_none());
    }

    #[test]
    fn verified_summary_counts_ok_routes() {
        let proof = RuntimeProof {
            timestamp: "2026-06-22T00:00:00Z".into(),
            status: RuntimeStatus::Verified,
            dev_server: Some("Vite dev server".into()),
            command: Some("npm run dev".into()),
            base_url: Some("http://localhost:5173".into()),
            ready_ms: Some(1200),
            routes: vec![
                RouteProbe {
                    path: "/".into(),
                    status: 200,
                    ms: 12,
                    ok: true,
                },
                RouteProbe {
                    path: "/api/users".into(),
                    status: 500,
                    ms: 30,
                    ok: false,
                },
            ],
            e2e: None,
        };
        let line = proof.summary_line();
        assert!(line.contains("1/2 route(s) answered"), "line was: {line}");
        assert!(line.contains("http://localhost:5173"));
    }

    #[test]
    fn write_runtime_proof_serializes_json() {
        let tmp = TempDir::new().unwrap();
        let proof = RuntimeProof::not_verified("curl not found on PATH");
        let path = write_runtime_proof(tmp.path(), &proof).unwrap();
        assert_eq!(path, tmp.path().join(".umadev/audit/runtime-proof.json"));
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("not_verified"));
        assert!(body.contains("curl not found on PATH"));
        // Round-trips back to the same struct.
        let parsed: RuntimeProof = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed, proof);
    }

    #[test]
    fn rel_path_matches_write_location() {
        let tmp = TempDir::new().unwrap();
        let proof = RuntimeProof::not_verified("x");
        let written = write_runtime_proof(tmp.path(), &proof).unwrap();
        let expected = tmp.path().join(runtime_proof_rel_path());
        assert_eq!(written, expected);
    }

    #[tokio::test]
    async fn run_runtime_proof_no_dev_server_is_not_verified() {
        // An empty workspace has no dev server → fail-open "not verified",
        // never a panic. (If curl is missing on the CI box, the reason differs
        // but it's still NotVerified — assert on the negative, not the text.)
        let tmp = TempDir::new().unwrap();
        let proof = run_runtime_proof(tmp.path()).await;
        assert!(!proof.status.is_verified());
        assert!(proof.routes.is_empty());
    }

    #[tokio::test]
    async fn probe_route_unreachable_is_status_zero() {
        // Probing a port nothing listens on yields status 0 (no response), ok=false.
        // Skip when curl is unavailable (the function would early-return None upstream).
        if !has_curl() {
            return;
        }
        let probe = probe_route("http://127.0.0.1:1", "/").await;
        assert_eq!(probe.status, 0);
        assert!(!probe.ok);
        assert_eq!(probe.path, "/");
    }

    #[test]
    fn truncate_keeps_char_boundary() {
        let mut s = "做做做做做".to_string();
        truncate(&mut s, 7);
        assert!(s.ends_with("[truncated]"));
        let _ = s.as_bytes(); // valid UTF-8, no panic
    }
}
