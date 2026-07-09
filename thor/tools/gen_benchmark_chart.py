# Generates assets/benchmark.svg from the measured aggregates (BENCHMARKS.md).
# The chart is data: regenerate it here after every fresh benchmark run instead
# of hand-editing bar widths. Values below are the 2026-07-09 fresh re-run.
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
out.append(f'<text x="20" y="{y}" fill="{MUT}" font-size="12">Same machine, blind-judged, every number re-measured fresh 2026-07-09 (independent jury). Wins and losses.</text>')
y += 30

# ---- test 1 ----
header("TEST 1 - COVERAGE  (200 questions: 118 shared-knowledge + 82 stratified)", "deliberate recall over the live store . higher is better")
for label, t, m in [
    ("Code structure", 53.0, 53.0),
    ("Code behavior", 70.8, 53.3),
    ("Doc reference", 65.0, 53.8),
    ("Config how-to", 82.4, 76.5),
    ("Gotcha", 69.6, 71.7),
    ("Decision", 72.2, 61.1),
]:
    d = round(t - m)
    delta = f"+{d}%" if d > 0 else (f"{d}%" if d < 0 else "tie")
    pair(label, t, m, delta, delta_color=SOFT if d == 0 else None)
rule()
note(f'Test 1 overall  <tspan fill="{CYAN}">THOR 68%</tspan> vs <tspan fill="{MUT}">mimir 59%</tspan>  (n=200)', color=FG, size=13, bold=True)
note("Balanced set (the earlier 504-question run was dominated by code-only questions, where the gap was far larger).")
y += 14

# ---- test 2 ----
header("TEST 2 - SAME KNOWLEDGE  (118 facts both systems have)", "pure retrieval quality on an equal corpus . the fair, apples-to-apples comparison")
pair("Answer present", 64.8, 56.8, "+8%")
note("THOR leads the broad shared set by +8%, but mimir wins the strictest dual-written-only cut (n=53): mimir 94% vs THOR 91%", color=SOFT)
y += 14

# ---- test 3 ----
header("TEST 3 - MULTI-PROJECT  (three private project repos seeded)", "45 real questions from each repo, scoped per project, blind-judged by 3 . top-5 full chunks")
pair("Project 1", 86.7, 96.7, "-10%")
pair("Project 2", 100.0, 0.0, "+100%")
pair("Project 3", 96.7, 80.0, "+17%")
rule()
note(f'Test 3 overall  <tspan fill="{CYAN}">THOR 94%</tspan> vs <tspan fill="{MUT}">mimir 59%</tspan>  (n=45)', color=FG, size=13, bold=True)
note("mimir still wins Project 1 on its hand-curated design docs (gap closed from 26 to 10 points after the ranking round);")
note("THOR wins where it uniquely holds the code. Project 2 has no mimir doc collection at all.")
y += 14

# ---- drift ----
header("SESSION DRIFT COMPENSATION  (73 fresh-session scenarios, prompt-association only)", "post-compaction, empty context: does recall surface the fact that stops the drift? THOR = deliberate recall")
pair("Preventer surfaced", 60.3, 53.4, "+7%")
pair("Clear catch (full)", 39.7, 43.8, "-4%")
note("As-deployed courier scores lower (51% / 19%): its noise gates filter weakly-associated preventers. THOR's real", color=SOFT)
note("drift answer is structural - pinned rules re-inject after every compaction and the file-touch guard fires at the", color=SOFT)
note("moment of action (8/8 on the committed corpus): cargo run --example drift_eval, reproducible in-repo.", color=SOFT)
y += 14

# ---- speed ----
header("SPEED AND TOKENS  (per-prompt cost, lower is better)", "warm daemon, median of 20, same long task prompts for both systems")
pair("Latency (warm)", 268, 505, "1.9x faster", delta_color=CYAN, unit=" ms", scale=330 / 505)
pair("Tokens injected", 351, 845, "2.4x fewer", delta_color=CYAN, unit="", scale=330 / 845)
y += 14

# ---- downsides ----
header("HONEST DOWNSIDES", "where THOR does not win, and the caveats")
y += 12
for b in [
    "On the strictest same-knowledge cut (dual-written memories, n=53) mimir leads 94% vs 91% - pure memory recall is its home turf",
    "On prompt-only drift, mimir's full-catch is higher (44% vs 40%); THOR's as-deployed noise gates cost catch (19%)",
    "Curated docs can beat raw ingest: on one project's design questions mimir's hand-written doc collection scored 97% vs 87%",
    "mimir has a code-symbol graph (graph/outline/peek) that THOR deliberately does NOT - for 'which functions call X'",
    "Semantic mode needs a ~235 MB model + a ~570 MB warm daemon (off by default; degrades cleanly to bm25)",
    "Newer and far less battle-tested than mimir's daily use",
    "One machine, private corpus, LLM-judged - only the drift mechanism is reproducible from this repo (examples/drift_eval.rs)",
]:
    bullet(b)
y += 8
rule()
y += 2
note(
    f'Bottom line  <tspan fill="{CYAN}">coverage 68% vs 59%</tspan>  .  <tspan fill="{SOFT}">same-knowledge 65% vs 57%</tspan>  .  '
    f'<tspan fill="{SOFT}">multi-project 94% vs 59%</tspan>  .  <tspan fill="{CYAN}">1.9x faster, 2.4x fewer tokens</tspan>',
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
