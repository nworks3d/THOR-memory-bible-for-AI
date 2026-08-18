//! THE SECOND LANE: a library of everyday knowledge - recipes, books read, a
//! training log, expenses - kept fully apart from the code lane.
//!
//! Not a scope, not a project, not an item kind, not a field on an item. Every
//! one of those puts the everyday facts and the code facts in one list, and a
//! shared list is a list they compete in. This crate cannot reach the event
//! store at all (see Cargo.toml: it depends on nothing from that side), so the
//! guard, the four slots and the global rules are not merely unaffected - they
//! are unreachable from here.
//!
//! See `PLAN-BIBLIOTHEEK.md` for the measured failures each rule below comes
//! from. In short:
//!
//! - Nothing is optional that must be filled in, because an optional field
//!   nobody fills stays empty (1362 items on the code side prove it).
//! - Nothing is a reminder that could be a refusal, because a reminder was
//!   measured not to change behaviour and a gate was.
//! - Nothing that shows less than everything stays quiet about it, because a
//!   silent cap reads as "that is all there is".
//! - A search that finds nothing answers with a list to read, never with
//!   "nothing" - the words a person asks with are not the words they wrote.

mod time;

use rusqlite::Connection;

/// The ceiling on the number of shelves.
///
/// THE CROWDING THIS PREVENTS. On the code side, more facts than slots meant
/// the best-ranked won and the rest vanished silently; the repair took days.
/// A library sprawls the same way, one reasonable-looking new shelf at a time,
/// and the cheapest moment to stop it is before the first one exists. At the
/// ceiling something must be merged - which is a decision, made once, out
/// loud, instead of a drift nobody notices.
pub const MAX_SHELVES: usize = 20;

/// Past this, a shelf listing stops being something you can read at a glance,
/// so the librarian says the shelf needs labels. It never splits one by
/// itself: more shelves is the failure this design exists to avoid.
pub const FAT_SHELF: usize = 200;

/// How alike two titles on one shelf may be before the second is refused.
/// Token overlap, so word order and punctuation do not matter.
const DUPLICATE_AT: f32 = 0.7;

/// The shortest prefix the third search step will match on. Three would make
/// "rib" reach half the kitchen; four keeps "ribb" reaching "ribben" and
/// "ribbetjes" without dragging in everything that merely starts the same.
const PREFIX_LEN: usize = 4;

/// The longest a title may be. It is the whole index line, and an index you
/// scroll sideways is not an index. Measured on the owner's own library: the
/// longest real title is 97 characters, so this refuses a paragraph without
/// touching anything he already has.
const TITLE_AT_MOST: usize = 160;

/// What the librarian says when it will not do something: what is wrong, and
/// what to do instead. Never one without the other - a refusal that does not
/// say the way out is just a wall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub problem: String,
    pub fix: String,
}

impl Refusal {
    fn new(problem: impl Into<String>, fix: impl Into<String>) -> Self {
        Refusal { problem: problem.into(), fix: fix.into() }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} - {}", self.problem, self.fix)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shelf {
    pub name: String,
    pub note: String,
    pub created: String,
    /// Live entries. The count the listing shows is the count that exists.
    pub entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: i64,
    pub shelf: String,
    /// One line. This is what a shelf listing shows, so it has to stand alone.
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub added: String,
    pub updated: String,
    pub retired: bool,
}

/// How a set of hits was found, so an answer can say which step reached it -
/// a prefix match is a weaker claim than an exact phrase and should read that
/// way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reached {
    Phrase,
    EveryWord,
    Prefix,
    /// Not every word was found, and that is not a reason to answer nothing.
    ///
    /// THE DEFECT THIS CLOSES, measured 2026-08-18: "eiwit training spier
    /// creatine gym" returned nothing at all, on a library holding an entry
    /// about protein and creatine - because one word of the five ("gym") was
    /// absent, and every step before this one demands ALL of them. A search
    /// that only answers a perfect question is a search you have to guess
    /// your way into.
    SomeWords { matched: usize, of: usize },
}

impl Reached {
    pub fn as_str(self) -> &'static str {
        match self {
            Reached::Phrase => "on the exact phrase",
            Reached::EveryWord => "on every word you used",
            Reached::Prefix => "on the start of your words, so these are near misses on wording",
            Reached::SomeWords { matched, of } => {
                // Leaked as a leading number by `render_found`, because a
                // &'static str cannot carry a count.
                let _ = (matched, of);
                "on some of your words - the rest of what you typed is not in here"
            }
        }
    }
}

/// The answer to a search. There is no empty variant on purpose: a library
/// that says "nothing" when it holds the thing is the failure this replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Hits { reached: Reached, entries: Vec<Entry> },
    /// Nothing matched, but the search was aimed at one shelf, so here is that
    /// shelf to read.
    ShelfInstead { shelf: String, entries: Vec<Entry> },
    /// Nothing matched anywhere, so here is what the library holds at all.
    ShelvesInstead { shelves: Vec<Shelf> },
}

/// Where the library lives for a given event store: beside it, under its own
/// name. Derived in one place so no caller can invent a second location - and
/// deliberately a SEPARATE FILE, never a table inside the store.
pub fn default_path(store_db: &std::path::Path) -> std::path::PathBuf {
    store_db.parent().unwrap_or_else(|| std::path::Path::new(".")).join("library.db")
}

pub struct Library {
    conn: Connection,
}

