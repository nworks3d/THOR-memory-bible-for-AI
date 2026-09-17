//! R3: delivery is observable. Every item actually shown in a block writes an
//! `ItemServed` event (the variant already exists in `thor_core::event_store`,
//! unwired until this crate) so "when did this last fire" and "how often" are
//! answerable, and "declared but never delivered" is a query (see `audit.rs`).
//!
//! This is called ONLY from the `hook` channel - the real production
//! delivery boundary. `check`/`why` call the same select+render path to
//! PREVIEW a block but must not count as a real firing, or "how often did
//! this actually fire" would be inflated by every dry run a human tried.
//! This split is a design choice the brief itself does not spell out (it says
//! "elke levering", every delivery) - flagged here rather than assumed silently.
//!
//! Fails open exactly like the rest of the read/serve boundary: a failed
//! write here must never stop the block that was already decided from
//! reaching stdout, and must never make `hook` speak or exit non-zero.

use model::served::ItemServed;
use thor_core::event_store::{EventKind, EventStore};

/// Record one `ItemServed` event per shown item id, with no `trigger` - the
/// plain entry point for every caller that either has none to give (`hook`'s
/// own `UserPromptSubmit` arm - a prompt is not a command, a file, or the
/// pinned `SessionStart` block) or does not care (every test in this
/// workspace that seeds servings without meaning to prove anything about
/// where they fired). Strictly a thin call onto `record_delivery_with_
/// trigger` below, never a second copy of the write - see that function's
/// own doc comment for what a trigger is and where the three real
/// production call sites (`bin/serve.rs`) get theirs.
pub fn record_delivery(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    served_at: &str,
    ids: &[String],
) {
    record_delivery_with_trigger(store, session_id, lineage_id, actor, served_at, ids, None);
}

/// The longest `trigger` a served event records verbatim - a command line
/// can run to any length, and the point of this field is naming the general
/// shape of where an item fired (see `ItemServed::trigger`'s own doc
/// comment), not reproducing a whole shell pipeline byte for byte. Chosen to
/// comfortably hold a realistic command or file path while keeping the body
/// small; truncated by CHARACTER count, not by byte-slicing, so a multi-byte
/// UTF-8 command (a path with a non-ASCII directory name, say) is never cut
/// mid-codepoint.
pub const TRIGGER_LIMIT: usize = 120;

/// Record one `ItemServed` event per shown item id, each carrying `trigger`
/// - see `ItemServed::trigger`'s own doc comment for what the three shapes
/// (a command, a file path, or `"session start"`) mean and the defect naming
/// them closes. Every other behaviour (fail-open on a broken log, one event
/// per id) is identical to `record_delivery` above, which is now simply this
/// function called with `None`.
///
/// `trigger` is truncated to `TRIGGER_LIMIT` HERE, once, rather than trusted
/// to every caller: `bin/serve.rs`'s own call sites pass a raw command line
/// or file path straight through, and a single shared truncation point is
/// the only way that limit can never quietly drift between them.
pub fn record_delivery_with_trigger(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    served_at: &str,
    ids: &[String],
    trigger: Option<&str>,
) {
    let trigger: Option<String> = trigger.map(|t| {
        if t.chars().count() > TRIGGER_LIMIT {
            t.chars().take(TRIGGER_LIMIT).collect()
        } else {
            t.to_string()
        }
    });
    for id in ids {
        let body = match serde_json::to_string(&ItemServed { served_at: served_at.to_string(), trigger: trigger.clone() }) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let _ = store.append_event(session_id, lineage_id, actor, EventKind::ItemServed, id, None, &body);
    }
}

