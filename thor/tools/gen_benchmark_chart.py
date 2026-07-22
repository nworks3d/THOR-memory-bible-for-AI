# Generates assets/benchmark.svg from the measured aggregates (BENCHMARKS.md).
# The chart is data: regenerate it here after every fresh benchmark run instead
# of hand-editing bar widths. Values below are the 2026-07-22 round: THOR
# v0.9.6 vs mimir v0.15.0, pre-registered, every test re-measured fresh,
# 3-judge median (three sonnet lenses), blind, with mimir's own indexer re-run
# over the same repositories first and its deliberate arms re-judged in
# full-body form per the documented amendment.
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
out.append(f'<text x="20" y="{y}" fill="{MUT}" font-size="12">Same machine, blind-judged, pre-registered - re-measured fresh 2026-07-22: THOR v0.9.6 vs mimir v0.15.0.</text>')
y += 30

# ---- test 1 ----
header("TEST 1 - COVERAGE  (200 questions: 152 shared-knowledge + 48 stratified, seeded)", "deliberate recall over the live store . higher is better . NO gap in this table is statistically significant")
for label, t, m in [
    ("Doc reference", 91.5, 80.5),
    ("Decision", 95.0, 88.8),
    ("Config how-to", 100.0, 93.8),
    ("Code behavior", 94.4, 91.9),
    ("Gotcha", 89.5, 84.2),
    ("Code structure", 72.6, 72.6),
]:
    d = round(t - m)
    delta = f"+{d}%" if d > 0 else (f"{d}%" if d < 0 else "tie")
    pair(label, t, m, delta, delta_color=SOFT if d == 0 else None)
rule()
note(f'Test 1 overall  <tspan fill="{CYAN}">THOR 89.2%</tspan> vs <tspan fill="{MUT}">mimir 84.6%</tspan>  (n=200) - a TIE with a THOR edge: 28 W / 17 L, p = 0.14', color=FG, size=13, bold=True)
note("No gap in this table is significant. Code-structure - last round's one significant mimir win (57.6 vs 74.2, p 0.013) -")
note("is closed to an EXACT TIE: 72.6 vs 72.6, 8 wins to 8, now that deliberate recall serves structure cards (v0.9.6).")
note("Battery and jury changed across rounds (200 = 152 shared + 48 stratified, seeded; 3 sonnet lenses this round), so")
note("absolutes are not comparable across rounds - read gaps within the round. The 2026-07-14 lead claim stays retracted.")
y += 14

# ---- test 2 ----
header("TEST 2 - SAME KNOWLEDGE  (facts both systems verifiably hold)", "cuts over the judged Test 1 medians . the equal-corpus comparison")
pair("Strict dual-written (n=53)", 97.2, 95.8, "+1%")
pair("Broad shared (n=151)", 89.1, 89.2, "tie", delta_color=SOFT)
note("Both cuts are ties. Strict differs on only 5 of 53 questions - below the 6-discordant floor, so no verdict; broad is", color=SOFT)
note("12 W / 13 L, p = 1.00. The 2026-07-14 claim that THOR wins both cuts stays retracted.", color=SOFT)
y += 14

# ---- test 3 ----
header("TEST 3 - MULTI-PROJECT  (three private project repos)", "45 questions written from each repo itself, scoped per project, top-5 full chunks, blind-judged")
pair("Project 1", 93.3, 100.0, "-7%")
pair("Project 2", 86.7, 90.0, "-3%")
pair("Project 3", 93.3, 93.3, "tie", delta_color=SOFT)
rule()
note(f'Test 3 overall  <tspan fill="{CYAN}">THOR 91.1%</tspan> vs <tspan fill="{MUT}">mimir 94.4%</tspan>  (n=45) - mimir ahead, 2 W / 4 L, p = 0.69, not significant', color=FG, size=13, bold=True)
note("One question moves a 15-question project by 6.7 points, so read the split as noisy. The 2026-07-14 claim of a THOR")
note("lead here (96.7 vs 95.6) stays retracted.")
y += 14

