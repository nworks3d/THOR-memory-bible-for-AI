# Generates assets/benchmark.svg from the measured aggregates (BENCHMARKS.md).
# The chart is data: regenerate it here after every fresh benchmark run instead
# of hand-editing bar widths. Values below are the 2026-07-21 round: production
# THOR vs mimir v0.15.0, every test re-measured fresh, 3-judge median, blind,
# with mimir's own indexer re-run over the same repositories first.
import os

CYAN = "#22d3ee"
GREY = "#484f58"
AMBER = "#e3b341"
FG = "#e6edf3"
MUT = "#8b949e"
SOFT = "#c9d1d9"
PX_PER_PCT = 3.3
# Bars start far enough right that a row label has real room. At 196 the labels
# ran straight through the THOR/mimir tags - the old width guard only checked the
# right-hand canvas edge and was blind to a collision on the left.
BAR_X = 268
TAG_X = 256          # right edge of the THOR / mimir tag
LABEL_X = 20
LABEL_FS = 13
CHAR_W = 0.58        # rough advance width per character, in em

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

def pair(label, thor, mimir, delta, delta_color=None, unit="%", scale=PX_PER_PCT,
         thor_tag="THOR", mimir_tag="mimir"):
    global y
    # A row label sits vertically between the two bars and horizontally to the
    # left of both tags, so it has to clear the WIDER of the two. Checked here,
    # per row, because it is the one collision the canvas-edge guard cannot see.
    label_w = len(label) * CHAR_W * LABEL_FS
    tag_w = max(len(thor_tag), len(mimir_tag)) * CHAR_W * 10.5
    assert LABEL_X + label_w + 8 <= TAG_X - tag_w, (
        f'row label "{label}" ({label_w:.0f}px) runs into the "{thor_tag}"/"{mimir_tag}" '
        f"tag - shorten it or move BAR_X right"
    )
    tw = thor * scale
    mw = mimir * scale
    out.append(f'<rect x="{BAR_X}" y="{y}" width="{tw:.1f}" height="14" rx="7" fill="{CYAN}"/>')
    out.append(f'<text x="{BAR_X + tw + 7:.2f}" y="{y + 11.5}" fill="{FG}" font-size="11.5" font-weight="700">{round(thor)}{unit}</text>')
    out.append(f'<text x="{TAG_X}" y="{y + 11.5}" fill="{CYAN}" font-size="10.5" font-weight="700" text-anchor="end">{esc(thor_tag)}</text>')
    label_y = y + 22
    out.append(f'<text x="{LABEL_X}" y="{label_y}" fill="{FG}" font-size="{LABEL_FS}" font-weight="700">{esc(label)}</text>')
    color = delta_color or (CYAN if not delta.startswith("-") else AMBER)
    out.append(f'<text x="838" y="{label_y + 1}" fill="{color}" font-size="15" font-weight="800" text-anchor="end">{esc(delta)}</text>')
    y += 20
    out.append(f'<rect x="{BAR_X}" y="{y}" width="{mw:.1f}" height="14" rx="7" fill="{GREY}"/>')
    out.append(f'<text x="{BAR_X + mw + 7:.2f}" y="{y + 11.5}" fill="{MUT}" font-size="11.5">{round(mimir)}{unit}</text>')
    out.append(f'<text x="{TAG_X}" y="{y + 11.5}" fill="{MUT}" font-size="10.5" text-anchor="end">{esc(mimir_tag)}</text>')
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
out.append(f'<text x="20" y="{y}" fill="{MUT}" font-size="12">Same machine, blind-judged, re-measured fresh 2026-07-21 vs mimir v0.15.0. Sign test on per-question wins.</text>')
y += 30

# ---- test 1 ----
header("TEST 1 - COVERAGE  (200 questions: shared-knowledge + category-stratified)", "deliberate recall over the live store . higher is better . only ONE gap here is statistically real")
for label, t, m in [
    ("Doc reference", 82.5, 68.8),
    ("Decision", 75.9, 68.5),
    ("Config how-to", 82.4, 82.4),
    ("Code behavior", 70.8, 70.8),
    ("Gotcha", 76.1, 78.3),
    ("Code structure", 57.6, 74.2),
]:
    d = round(t - m)
    delta = f"+{d}%" if d > 0 else (f"{d}%" if d < 0 else "tie")
    pair(label, t, m, delta, delta_color=SOFT if d == 0 else None)
rule()
note(f'Test 1 overall  <tspan fill="{CYAN}">THOR 73.2%</tspan> vs <tspan fill="{MUT}">mimir 72.5%</tspan>  (n=200) - a TIE: 33 wins to 34, p = 1.00', color=FG, size=13, bold=True)
note("133 of 200 questions score identically. Only code-structure survives a significance test (p 0.013) and it is mimir's:")
note("it takes 12 of the 14 questions the two disagree on. The +14 on doc-reference and +7 on decision do NOT reach it")
note("(p 0.17 and p 0.45) - read those bars as noise. The previous round published 77.0 vs 70.5 as a lead; retracted.")
y += 14

