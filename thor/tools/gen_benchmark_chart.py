# Generates assets/benchmark.svg from the measured aggregates (BENCHMARKS.md).
# The chart is data: regenerate it here after every fresh benchmark run instead
# of hand-editing bar widths. Values below are the V5 round (run_v6, THOR
# b03c920): every test re-measured fresh, including the rebuilt Test 2 cuts.
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
out.append(f'<text x="20" y="{y}" fill="{MUT}" font-size="12">Same machine, blind-judged, re-measured fresh in the V5 round + V6/V7 code re-judges (3-judge median, blind maps).</text>')
y += 30

# ---- test 1 ----
header("TEST 1 - COVERAGE  (200 questions: 118 shared-knowledge + 82 stratified)", "deliberate recall over the live store . higher is better")
for label, t, m in [
    ("Code structure (re-judged)", 54.5, 56.1),
    ("Code behavior (re-judged)", 72.5, 59.2),
    ("Doc reference", 70.0, 66.2),
    ("Config how-to", 79.4, 76.5),
    ("Gotcha", 71.7, 65.2),
    ("Decision", 72.2, 55.6),
]:
    d = round(t - m)
    delta = f"+{d}%" if d > 0 else (f"{d}%" if d < 0 else "tie")
    pair(label, t, m, delta, delta_color=SOFT if d == 0 else None)
rule()
note(f'Test 1 overall  <tspan fill="{CYAN}">THOR 63.8%</tspan> vs <tspan fill="{MUT}">mimir 64.0%</tspan>  (n=200, full V5 run) - a statistical tie', color=FG, size=13, bold=True)
note("The CODE rows show the current state: re-judged after the V6/V7 fixes (fresh dumps, fresh blind juries, same 93")
note("items). In the original full run they read 42.4 vs 63.6 and 60.0 vs 62.5 - the overall above still uses those, so")
note("it UNDERSTATES THOR's current position. Knowledge rows and the overall are the full V5 run, unchanged.")
y += 14

# ---- test 1 addendum: the V6/V7 code re-judges ----
header("HOW THE CODE CATEGORIES GOT THERE (V6 + V7 rounds)", "serving parity + path affinity, then the derived symbol sidecar . re-judged per round")
note("Behavior flipped and held above 70 twice (60.0 -> 71.7 -> 72.5 vs mimir 62.5 -> 57.5 -> 59.2). Structure went", color=SOFT)
note("42.4 -> 50.0 -> 54.5 while mimir fell 63.6 -> 57.6 -> 56.1: a 21-point gap closed to 1.6 - and excluding four", color=SOFT)
note("dead-source items (files deliberately removed; both systems score zero) both judge at exactly 60.3% vs 60.3%.", color=SOFT)
note("New agent tools from the same round: where_used / impact / outline on a derived, rebuildable symbol sidecar.", color=SOFT)
y += 14

# ---- test 2 ----
header("TEST 2 - SAME KNOWLEDGE  (facts both systems verifiably hold)", "RE-MEASURED on a rebuilt shared subset . cuts over the judged Test 1 medians")
pair("Strict dual-written (n=53)", 96.2, 93.4, "+3%")
pair("Broad shared (n=152)", 77.0, 81.2, "-4%")
note("THOR takes the strict dual-written cut FOR THE FIRST TIME (96.2% vs 93.4%) - pure memory recall was mimir's home", color=SOFT)
note("turf across four earlier juries. mimir takes the broad cut, which is two-thirds code/doc-chunk questions.", color=SOFT)
y += 14

# ---- test 3 ----
header("TEST 3 - MULTI-PROJECT  (three private project repos seeded)", "45 real questions from each repo, scoped per project, blind-judged by 3-judge median . top-5 full chunks")
pair("Project 1", 90.0, 93.3, "-3%")
pair("Project 2", 100.0, 86.7, "+13%")
pair("Project 3", 86.7, 96.7, "-10%")
rule()
note(f'Test 3 overall  <tspan fill="{CYAN}">THOR 92.2%</tspan> vs <tspan fill="{MUT}">mimir 92.2%</tspan>  (n=45) - a dead tie', color=FG, size=13, bold=True)
note("mimir's previous outright lead (98.9% vs 92.2%) is gone. Per-project numbers on n=15 move with a single question")
note("(6.7 points each): treat the project split as noisy and the overall tie as the takeaway.")
y += 14

# ---- drift ----
header("SESSION DRIFT COMPENSATION  (73 fresh-session scenarios)", "post-compaction, empty context: does the AS-DEPLOYED auto-injection surface the fact that stops the drift?")
pair("Preventer surfaced", 72.1, 58.9, "+13%")
pair("Clear catch (full)", 55.9, 47.9, "+8%")
note("THOR's best drift numbers to date, judged THREE-WAY BLIND for the first time (courier / deliberate / mimir shuffled", color=SOFT)
note("onto anonymous sides in one jury pass). The as-deployed courier now also beats THOR's own deliberate recall", color=SOFT)
note("(62.5 / 48.6) - author-declared bilingual fires-when triggers fire on task prompts score-only ranking misses.", color=SOFT)
note("Pins + file/command guards cover the rest by construction: cargo run --example drift_eval, reproducible in-repo.", color=SOFT)
y += 14

