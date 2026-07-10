# Generates assets/benchmark.svg from the measured aggregates (BENCHMARKS.md).
# The chart is data: regenerate it here after every fresh benchmark run instead
# of hand-editing bar widths. Values below are the 2026-07-11 round-2 re-run
# (Test 2 numbers are the last-measured v3 values - not re-measured this round).
import os

CYAN = "#22d3ee"
GREY = "#484f58"
AMBER = "#e3b341"
FG = "#e6edf3"
MUT = "#8b949e"
SOFT = "#c9d1d9"
PX_PER_PCT = 3.3
BAR_X = 196

out = []
y = 0

def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")

def header(title, sub):
    global y
    out.append(f'<text x="20" y="{y}" fill="{SOFT}" font-size="12.5" font-weight="700" letter-spacing="0.4">{esc(title)}</text>')
    y += 16
    out.append(f'<text x="20" y="{y}" fill="{MUT}" font-size="11">{esc(sub)}</text>')
    y += 10

def pair(label, thor, mimir, delta, delta_color=None, unit="%", scale=PX_PER_PCT):
    global y
    tw = thor * scale
    mw = mimir * scale
    out.append(f'<rect x="{BAR_X}" y="{y}" width="{tw:.1f}" height="14" rx="7" fill="{CYAN}"/>')
    out.append(f'<text x="{BAR_X + tw + 7:.2f}" y="{y + 11.5}" fill="{FG}" font-size="11.5" font-weight="700">{round(thor)}{unit}</text>')
    out.append(f'<text x="184" y="{y + 11.5}" fill="{CYAN}" font-size="10.5" font-weight="700" text-anchor="end">THOR</text>')
    label_y = y + 22
    out.append(f'<text x="20" y="{label_y}" fill="{FG}" font-size="13" font-weight="700">{esc(label)}</text>')
    color = delta_color or (CYAN if not delta.startswith("-") else AMBER)
    out.append(f'<text x="838" y="{label_y + 1}" fill="{color}" font-size="15" font-weight="800" text-anchor="end">{esc(delta)}</text>')
    y += 20
    out.append(f'<rect x="{BAR_X}" y="{y}" width="{mw:.1f}" height="14" rx="7" fill="{GREY}"/>')
    out.append(f'<text x="{BAR_X + mw + 7:.2f}" y="{y + 11.5}" fill="{MUT}" font-size="11.5">{round(mimir)}{unit}</text>')
    out.append(f'<text x="184" y="{y + 11.5}" fill="{MUT}" font-size="10.5" text-anchor="end">mimir</text>')
    y += 30

def rule():
    global y
    out.append(f'<line x1="20" y1="{y}" x2="840" y2="{y}" stroke="#21262d" stroke-width="1"/>')
    y += 18

def note(text, color=MUT, size=11, bold=False):
    global y
    w = ' font-weight="700"' if bold else ""
    out.append(f'<text x="20" y="{y}" fill="{color}" font-size="{size}"{w}>{text}</text>')
    y += 16

def bullet(text):
    global y
    out.append(f'<circle cx="26" cy="{y - 4}" r="3" fill="{AMBER}"/>')
    out.append(f'<text x="38" y="{y}" fill="{FG}" font-size="11.5">{esc(text)}</text>')
    y += 21

# ---- title ----
y = 32
out.append(f'<text x="20" y="{y}" fill="{FG}" font-size="22" font-weight="800">THOR vs mimir - the honest picture</text>')
y += 22
out.append(f'<text x="20" y="{y}" fill="{MUT}" font-size="12">Same machine, blind-judged, every test re-measured fresh 2026-07-11 round 2 (3-judge median, fresh salted blind maps, level playing field). Wins and losses.</text>')
y += 30

# ---- test 1 ----
header("TEST 1 - COVERAGE  (200 questions: 118 shared-knowledge + 82 stratified)", "deliberate recall over the live store . higher is better")
for label, t, m in [
    ("Code structure", 50.0, 57.6),
    ("Code behavior", 67.5, 54.2),
    ("Doc reference", 75.0, 60.0),
    ("Config how-to", 79.4, 73.5),
    ("Gotcha", 76.1, 69.6),
    ("Decision", 70.4, 57.4),
]:
    d = round(t - m)
    delta = f"+{d}%" if d > 0 else (f"{d}%" if d < 0 else "tie")
    pair(label, t, m, delta, delta_color=SOFT if d == 0 else None)
rule()
note(f'Test 1 overall  <tspan fill="{CYAN}">THOR 68.5%</tspan> vs <tspan fill="{MUT}">mimir 59.8%</tspan>  (n=200)', color=FG, size=13, bold=True)
note("mimir keeps code-structure (its tree-sitter CodeChunk indexing); THOR leads the other five categories and overall.")
note("Both systems scored lower than the same-day earlier round on the identical corpus - jury strictness varies; the relative picture carries.")
y += 14

# ---- test 2 ----
header("TEST 2 - SAME KNOWLEDGE  (118 facts both systems have)", "NOT RE-MEASURED this round . previous (v3) round's numbers shown for reference")
pair("Answer present", 61.4, 53.8, "+8%")
note("Previous-round result: THOR led the broad shared set by +8%; mimir kept the strictest dual-written-only cut (n=53), 94% vs 92%.", color=SOFT)
note("No fresh Test 2 data this round - see BENCHMARKS.md.", color=SOFT)
y += 14

