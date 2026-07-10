# Generates assets/benchmark.svg from the measured aggregates (BENCHMARKS.md).
# The chart is data: regenerate it here after every fresh benchmark run instead
# of hand-editing bar widths. Values below are the 2026-07-11 v4-round re-run
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
out.append(f'<text x="20" y="{y}" fill="{MUT}" font-size="12">Same machine, blind-judged, every test re-measured fresh 2026-07-11 after the v4 round (3-judge median, level playing field). Wins and losses.</text>')
y += 30

# ---- test 1 ----
header("TEST 1 - COVERAGE  (200 questions: 118 shared-knowledge + 82 stratified)", "deliberate recall over the live store . higher is better")
for label, t, m in [
    ("Code structure", 53.8, 57.6),
    ("Code behavior", 70.8, 55.8),
    ("Doc reference", 77.5, 60.0),
    ("Config how-to", 85.3, 70.6),
    ("Gotcha", 76.1, 71.7),
    ("Decision", 72.2, 61.1),
]:
    d = round(t - m)
    delta = f"+{d}%" if d > 0 else (f"{d}%" if d < 0 else "tie")
    pair(label, t, m, delta, delta_color=SOFT if d == 0 else None)
rule()
note(f'Test 1 overall  <tspan fill="{CYAN}">THOR 71%</tspan> vs <tspan fill="{MUT}">mimir 61%</tspan>  (n=200)', color=FG, size=13, bold=True)
note("mimir now wins code-structure (its new tree-sitter CodeChunk indexing); THOR leads every other category and overall.")
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
pair("Project 2", 100.0, 96.7, "+3%")
pair("Project 3", 93.3, 93.3, "tie", delta_color=SOFT)
rule()
note(f'Test 3 overall  <tspan fill="{MUT}">mimir 97%</tspan> vs <tspan fill="{CYAN}">THOR 94%</tspan>  (n=45)', color=FG, size=13, bold=True)
note("mimir retakes the overall lead: its new code-content indexing closes Project 3's gap completely (was a 17-point THOR lead).")
note("mimir widens its lead on Project 1's curated docs (100 vs 90); THOR keeps Project 2 (100 vs 97).")
y += 14

# ---- drift ----
header("SESSION DRIFT COMPENSATION  (73 fresh-session scenarios)", "post-compaction, empty context: does the AS-DEPLOYED auto-injection surface the fact that stops the drift?")
pair("Preventer surfaced", 74.0, 56.2, "+18%")
pair("Clear catch (full)", 47.9, 42.5, "+5%")
note("CORRECTED this round: the courier was first measured with a fixed cwd for all 73 scenarios (an instrument bug -", color=SOFT)
note("also present, unnoticed, in the numbers previously published here); regenerating per-scenario with the scenario's", color=SOFT)
note("real home project raised surfaced from 35.6% to 74.0%. Pins + file/command guards cover the rest by construction", color=SOFT)
note("(16/16 surfaced on the committed corpus): cargo run --example drift_eval, reproducible in-repo.", color=SOFT)
y += 14

# ---- speed ----
header("SPEED AND TOKENS  (per-prompt cost, lower is better)", "THOR courier vs mimir's as-deployed cold path, median of 20, same 20 task prompts")
pair("Latency (as-deployed)", 234, 563.5, "2.4x faster", delta_color=CYAN, unit=" ms", scale=330 / 563.5)
pair("Tokens injected", 579, 235.9, "2.5x MORE (not fewer)", delta_color=AMBER, unit="", scale=330 / 579)
note("mimir's opt-in warm daemon (single best-effort memory, not shown as a bar) is faster still: 66 ms median.", color=SOFT)
note("Old '1.5x faster / 2.1x fewer tokens' headline does not hold on this round's sample and is retired.", color=SOFT)
y += 14

# ---- downsides ----
header("HONEST DOWNSIDES", "where THOR does not win, and the caveats")
y += 12
for b in [
    "mimir retook the overall multi-project lead this round, 97% vs 94% - its new code-content indexing closed THOR's biggest structural edge",
    "mimir now wins code-structure on Test 1, 58% vs 54% - its own tree-sitter CodeChunk indexing, folded into ordinary recall",
    "Only config-how-to cleared an 80% bar this round (THOR 85%); every other T1 category and every drift metric stayed below it",
    "On this round's speed sample mimir's opt-in warm daemon beats THOR's latency (66 ms vs 234 ms), and THOR injects more tokens than mimir's cold path, not fewer",
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
    f'Bottom line  <tspan fill="{CYAN}">coverage 71% vs 61%</tspan>  .  <tspan fill="{SOFT}">same-knowledge not re-measured</tspan>  .  '
    f'<tspan fill="{MUT}">multi-project 94% vs 97% (mimir)</tspan>  .  <tspan fill="{CYAN}">drift surfaced 74% vs 56%, latency 2.4x faster than mimir cold</tspan>',
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