# ---- speed ----
header("SPEED AND TOKENS  (per-prompt cost, lower is better)", "THOR cold vs mimir's as-deployed cold path, median of 20, canonical fixed 20-prompt set")
pair("Latency (as-deployed)", 206.8, 580.7, "2.8x faster", delta_color=CYAN, unit=" ms", scale=330 / 580.7)
pair("Tokens injected", 784, 237, "3.3x MORE (not fewer)", delta_color=AMBER, unit="", scale=330 / 784)
note("NEW: THOR's own opt-in warm inject daemon (idea credit mimir) serves the provably IDENTICAL decision at 192.7 ms.", color=SOFT)
note("mimir's opt-in warm daemon stays the fastest channel outright - 38.9 ms, ~32 tokens - but served NOTHING on 10 of", color=SOFT)
note("the 20 prompts (single floor-gated memory): faster and cheaper because it serves less. THOR's cold median is back", color=SOFT)
note("under its own 250 ms guardrail (253 -> 206.8 ms). Old '1.5x faster / 2.1x fewer tokens' headline stays retired.", color=SOFT)
y += 14

# ---- downsides ----
header("HONEST DOWNSIDES", "where THOR does not win, and the caveats")
y += 12
for b in [
    "Test 1 overall is a tie now, not a THOR lead - and the movement was asymmetric (THOR down, mimir up, same corpus)",
    "Code-structure: level but not won (54.5 vs 56.1 re-judged; 60.3-60.3 excl. dead sources) - 70% target unmet",
    "mimir wins the broad shared cut (81.2% vs 77.0%) - two-thirds code questions, same weakness on an equal corpus",
    "The 80%-everywhere goal still does not stand: 0 of 8 v4 gates (config how-to closest at 79.4%, drift surfaced 72.1%)",
    "mimir's warm daemon stays fastest outright (38.9 vs 192.7 ms warm); THOR injects ~3.3x more tokens than mimir cold",
    "Three drift golds stay honest misses (deep drift, zero shared vocabulary) - left as gaps, not trigger-stuffed",
    "mimir has a code-symbol graph (graph/outline/peek) that THOR deliberately does NOT - for 'which functions call X'",
    "Semantic mode needs a ~235 MB model + a warm embed-daemon resident (off by default; degrades cleanly to bm25)",
    "Newer and far less battle-tested than mimir's daily use",
    "One machine, private corpus, LLM-judged - only drift is repo-reproducible (examples/drift_eval.rs)",
]:
    bullet(b)
y += 8
rule()
y += 2
note(
    f'Bottom line <tspan fill="{SOFT}">coverage tie (63.8 vs 64.0)</tspan> . <tspan fill="{CYAN}">strict dual-written 96.2 vs 93.4, 1st win</tspan> . <tspan fill="{SOFT}">multi-project tie</tspan>',
    color=FG, size=13.5, bold=True,
)
y += 4
note(
    f'<tspan fill="{CYAN}">drift 72 vs 59</tspan> . <tspan fill="{CYAN}">code re-judge: behavior 72.5 vs 59.2, structure level</tspan> . <tspan fill="{CYAN}">2.8x faster than mimir cold</tspan>',
    color=FG, size=13.5, bold=True,
)

# Width guard: estimate every left-anchored text line's rendered width and
# fail LOUDLY on overflow - overlapping/clipped text shipped once; never again.
import re as _re
for line in out:
    m = _re.match(r'<text x="([\d.]+)" y="([\d.]+)"[^>]*font-size="([\d.]+)"[^>]*>(.*)</text>', line)
    if not m or 'text-anchor="end"' in line:
        continue
    _x, _y, _fs, _content = float(m.group(1)), m.group(2), float(m.group(3)), m.group(4)
    _plain = _re.sub(r'<[^>]+>', '', _content)
    _est = _x + len(_plain) * 0.58 * _fs
    assert _est <= 852, f"text overflows the 860 canvas (~{_est:.0f}px) at y={_y}: {_plain[:70]}"

H = y + 16
svg = (
    f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 860 {H}" font-family="Segoe UI, Helvetica Neue, Arial, sans-serif">\n'
    f'<rect x="0" y="0" width="860" height="{H}" rx="10" fill="#0d1117"/>\n' + "\n".join(out) + "\n</svg>\n"
)
path = os.path.join(os.path.dirname(__file__), "..", "..", "assets", "benchmark.svg")
with open(path, "w", encoding="utf-8", newline="\n") as f:
    f.write(svg)
print(f"wrote {os.path.abspath(path)} (height {H})")
