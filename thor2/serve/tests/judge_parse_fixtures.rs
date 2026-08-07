//! Task 3 fixture set: realistic messy judge outputs the kind of thing a
//! small LOCAL model actually returns - as opposed to the clean, well-
//! behaved `VERDICT: <token>` / `JUSTIFICATION: <text>` shape a hosted model
//! (Claude Haiku, in the measured A/B) reliably produces. Every fixture is
//! labeled with its class and run through `judge::parse_judge_output`
//! directly - pure, no process spawn, no `fake_judge` binary needed here.
//!
//! Run with `cargo test -p serve --test judge_parse_fixtures -- --nocapture`
//! to see the per-class parse-rate table this file prints (also reported in
//! JUDGE-TRANSPORT.md, captured from an actual run on this machine).
//!
//! The required six regression tests named after the specific defect each
//! prevents (a verdict wrapped in prose, a fenced verdict, a json-shaped
//! verdict, an ambiguous answer, garbage, "no verdict can never block") live
//! in `serve/src/judge.rs`'s own `#[cfg(test)] mod tests`, colocated with
//! `parse_judge_output` itself; this file is the broader, labeled fixture
//! set Task 3 asked for, one level up.

use serve::judge::{parse_judge_output, JudgeVerdict};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    Block,
    Warn,
    Allow,
    NoVerdict,
}

struct Fixture {
    class: &'static str,
    label: &'static str,
    raw: &'static str,
    expect: Expect,
}