/// Record what the write guard actually DID: refused a call, or stood aside
/// with something outstanding it chose not to raise again this session.
///
/// The stand-aside used to mean "a call it had already refused once", which is
/// what the file and command arms did until 2026-08-08. Neither does any more -
/// no prohibition stands aside at all. What is left is the stale-rule nudge at
/// Stop, which holds off for the rest of a session after it has been paid once.
///
/// WHY BOTH, AND WHY THE SECOND ONE MATTERS MORE. A refusal is visible to the
/// person it refused. A gate that quietly declines to look is visible to
/// nobody, which is exactly how the once-per-session marker survived from the
/// first day of 2.0 until four separate reviews read it out loud on
/// 2026-08-08. Until this function existed, nothing in the log recorded that
/// enforcement had happened at all: the gate was the whole reason this
/// version was built and the one capability with no measurement, which is the
/// third time this project has been caught by that exact shape.
///
/// Fail-silent like every other telemetry write here: a log that cannot take
/// a measurement must never cost a refusal.
pub fn record_gate_outcome(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    at: &str,
    refused: bool,
    entity_id: &str,
    target: &str,
) {
    let body = match serde_json::to_string(&serde_json::json!({ "at": at, "target": target })) {
        Ok(b) => b,
        Err(_) => return,
    };
    let kind = if refused { EventKind::GateRefused } else { EventKind::GateStoodAside };
    let _ = store.append_event(session_id, lineage_id, actor, kind, entity_id, None, &body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serving::serving_stats;

    #[test]
    fn recording_delivery_is_queryable_immediately_after_the_write() {
        let mut store = EventStore::in_memory().unwrap();
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["x1".to_string()]);
        let stats = serving_stats(&store);
        let s = stats.get("x1").expect("must be queryable right after the write, per R3");
        assert_eq!(s.times_served, 1);
        assert_eq!(s.last_served_at.as_deref(), Some("2026-08-02T12:00:00Z"));
    }

    /// `record_delivery` itself never carries a trigger (see its own doc
    /// comment) - proven here so the plain, no-trigger path this crate's
    /// many other tests already depend on stays pinned down alongside the
    /// new one below.
    #[test]
    fn recording_delivery_carries_no_trigger() {
        let mut store = EventStore::in_memory().unwrap();
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["x1".to_string()]);
        assert_eq!(model::served::last_trigger(&store, "x1"), None);
    }

    /// THE DEFECT THIS CLOSES: an evaluation could not see WHERE an owed
    /// item had actually been firing (measured 2026-09-17, acme-shop
    /// eval 1 and 2 - see `model::served::ItemServed::trigger`'s own doc
    /// comment). `record_delivery_with_trigger` is the half of the fix that
    /// writes it; `model::served::last_trigger` (proven in `model`'s own
    /// tests) is the half that reads it back.
    #[test]
    fn record_delivery_with_trigger_carries_the_trigger_in_the_body() {
        let mut store = EventStore::in_memory().unwrap();
        record_delivery_with_trigger(
            &mut store,
            "s",
            "l",
            "hook",
            "2026-09-17T00:00:00Z",
            &["x1".to_string()],
            Some("git push --force origin main"),
        );
        assert_eq!(model::served::last_trigger(&store, "x1").as_deref(), Some("git push --force origin main"));
    }

    /// THE TRUNCATION: a command line can run to any length, and the point
    /// of this field is naming the general shape of where an item fired, not
    /// reproducing a whole shell pipeline verbatim - so a trigger longer than
    /// `TRIGGER_LIMIT` is cut down to it, at this one write site, rather than
    /// trusting every future caller to remember to.
    #[test]
    fn a_trigger_longer_than_the_limit_is_truncated() {
        let mut store = EventStore::in_memory().unwrap();
        let long = "x".repeat(TRIGGER_LIMIT + 50);
        record_delivery_with_trigger(&mut store, "s", "l", "hook", "2026-09-17T00:00:00Z", &["x1".to_string()], Some(&long));
        let trigger = model::served::last_trigger(&store, "x1").expect("a trigger was given");
        assert_eq!(trigger.chars().count(), TRIGGER_LIMIT, "must be cut down to the limit, not left full length");
        assert_eq!(trigger, "x".repeat(TRIGGER_LIMIT));
    }

    #[test]
    fn recording_delivery_never_touches_the_items_head_set() {
        // ItemServed is head-neutral (core::cas/auditor already treat it that
        // way); this pins it from the serve side too: recording a delivery
        // must never change what `live_items` reports for that entity.
        use model::item::{Binding, Item, Kind};
        let mut store = EventStore::in_memory().unwrap();
        let item = Item {
            id: "x2".to_string(),
            kind: Kind::Rule,
            text: "do the thing".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this item turns out to be wrong for this synthetic fixture".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["x2".to_string()]);
        let live = crate::live::live_items(&store);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].item, item);
    }

    #[test]
    fn recording_delivery_for_an_unknown_id_never_panics() {
        // hook is fail-open at the boundary; the delivery write for an id
        // that was never declared must be a harmless no-op-ish event, not a
        // crash (defense in depth even though the caller only ever passes
        // ids that select() just returned).
        let mut store = EventStore::in_memory().unwrap();
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["ghost".to_string()]);
        let stats = serving_stats(&store);
        assert_eq!(stats.get("ghost").map(|s| s.times_served), Some(1));
    }
}
