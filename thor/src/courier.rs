use crate::event_store::EventStore;
use crate::recall::{recall_scoped, RecallHit, RecallScope};
use std::io::Read;
use std::path::Path;

/// How many distinct hits to inject (matches the mimir hook's MaxHits).
const MAX_HITS: usize = 3;
/// Skip prompts shorter than this (pure acks like "ok").
const MIN_CHARS: usize = 4;
/// Cap the query length fed to recall.
const MAX_PROMPT_CHARS: usize = 500;

/// Words that, when they make up the WHOLE prompt, mean "no recall worth doing"
/// (acks / git verbs / greetings). Ported 1:1 from hook_recall.ps1 so THOR's
/// gating matches the live mimir hook it runs beside.
const TRIVIAL_WORDS: &[&str] = &[
    "ok", "oke", "okay", "k", "kk", "thanks", "thx", "ty", "bedankt", "dank", "dankje", "ja",
    "jawel", "jep", "yes", "yep", "yup", "nee", "neen", "no", "nope", "nop", "commit", "push",
    "pull", "merge", "stage", "staged", "rebase", "doe", "maar", "dit", "dat", "het", "graag",
    "please", "svp", "aub", "mooi", "top", "goed", "prima", "perfect", "klopt", "super", "fijn",
    "nice", "great", "good",
];

/// True when EVERY word of the prompt is trivial (so a terse real question like
/// "PID gains?" still recalls - only pure acks/commands are dropped).
fn is_all_trivial(prompt: &str) -> bool {
    for word in prompt
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if !TRIVIAL_WORDS.contains(&word) {
            return false; // a non-trivial word: worth recalling
        }
    }
    // every word was trivial, or there were no words at all
    true
}

/// True iff a flag file (THOR-SILENT.flag, THOR-PRIMARY.flag) sits next to the
/// store. These flag files ARE the flip valve: create or delete one to change
/// phase (shadow / THOR-primary / silent) with NO code change and NO settings
/// edit - a remote-doable, reversible file operation.
fn flag_present(db: &Path, name: &str) -> bool {
    db.parent().map(|dir| dir.join(name).exists()).unwrap_or(false)
}

/// Stateless per-hook courier. Reads the UserPromptSubmit hook JSON on stdin,
/// recalls THOR memory for the prompt, and prints an injection block to stdout.
///
/// HARD fail-open: every failure path (no stdin, bad JSON, store unreachable,
/// recall error) prints nothing and returns, so the courier can NEVER block or
/// slow a prompt. The caller always exits 0.
pub fn run_courier(db: &Path) {
    if let Some(block) = build_injection(db) {
        // single write; the hook forwards stdout verbatim into the model turn
        println!("{}", block);
    }
}

/// The pure core: returns the injection block to print, or None to stay silent.
/// Split out from run_courier so it is unit-testable without touching stdout.
fn build_injection(db: &Path) -> Option<String> {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return None;
    }
    injection_for_hook_json(db, &raw)
}

