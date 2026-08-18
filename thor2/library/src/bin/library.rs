//! The terminal door onto the library. The owner's own door - in particular,
//! `shelf-new` lives here and nowhere else, because creating a shelf is his
//! call and an agent that finds no fitting shelf must ask rather than invent.

use library::{render_entry, render_found, render_shelf, render_shelves, Library};
use std::path::PathBuf;

const SHOWN: usize = 50;

fn usage() -> ! {
    eprintln!(
        "usage: library --db <path> <command>

  shelves                          what the library holds
  shelf <name> [--label <l>]       one shelf, one line per entry
  read <id>                        one entry, whole
  search <words...> [--shelf <s>]  four steps, ending in a list to read
  add <shelf> <title> [--body <t>] [--label <l>]...
  revise <id> [--title <t>] [--body <t>] [--label <l>]...
                                   correct an entry, keeping its number; the
                                   old version stays readable with `history`
  history <id>                     what this entry used to say, oldest first
  move <id> <shelf>                put it on another shelf, same number
  retire <id> --why <reason>       stops listing it; never deletes it
  labels <shelf>                   the labels in use, with counts
  health                           what the librarian would say out loud
  shelf-new <name> [--note <t>]    THE OWNER'S CALL. An agent asks; it never
                                   creates a shelf itself.
  export <file>                    write the whole library out as one JSON
                                   object per line, retired entries included
  restore <file>                   rebuild an EMPTY library from such a file"
    );
    std::process::exit(2)
}

/// Pull `--flag value` out of the arguments, leaving the positional ones.
fn take_flag(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let at = args.iter().position(|a| a == flag)?;
    if at + 1 >= args.len() {
        eprintln!("{flag} needs a value");
        std::process::exit(2);
    }
    args.remove(at);
    Some(args.remove(at))
}

fn take_all(args: &mut Vec<String>, flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(v) = take_flag(args, flag) {
        out.push(v);
    }
    out
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let db = take_flag(&mut args, "--db").map(PathBuf::from).unwrap_or_else(|| usage());
    let labels = take_all(&mut args, "--label");
    let body = take_flag(&mut args, "--body").unwrap_or_default();
    let why = take_flag(&mut args, "--why").unwrap_or_default();
    let note = take_flag(&mut args, "--note").unwrap_or_default();
    let shelf_flag = take_flag(&mut args, "--shelf");
    let title_flag = take_flag(&mut args, "--title");

    let lib = Library::open(&db).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });

    let command = args.first().cloned().unwrap_or_else(|| usage());
    let rest = &args[1..];

    // A refusal goes to stderr with exit 1, the same shape the write gate
    // uses: nothing was written, and the reason plus the way out are printed.
    let refused = |e: library::Refusal| -> ! {
        eprintln!("refused: {e}");
        std::process::exit(1)
    };
    let failed = |e: String| -> ! {
        eprintln!("{e}");
        std::process::exit(1)
    };

    match command.as_str() {
        "shelves" => print!("{}", render_shelves(&lib.shelves().unwrap_or_else(|e| failed(e)))),
        "shelf" => {
            let name = rest.first().unwrap_or_else(|| usage());
            let entries = lib.entries(name, labels.first().map(String::as_str)).unwrap_or_else(|e| failed(e));
            // The size of the SHELF, not of the narrowed list - see render_shelf.
            let total = lib.entries(name, None).unwrap_or_else(|e| failed(e)).len();
            print!("{}", render_shelf(name, &entries, SHOWN, total));
        }
        "read" => {
            let id: i64 = rest.first().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
            match lib.get(id).unwrap_or_else(|e| failed(e)) {
                Some(entry) => print!("{}", render_entry(&entry)),
                None => failed(format!("no entry {id}")),
            }
        }
        "search" => {
            if rest.is_empty() {
                usage();
            }
            let found = lib.search(&rest.join(" "), shelf_flag.as_deref()).unwrap_or_else(|e| failed(e));
            print!("{}", render_found(&found, SHOWN));
        }
        "add" => {
            let (shelf, title) = match rest {
                [shelf, title, ..] => (shelf, title),
                _ => usage(),
            };
            match lib.add(shelf, title, &body, &labels) {
                Ok(id) => println!("filed {id} on shelf {shelf}"),
                Err(e) => refused(e),
            }
        }
        "revise" => {
            let id: i64 = rest.first().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
            let labels_given = if labels.is_empty() { None } else { Some(labels.as_slice()) };
            let body_given = if body.is_empty() { None } else { Some(body.as_str()) };
            match lib.revise(id, title_flag.as_deref(), body_given, labels_given) {
                Ok(()) => println!("revised {id} - the old version stays in `history {id}`"),
                Err(e) => refused(e),
            }
        }
        "history" => {
            let id: i64 = rest.first().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
            let past = lib.history(id).unwrap_or_else(|e| failed(e));
            if past.is_empty() {
                println!("entry {id} was never corrected");
            }
            for (when, title, body, shelf) in past {
                let where_ = if shelf.is_empty() { String::new() } else { format!("  (stond op {shelf})") };
                println!("{when}{where_}  {title}
{body}
");
            }
        }
        "move" => {
            let (id, shelf) = match rest {
                [id, shelf, ..] => (id.parse::<i64>().unwrap_or_else(|_| usage()), shelf),
                _ => usage(),
            };
            match lib.move_to(id, shelf) {
                Ok(()) => println!("moved {id} to shelf {shelf} - same number, and `history {id}` says where it was"),
                Err(e) => refused(e),
            }
        }
        "retire" => {
            let id: i64 = rest.first().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage());
            match lib.retire(id, &why) {
                Ok(()) => println!("retired {id} - still readable with `read {id}`"),
                Err(e) => refused(e),
            }
        }
        "labels" => {
            let name = rest.first().unwrap_or_else(|| usage());
            let counts = lib.labels(name).unwrap_or_else(|e| failed(e));
            if counts.is_empty() {
                println!("shelf {name} uses no labels yet");
            }
            for (label, n) in counts {
                println!("{label:<20} {n}");
            }
        }
        "health" => {
            let said = lib.health().unwrap_or_else(|e| failed(e));
            if said.is_empty() {
                println!("nothing to report");
            }
            for line in said {
                println!("{line}");
            }
        }
        "export" => {
            let path = rest.first().unwrap_or_else(|| usage());
            let mut f = std::fs::File::create(path).unwrap_or_else(|e| failed(e.to_string()));
            let n = lib.export_jsonl(&mut f).unwrap_or_else(|e| failed(e));
            println!("wrote {n} line(s) to {path}");
        }
        "restore" => {
            let path = rest.first().unwrap_or_else(|| usage());
            let text = std::fs::read_to_string(path).unwrap_or_else(|e| failed(e.to_string()));
            let n = lib.import_jsonl(&text).unwrap_or_else(|e| failed(e));
            println!("restored {n} line(s) from {path}");
        }
        "shelf-new" => {
            let name = rest.first().unwrap_or_else(|| usage());
            match lib.create_shelf(name, &note) {
                Ok(()) => println!("shelf {name} created"),
                Err(e) => refused(e),
            }
        }
        _ => usage(),
    }
}