# ---- drift ----
header("SESSION DRIFT  (59 fresh-session scenarios, five arms, cleaned corpus)", "post-compaction, empty context: does memory surface the fact that stops the drift? silence scores zero")
pair("Session vs mimir best", 79.7, 64.4, "+15%", thor_tag="session", mimir_tag="mimir full")
pair("Courier vs mimir best", 75.4, 64.4, "+11%", thor_tag="courier", mimir_tag="mimir full")
pair("Vs mimir's auto hook", 79.7, 7.6, "+72%", thor_tag="session", mimir_tag="mimir hook")
note("Five arms judged blind together; silence scores zero. Session = courier + guard advisories - the as-deployed channel", color=SOFT)
note("since the action guard landed. Courier alone 75.4, deliberate 70.3, mimir's hook 7.6 (misses 50 of 59, 0 full catches).", color=SOFT)
note("Sign tests: session beats mimir's best 11 W / 0 L (p 0.001), courier beats it 10 W / 2 L (p 0.039); deliberate vs best", color=SOFT)
note("is a tie (9 W / 5 L, p 0.42). This REVERSES last round, where mimir's best beat the courier (p 0.043) - the serving-", color=SOFT)
note("form work sits in between; the corpus was cleaned (73 -> 59) and the jury changed, so it is two honest rounds, not", color=SOFT)
note("one number moving. 17 of 59 scenarios carry no expected call - there session equals courier by construction.", color=SOFT)
note("Separately reproducible in-repo, no judge needed: cargo run --example drift_eval (catches AND false fires).", color=SOFT)
y += 14

# ---- speed ----
header("SPEED  (per-prompt cost, lower is better)", "as-deployed, correct invocation each side (THOR stdin hook, mimir prompt arg) . median of 20, warm-up discarded")
pair("Cost per prompt", 146.4, 323.4, "2.2x faster",
     delta_color=CYAN, unit=" ms", scale=330 / 323.4, mimir_tag="mimir full")
note("THOR's courier answers EVERY prompt - 0 of 20 empty - in 146 ms (p90 167). mimir's hook is faster at 40 ms but", color=SOFT)
note("returns nothing on 6 of 20: it is fast because it often serves nothing. The mimir channel that competes on drift", color=SOFT)
note("costs 323 ms and is not a hook. Honestly worse since last round: the courier read 125 ms at 16.1k events and reads", color=SOFT)
note("146 ms at 19.8k - real growth; the per-query fold behind it is materialized since 2026-07-22 (cold paths ~4x).", color=SOFT)
y += 14

# ---- downsides ----
header("HONEST DOWNSIDES", "where THOR does not win, and the caveats")
y += 12
for b in [
    "No THOR quality lead is significant: the coverage edge reads p 0.14, and multi-project has mimir ahead (91.1 vs 94.4).",
    "Code-structure closed to a TIE, not a win - battery and jury changed across rounds, so it is a within-round reading.",
    "The drift reversal carries cross-round caveats: corpus cleaned 73 -> 59, a new jury, and the serving-form work between.",
    "This round's FIRST PASS judged mimir's one-line summaries; its arms were regenerated full-body and re-judged before",
    "publication - the same error class as the run discarded last round, the other direction. Pass 1 is on file, unpublished.",
    "A 7000-char judging cap bound BOTH sides: THOR on 47/59 deliberate and 24/59 session items, mimir full-body on most.",
    "Do not compare absolutes across rounds: Test 1 reads 89.2 here vs 73.2 last round - the jury and battery moved, not",
    "the products. Read gaps within a round.",
    "THOR is compute-bound: 125 ms at 16.1k -> 146 ms at 19.8k warm. The fold is materialized since 2026-07-22 (guard 499 -> 211 ms, ~4x cold); the warm figure stands until re-measured.",
    "Newer and far less battle-tested than mimir's daily use; mimir ships at a high cadence (v0.15: inference delegation).",
    "One machine, private corpus, LLM-judged - only drift is repo-reproducible (examples/drift_eval.rs).",
]:
    bullet(b)
y += 8
rule()
y += 2
note(
    f'Bottom line <tspan fill="{MUT}">quality is a tie with a non-significant THOR edge; multi-project leans mimir</tspan>',
    color=FG, size=13.5, bold=True,
)
y += 4
note(
    f'<tspan fill="{CYAN}">THOR\'s as-deployed channel wins drift 11-0 (p 0.001): every prompt, 146 ms, never silent</tspan>',
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
# Parse what we are about to write. note() and bullet() pass their text through
# unescaped so a line can carry <tspan> colouring, which means one stray "<" in
# prose - "p <0.0001" - silently produces a file GitHub refuses to render. The
# generator happily wrote it and every other check passed.
import xml.etree.ElementTree as _ET
try:
    _ET.fromstring(svg)
except _ET.ParseError as exc:
    raise SystemExit(f"generated SVG is not well-formed XML: {exc}\n"
                     "usually a raw '<' or '&' in a note()/bullet() string") from exc

path = os.path.join(os.path.dirname(__file__), "..", "..", "assets", "benchmark.svg")
with open(path, "w", encoding="utf-8", newline="\n") as f:
    f.write(svg)
print(f"wrote {os.path.abspath(path)} (height {H})")
