// Micro-bench of the exact work the provenance feature adds to a courier
// injection: one env read per prompt (paid even when the flag is OFF), plus one
// footer::provenance parse per served hit (paid only when the flag is ON).
use std::time::Instant;

fn main() {
    // A representative served hit body: real memories are a few hundred chars.
    let body = format!(
        "the metrics server config detail lives here with several words of context \
         so the substring search has a realistic body to scan through\n\n{}",
        thor::footer::compose_full(
            "gotcha",
            &["a".into(), "b".into()],
            "global",
            &["metrics".into(), "port".into()],
            &[],
            None,
            Some("inferred"),
        )
    );

    let n: u64 = 2_000_000;
    let mut acc: u64 = 0;

    // (1) The per-PROMPT cost: one env var read. Paid on every injection, flag
    // off or on (it is read once before the hit loop).
    std::env::set_var("THOR_EXP_PROVENANCE", "1");
    let t = Instant::now();
    for _ in 0..n {
        if std::env::var("THOR_EXP_PROVENANCE").is_ok() {
            acc += 1;
        }
    }
    let env_ns = t.elapsed().as_nanos() as f64 / n as f64;

    // (2) The per-HIT cost (flag ON only): parse the provenance footer field.
    let t = Instant::now();
    for _ in 0..n {
        if thor::footer::provenance(&body).is_some() {
            acc += 1;
        }
    }
    let prov_ns = t.elapsed().as_nanos() as f64 / n as f64;
    std::env::remove_var("THOR_EXP_PROVENANCE");

    println!("(sink acc={acc})");
    println!("env read (per prompt):          {env_ns:.1} ns/op");
    println!("footer::provenance (per hit):   {prov_ns:.1} ns/op");
    for k in [2u64, 5, 8] {
        let per = env_ns + (k as f64) * prov_ns;
        println!(
            "=> added cost per prompt @ {k} served hits: {:.0} ns = {:.4} us",
            per,
            per / 1000.0
        );
    }
    println!("(for scale: a full THOR per-prompt injection is ~100 ms = 100_000 us)");
}