impl Library {
    /// Open (or create) the library beside wherever the caller keeps it.
    /// Its own file, always - never the event store's.
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("cannot open the library at {}: {e}", path.display()))?;
        // WAIT FOR A BUSY WRITER INSTEAD OF FAILING AT ONCE. Three programs
        // reach this file - the connector when it files something, the drain
        // when it replays a filing captured away from the desk, and the hourly
        // backup while it reads - and without this the second one gets
        // "database is locked" the instant they overlap. The event store has
        // done exactly this since it was written; the library was missing it.
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| format!("cannot prepare the library: {e}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS shelf (
                 name    TEXT PRIMARY KEY,
                 note    TEXT NOT NULL DEFAULT '',
                 created TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS entry (
                 id      INTEGER PRIMARY KEY AUTOINCREMENT,
                 shelf   TEXT NOT NULL REFERENCES shelf(name),
                 title   TEXT NOT NULL,
                 body    TEXT NOT NULL DEFAULT '',
                 labels  TEXT NOT NULL DEFAULT '',
                 added   TEXT NOT NULL,
                 updated TEXT NOT NULL,
                 retired TEXT NOT NULL DEFAULT ''
             );
             CREATE INDEX IF NOT EXISTS entry_by_shelf ON entry(shelf);
             CREATE TABLE IF NOT EXISTS revision (
                 entry_id INTEGER NOT NULL REFERENCES entry(id),
                 title    TEXT NOT NULL,
                 body     TEXT NOT NULL,
                 labels   TEXT NOT NULL DEFAULT '',
                 replaced TEXT NOT NULL,
                 shelf    TEXT NOT NULL DEFAULT ''
             );
             CREATE INDEX IF NOT EXISTS revision_by_entry ON revision(entry_id);",
        )
        .map_err(|e| format!("cannot prepare the library: {e}"))?;
        // A library made before entries could move has no `shelf` on its
        // revisions. Added here rather than in a migration step somebody has
        // to remember to run.
        let has_shelf = conn
            .prepare("SELECT shelf FROM revision LIMIT 1")
            .map(|_| true)
            .unwrap_or(false);
        if !has_shelf {
            conn.execute_batch("ALTER TABLE revision ADD COLUMN shelf TEXT NOT NULL DEFAULT '';")
                .map_err(|e| format!("cannot prepare the library: {e}"))?;
        }
        Ok(Library { conn })
    }

    // ------------------------------------------------------------- shelves

    pub fn shelves(&self) -> Result<Vec<Shelf>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.name, s.note, s.created,
                        (SELECT COUNT(*) FROM entry e WHERE e.shelf = s.name AND e.retired = '')
                 FROM shelf s ORDER BY s.name",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Shelf {
                    name: r.get(0)?,
                    note: r.get(1)?,
                    created: r.get(2)?,
                    entries: r.get::<_, i64>(3)? as usize,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Create a shelf. THE OWNER'S CALL, NEVER AN AGENT'S.
    ///
    /// An agent that finds no fitting shelf has exactly one correct move: ask.
    /// This function cannot tell who is calling it, so the honest guarantee is
    /// the one it CAN give - the ceiling below, the flat-name rule, and the
    /// fact that every shelf carries the date it appeared, so a shelf that was
    /// invented rather than asked for is visible afterwards.
    pub fn create_shelf(&self, name: &str, note: &str) -> Result<(), Refusal> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::new("a shelf with no name", "give it a short, plain name - the word you would use out loud"));
        }
        // Flat, and it has to be enforced rather than assumed: nesting is
        // exactly how the code side sprawled, and it always starts with one
        // reasonable-looking "eten/bbq".
        if name.contains(['/', '\\', ':', '.']) {
            return Err(Refusal::new(
                format!("'{name}' looks like a path, and shelves do not nest"),
                "use one flat name. If a shelf is getting broad, that is what labels inside it are for",
            ));
        }
        if name.chars().count() > 24 {
            return Err(Refusal::new(
                format!("'{name}' is too long for a shelf name"),
                "keep it under 25 characters - a shelf name is a label on a spine, not a description",
            ));
        }
        let existing = self.shelves().map_err(|e| Refusal::new(e, "the library could not be read"))?;
        if let Some(hit) = existing.iter().find(|s| s.name.eq_ignore_ascii_case(name)) {
            return Err(Refusal::new(
                format!("a shelf called '{}' already exists", hit.name),
                "put the entry on that one",
            ));
        }
        if existing.len() >= MAX_SHELVES {
            return Err(Refusal::new(
                format!("the library already holds {MAX_SHELVES} shelves, which is the ceiling"),
                "merge two that overlap before adding another. The ceiling is deliberate: a list you cannot hold in your head is the crowding this library was built to avoid",
            ));
        }
        self.conn
            .execute("INSERT INTO shelf (name, note, created) VALUES (?1, ?2, ?3)", (name, note.trim(), time::today()))
            .map_err(|e| Refusal::new(format!("the shelf could not be written: {e}"), "try again"))?;
        Ok(())
    }

    // ------------------------------------------------------------- entries

    /// File an entry. Refused without a shelf that already exists, and refused
    /// when the same shelf already holds something this close to it.
    pub fn add(&self, shelf: &str, title: &str, body: &str, labels: &[String]) -> Result<i64, Refusal> {
        let title = title.trim();
        if title.is_empty() {
            return Err(Refusal::new(
                "an entry with no title",
                "give it one line that stands on its own - that line IS the index, and it is all a shelf listing shows",
            ));
        }
        if title.contains('\n') {
            return Err(Refusal::new(
                "the title runs over more than one line",
                "keep the title to one line and put the rest in the body",
            ));
        }
        if title.chars().count() > TITLE_AT_MOST {
            return Err(Refusal::new(
                format!("the title is {} characters, and it is the whole index line", title.chars().count()),
                format!("keep it under {TITLE_AT_MOST} - say the one thing it is and move the detail into the body"),
            ));
        }
        let shelves = self.shelves().map_err(|e| Refusal::new(e, "the library could not be read"))?;
        let Some(shelf) = shelves.iter().find(|s| s.name.eq_ignore_ascii_case(shelf.trim())) else {
            return Err(Refusal::new(
                format!("there is no shelf called '{}'", shelf.trim()),
                format!(
                    "put it on one of these: {}. If none of them fits, ASK the owner whether this needs a new shelf and what to call it - never invent one, and never file something where it does not belong just to get past this",
                    shelf_names(&shelves)
                ),
            ));
        };
        if let Some(twin) = self.near_duplicate(&shelf.name, title)? {
            return Err(Refusal::new(
                format!("'{}' is already on this shelf, as entry {}", twin.title, twin.id),
                "revise that one instead of adding a second copy of the same thing",
            ));
        }
        let now = time::today();
        self.conn
            .execute(
                "INSERT INTO entry (shelf, title, body, labels, added, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                (&shelf.name, title, body.trim(), normalize_labels(labels), &now),
            )
            .map_err(|e| Refusal::new(format!("the entry could not be written: {e}"), "try again"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Everything on one shelf, oldest first, optionally narrowed to a label.
    pub fn entries(&self, shelf: &str, label: Option<&str>) -> Result<Vec<Entry>, String> {
        let all = self.rows("SELECT id, shelf, title, body, labels, added, updated, retired FROM entry WHERE shelf = ?1 COLLATE NOCASE AND retired = '' ORDER BY added, id", [shelf])?;
        let Some(label) = label.map(str::trim).filter(|l| !l.is_empty()) else { return Ok(all) };
        let want = label.to_lowercase();
        Ok(all.into_iter().filter(|e| e.labels.iter().any(|l| *l == want)).collect())
    }

    pub fn get(&self, id: i64) -> Result<Option<Entry>, String> {
        let rows = self.rows(
            "SELECT id, shelf, title, body, labels, added, updated, retired FROM entry WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(rows.into_iter().next())
    }

    /// Retire an entry. NOTHING IS EVER DELETED: it stops appearing in the
    /// shelf listing and in search, and `get` still returns it whole. A
    /// library that can lose things is a library you have to keep a copy of.
    pub fn retire(&self, id: i64, why: &str) -> Result<(), Refusal> {
        let why = why.trim();
        if why.is_empty() {
            return Err(Refusal::new(
                "retiring without a reason",
                "say why in a few words - it is the only thing that explains the gap later",
            ));
        }
        let n = self
            .conn
            .execute("UPDATE entry SET retired = ?2, updated = ?3 WHERE id = ?1 AND retired = ''", (id, why, time::today()))
            .map_err(|e| Refusal::new(e.to_string(), "try again"))?;
        if n == 0 {
            return Err(Refusal::new(format!("no live entry {id}"), "check the id, or it was already retired"));
        }
        Ok(())
    }

    /// The labels in use on one shelf, with how many entries carry each. What
    /// a big shelf is filtered by, and the answer to "what is in here".
    pub fn labels(&self, shelf: &str) -> Result<Vec<(String, usize)>, String> {
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for entry in self.entries(shelf, None)? {
            for label in entry.labels {
                *counts.entry(label).or_default() += 1;
            }
        }
        Ok(counts.into_iter().collect())
    }

    // -------------------------------------------------------------- search

    /// Four steps, each running only when the one before it found nothing, so
    /// a good match is never widened into a worse one. The last step is not a
    /// match at all - it is a list to read, which is the whole point: the
    /// words a person asks with are not the words they wrote.
    pub fn search(&self, query: &str, shelf: Option<&str>) -> Result<Found, String> {
        let pool: Vec<Entry> = match shelf.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => self.entries(s, None)?,
            None => self.rows(
                "SELECT id, shelf, title, body, labels, added, updated, retired FROM entry WHERE retired = '' ORDER BY shelf, added, id",
                [],
            )?,
        };
        let q = query.trim().to_lowercase();
        let words: Vec<&str> = q.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();

        if !q.is_empty() && !words.is_empty() {
            let phrase: Vec<Entry> = pool.iter().filter(|e| haystack(e).contains(&q)).cloned().collect();
            if !phrase.is_empty() {
                return Ok(Found::Hits { reached: Reached::Phrase, entries: phrase });
            }
            let every: Vec<Entry> =
                pool.iter().filter(|e| words.iter().all(|w| haystack(e).contains(w))).cloned().collect();
            if !every.is_empty() {
                return Ok(Found::Hits { reached: Reached::EveryWord, entries: every });
            }
            // THE "ribbetjes" STEP. Measured on the code side: "ribbetjes"
            // found nothing while "ribben" found three, because matching is
            // whole-word and a diminutive is a different word. Comparing the
            // first few letters of each side reaches both without a stemmer
            // and without a dictionary.
            let prefixed: Vec<Entry> = pool
                .iter()
                .filter(|e| {
                    let tokens = tokens(&haystack(e));
                    words.iter().all(|w| {
                        let want = prefix(w);
                        want.len() >= PREFIX_LEN && tokens.iter().any(|t| t.starts_with(&want))
                    })
                })
                .cloned()
                .collect();
            if !prefixed.is_empty() {
                return Ok(Found::Hits { reached: Reached::Prefix, entries: prefixed });
            }

            // THE "MOST OF YOUR WORDS" STEP. Everything above demands every
            // word; this one asks how many were found and hands back the best
            // it has, saying how weak the claim is. Only when not a single
            // word lands does the list come back - and then it really is the
            // right answer, because nothing in the library touches what was
            // asked.
            let mut best = 0usize;
            let mut scored: Vec<(usize, Entry)> = Vec::new();
            for e in &pool {
                let tokens = tokens(&haystack(e));
                // Whole word or a prefix of at least four, exactly as the
                // steps above match. A raw substring would let "op" or "de"
                // score against almost every entry and drown the real hit.
                let hits = words
                    .iter()
                    .filter(|w| {
                        let want = prefix(w);
                        tokens.iter().any(|t| t == *w)
                            || (want.len() >= PREFIX_LEN && tokens.iter().any(|t| t.starts_with(&want)))
                    })
                    .count();
                if hits > 0 {
                    best = best.max(hits);
                    scored.push((hits, e.clone()));
                }
            }
            if best > 0 {
                let entries: Vec<Entry> =
                    scored.into_iter().filter(|(n, _)| *n == best).map(|(_, e)| e).collect();
                return Ok(Found::Hits {
                    reached: Reached::SomeWords { matched: best, of: words.len() },
                    entries,
                });
            }
        }

        match shelf.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => Ok(Found::ShelfInstead { shelf: s.to_string(), entries: pool }),
            None => Ok(Found::ShelvesInstead { shelves: self.shelves()? }),
        }
    }

    /// Correct an entry that already exists, keeping its number.
    ///
    /// WHY THIS EXISTS. Until 2026-08-18 the only way to fix a title, a typo
    /// or a wrong label was to retire the entry and file a new one - which
    /// changes the number every reference points at, and splits one thing's
    /// story across two entries. The code lane has had `revise` from the
    /// start, for exactly the reason its own doctrine gives: a memory is to
    /// MAINTAIN, not only to fill.
    ///
    /// NOTHING IS LOST: the version being replaced is copied into `revision`
    /// first, so `history` can walk what an entry used to say. Same promise
    /// the event log makes, kept the cheap way.
    pub fn revise(
        &self,
        id: i64,
        title: Option<&str>,
        body: Option<&str>,
        labels: Option<&[String]>,
    ) -> Result<(), Refusal> {
        let Some(current) = self.get(id).map_err(|e| Refusal::new(e, "the library could not be read"))? else {
            return Err(Refusal::new(format!("no entry {id}"), "check the number in the shelf listing"));
        };
        let new_title = title.map(str::trim).unwrap_or(current.title.as_str());
        if new_title.is_empty() {
            return Err(Refusal::new("a title that is now empty", "give it one line that stands on its own"));
        }
        if new_title.contains('\n') {
            return Err(Refusal::new("the title runs over more than one line", "keep the title to one line"));
        }
        if new_title.chars().count() > TITLE_AT_MOST {
            return Err(Refusal::new(
                format!("the title is {} characters, and it is the whole index line", new_title.chars().count()),
                format!("keep it under {TITLE_AT_MOST}"),
            ));
        }
        if title.is_some() && new_title != current.title {
            if let Some(twin) = self.near_duplicate(&current.shelf, new_title)? {
                if twin.id != id {
                    return Err(Refusal::new(
                        format!("'{}' is already on this shelf, as entry {}", twin.title, twin.id),
                        "revise that one instead, or give this one a title that tells them apart",
                    ));
                }
            }
        }
        let now = time::today();
        self.conn
            .execute(
                "INSERT INTO revision (entry_id, title, body, labels, replaced, shelf) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (id, &current.title, &current.body, current.labels.join(","), &now, &current.shelf),
            )
            .map_err(|e| Refusal::new(format!("the old version could not be kept: {e}"), "nothing was changed"))?;
        self.conn
            .execute(
                "UPDATE entry SET title = ?2, body = ?3, labels = ?4, updated = ?5 WHERE id = ?1",
                (
                    id,
                    new_title,
                    body.map(str::trim).unwrap_or(current.body.as_str()),
                    labels.map(normalize_labels).unwrap_or_else(|| current.labels.join(",")),
                    &now,
                ),
            )
            .map_err(|e| Refusal::new(format!("the entry could not be written: {e}"), "try again"))?;
        Ok(())
    }

    /// What this entry used to say, oldest first. Empty when it was never
    /// corrected - which is a different answer from "no such entry", and the
    /// caller can tell because `get` answers that one.
    pub fn history(&self, id: i64) -> Result<Vec<(String, String, String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT replaced, title, body, shelf FROM revision WHERE entry_id = ?1 ORDER BY rowid")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    // -------------------------------------------------------------- health

    /// What the librarian would say out loud. It reports; it never acts - a
    /// system that tidies itself is a system that loses things quietly.
    pub fn health(&self) -> Result<Vec<String>, String> {
        let shelves = self.shelves()?;
        let mut out = Vec::new();
        if shelves.len() >= MAX_SHELVES {
            out.push(format!("{} shelves, which is the ceiling: merge two before adding another", shelves.len()));
        }
        for s in &shelves {
            if s.entries == 0 {
                out.push(format!("shelf '{}' is empty - fold it into another one or say what belongs on it", s.name));
            } else if s.entries == 1 {
                out.push(format!("shelf '{}' holds one entry, which rarely earns a shelf of its own", s.name));
            } else if s.entries > FAT_SHELF {
                out.push(format!(
                    "shelf '{}' holds {} entries, past the {FAT_SHELF} a listing stays readable at - give its entries labels rather than splitting the shelf",
                    s.name, s.entries
                ));
            }
        }
        Ok(out)
    }

    /// Move an entry to another shelf, keeping its number and its history.
    ///
    /// The repair for a misfile. Without it the only way was to retire the
    /// entry and file it again, which changes the number every reference
    /// points at and splits one thing's story in two.
    pub fn move_to(&self, id: i64, shelf: &str) -> Result<(), Refusal> {
        let Some(entry) = self.get(id).map_err(|e| Refusal::new(e, "the library could not be read"))? else {
            return Err(Refusal::new(format!("no entry {id}"), "check the number in the shelf listing"));
        };
        let shelves = self.shelves().map_err(|e| Refusal::new(e, "the library could not be read"))?;
        let Some(target) = shelves.iter().find(|s| s.name.eq_ignore_ascii_case(shelf.trim())) else {
            return Err(Refusal::new(
                format!("there is no shelf called '{}'", shelf.trim()),
                format!("move it to one of these: {}", shelf_names(&shelves)),
            ));
        };
        if target.name.eq_ignore_ascii_case(&entry.shelf) {
            return Err(Refusal::new(
                format!("entry {id} is already on '{}'", entry.shelf),
                "name the shelf it should move to",
            ));
        }
        if let Some(twin) = self.near_duplicate(&target.name, &entry.title)? {
            return Err(Refusal::new(
                format!("'{}' is already on '{}', as entry {}", twin.title, target.name, twin.id),
                "fold the two together instead of keeping both",
            ));
        }
        let now = time::today();
        self.conn
            .execute(
                "INSERT INTO revision (entry_id, title, body, labels, replaced, shelf) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (id, &entry.title, &entry.body, entry.labels.join(","), &now, &entry.shelf),
            )
            .map_err(|e| Refusal::new(format!("the old version could not be kept: {e}"), "nothing was changed"))?;
        self.conn
            .execute("UPDATE entry SET shelf = ?2, updated = ?3 WHERE id = ?1", (id, &target.name, &now))
            .map_err(|e| Refusal::new(format!("the entry could not be moved: {e}"), "try again"))?;
        Ok(())
    }

    // -------------------------------------------------------------- backup

    /// Write the whole library out as one JSON object per line: every shelf
    /// first, then every entry INCLUDING the retired ones.
    ///
    /// WHY THIS EXISTS. The hourly off-site backup exports the event log and
    /// nothing else, so on 2026-08-18 the library was 84 entries living in one
    /// file on one machine with no copy anywhere - the exact shape of loss the
    /// event log was built to make impossible. Text, one line per thing, so a
    /// git history of it is readable and a half-written file costs one line
    /// rather than the lot.
    pub fn export_jsonl(&self, out: &mut impl std::io::Write) -> Result<usize, String> {
        let mut n = 0;
        for s in self.shelves()? {
            let line = serde_json::json!({"kind": "shelf", "name": s.name, "note": s.note, "created": s.created});
            writeln!(out, "{line}").map_err(|e| e.to_string())?;
            n += 1;
        }
        let all = self.rows(
            "SELECT id, shelf, title, body, labels, added, updated, retired FROM entry ORDER BY id",
            [],
        )?;
        let mut retired_reason = std::collections::BTreeMap::new();
        {
            let mut stmt = self.conn.prepare("SELECT id, retired FROM entry").map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (id, why) = row.map_err(|e| e.to_string())?;
                retired_reason.insert(id, why);
            }
        }
        for e in all {
            let line = serde_json::json!({
                "kind": "entry",
                "id": e.id,
                "shelf": e.shelf,
                "title": e.title,
                "body": e.body,
                "labels": e.labels,
                "added": e.added,
                "updated": e.updated,
                "retired": retired_reason.get(&e.id).cloned().unwrap_or_default(),
            });
            writeln!(out, "{line}").map_err(|e| e.to_string())?;
            n += 1;
        }
        // The corrections too, or a restore quietly drops every older version
        // and "nothing is ever lost" stops being true the moment you need it.
        let mut stmt = self
            .conn
            .prepare("SELECT entry_id, title, body, labels, replaced, shelf FROM revision ORDER BY rowid")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(serde_json::json!({
                    "kind": "revision",
                    "entry_id": r.get::<_, i64>(0)?,
                    "title": r.get::<_, String>(1)?,
                    "body": r.get::<_, String>(2)?,
                    "labels": r.get::<_, String>(3)?,
                    "replaced": r.get::<_, String>(4)?,
                    "shelf": r.get::<_, String>(5)?,
                }))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let line = row.map_err(|e| e.to_string())?;
            writeln!(out, "{line}").map_err(|e| e.to_string())?;
            n += 1;
        }
        Ok(n)
    }

    /// Rebuild a library from an export. Only into an EMPTY one, and ids are
    /// kept exactly as they were, so every reference to an entry number
    /// survives the restore - a backup that renumbers what it restores is a
    /// backup that quietly breaks every pointer into it.
    pub fn import_jsonl(&self, text: &str) -> Result<usize, String> {
        let existing: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM entry", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if existing > 0 {
            return Err("this library already holds entries - restore into an empty one".to_string());
        }
        let mut n = 0;
        for (no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value =
                serde_json::from_str(line).map_err(|e| format!("line {}: {e}", no + 1))?;
            match v.get("kind").and_then(|k| k.as_str()) {
                Some("shelf") => {
                    self.conn
                        .execute(
                            "INSERT OR IGNORE INTO shelf (name, note, created) VALUES (?1, ?2, ?3)",
                            (
                                v["name"].as_str().unwrap_or_default(),
                                v["note"].as_str().unwrap_or_default(),
                                v["created"].as_str().unwrap_or_default(),
                            ),
                        )
                        .map_err(|e| e.to_string())?;
                }
                Some("entry") => {
                    let labels: Vec<String> = v["labels"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect())
                        .unwrap_or_default();
                    self.conn
                        .execute(
                            "INSERT INTO entry (id, shelf, title, body, labels, added, updated, retired)                              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            (
                                v["id"].as_i64().unwrap_or_default(),
                                v["shelf"].as_str().unwrap_or_default(),
                                v["title"].as_str().unwrap_or_default(),
                                v["body"].as_str().unwrap_or_default(),
                                labels.join(","),
                                v["added"].as_str().unwrap_or_default(),
                                v["updated"].as_str().unwrap_or_default(),
                                v["retired"].as_str().unwrap_or_default(),
                            ),
                        )
                        .map_err(|e| e.to_string())?;
                }
                Some("revision") => {
                    self.conn
                        .execute(
                            "INSERT INTO revision (entry_id, title, body, labels, replaced) \
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            (
                                v["entry_id"].as_i64().unwrap_or_default(),
                                v["title"].as_str().unwrap_or_default(),
                                v["body"].as_str().unwrap_or_default(),
                                v["labels"].as_str().unwrap_or_default(),
                                v["replaced"].as_str().unwrap_or_default(),
                            ),
                        )
                        .map_err(|e| e.to_string())?;
                }
                _ => return Err(format!("line {}: not a shelf, an entry or a revision", no + 1)),
            }
            n += 1;
        }
        Ok(n)
    }

    // ------------------------------------------------------------ internals

    fn near_duplicate(&self, shelf: &str, title: &str) -> Result<Option<Entry>, Refusal> {
        let existing = self
            .entries(shelf, None)
            .map_err(|e| Refusal::new(e, "the library could not be read"))?;
        let want = tokens(&title.to_lowercase());
        Ok(existing.into_iter().find(|e| overlap(&want, &tokens(&e.title.to_lowercase())) >= DUPLICATE_AT))
    }

    fn rows<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<Vec<Entry>, String> {
        let mut stmt = self.conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params, |r| {
                let labels: String = r.get(4)?;
                let retired: String = r.get(7)?;
                Ok(Entry {
                    id: r.get(0)?,
                    shelf: r.get(1)?,
                    title: r.get(2)?,
                    body: r.get(3)?,
                    labels: labels.split(',').filter(|l| !l.is_empty()).map(str::to_string).collect(),
                    added: r.get(5)?,
                    updated: r.get(6)?,
                    retired: !retired.is_empty(),
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }
}

// ------------------------------------------------------------------ helpers

fn haystack(e: &Entry) -> String {
    format!("{} {} {}", e.title, e.body, e.labels.join(" ")).to_lowercase()
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

/// The comparable head of a word: its first `PREFIX_LEN` characters, or the
/// whole word when it is shorter.
fn prefix(word: &str) -> String {
    word.chars().take(PREFIX_LEN).collect()
}

/// The comma is the separator this column stores labels with, so a label that
/// contains one is SPLIT rather than dropped. It used to be filtered out
/// silently, which meant an entry quietly landed with fewer labels than the
/// caller asked for - the kind of loss nobody notices until they filter on a
/// label that was never stored.
fn normalize_labels(labels: &[String]) -> String {
    let mut clean: Vec<String> = labels
        .iter()
        .flat_map(|l| l.split(','))
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    clean.sort();
    clean.dedup();
    clean.join(",")
}

fn shelf_names(shelves: &[Shelf]) -> String {
    if shelves.is_empty() {
        return "there are none yet, so this would be the first - ask the owner what to call it".to_string();
    }
    shelves.iter().map(|s| format!("{} ({})", s.name, s.entries)).collect::<Vec<_>>().join(", ")
}

/// How alike two token sets are, 0.0 to 1.0. Order and punctuation do not
/// matter, so "Pizzadeeg voor de Kamado" and "pizzadeeg, kamado" count as the
/// same thing - which is what a person would say too.
fn overlap(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let sa: std::collections::BTreeSet<&String> = a.iter().collect();
    let sb: std::collections::BTreeSet<&String> = b.iter().collect();
    let shared = sa.intersection(&sb).count() as f32;
    let total = sa.union(&sb).count() as f32;
    shared / total
}

// ------------------------------------------------------------------ render
//
// One definition per answer, so the terminal door and the agent door can never
// drift into saying different things about the same library.

pub fn render_shelves(shelves: &[Shelf]) -> String {
    if shelves.is_empty() {
        return "the library has no shelves yet. Ask the owner what the first one should be called.\n".to_string();
    }
    let mut out = String::new();
    for s in shelves {
        let note = if s.note.is_empty() { String::new() } else { format!("  {}", s.note) };
        out.push_str(&format!("{:<20} {:>4} entr{}{note}\n", s.name, s.entries, if s.entries == 1 { "y" } else { "ies" }));
    }
    out.push_str("Open a shelf by name to read its index, one line per entry.\n");
    out
}

/// A shelf as an index: one line per entry, never the whole text. The count on
/// the first line is the true size even when the listing below it is cut.
pub fn render_shelf(shelf: &str, entries: &[Entry], shown: usize, total: usize) -> String {
    if entries.is_empty() {
        return format!("shelf '{shelf}' is empty.\n");
    }
    // WHY `total` IS A SEPARATE NUMBER: a listing narrowed by a label used to
    // print its own length as the size of the shelf, so "shelf eten: 12
    // entries" appeared for a shelf holding 25. A reader who believes that
    // number stops looking for the other thirteen.
    let mut out = if total > entries.len() {
        format!("shelf {shelf}: {} of {total} entries, narrowed by your label\n", entries.len())
    } else {
        format!("shelf {shelf}: {} entr{}\n", entries.len(), if entries.len() == 1 { "y" } else { "ies" })
    };
    for e in entries.iter().take(shown) {
        let labels = if e.labels.is_empty() { String::new() } else { format!("   [{}]", e.labels.join(" ")) };
        out.push_str(&format!("  {:>5}  {}  {}{labels}\n", e.id, e.added, e.title));
    }
    if entries.len() > shown {
        out.push_str(&format!(
            "({} more not shown. Narrow with a label, or search inside this shelf.)\n",
            entries.len() - shown
        ));
    }
    out.push_str("Ask for one by number to read it whole.\n");
    out
}

pub fn render_entry(e: &Entry) -> String {
    let labels = if e.labels.is_empty() { String::new() } else { format!("\nlabels: {}", e.labels.join(", ")) };
    let retired = if e.retired { "\n(retired)" } else { "" };
    format!("{} - {} (shelf {}, added {}){labels}{retired}\n\n{}\n", e.id, e.title, e.shelf, e.added, e.body)
}

pub fn render_found(found: &Found, shown: usize) -> String {
    match found {
        Found::Hits { reached, entries } => {
            let how = match reached {
                Reached::SomeWords { matched, of } => {
                    format!("on {matched} of the {of} words you used - the rest is not in here")
                }
                other => other.as_str().to_string(),
            };
            let mut out = format!("{} match(es), {how}\n", entries.len());
            for e in entries.iter().take(shown) {
                out.push_str(&format!("  {:>5}  {:<12} {}\n", e.id, e.shelf, e.title));
            }
            if entries.len() > shown {
                out.push_str(&format!("({} more not shown.)\n", entries.len() - shown));
            }
            out.push_str("Ask for one by number to read it whole.\n");
            out
        }
        // NEVER "no results". The thing being asked for is very often on the
        // shelf under a word the asker did not use.
        Found::ShelfInstead { shelf, entries } => {
            format!("Nothing matched those words, so here is the whole shelf to read instead.\n{}", render_shelf(shelf, entries, shown, entries.len()))
        }
        Found::ShelvesInstead { shelves } => {
            format!("Nothing matched those words. Here is what the library holds - open the shelf it would be on.\n{}", render_shelves(shelves))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> (Library, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&dir.path().join("library.db")).unwrap();
        (lib, dir)
    }

    fn stocked() -> (Library, tempfile::TempDir) {
        let (lib, dir) = library();
        lib.create_shelf("eten", "recepten en BBQ").unwrap();
        // Worded the way the owner's real entry is: the title says
        // "Spareribs", the body says "Ribben". That difference is exactly what
        // the search has to survive.
        lib.add("eten", "Spareribs op de Kamado, 120 graden met kersenhout", "Ribben 2u open, dan 30 min in folie.", &["bbq".to_string()])
            .unwrap();
        lib.add("eten", "Pizzadeeg voor de Kamado, 65 procent hydratatie", "1000 g bloem, 650 ml water.", &[])
            .unwrap();
        (lib, dir)
    }

    // ------------------------------------------------------------- shelves

    /// The ceiling is the whole point: a library sprawls one reasonable-looking
    /// shelf at a time, exactly the way the code side did.
    #[test]
    fn a_new_shelf_past_the_ceiling_is_refused_and_says_to_merge_first() {
        let (lib, _d) = library();
        for i in 0..MAX_SHELVES {
            lib.create_shelf(&format!("plank{i}"), "").unwrap();
        }
        let refusal = lib.create_shelf("nog-een", "").expect_err("the ceiling must hold");
        assert!(refusal.problem.contains("ceiling"), "{refusal:?}");
        assert!(refusal.fix.contains("merge"), "the way out must be named: {refusal:?}");
    }

    /// Nesting is how the code side sprawled, and it always starts with one
    /// reasonable-looking "eten/bbq".
    #[test]
    fn a_shelf_name_that_looks_like_a_path_is_refused() {
        let (lib, _d) = library();
        let refusal = lib.create_shelf("eten/bbq", "").expect_err("shelves do not nest");
        assert!(refusal.fix.contains("labels"), "it must point at the thing that DOES grow: {refusal:?}");
    }

    #[test]
    fn the_same_shelf_twice_is_refused_whatever_the_casing() {
        let (lib, _d) = library();
        lib.create_shelf("eten", "").unwrap();
        assert!(lib.create_shelf("Eten", "").is_err());
    }

    // ------------------------------------------------------------- entries

    /// The refusal has to carry the choice. Without it a fresh agent needs
    /// three calls to file one thing: write, get refused, look up the shelves,
    /// write again.
    #[test]
    fn an_entry_for_a_shelf_that_does_not_exist_is_refused_with_the_shelves_named() {
        let (lib, _d) = stocked();
        let refusal = lib.add("sport", "Push pull legs", "", &[]).expect_err("an unknown shelf must be refused");
        assert!(refusal.problem.contains("sport"), "{refusal:?}");
        assert!(refusal.fix.contains("eten (2)"), "the existing shelves and their sizes: {refusal:?}");
        assert!(refusal.fix.contains("ASK the owner"), "asking beats inventing: {refusal:?}");
    }

    /// Nothing on the code side checked before writing, and near-duplicates
    /// accumulated for months.
    #[test]
    fn a_second_entry_saying_the_same_thing_is_refused_pointing_at_the_first() {
        let (lib, _d) = stocked();
        let refusal = lib
            .add("eten", "pizzadeeg voor de kamado, 65 procent hydratatie", "", &[])
            .expect_err("a near-duplicate must be refused");
        assert!(refusal.problem.contains("Pizzadeeg"), "it must name the one that exists: {refusal:?}");
        assert!(refusal.fix.contains("revise"), "{refusal:?}");
    }

    /// Different things on one shelf are not duplicates, however much
    /// vocabulary they share. Refusing these would be worse than the defect.
    #[test]
    fn two_different_recipes_sharing_words_are_both_allowed() {
        let (lib, _d) = stocked();
        assert!(lib.add("eten", "Focaccia voor de Kamado, lange rijs", "", &[]).is_ok());
    }

    #[test]
    fn an_entry_needs_a_title_that_stands_on_its_own_line() {
        let (lib, _d) = stocked();
        assert!(lib.add("eten", "  ", "iets", &[]).is_err());
        assert!(lib.add("eten", "regel een\nregel twee", "iets", &[]).is_err());
    }

    /// A library you can lose things from is one you keep a copy of.
    #[test]
    fn a_retired_entry_leaves_the_listing_but_is_still_readable_whole() {
        let (lib, _d) = stocked();
        let id = lib.add("eten", "Mislukt experiment met bier in het deeg", "", &[]).unwrap();
        lib.retire(id, "smaakte nergens naar").unwrap();

        assert!(!lib.entries("eten", None).unwrap().iter().any(|e| e.id == id), "gone from the shelf");
        let still = lib.get(id).unwrap().expect("but never deleted");
        assert!(still.retired);
        assert_eq!(still.title, "Mislukt experiment met bier in het deeg");
    }

    #[test]
    fn retiring_without_a_reason_is_refused() {
        let (lib, _d) = stocked();
        let id = lib.entries("eten", None).unwrap()[0].id;
        assert!(lib.retire(id, "   ").is_err());
    }

    // -------------------------------------------------------------- search

    /// THE MEASURED DEFECT THIS CLOSES. On the code side "ribbetjes" returned
    /// nothing while "ribben" returned three, because matching is whole-word
    /// and a diminutive is a different word. An empty answer for something the
    /// store holds is the failure this library exists to prevent.
    #[test]
    fn a_diminutive_still_reaches_the_entry_written_with_the_plain_word() {
        let (lib, _d) = stocked();
        let found = lib.search("ribbetjes", None).unwrap();
        match found {
            Found::Hits { reached, entries } => {
                assert_eq!(reached, Reached::Prefix, "reached on the start of the word");
                assert!(entries[0].title.contains("Spareribs"), "{:?}", entries[0]);
            }
            other => panic!("the diminutive must still find it, got {other:?}"),
        }
    }

    /// An exact phrase is never widened by a later step, so a good match can
    /// never be turned into a worse one.
    #[test]
    fn an_exact_phrase_wins_and_is_reported_as_such() {
        let (lib, _d) = stocked();
        match lib.search("65 procent hydratatie", None).unwrap() {
            Found::Hits { reached, entries } => {
                assert_eq!(reached, Reached::Phrase);
                assert_eq!(entries.len(), 1);
            }
            other => panic!("expected a phrase hit, got {other:?}"),
        }
    }

    /// The librarian never answers "nothing". A list you can read beats a
    /// guess at your wording.
    #[test]
    fn a_search_that_matches_nothing_answers_with_the_shelves_to_read() {
        let (lib, _d) = stocked();
        match lib.search("volstrekt onvindbaar onderwerp", None).unwrap() {
            Found::ShelvesInstead { shelves } => assert_eq!(shelves.len(), 1),
            other => panic!("expected the shelves back, got {other:?}"),
        }
        let rendered = render_found(&lib.search("volstrekt onvindbaar onderwerp", None).unwrap(), 25);
        assert!(!rendered.contains("no match"), "never the empty answer: {rendered}");
        assert!(rendered.contains("eten"), "{rendered}");
    }

    /// Aimed at one shelf, a miss returns that shelf rather than the whole
    /// library - the narrower list is the more useful one to read.
    #[test]
    fn a_miss_inside_one_shelf_answers_with_that_shelf() {
        let (lib, _d) = stocked();
        match lib.search("onvindbaar", Some("eten")).unwrap() {
            Found::ShelfInstead { shelf, entries } => {
                assert_eq!(shelf, "eten");
                assert_eq!(entries.len(), 2);
            }
            other => panic!("expected the shelf back, got {other:?}"),
        }
    }

    // -------------------------------------------------------------- labels

    /// Growth goes into labels, never into more shelves. This is the rule that
    /// keeps the shelf list flat while the content grows.
    #[test]
    fn a_label_narrows_a_shelf_without_creating_one() {
        let (lib, _d) = stocked();
        lib.add("eten", "Mac and cheese met cheddar en gruyere", "", &["bijgerecht".to_string()]).unwrap();

        assert_eq!(lib.entries("eten", Some("bbq")).unwrap().len(), 1);
        assert_eq!(lib.entries("eten", Some("bijgerecht")).unwrap().len(), 1);
        assert_eq!(lib.entries("eten", None).unwrap().len(), 3);
        assert_eq!(lib.shelves().unwrap().len(), 1, "no shelf was created by labelling");
    }

    // -------------------------------------------------------------- revise

    /// The defect this closes: correcting a typo used to mean retiring the
    /// entry and filing a new one, which changes the number every reference
    /// points at.
    #[test]
    fn revising_keeps_the_number_and_the_old_version_stays_readable() {
        let (lib, _d) = stocked();
        let id = lib.entries("eten", None).unwrap()[0].id;
        lib.revise(id, Some("Spareribs op de Kamado, 120 graden met appelhout"), None, None).unwrap();

        let now = lib.get(id).unwrap().unwrap();
        assert_eq!(now.id, id, "the number survives a correction");
        assert!(now.title.contains("appelhout"), "{now:?}");
        let past = lib.history(id).unwrap();
        assert_eq!(past.len(), 1, "the version it replaced is kept");
        assert!(past[0].1.contains("kersenhout"), "and it still says what it used to: {past:?}");
    }

    #[test]
    fn revising_only_the_labels_leaves_the_text_alone() {
        let (lib, _d) = stocked();
        let id = lib.entries("eten", None).unwrap()[0].id;
        let before = lib.get(id).unwrap().unwrap().body;
        lib.revise(id, None, None, Some(&["bbq".to_string(), "vlees".to_string()])).unwrap();
        let after = lib.get(id).unwrap().unwrap();
        assert_eq!(after.body, before);
        assert_eq!(after.labels, vec!["bbq".to_string(), "vlees".to_string()]);
    }

    /// A correction may not turn one entry into a copy of its neighbour.
    #[test]
    fn revising_into_a_title_the_shelf_already_has_is_refused() {
        let (lib, _d) = stocked();
        let ids: Vec<i64> = lib.entries("eten", None).unwrap().iter().map(|e| e.id).collect();
        let refusal = lib
            .revise(ids[1], Some("Spareribs op de Kamado, 120 graden met kersenhout"), None, None)
            .expect_err("that is the other entry");
        assert!(refusal.problem.contains("already on this shelf"), "{refusal:?}");
    }

    #[test]
    fn revising_an_entry_that_does_not_exist_says_so() {
        let (lib, _d) = stocked();
        assert!(lib.revise(9999, Some("iets"), None, None).is_err());
    }

    /// THE MEASURED MISS: one word of five was absent and the whole answer
    /// collapsed to "here is the shelf". A search may not depend on a perfect
    /// question.
    #[test]
    fn a_word_that_is_not_in_the_library_no_longer_wipes_the_answer() {
        let (lib, _d) = stocked();
        match lib.search("ribben kersenhout sportschool", None).unwrap() {
            Found::Hits { reached, entries } => {
                assert!(matches!(reached, Reached::SomeWords { matched: 2, of: 3 }), "{reached:?}");
                assert!(entries[0].title.contains("Spareribs"), "{:?}", entries[0]);
            }
            other => panic!("the two words that ARE here must still answer, got {other:?}"),
        }
    }

    /// A question that touches nothing still gets the list, because then the
    /// list really is the answer.
    #[test]
    fn a_question_that_touches_nothing_still_answers_with_the_shelves() {
        let (lib, _d) = stocked();
        assert!(matches!(
            lib.search("zeppelin quantumharmonica", None).unwrap(),
            Found::ShelvesInstead { .. }
        ));
    }

    /// An exact phrase still wins: the weaker steps only run when the ones
    /// above them found nothing.
    #[test]
    fn the_precise_steps_still_come_first() {
        let (lib, _d) = stocked();
        match lib.search("65 procent hydratatie", None).unwrap() {
            Found::Hits { reached, entries } => {
                assert_eq!(reached, Reached::Phrase);
                assert_eq!(entries.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    // ------------------------------------------------------------ verplaatsen

    #[test]
    fn moving_an_entry_keeps_its_number_and_records_where_it_was() {
        let (lib, _d) = stocked();
        lib.create_shelf("boeken", "").unwrap();
        let id = lib.entries("eten", None).unwrap()[0].id;
        lib.move_to(id, "boeken").unwrap();

        let moved = lib.get(id).unwrap().unwrap();
        assert_eq!(moved.id, id);
        assert_eq!(moved.shelf, "boeken");
        assert!(!lib.entries("eten", None).unwrap().iter().any(|e| e.id == id), "gone from the old shelf");
        assert_eq!(lib.history(id).unwrap().len(), 1, "the move is written down");
    }

    #[test]
    fn moving_to_a_shelf_that_does_not_exist_is_refused_with_the_shelves_named() {
        let (lib, _d) = stocked();
        let id = lib.entries("eten", None).unwrap()[0].id;
        let refusal = lib.move_to(id, "sport").expect_err("no such shelf");
        assert!(refusal.fix.contains("eten"), "{refusal:?}");
    }

    #[test]
    fn moving_onto_a_shelf_that_already_holds_the_same_thing_is_refused() {
        let (lib, _d) = stocked();
        lib.create_shelf("boeken", "").unwrap();
        let id = lib.entries("eten", None).unwrap()[0].id;
        lib.add("boeken", "Spareribs op de Kamado, 120 graden met kersenhout", "", &[]).unwrap();
        assert!(lib.move_to(id, "boeken").is_err());
    }

    // ------------------------------------------------- rommel uit de review

    /// It used to be dropped without a word, so an entry landed with fewer
    /// labels than the caller asked for and nobody noticed until a filter
    /// came up empty.
    #[test]
    fn a_label_with_a_comma_in_it_is_split_not_silently_dropped() {
        let (lib, _d) = stocked();
        let id = lib.add("eten", "Frietjes in de airfryer", "", &["snack,bijgerecht".to_string()]).unwrap();
        let e = lib.get(id).unwrap().unwrap();
        assert_eq!(e.labels, vec!["bijgerecht".to_string(), "snack".to_string()]);
    }

    /// The title IS the index line. A paragraph there is an index you cannot
    /// read.
    #[test]
    fn a_title_the_size_of_a_paragraph_is_refused() {
        let (lib, _d) = stocked();
        let refusal = lib.add("eten", &"x".repeat(200), "iets", &[]).expect_err("that is not an index line");
        assert!(refusal.problem.contains("index line"), "{refusal:?}");
    }

    /// A listing narrowed by a label used to report its own length as the size
    /// of the shelf, so a reader stopped looking for the rest.
    #[test]
    fn a_listing_narrowed_by_a_label_says_how_big_the_shelf_really_is() {
        let (lib, _d) = stocked();
        lib.add("eten", "Mac and cheese met cheddar", "", &["bijgerecht".to_string()]).unwrap();
        let narrowed = lib.entries("eten", Some("bijgerecht")).unwrap();
        let total = lib.entries("eten", None).unwrap().len();
        let rendered = render_shelf("eten", &narrowed, 25, total);
        assert!(rendered.contains("1 of 3 entries"), "{rendered}");
    }

    /// A correction is only kept if it travels with the backup.
    #[test]
    fn a_correction_survives_an_export_and_a_restore() {
        let (lib, _d) = stocked();
        let id = lib.entries("eten", None).unwrap()[0].id;
        lib.revise(id, Some("Spareribs op de Kamado met appelhout"), None, None).unwrap();
        let mut out: Vec<u8> = Vec::new();
        lib.export_jsonl(&mut out).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let fresh = Library::open(&dir.path().join("library.db")).unwrap();
        fresh.import_jsonl(&String::from_utf8(out).unwrap()).unwrap();
        let past = fresh.history(id).unwrap();
        assert_eq!(past.len(), 1, "what it used to say travels too");
        assert!(past[0].1.contains("kersenhout"), "{past:?}");
    }

    // -------------------------------------------------------------- backup

    /// A backup you cannot restore is a promise, not a fallback - the same
    /// lesson the event log learned on 2026-08-15.
    #[test]
    fn an_export_restores_into_an_empty_library_with_every_number_intact() {
        let (lib, _d) = stocked();
        let retired = lib.add("eten", "Mislukt experiment met bier", "", &[]).unwrap();
        lib.retire(retired, "smaakte nergens naar").unwrap();
        let mut out: Vec<u8> = Vec::new();
        let n = lib.export_jsonl(&mut out).unwrap();
        assert!(n >= 4, "one line per shelf and per entry, retired included");

        let dir = tempfile::tempdir().unwrap();
        let fresh = Library::open(&dir.path().join("library.db")).unwrap();
        fresh.import_jsonl(&String::from_utf8(out).unwrap()).unwrap();

        assert_eq!(fresh.shelves().unwrap().len(), lib.shelves().unwrap().len());
        assert_eq!(fresh.entries("eten", None).unwrap().len(), lib.entries("eten", None).unwrap().len());
        let back = fresh.get(retired).unwrap().expect("the retired one travels too, under its own number");
        assert!(back.retired);
    }

    #[test]
    fn restoring_into_a_library_that_already_holds_something_is_refused() {
        let (lib, _d) = stocked();
        let mut out: Vec<u8> = Vec::new();
        lib.export_jsonl(&mut out).unwrap();
        assert!(lib.import_jsonl(&String::from_utf8(out).unwrap()).is_err());
    }

    // -------------------------------------------------------------- health

    #[test]
    fn a_shelf_with_one_entry_is_reported_as_barely_earning_a_shelf() {
        let (lib, _d) = stocked();
        lib.create_shelf("boeken", "").unwrap();
        lib.add("boeken", "Dune - Frank Herbert", "", &[]).unwrap();

        let said = lib.health().unwrap().join("\n");
        assert!(said.contains("boeken"), "{said}");
        assert!(said.contains("one entry"), "{said}");
    }

    #[test]
    fn a_healthy_library_says_nothing() {
        let (lib, _d) = stocked();
        assert!(lib.health().unwrap().is_empty(), "no news is the right answer here");
    }

    // -------------------------------------------------------------- render

    /// A listing is an index, not a dump: the whole text of an entry appears
    /// only when you ask for that entry.
    #[test]
    fn a_shelf_listing_shows_titles_only_and_says_how_many_it_held_back() {
        let (lib, _d) = stocked();
        let entries = lib.entries("eten", None).unwrap();
        let rendered = render_shelf("eten", &entries, 1, entries.len());
        assert!(rendered.contains("shelf eten: 2 entries"), "{rendered}");
        assert!(!rendered.contains("1000 g bloem"), "a body must never appear in an index: {rendered}");
        assert!(rendered.contains("1 more not shown"), "a cap must say so: {rendered}");
    }
}