fn fixtures() -> Vec<Fixture> {
    vec![
        // ---- clean: the hosted-model shape parse_judge_output originally
        // assumed, kept as the baseline every other class is measured against ----
        Fixture {
            class: "clean",
            label: "clean block",
            raw: "VERDICT: BLOCK\nJUSTIFICATION: \"from now on always run the linter first\"",
            expect: Expect::Block,
        },
        Fixture { class: "clean", label: "clean warn", raw: "VERDICT: WARN\nJUSTIFICATION: genuinely ambiguous", expect: Expect::Warn },
        Fixture { class: "clean", label: "clean allow", raw: "VERDICT: ALLOW\nJUSTIFICATION: just a question", expect: Expect::Allow },
        Fixture { class: "clean", label: "bare token, no justification at all", raw: "VERDICT: BLOCK", expect: Expect::Block },

        // ---- prose-wrapped: reasoning before and/or after the verdict ----
        Fixture {
            class: "prose-wrapped",
            label: "reasoning before the verdict",
            raw: "Let me think about this carefully. The user is stating a standing rule for the future, not asking a question.\n\nVERDICT: BLOCK\nJUSTIFICATION: \"from now on always run the linter first\"",
            expect: Expect::Block,
        },
        Fixture {
            class: "prose-wrapped",
            label: "commentary after the verdict",
            raw: "VERDICT: ALLOW\nJUSTIFICATION: \"can you explain how the linter config works?\"\n\nHope that helps! Let me know if there's another prompt to judge.",
            expect: Expect::Allow,
        },
        Fixture {
            class: "prose-wrapped",
            label: "verdict stated mid-sentence",
            raw: "Based on my analysis, my VERDICT: WARN reflects genuine ambiguity here, since this could be read either way.",
            expect: Expect::Warn,
        },
        Fixture {
            class: "prose-wrapped",
            label: "reasoning both before and after",
            raw: "This prompt reads like ordinary work to me, a one-off bug fix with no standing rule attached.\n\nVERDICT: ALLOW\nJUSTIFICATION: \"fix the off-by-one in the paginator\"\n\nLet me know if you disagree with this call.",
            expect: Expect::Allow,
        },

        // ---- fenced: markdown code fences, inline code, bold ----
        Fixture {
            class: "fenced",
            label: "plain triple-backtick fence",
            raw: "```\nVERDICT: BLOCK\nJUSTIFICATION: \"the new rule is\"\n```",
            expect: Expect::Block,
        },
        Fixture {
            class: "fenced",
            label: "language-tagged fence",
            raw: "```text\nVERDICT: ALLOW\nJUSTIFICATION: ordinary bug fix\n```",
            expect: Expect::Allow,
        },
        Fixture {
            class: "fenced",
            label: "bolded verdict and justification lines",
            raw: "**VERDICT: WARN**\n**JUSTIFICATION:** could go either way",
            expect: Expect::Warn,
        },
        Fixture {
            class: "fenced",
            label: "inline-code verdict",
            raw: "My answer: `VERDICT: BLOCK`\n`JUSTIFICATION: from now on always run the linter`",
            expect: Expect::Block,
        },

        // ---- json: {"verdict": "...", "reason"/"justification": "..."} ----
        Fixture {
            class: "json",
            label: "clean top-level json",
            raw: r#"{"verdict": "BLOCK", "reason": "from now on always run the linter first"}"#,
            expect: Expect::Block,
        },
        Fixture {
            class: "json",
            label: "lowercase value, justification key",
            raw: r#"{"verdict": "allow", "justification": "just a question"}"#,
            expect: Expect::Allow,
        },
        Fixture {
            class: "json",
            label: "json wrapped in prose",
            raw: "Sure, here is my answer:\n\n{\"verdict\": \"WARN\", \"reason\": \"borderline\"}\n\nLet me know if you need anything else.",
            expect: Expect::Warn,
        },
        Fixture {
            class: "json",
            label: "json inside a fenced code block",
            raw: "```json\n{\"verdict\": \"BLOCK\", \"reason\": \"new rule stated\"}\n```",
            expect: Expect::Block,
        },

        // ---- restates-question: the model repeats the prompt or the
        // instructions back before answering ----
        Fixture {
            class: "restates-question",
            label: "restates the owner's prompt first",
            raw: "The prompt you gave me was: \"from now on always run the linter first\".\n\nThis is clearly a standing rule for the future.\n\nVERDICT: BLOCK\nJUSTIFICATION: \"from now on always run the linter first\"",
            expect: Expect::Block,
        },
        Fixture {
            class: "restates-question",
            label: "restates the task before answering ALLOW",
            raw: "You want me to judge whether this prompt states a durable decision: \"can you explain how the linter config works?\"\n\nThis is just a question, not a new rule.\n\nVERDICT: ALLOW\nJUSTIFICATION: \"can you explain how the linter config works?\"",
            expect: Expect::Allow,
        },
        Fixture {
            class: "restates-question",
            label: "echoes the literal answer-format placeholder first",
            raw: "You asked me to answer in the format:\nVERDICT: <BLOCK|WARN|ALLOW>\nJUSTIFICATION: <one line, must quote the exact phrase from the prompt you relied on>\n\nMy real answer:\nVERDICT: ALLOW\nJUSTIFICATION: just a question",
            expect: Expect::Allow,
        },

        // ---- ambiguous: must yield no verdict, never a guess ----
        Fixture {
            class: "ambiguous",
            label: "two contradicting VERDICT lines",
            raw: "VERDICT: BLOCK\nJUSTIFICATION: looks like a rule\n...actually, on reflection...\nVERDICT: ALLOW\nJUSTIFICATION: no, it's just a question",
            expect: Expect::NoVerdict,
        },
        Fixture {
            class: "ambiguous",
            label: "a hedge naming two verdicts, no structured signal",
            raw: "This is a tough one - it could be BLOCK or ALLOW depending on how you read it.",
            expect: Expect::NoVerdict,
        },
        Fixture {
            class: "ambiguous",
            label: "json verdict disagrees with a VERDICT line elsewhere",
            raw: "{\"verdict\": \"BLOCK\"}\n\nVERDICT: WARN\nJUSTIFICATION: unsure",
            expect: Expect::NoVerdict,
        },
        Fixture {
            class: "ambiguous",
            label: "hedge naming all three verdicts",
            raw: "Honestly this prompt sits right on the line between BLOCK, WARN, and ALLOW - I can see it going any of those three ways.",
            expect: Expect::NoVerdict,
        },

        // ---- garbage and empty output ----
        Fixture { class: "garbage-and-empty", label: "empty output", raw: "", expect: Expect::NoVerdict },
        Fixture { class: "garbage-and-empty", label: "whitespace only", raw: "   \n\n  \t\n", expect: Expect::NoVerdict },
        Fixture {
            class: "garbage-and-empty",
            label: "a refusal with no verdict anywhere",
            raw: "I'm sorry, I can't help with that request.",
            expect: Expect::NoVerdict,
        },
        Fixture { class: "garbage-and-empty", label: "truncated mid-word", raw: "VERD", expect: Expect::NoVerdict },
        Fixture {
            class: "garbage-and-empty",
            label: "json with no verdict field at all",
            raw: "{\"status\": \"ok\", \"note\": \"model warmed up\"}",
            expect: Expect::NoVerdict,
        },
        Fixture {
            class: "garbage-and-empty",
            label: "the common word 'block' in unrelated code, not a verdict",
            raw: "Sure, let's look at this code block:\n```rust\nfn foo() { block_on(fut) }\n```",
            expect: Expect::NoVerdict,
        },
    ]
}