# ---- test 3 ----
header("TEST 3 - MULTI-PROJECT  (three private project repos seeded)", "45 real questions from each repo, scoped per project, blind-judged by 3-judge median . top-5 full chunks")
pair("Project 1", 90.0, 100.0, "-10%")
pair("Project 2", 100.0, 100.0, "tie", delta_color=SOFT)
pair("Project 3", 86.7, 96.7, "-10%")
rule()
note(f'Test 3 overall  <tspan fill="{MUT}">mimir 98.9%</tspan> vs <tspan fill="{CYAN}">THOR 92.2%</tspan>  (n=45)', color=FG, size=13, bold=True)
note("mimir leads outright: its code-content indexing erased THOR's biggest structural edge (Project 3, a 17-point THOR")
note("lead two runs ago, is now a mimir win); THOR's earlier Project 2 win is now a 100% tie. THOR wins no project outright.")
y += 14

# ---- drift ----
header("SESSION DRIFT COMPENSATION  (73 fresh-session scenarios)", "post-compaction, empty context: does the AS-DEPLOYED auto-injection surface the fact that stops the drift?")
pair("Preventer surfaced", 69.9, 50.7, "+19%")
pair("Clear catch (full)", 50.7, 39.7, "+11%")
note("Catch CONVERSION improved: 72.5% of courier surfacings are now full catches (was 64.7%) - the wider chunk windows.", color=SOFT)
note("The courier's per-scenario home scoping (correcting an instrument bug that also understated all previously published", color=SOFT)
note("courier numbers) is baked in from the start this round. Pins + file/command guards cover the rest by construction", color=SOFT)
note("(16/16 surfaced on the committed corpus): cargo run --example drift_eval, reproducible in-repo.", color=SOFT)
y += 14

# ---- speed ----
header("SPEED AND TOKENS  (per-prompt cost, lower is better)", "THOR courier vs mimir's as-deployed cold path, median of 20, canonical fixed 20-prompt set")
pair("Latency (as-deployed)", 253, 589.5, "2.3x faster", delta_color=CYAN, unit=" ms", scale=330 / 589.5)
pair("Tokens injected", 679, 235.9, "2.9x MORE (not fewer)", delta_color=AMBER, unit="", scale=330 / 679)
note("mimir's opt-in warm daemon (single best-effort memory, not shown as a bar) is faster and quieter still - 62 ms median,", color=SOFT)
note("48 tokens avg - but served NOTHING on 5 of the 20 prompts (relevance floor): lower coverage on the same prompts.", color=SOFT)
note("Old '1.5x faster / 2.1x fewer tokens' headline stays retired. THOR's 253 ms median is marginally over its own 250 ms guardrail.", color=SOFT)
y += 14

# ---- downsides ----
header("HONEST DOWNSIDES", "where THOR does not win, and the caveats")
y += 12
for b in [
    "mimir leads the multi-project test outright, 98.9% vs 92.2% - its code-content indexing erased THOR's biggest structural edge; THOR wins no project outright",
    "mimir keeps code-structure on Test 1, 58% vs 50% - THOR's new symbol-boundary chunker landed but did not measurably lift the judged score",
    "The v4 80% goal round closed at 0 of 8 gates on this stricter run (config-how-to slipped from 85.3% to 79.4%)",
    "mimir's opt-in warm daemon beats THOR's latency (62 ms vs 253 ms), and THOR injects more tokens than mimir's cold path (679 vs 236), not fewer",
    "THOR's courier median (253 ms) is marginally over its own 250 ms latency guardrail (symbol chunking on the freshness path)",
    "Same-knowledge (Test 2) not re-measured this round; previous round mimir led the strictest dual-written cut (n=53), 94% vs 92% - pure memory recall is its home turf",
    "mimir has a code-symbol graph (graph/outline/peek) that THOR deliberately does NOT - for 'which functions call X'",
    "Semantic mode needs a ~235 MB model + a warm embed-daemon resident (off by default; degrades cleanly to bm25)",
    "Newer and far less battle-tested than mimir's daily use",
    "One machine, private corpus, LLM-judged - only the drift mechanism is reproducible from this repo (examples/drift_eval.rs)",
]:
    bullet(b)
y += 8
rule()
y += 2
note(
    f'Bottom line  <tspan fill="{CYAN}">coverage 68.5% vs 59.8%</tspan>  .  <tspan fill="{SOFT}">same-knowledge not re-measured</tspan>  .  '
    f'<tspan fill="{MUT}">multi-project 92% vs 99% (mimir)</tspan>  .  <tspan fill="{CYAN}">drift surfaced 70% vs 51%, latency 2.3x faster than mimir cold</tspan>',
    color=FG, size=13.5, bold=True,
)

H = y + 16
svg = (
    f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 860 {H}" font-family="Segoe UI, Helvetica Neue, Arial, sans-serif">\n'
    f'<rect x="0" y="0" width="860" height="{H}" rx="10" fill="#0d1117"/>\n' + "\n".join(out) + "\n</svg>\n"
)
path = os.path.join(os.path.dirname(__file__), "..", "..", "assets", "benchmark.svg")
with open(path, "w", encoding="utf-8", newline="\n") as f:
    f.write(svg)
print(f"wrote {os.path.abspath(path)} (height {H})")
