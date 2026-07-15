# Generates assets/benchmark.svg from the measured aggregates (BENCHMARKS.md).
# The chart is data: regenerate it here after every fresh benchmark run instead
# of hand-editing bar widths. Values below are the 2026-07-14 round: production
# THOR vs mimir v0.14.0, every test re-measured fresh, 3-judge median, blind.
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
out.append(f'<text x="20" y="{y}" fill="{MUT}" font-size="12">Same machine, blind-judged, re-measured fresh 2026-07-14 vs mimir v0.14.0 (3-judge median, fresh blind maps).</text>')
y += 30

# ---- test 1 ----
header("TEST 1 - COVERAGE  (200 questions: shared-knowledge + category-stratified)", "deliberate recall over the live store . higher is better")
for label, t, m in [
    ("Code structure", 63.6, 74.2),
    ("Code behavior", 78.3, 70.0),
    ("Doc reference", 81.2, 70.0),
    ("Config how-to", 88.2, 79.4),
    ("Gotcha", 73.9, 73.9),
    ("Decision", 79.6, 59.3),
]:
    d = round(t - m)
    delta = f"+{d}%" if d > 0 else (f"{d}%" if d < 0 else "tie")
    pair(label, t, m, delta, delta_color=SOFT if d == 0 else None)
rule()
note(f'Test 1 overall  <tspan fill="{CYAN}">THOR 77.0%</tspan> vs <tspan fill="{MUT}">mimir 70.5%</tspan>  (n=200) - THOR leads by 6.5 points', color=FG, size=13, bold=True)
note("THOR wins four of six categories (decision +20, doc-reference +11, config +9, behavior +8), ties gotcha, and loses")
note("only code-structure (63.6 vs 74.2) - a real, open gap, but see the downside below: the symbol-graph explanation")
note("we first published for it did not survive scrutiny.")
y += 14

# ---- test 2 ----
header("TEST 2 - SAME KNOWLEDGE  (facts both systems verifiably hold)", "cuts over the judged Test 1 medians . the equal-corpus comparison")
pair("Strict dual-written (n=53)", 97.2, 94.3, "+3%")
pair("Broad shared (n=152)", 88.8, 86.2, "+3%")
note("THOR wins BOTH same-knowledge cuts, including the strict dual-written cut (97.2 vs 94.3) - the cleanest equal-corpus", color=SOFT)
note("test there is, and pure memory recall was mimir's home turf across several earlier juries. THOR now leads it.", color=SOFT)
y += 14

# ---- test 3 ----
header("TEST 3 - MULTI-PROJECT  (three private project repos seeded)", "45 real questions from each repo, scoped per project, blind-judged 3-judge median . top-5 full chunks")
pair("Project 1", 93.3, 93.3, "tie", delta_color=SOFT)
pair("Project 2", 96.7, 100.0, "-3%")
pair("Project 3", 100.0, 93.3, "+7%")
rule()
note(f'Test 3 overall  <tspan fill="{CYAN}">THOR 96.7%</tspan> vs <tspan fill="{MUT}">mimir 95.6%</tspan>  (n=45) - THOR edges it', color=FG, size=13, bold=True)
note("Per-project numbers on n=15 move with a single question (6.7 points each): treat the split as noisy and the near-tie")
note("overall as the takeaway - both systems answer scoped project questions very well.")
y += 14

# ---- drift ----
header("SESSION DRIFT COMPENSATION  (73 fresh-session scenarios)", "post-compaction, empty context: does the AS-DEPLOYED auto-injection surface the fact that stops the drift?")
pair("Preventer surfaced", 86.3, 74.0, "+12%")
pair("Clear catch (full)", 58.9, 50.7, "+8%")
note("THOR leads BOTH drift metrics decisively - the as-deployed courier surfaces the preventing fact 86.3% (vs mimir's", color=SOFT)
note("best case 74.0%) and fully catches it 58.9% (vs 50.7%). This is the product's core purpose, and the channel built", color=SOFT)
note("for it wins. Separately reproducible in-repo, no judge: cargo run --example drift_eval (catches + false fires).", color=SOFT)
y += 14

# ---- speed ----
header("SPEED  (per-prompt cost, lower is better)", "as-deployed, correct invocation each side (THOR stdin hook, mimir prompt arg) . median of 20 . 2026-07-15 vs mimir 0.14")
pair("Full recall (like-for-like)", 119.8, 321.7, "2.7x faster", delta_color=CYAN, unit=" ms", scale=330 / 321.7)
note("THOR's inject daemon now holds the folded log + vector matrix resident: 349 -> 120 ms (-66%), byte-identical output.", color=SOFT)
note("On a like-for-like FULL recall THOR is 2.7x faster (120 vs 322 ms) and injects a full block on every prompt. With the", color=SOFT)
note("daemon STOPPED it is 349 ms - slightly slower than mimir there, and it grows with store size (230 ms at 12.7k events,", color=SOFT)
note("349 at 16.1k). mimir's as-deployed hook is much faster (~34 ms) but serves ONE floor-gated memory (175 chars) and is", color=SOFT)
note("empty on 6/20 prompts: fast because it serves less.", color=SOFT)
y += 14

# ---- downsides ----
header("HONEST DOWNSIDES", "where THOR does not win, and the caveats")
y += 12
for b in [
    "Code-structure is THOR's one category loss (63.6 vs 74.2). But mimir answers it mostly from PROSE, not code: its top",
    "hit is a doc 70% of the time (THOR 48%) and code only 21% (THOR 42%), and where the gold names a file THOR serves it",
    "more often (81% vs 69%). The golds are prose, which favours a doc that states the answer over source to interpret.",
    "On raw hook latency mimir is much faster (~34 vs ~120 ms) - but serves a single floor-gated memory, empty on 6/20",
    "THOR is compute-bound; latency grows with store size (230 ms at 12.7k events, 349 at 16.1k). The resident cache",
    "removes that growth only WHILE A DAEMON IS UP (120 ms); a bare per-prompt hook has nothing to keep resident.",
    "mimir has a first-class code-symbol graph (graph/outline/peek); THOR's is a derived sidecar (where_used/impact)",
    "Newer and far less battle-tested than mimir's daily use; mimir ships at a high cadence (v0.14: sync, GPU, fast cold)",
    "Semantic mode needs a ~235 MB model + a warm embed-daemon resident (off by default; degrades cleanly to bm25)",
    "One machine, private corpus, LLM-judged - only drift is repo-reproducible (examples/drift_eval.rs)",
]:
    bullet(b)
y += 8
rule()
y += 2
note(
    f'Bottom line <tspan fill="{CYAN}">coverage 77 vs 70.5</tspan> . <tspan fill="{CYAN}">both same-knowledge cuts</tspan> . <tspan fill="{CYAN}">drift 86 vs 74</tspan> . <tspan fill="{CYAN}">multi-project edge</tspan>',
    color=FG, size=13.5, bold=True,
)
y += 4
note(
    f'<tspan fill="{CYAN}">2.7x faster on like-for-like full recall (120 vs 322 ms, daemon up)</tspan> . <tspan fill="{AMBER}">loses code-structure (63.6 vs 74.2)</tspan>',
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
