"""Plot the phase 2 cache experiments.
Usage: python scripts/plot_cache.py <cache_experiments.csv> <raw1.jsonl[,raw2.jsonl]> <out_dir>
"""
import csv, json, sys, collections
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.ticker import FuncFormatter

csv_path, raw_path, out = sys.argv[1], sys.argv[2], sys.argv[3]
BLUE, ORANGE, INK, MUTED, GRID = "#1b6ca8", "#d4661f", "#222222", "#666666", "#e3e3e0"
plt.rcParams.update({"font.size": 10, "axes.edgecolor": MUTED, "axes.labelcolor": INK, "xtick.color": MUTED,
                     "ytick.color": MUTED, "axes.spines.top": False, "axes.spines.right": False,
                     "axes.grid": True, "grid.color": GRID, "grid.linewidth": 0.6, "figure.facecolor": "white"})
rows = collections.defaultdict(list)
for r in csv.reader(open(csv_path, encoding="utf-8")):
    rows[r[0]].append(r[1:])
LEVELS = ["L1D", "L2", "L3"]

# 1. associativity ---------------------------------------------------------------------------
fig, axes = plt.subplots(1, 3, figsize=(12, 3.8))
notes = {"L1D": "4 ways", "L2": "8 ways", "L3": "24 = 8 (L2) + 16 (L3)"}
for ax, lv in zip(axes, LEVELS):
    d = [r for r in rows["assoc"] if r[0] == lv]
    k = [int(r[1]) for r in d]
    rate = [float(r[2]) for r in d]
    lo = [float(r[3]) for r in d]
    hi = [float(r[4]) for r in d]
    ax.fill_between(k, lo, hi, color=BLUE, alpha=0.25, linewidth=0)
    ax.plot(k, rate, color=BLUE, linewidth=1.6, marker="o", markersize=3)
    onset = next((kk for kk, rr in zip(k, rate) if rr >= 0.05), None)
    if onset:
        ax.axvline(onset - 0.5, color=ORANGE, linestyle=":", linewidth=1.2)
        ax.text(onset - 0.9, 0.55, f"conflict capacity\n{notes[lv]}", color=ORANGE, ha="right", fontsize=8)
    ax.set_title(f"{lv}", loc="left", color=INK, fontsize=11)
    ax.set_xlabel("lines competing for one set (K)")
    ax.set_ylim(-0.03, 1.05)
axes[0].set_ylabel({"L1D": "refills per load"}.get("L1D"))
fig.suptitle("Associativity: refills per load vs lines in one set (median, 95 % CI)", x=0.01, ha="left", color=INK, fontsize=11)
fig.tight_layout()
fig.savefig(f"{out}/cache_assoc.png", dpi=150)
plt.close(fig)

# 2. index bits --------------------------------------------------------------------------------
fig, axes = plt.subplots(1, 3, figsize=(12, 3.6), sharey=True)
for ax, lv in zip(axes, LEVELS):
    d = [r for r in rows["flip"] if r[0] == lv]
    bits = [int(r[1]) for r in d]
    rate = [float(r[2]) for r in d]
    isidx = [r[4] == "1" for r in d]
    base = float(d[0][3])
    ax.bar([b for b, i in zip(bits, isidx) if not i], [r for r, i in zip(rate, isidx) if not i], color=BLUE, width=0.7)
    ax.scatter([b for b, i in zip(bits, isidx) if i], [r for r, i in zip(rate, isidx) if i], marker="D", s=40, color=ORANGE, zorder=3)
    ax.axhline(base, color=MUTED, linestyle=":", linewidth=1)
    ax.text(bits[-1], base + 0.03, "all bits equal", color=MUTED, ha="right", fontsize=8)
    idx = [b for b, i in zip(bits, isidx) if i]
    ax.set_title(f"{lv}: index bits {idx[0]}-{idx[-1]} = {2 ** len(idx)} sets", loc="left", color=INK, fontsize=10)
    ax.set_xlabel("physical address bit flipped in half of the lines")
axes[0].set_ylabel("refills per load")
fig.suptitle("Set-index bits: the overflow disappears (orange diamonds at 0) when the flipped bit selects the set", x=0.01, ha="left", color=INK, fontsize=11)
fig.tight_layout()
fig.savefig(f"{out}/cache_index_bits.png", dpi=150)
plt.close(fig)

# 3. replacement signature -------------------------------------------------------------------
per = collections.defaultdict(list)
def _lines():
    for rp in raw_path.split(","):
        yield from open(rp, encoding="utf-8")
for line in _lines():
    r = json.loads(line)
    if r.get("kind") == "rep" and r["exp"].startswith("repl/") and r["meta"]["valid"]:
        _, lv, pat = r["exp"].split("/", 2)
        key = "l1d_refill_per_access" if lv == "L1D" else "l2d_refill_per_access"
        per[(lv, pat)].append(r["data"][key])
fig, axes = plt.subplots(1, 2, figsize=(12, 4.2), sharey=True)
pats = [r[1] for r in rows["repl"] if r[0] == "L2" and not r[1].startswith("control")]
model_cols = {}
for h in rows["repl_header"]:
    model_cols[h[0]] = h[5:]
for ax, lv in zip(axes, ["L1D", "L2"]):
    hdr = model_cols[lv]
    x = list(range(len(pats)))
    for i, p in enumerate(pats):
        vals = per[(lv, p)]
        cnt = collections.Counter(round(v, 2) for v in vals)
        for v, c in cnt.items():
            ax.scatter([i], [v], s=18 + 10 * c, color=BLUE, alpha=0.8, zorder=3)
    for name, col, marker in (("LRU", "#777777", "_"), ("tree-PLRU", ORANGE, "x")):
        if name in hdr:
            j = hdr.index(name)
            ys = []
            for p in pats:
                row = next(r for r in rows["repl"] if r[0] == lv and r[1] == p)
                ys.append(float(row[5 + j]))
            ax.scatter(x, ys, marker=marker, s=90, color=col, zorder=4, linewidths=1.6)
    ax.set_xticks(x)
    ax.set_xticklabels(pats, rotation=35, ha="right", fontsize=8)
    ax.set_title(f"{lv}", loc="left", color=INK, fontsize=11)
axes[0].set_ylabel("misses per access at this level")
fig.suptitle("Replacement signature: measured repetitions (blue, size = repeats) from all runs vs models started from an empty set (LRU grey, tree-PLRU orange)", x=0.01, ha="left", color=INK, fontsize=10)
fig.tight_layout()
fig.savefig(f"{out}/cache_replacement.png", dpi=150)
plt.close(fig)
print("ok")