/// One assertion per fixture: every fixture must parse into EXACTLY the
/// verdict its class promises (or into no verdict at all, for the ambiguous
/// and garbage classes) - then a per-class parse-rate table is printed.
#[test]
fn fixture_set_parses_exactly_as_labeled() {
    let all = fixtures();
    let mut classes: Vec<&str> = all.iter().map(|f| f.class).collect();
    classes.sort();
    classes.dedup();

    let mut failures: Vec<String> = Vec::new();
    let mut per_class: Vec<(&str, usize, usize)> = Vec::new(); // (class, matched, total)

    for class in &classes {
        let in_class: Vec<&Fixture> = all.iter().filter(|f| f.class == *class).collect();
        let mut matched = 0usize;
        for f in &in_class {
            let actual = parse_judge_output(f.raw);
            let ok = match (&f.expect, &actual) {
                (Expect::Block, Some(JudgeVerdict::Block(_))) => true,
                (Expect::Warn, Some(JudgeVerdict::Warn(_))) => true,
                (Expect::Allow, Some(JudgeVerdict::Allow)) => true,
                (Expect::NoVerdict, None) => true,
                _ => false,
            };
            if ok {
                matched += 1;
            } else {
                failures.push(format!(
                    "[{}] \"{}\": expected {:?}, got {:?} (raw: {:?})",
                    f.class, f.label, f.expect, actual, f.raw
                ));
            }
        }
        per_class.push((class, matched, in_class.len()));
    }

    println!("\nJudge output parser: fixture parse rate per class");
    println!("{:-<70}", "");
    for (class, matched, total) in &per_class {
        let pct = if *total == 0 { 0.0 } else { 100.0 * (*matched as f64) / (*total as f64) };
        println!("{:<24} {:>3}/{:<3} ({:>5.1}%)", class, matched, total, pct);
    }
    println!("{:-<70}", "");
    let total_matched: usize = per_class.iter().map(|(_, m, _)| *m).sum();
    let total_all: usize = per_class.iter().map(|(_, _, t)| *t).sum();
    println!("{:<24} {:>3}/{:<3}", "TOTAL", total_matched, total_all);

    assert!(failures.is_empty(), "fixture mismatches:\n{}", failures.join("\n"));
}

/// THE SAFETY PROPERTY, restated directly over the whole fixture set: every
/// fixture labeled "ambiguous" or "garbage-and-empty" must yield `None` -
/// and, doubly, must never come back as `Some(JudgeVerdict::Block(_))`
/// specifically. A failure here is a real defect (a guess reaching a
/// block), not a tuning matter.
#[test]
fn every_ambiguous_and_garbage_fixture_yields_no_verdict_and_never_blocks() {
    let unsafe_classes = ["ambiguous", "garbage-and-empty"];
    let mut checked = 0usize;
    for f in fixtures().iter().filter(|f| unsafe_classes.contains(&f.class)) {
        checked += 1;
        let actual = parse_judge_output(f.raw);
        assert_eq!(actual, None, "[{}] \"{}\" must yield no verdict, got {:?}", f.class, f.label, actual);
        assert!(
            !matches!(actual, Some(JudgeVerdict::Block(_))),
            "[{}] \"{}\" must never produce a Block, got {:?}",
            f.class,
            f.label,
            actual
        );
    }
    assert!(checked >= 8, "expected a real fixture set for the ambiguous/garbage classes, only found {checked}");
}
