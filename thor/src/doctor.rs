//! `thor doctor`: one health line per warm/cold surface, so "why is recall
//! slow/silent" is a single command instead of an investigation. It opens the
//! store with `open_existing`, so a wrong `--db` reports UNREACHABLE instead of
//! creating an empty store that then looks like a healthy but forgetful THOR.

use crate::event_store::EventStore;
use std::path::Path;

pub fn print_doctor(db: &Path) {
    match EventStore::open_existing(db) {
        Ok(store) => match store.get_all_events() {
            Ok(evs) => println!("store: OK ({} events at {})", evs.len(), db.display()),
            Err(e) => println!("store: OPENS but read failed ({e})"),
        },
        Err(e) => println!("store: UNREACHABLE ({e})"),
    }

    #[cfg(feature = "semantic")]
    {
        // Ask the same question recall asks, in the same folder, with the same
        // predicate: this used to derive its own path from the store file and
        // only check that the folder existed, so on Linux and macOS doctor could
        // report the model present while the courier looked elsewhere and found
        // nothing. Print the folder too, so the answer is checkable.
        match crate::embed::default_model_dir() {
            Some(dir) if crate::embed::model_present(&dir) => {
                println!("semantic model: present ({})", dir.display())
            }
            Some(dir) => println!(
                "semantic model: absent (bm25-only recall; expected the {} model files in {})",
                crate::embed::MODEL_FILES.len(),
                dir.display()
            ),
            None => println!(
                "semantic model: nowhere to look (bm25-only recall; LOCALAPPDATA, XDG_DATA_HOME and HOME are all unset)"
            ),
        }
        let vectors = db.with_file_name("thor-vectors.db");
        println!(
            "vectors sidecar: {}",
            if vectors.exists() { "present" } else { "absent (bm25-only recall)" }
        );
    }
    #[cfg(not(feature = "semantic"))]
    println!("semantic: not built in (bm25-only binary)");

    let sympath = crate::symbols::default_symbols_path(db);
    println!(
        "symbols sidecar: {}",
        if sympath.exists() { "present" } else { "absent (run `thor symbols`; where_used/impact and the symbol recall bonus stay off)" }
    );

    match crate::daemon_client::health(db) {
        Some(h) => println!(
            "injection daemon: WARM (pid {}, bind {}, db {})",
            h.get("pid").and_then(|v| v.as_u64()).unwrap_or(0),
            h.get("bind").and_then(|v| v.as_str()).unwrap_or("?"),
            h.get("db").and_then(|v| v.as_str()).unwrap_or("?"),
        ),
        None => println!(
            "injection daemon: COLD (hook falls back to the in-process path; \
             run `thor daemon` or install with --with-daemon to warm it)"
        ),
    }

    for flag in ["THOR-SILENT.flag", "THOR-PRIMARY.flag", "SEEDED.flag", "THOR-DAEMON.flag"] {
        if db.with_file_name(flag).exists() {
            println!("flag: {flag} present");
        }
    }
}