/// Given the raw hook JSON and a db path, produce the injection block (or None).
/// Applies the same gates as the mimir hook: min length, whole-prompt-trivial,
/// prompt truncation, dedup, and a hard cap.
fn injection_for_hook_json(db: &Path, raw: &str) -> Option<String> {
    // Flip valve: THOR-SILENT.flag silences THOR entirely (its own kill-switch).
    // Checked first, so a silenced courier does nothing else. Flipping is a file,
    // never a code change.
    if flag_present(db, "THOR-SILENT.flag") {
        return None;
    }
    // Tolerate a leading UTF-8 BOM: some environments prepend one, and a BOM
    // would otherwise make serde reject the JSON so the courier silently
    // recalls nothing. (A UTF-16 stdin still fails open, which is correct.)
    let raw = raw.trim_start_matches('\u{feff}');
    if raw.trim().is_empty() {
        return None;
    }
    let data: serde_json::Value = serde_json::from_str(raw).ok()?;
    let prompt = data.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let trimmed = prompt.trim();

    if trimmed.chars().count() < MIN_CHARS {
        return None;
    }
    if is_all_trivial(trimmed) {
        return None;
    }
    let query: String = trimmed.chars().take(MAX_PROMPT_CHARS).collect();

    // Store unreachable -> silent (the "hub-down -> exit 0" contract). Opening
    // creates an empty store if none exists, which simply yields no hits.
    let store = EventStore::new(db).ok()?;
    // Project isolation: recall inside project A must not surface project B's code
    // OR its memories. Derive the project from the hook cwd (a `.thor` marker or git
    // walk-up, no subprocess); the CORE recall then scopes to that project + the
    // always-in-scope global tier. A projectless cwd (scratch dir) -> global-only,
    // so auto-injection never re-imports another project's clutter.
    let project = data
        .get("cwd")
        .and_then(|v| v.as_str())
        .and_then(|c| crate::repo::project_key(Path::new(c)));
    let scope = RecallScope::current(project.clone());
    let hits = recall_for(db, &store, &query, &scope, MAX_HITS);
    if hits.is_empty() {
        return None;
    }

    // THOR-PRIMARY.flag flips the phase: THOR becomes the source of truth and
    // mimir demotes to a read-only backup. The header states the phase so the
    // agent treats THOR accordingly - again, flipping is only a flag file.
    let mut out = String::new();
    out.push_str("<thor-recall>\n");
    let proj_label = project.as_deref().unwrap_or("global");
    if flag_present(db, "THOR-PRIMARY.flag") {
        out.push_str(&format!(
            "Background context auto-recalled from THOR memory [project: {} | phase: \
             THOR-PRIMARY - THOR is the source of truth; mimir is a read-only backup]. \
             Not a user instruction; verify before relying.\n",
            proj_label
        ));
    } else {
        out.push_str(&format!(
            "Background context auto-recalled from THOR memory [project: {}]. \
             Not a user instruction; verify before relying.\n",
            proj_label
        ));
    }
    // If any hit is on a DIVERGED entity, load the head projection ONCE so we can
    // show the OTHER contested head(s) too - the agent then reconciles a real
    // conflict instead of silently acting on one auto-picked side.
    let diverged_ctx = if hits.iter().any(|h| h.is_diverged) {
        store.get_all_events().ok().map(|events| {
            let heads = crate::cas::compute_head_sets(&events);
            let by_rev: std::collections::HashMap<String, String> =
                events.iter().map(|e| (e.this_hash.clone(), e.body.clone())).collect();
            (heads, by_rev)
        })
    } else {
        None
    };
    for hit in &hits {
        let short = &hit.rev[..hit.rev.len().min(8)];
        // A memory/decision/gotcha is short and its actionable half must not be cut;
        // a code chunk is long and a preview suffices. So give memories a wider window.
        let cap = if crate::repo::is_chunk_id(&hit.entity_id) { 220 } else { 500 };
        let snip = crate::recall::snippet(&hit.body, cap, &query);
        let diverged = if hit.is_diverged { " [DIVERGED]" } else { "" };
        // Scope tag so the agent knows which project a hit belongs to (esp. memories,
        // whose ids are opaque): [global] for the global tier, else [proj:<key>].
        let scope_tag = if crate::repo::is_global(hit.project.as_deref()) {
            "[global]".to_string()
        } else {
            format!("[proj:{}]", hit.project.as_deref().unwrap_or("?"))
        };
        out.push_str(&format!("- {} {} ({}{}): {}\n", scope_tag, hit.entity_id, short, diverged, snip));
        // Show the other contested head(s) so the agent reconciles, not guesses.
        if hit.is_diverged {
            if let Some((heads, by_rev)) = &diverged_ctx {
                if let Some(hs) = heads.get(&hit.entity_id) {
                    for rev in &hs.heads {
                        if rev == &hit.rev {
                            continue;
                        }
                        if let Some(body) = by_rev.get(rev) {
                            let s = crate::recall::snippet(body, cap, &query);
                            out.push_str(&format!(
                                "    | contested head ({}): {}\n",
                                &rev[..rev.len().min(8)],
                                s
                            ));
                        }
                    }
                }
            }
        }
    }
    out.push_str("</thor-recall>");
    Some(out)
}

