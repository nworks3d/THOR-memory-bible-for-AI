//! Small helper for the thor2 migration tooling (tools/convert.py): reads
//! JSON-lines `{"id": ..., "body": ...}` from stdin and, for each, calls
//! `thor_core::footer`'s own `strip` / `extract` / `anchors` / `fact_type` -
//! never re-ported in Python - writing one JSON-line result per input line to
//! stdout: `{"id", "stripped_body", "footer_line", "anchors", "fact_type"}`.
//!
//! Lives here (in `core`, not `model`) because `model` must never call
//! `core::footer` (see `model/tests/no_footer_calls.rs`, `FINDINGS-1.0.md`
//! F1): reading a 1.0-era prose footer during migration is exactly the one
//! purpose `core::footer`'s own doc comment says it survives for.

use serde::Serialize;
use std::io::{self, BufRead, Write};
use thor_core::footer;
use thor_core::repo::FactType;

#[derive(Serialize)]
struct FooterInfo<'a> {
    id: &'a str,
    stripped_body: &'a str,
    footer_line: Option<&'a str>,
    anchors: Vec<String>,
    fact_type: Option<&'static str>,
}

fn fact_type_str(ft: Option<FactType>) -> Option<&'static str> {
    ft.map(|t| t.as_str())
}

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line.expect("failed to read stdin line");
        if line.trim().is_empty() {
            continue;
        }
        let input: serde_json::Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("bad input JSON line: {e}\nline: {line}"));
        let id = input["id"].as_str().expect("input line missing 'id' string");
        let body = input["body"].as_str().expect("input line missing 'body' string");

        let info = FooterInfo {
            id,
            stripped_body: footer::strip(body),
            footer_line: footer::extract(body),
            anchors: footer::anchors(body),
            fact_type: fact_type_str(footer::fact_type(body)),
        };
        let json = serde_json::to_string(&info).expect("failed to serialize FooterInfo");
        writeln!(out, "{json}").expect("failed to write stdout line");
    }
}