# ---- test 2 ----
header("TEST 2 - SAME KNOWLEDGE  (facts both systems verifiably hold)", "cuts over the judged Test 1 medians . the equal-corpus comparison")
pair("Strict dual-written (n=53)", 94.3, 96.2, "-2%")
pair("Broad shared (n=152)", 81.2, 83.9, "-3%")
note("Both are ties with mimir nominally ahead (p 0.73 and p 0.39). The previous round published 97.2 vs 94.3 and 88.8 vs", color=SOFT)
note("86.2 and claimed THOR won both cuts - retracted. Pure ranking over an equal corpus is not a THOR advantage.", color=SOFT)
y += 14

# ---- test 3 ----
header("TEST 3 - MULTI-PROJECT  (three private project repos)", "45 questions written from each repo itself, scoped per project, top-5 full chunks, blind-judged")
pair("Project 1", 93.3, 93.3, "tie", delta_color=SOFT)
pair("Project 2", 86.7, 93.3, "-7%")
pair("Project 3", 83.3, 96.7, "-13%")
rule()
note(f'Test 3 overall  <tspan fill="{CYAN}">THOR 87.8%</tspan> vs <tspan fill="{MUT}">mimir 94.4%</tspan>  (n=45) - mimir ahead, p = 0.11, not significant', color=FG, size=13, bold=True)
note("One question moves a 15-question project by 6.7 points, so read the split as noisy. The previous round published")
note("96.7 vs 95.6 in THOR's favour - also retracted.")
y += 14

# ---- drift ----
header("SESSION DRIFT  (73 fresh-session scenarios)", "post-compaction, empty context: does memory surface the fact that stops the drift? silence scores zero")
pair("Vs mimir at its best", 67.1, 74.0, "-7%", mimir_tag="mimir full")
pair("Vs mimir's auto hook", 67.1, 37.0, "+30%", mimir_tag="mimir hook")
note("Both gaps ARE significant (p 0.043 against the best channel, p <0.0001 against the hook) and they point opposite ways.", color=SOFT)
note("The comparison splits in two and both halves are real. On CAPABILITY mimir wins: its full recall beats THOR's courier", color=SOFT)
note("(21 wins to 9) and ties THOR's deliberate channel at 72.6%. On WHAT RUNS UNASKED THOR wins 37 to 6: mimir's hook", color=SOFT)
note("misses 46 of 73 scenarios and returns nothing at all on 2. mimir's winning channel is not a hook - you must call it.", color=SOFT)
note("Separately reproducible in-repo, no judge needed: cargo run --example drift_eval (catches AND false fires).", color=SOFT)
y += 14

# ---- speed ----
header("SPEED  (per-prompt cost, lower is better)", "as-deployed, correct invocation each side (THOR stdin hook, mimir prompt arg) . median of 20, warm-up discarded")
pair("Cost per prompt", 125, 334, "2.7x faster",
     delta_color=CYAN, unit=" ms", scale=330 / 334, mimir_tag="mimir full")
note("THOR's courier answers EVERY prompt in 125 ms - 0 of 20 empty. mimir's hook is three times faster at 41 ms but", color=SOFT)
note("returns nothing on 6 of 20: it is fast because it often serves nothing. The mimir channel that WINS the drift test", color=SOFT)
note("costs 334 ms and is not a hook. That trade - always-on and adequate, versus on-demand and better - is the whole", color=SOFT)
note("argument, and it is a judgement about how you work rather than a number this benchmark can settle.", color=SOFT)
y += 14

# ---- downsides ----
header("HONEST DOWNSIDES", "where THOR does not win, and the caveats")
y += 12
for b in [
    "THOR leads NO quality metric this round. Coverage, both same-knowledge cuts, multi-project and drift-capability are",
    "all ties or mimir wins. The one defensible THOR advantage is operational: it answers every prompt and never goes silent.",
    "Code-structure is a SIGNIFICANT loss (57.6 vs 74.2, p 0.013) and it widened since the last round. The symbol-graph",
    "explanation we published for it did not survive scrutiny and has not been re-tested, so the cause is still unknown.",
    "An earlier version of THIS round measured drift against mimir's hook only and reported THOR winning 3x. That gave",
    "mimir its weakest channel; the run was discarded before publication and is recorded here rather than quietly dropped.",
    "Do not compare absolutes across rounds: THOR's courier reads 67.1% here against 86.3% last round, while an A/B",
    "against the previous release proved its drift output is byte-identical. The code did not change; the jury did.",
    "THOR is compute-bound; cold latency grows with store size. The resident cache removes that only while a daemon is up.",
    "Newer and far less battle-tested than mimir's daily use; mimir ships at a high cadence (v0.15: inference delegation).",
    "One machine, private corpus, LLM-judged - only drift is repo-reproducible (examples/drift_eval.rs).",
]:
    bullet(b)
y += 8
rule()
y += 2
note(
    f'Bottom line <tspan fill="{MUT}">quality is a tie, and mimir wins code-structure and drift-capability outright</tspan>',
    color=FG, size=13.5, bold=True,
)
y += 4
note(
    f'<tspan fill="{CYAN}">THOR\'s claim is what runs unasked: every prompt, 125 ms, never silent</tspan> . <tspan fill="{AMBER}">mimir does better when asked</tspan>',
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