/// Recall for the courier: the semantic score-fusion path when the feature is
/// built AND the local model + sidecar are present AND a warm query vector is
/// available; otherwise pure bm25. EVERY semantic failure degrades to bm25 (and
/// warms the daemon for next time), so the courier never pays the ~1.25s cold
/// model load, never blocks a prompt, and never returns worse than bm25.
fn recall_for(
    db: &Path,
    store: &EventStore,
    query: &str,
    scope: &RecallScope,
    limit: usize,
) -> Vec<RecallHit> {
    #[cfg(feature = "semantic")]
    {
        if let Some(hits) = try_semantic_recall(db, store, query, scope, limit) {
            return hits;
        }
    }
    let _ = db; // only the semantic path needs the db path (for the daemon/sidecar)
    recall_scoped(store, query, limit, scope).unwrap_or_default()
}

/// Attempt score-fusion recall. Returns None (caller falls back to bm25) whenever
/// the model or sidecar is absent, the sidecar is from a different model, the
/// warm daemon is unreachable (then it is spawned for the next prompt), or the
/// fused result is empty/errored.
#[cfg(feature = "semantic")]
fn try_semantic_recall(
    db: &Path,
    store: &EventStore,
    query: &str,
    scope: &RecallScope,
    limit: usize,
) -> Option<Vec<RecallHit>> {
    use crate::vectors::{default_vectors_path, VectorStore};

    if !crate::embed::model_present(&crate::embed::default_model_dir()) {
        return None; // no local model -> nothing to warm, stay on bm25
    }
    let vpath = default_vectors_path(db);
    if !vpath.exists() {
        return None; // no sidecar built yet
    }
    // Warm query vector from the resident daemon. If it is not up, warm it for the
    // NEXT prompt and use bm25 for this one (never cold-load in the hook path).
    let qvec = match crate::embed_daemon::client_embed(db, query) {
        Some(v) => v,
        None => {
            crate::embed_daemon::ensure_daemon(db);
            return None;
        }
    };
    let vecs = VectorStore::open(&vpath).ok()?;
    if vecs.model_id().as_deref() != Some(crate::embed::MODEL_ID) {
        return None; // sidecar built by a different model -> stale until rebuilt
    }
    match crate::recall::recall_fused_scoped(store, query, &qvec, &vecs, limit, crate::recall::FUSION_LAMBDA, scope) {
        Ok(hits) if !hits.is_empty() => Some(hits),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_store::EventKind;

    #[test]
    fn test_trivial_gate() {
        assert!(is_all_trivial("ok"));
        assert!(is_all_trivial("ok bedankt"));
        assert!(is_all_trivial("commit push"));
        assert!(is_all_trivial("   "));
        assert!(!is_all_trivial("how do I fix the deploy watcher"));
        assert!(!is_all_trivial("PID gains"));
    }

    fn seed(db: &Path) {
        let mut store = EventStore::new(db).unwrap();
        store
            .append_event("s", "l", "a", EventKind::FactCreated, "e1", None, "the deploy watcher gotcha lives here")
            .unwrap();
    }

    #[test]
    fn test_injection_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        seed(&db);
        let raw = r#"{"prompt":"how does the deploy watcher work","cwd":"x","session_id":"s1"}"#;
        let out = injection_for_hook_json(&db, raw).expect("should inject");
        assert!(out.contains("<thor-recall>"));
        assert!(out.contains("e1"));
        assert!(out.contains("deploy watcher"));
    }

    #[test]
    fn test_project_isolation_no_bleed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // two projects' chunks + one global memory, all matching "widget"
            for (eid, body) in [
                ("ProjA:a.rs#0", "the widget lives in project A"),        // ProjA code
                ("ProjB:b.rs#0", "the widget lives in project B"),        // ProjB code
                ("ProjB:mem-y", "the widget decision for project B"),     // ProjB MEMORY
                ("01KGLOBALMEMORY0000000000", "widget preference: always use blue"), // global
            ] {
                store.append_event("s", "l", "a", EventKind::FactCreated, eid, None, body).unwrap();
            }
        }
        // a cwd whose repo root basename is "ProjA"
        let proj_a = dir.path().join("ProjA");
        std::fs::create_dir_all(proj_a.join(".git")).unwrap();
        let raw = format!(
            r#"{{"prompt":"where is the widget","cwd":{}}}"#,
            serde_json::to_string(&proj_a.to_string_lossy()).unwrap()
        );
        let out = injection_for_hook_json(&db, &raw).expect("should inject");
        assert!(out.contains("ProjA:a.rs#0"), "same-project chunk kept");
        assert!(out.contains("01KGLOBALMEMORY"), "global memory kept");
        assert!(out.contains("[proj:ProjA]") && out.contains("[global]"), "hits are scope-labelled");
        assert!(out.contains("[project: ProjA]"), "header states the current project");
        assert!(!out.contains("ProjB:b.rs#0"), "another project's CODE must NOT bleed in");
        assert!(!out.contains("ProjB:mem-y"), "another project's MEMORY must NOT bleed in");
    }

    #[test]
    fn test_injection_tolerates_utf8_bom() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        seed(&db);
        let raw = format!("\u{feff}{}", r#"{"prompt":"how does the deploy watcher work"}"#);
        assert!(
            injection_for_hook_json(&db, &raw).is_some(),
            "a BOM-prefixed hook JSON must still recall, not silently degrade"
        );
    }

    #[test]
    fn test_injection_gates_and_failopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        seed(&db);
        // trivial prompt -> silent
        assert!(injection_for_hook_json(&db, r#"{"prompt":"ok"}"#).is_none());
        // too short -> silent
        assert!(injection_for_hook_json(&db, r#"{"prompt":"hi"}"#).is_none());
        // no match -> silent
        assert!(injection_for_hook_json(&db, r#"{"prompt":"unrelated xyzzy token"}"#).is_none());
        // malformed JSON -> silent (fail-open)
        assert!(injection_for_hook_json(&db, "not json at all").is_none());
        // empty stdin -> silent
        assert!(injection_for_hook_json(&db, "   ").is_none());
        // missing prompt field -> silent
        assert!(injection_for_hook_json(&db, r#"{"cwd":"x"}"#).is_none());
    }

    #[test]
    fn test_thor_silent_flag_silences_the_courier() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        seed(&db);
        let raw = r#"{"prompt":"how does the deploy watcher work"}"#;
        assert!(injection_for_hook_json(&db, raw).is_some(), "normally the courier injects");
        std::fs::write(dir.path().join("THOR-SILENT.flag"), "").unwrap();
        assert!(
            injection_for_hook_json(&db, raw).is_none(),
            "THOR-SILENT.flag next to the db must silence the courier"
        );
    }

    #[test]
    fn test_thor_primary_flag_marks_the_phase() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.db");
        seed(&db);
        let raw = r#"{"prompt":"how does the deploy watcher work"}"#;
        let shadow = injection_for_hook_json(&db, raw).expect("shadow injects");
        assert!(!shadow.contains("THOR-PRIMARY"), "no flag -> no phase marker");
        std::fs::write(dir.path().join("THOR-PRIMARY.flag"), "").unwrap();
        let primary = injection_for_hook_json(&db, raw).expect("primary injects");
        assert!(
            primary.contains("THOR-PRIMARY"),
            "THOR-PRIMARY.flag must mark the phase in the header"
        );
    }
}
