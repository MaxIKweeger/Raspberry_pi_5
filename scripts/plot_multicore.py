"""Plot the phase 6 experiments.
Usage: python scripts/plot_multicore.py <multicore_experiments.csv> <out_dir>
"""
import csv, sys, collections
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

csv_path, out = sys.argv[1], sys.argv[2]
BLUE, ORANGE, INK, MUTED, GRID = "#1b6ca8", "#d4661f", "#222222", "#666666", "#e3e3e0"
plt.rcParams.update({"font.size": 10, "axes.edgecolor": MUTED, "axes.labelcolor": INK, "xtick.color": MUTED,
                     "ytick.color": MUTED, "axes.spines.top": False, "axes.spines.right": False,
                     "axes.grid": True, "grid.color": GRID, "grid.linewidth": 0.6, "figure.facecolor": "white"})
rows = collections.defaultdict(list)
for r in csv.reader(open(csv_path, encoding="utf-8")):
    rows[r[0]].append(r[1:])

# 1. matrices -------------------------------------------------------------------------------------
def matrix(kind, key, sel):
    m = [[float("nan")] * 4 for _ in range(4)]
    for r in rows[kind]:
        if sel(r):
            m[int(r[key])][int(r[key + 1])] = float(r[key + 2])
    return m

fig, axes = plt.subplots(1, 3, figsize=(13, 4.1))
mats = [
    ("store-release / load-acquire (line 0)", matrix("pp", 1, lambda r: r[0] == "stlr_ldar")),
    ("store-release / load-acquire, mean of 4 different lines", None),
    ("atomic add (ldaddal) / load-acquire", matrix("pp", 1, lambda r: r[0] == "ldaddal_ldar")),
]
avg = [[0.0] * 4 for _ in range(4)]
for li in range(4):
    mm = matrix("ppl", 1, lambda r, li=li: int(r[0]) == li)
    for a in range(4):
        for b in range(4):
            avg[a][b] += mm[a][b] / 4
mats[1] = (mats[1][0], avg)
for ax, (title, m) in zip(axes, mats):
    im = ax.imshow(m, cmap="Blues", vmin=60, vmax=75)
    ax.grid(False)
    for a in range(4):
        for b in range(4):
            if a != b:
                ax.text(b, a, f"{m[a][b]:.1f}", ha="center", va="center", fontsize=10, color=INK if m[a][b] < 71 else "white")
    ax.set_xticks(range(4))
    ax.set_yticks(range(4))
    ax.set_xticklabels([f"core {i}" for i in range(4)])
    ax.set_yticklabels([f"core {i}" for i in range(4)])
    ax.set_xlabel("responder")
    ax.set_ylabel("initiator")
    ax.set_title(title, loc="left", color=INK, fontsize=9)
fig.suptitle("One-way cache-line transfer latency between cores (ns)", x=0.01, ha="left", color=INK, fontsize=11)
fig.tight_layout()
fig.savefig(f"{out}/multicore_matrix.png", dpi=150)
plt.close(fig)

# 2. forwarding ------------------------------------------------------------------------------------
sf = [(r[0], float(r[1])) for r in rows["sf"]]
fig, ax = plt.subplots(figsize=(10, 6.2))
ax.barh(range(len(sf)), [x[1] for x in sf], color=[ORANGE if x[1] > 9 else BLUE for x in sf], height=0.65)
ax.set_yticks(range(len(sf)))
ax.set_yticklabels([x[0] for x in sf], fontsize=8)
ax.invert_yaxis()
for i, x in enumerate(sf):
    ax.text(x[1] + 0.15, i, f"{x[1]:.2f}", va="center", fontsize=8, color=INK)
ax.set_xlabel("cycles per dependent store + load pair (the loaded value feeds the next store)")
ax.set_title("Store-to-load forwarding: blue = fast path (≈ 5.5 cycles), orange = slow path (> 9 cycles)", loc="left", color=INK, fontsize=10)
fig.tight_layout()
fig.savefig(f"{out}/multicore_forwarding.png", dpi=150)
plt.close(fig)

# 3. unaligned -------------------------------------------------------------------------------------
fig, axes = plt.subplots(1, 4, figsize=(14, 3.8), sharey=False)
for ax, (kind, size) in zip(axes, (("load", "8"), ("store", "8"), ("load", "16"), ("store", "16"))):
    d = [(r[3], float(r[4])) for r in rows["unal"] if r[0] == kind and r[1] == size and r[2] == "line"]
    ax.plot([int(x[0]) for x in d], [x[1] for x in d], color=BLUE, marker="o", markersize=3.5, linewidth=1.2)
    cross = [(r[2], float(r[4])) for r in rows["unal"] if r[0] == kind and r[1] == size and r[2] in ("4k", "page")]
    txt = "; ".join(f"{'4 KiB' if k == '4k' else '16 KiB page'} boundary: {v:.2f}" for k, v in cross)
    ax.text(0.02, 0.95, txt, transform=ax.transAxes, fontsize=7, va="top", color=ORANGE)
    ax.set_title(f"{kind} {size} B", loc="left", color=INK, fontsize=10)
    ax.set_xlabel("offset inside the 64-byte line")
    ax.set_ylabel("cycles per access")
fig.suptitle("Unaligned accesses (independent, L1-resident; every access class shares the same L1 bank, so read the shape, not the peak)", x=0.01, ha="left", color=INK, fontsize=10)
fig.tight_layout()
fig.savefig(f"{out}/multicore_unaligned.png", dpi=150)
plt.close(fig)
print("ok")
