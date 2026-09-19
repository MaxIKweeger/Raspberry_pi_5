"""Plot the phase 4 branch-predictor experiments.
Usage: python scripts/plot_branch.py <branch_experiments.csv> <out_dir>
"""
import csv, sys, collections
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

csv_path, out = sys.argv[1], sys.argv[2]
BLUE, ORANGE, INK, MUTED, GRID = "#1b6ca8", "#d4661f", "#222222", "#666666", "#e3e3e0"
GREEN, PURPLE = "#2a9d8f", "#8a6bbe"
plt.rcParams.update({"font.size": 10, "axes.edgecolor": MUTED, "axes.labelcolor": INK, "xtick.color": MUTED,
                     "ytick.color": MUTED, "axes.spines.top": False, "axes.spines.right": False,
                     "axes.grid": True, "grid.color": GRID, "grid.linewidth": 0.6, "figure.facecolor": "white"})
rows = collections.defaultdict(list)
for r in csv.reader(open(csv_path, encoding="utf-8")):
    rows[r[0]].append(r[1:])

# 1. BTB ------------------------------------------------------------------------------------------
fig, ax = plt.subplots(figsize=(9, 4.6))
cols = {4: "#9aa5b1", 8: PURPLE, 16: GREEN, 32: BLUE, 64: ORANGE, 256: "#a13d3d"}
for s in (4, 8, 16, 32, 64, 256):
    d = sorted([(int(r[1]), float(r[2]), float(r[4])) for r in rows["btb"] if int(r[0]) == s])
    ax.plot([x[0] for x in d], [x[1] for x in d], color=cols[s], marker="o", markersize=3, linewidth=1.4, label=f"spacing {s} B")
ax.set_xscale("log", base=2)
ax.set_yscale("log")
ax.set_xlabel("branches in the ring (N)")
ax.set_ylabel("cycles per taken direct branch")
ax.set_title("BTB: cost of a taken branch vs number of branches (random ring, 1 branch per slot)", loc="left", color=INK, fontsize=11)
ax.legend(frameon=False, fontsize=8, ncol=2)
fig.tight_layout()
fig.savefig(f"{out}/branch_btb.png", dpi=150)
plt.close(fig)

# 2. conditional / indirect predictors ---------------------------------------------------------
fig, axes = plt.subplots(2, 3, figsize=(13, 7))
ax = axes[0][0]
d = sorted([(int(r[0]), float(r[1])) for r in rows["pattern"]])
ax.plot([x[0] for x in d], [x[1] for x in d], color=BLUE, marker="o", markersize=3)
ax.set_xscale("log", base=2)
ax.set_xlabel("pattern period P (random, repeated)")
ax.set_ylabel("mispredictions per branch")
ax.set_title("Learning a random pattern", loc="left", color=INK, fontsize=10)

ax = axes[0][1]
for v, col in (("taken", BLUE), ("nottaken", ORANGE), ("control", MUTED)):
    d = sorted([(int(r[1]), float(r[3])) for r in rows["corr"] if r[0] == v])
    ax.plot([x[0] for x in d if x[0] <= 4096], [x[1] for x in d if x[0] <= 4096], color=col, marker="o", markersize=3,
            linestyle=":" if v == "control" else "-", label={"taken": "correlated, taken fillers", "nottaken": "correlated, not-taken fillers", "control": "control (independent)"}[v])
ax.set_xscale("log", base=2)
ax.set_ylim(-0.05, 0.65)
ax.set_xlabel("distance K to the correlated branch (branches)")
ax.set_ylabel("final-branch misprediction rate")
ax.set_title("History reach", loc="left", color=INK, fontsize=10)
ax.legend(frameon=False, fontsize=7, loc="center left")

ax = axes[0][2]
for sp, col in ((4, MUTED), (16, BLUE)):
    d = sorted([(int(r[1]), float(r[2])) for r in rows["nbr"] if int(r[0]) == sp])
    ax.plot([x[0] for x in d], [x[1] for x in d], color=col, marker="o", markersize=3, label=f"branches {sp} B apart")
ax.set_xscale("log", base=2)
ax.set_xlabel("static conditional branches (N)")
ax.set_ylabel("mispredictions per branch")
ax.set_title("Branch count", loc="left", color=INK, fontsize=10)
ax.legend(frameon=False, fontsize=8)

ax = axes[1][0]
d = sorted([(int(r[0]), float(r[1])) for r in rows["ind_rr"]])
ax.plot([x[0] for x in d], [x[1] for x in d], color=GREEN, marker="o", markersize=3)
ax.set_xscale("log", base=2)
ax.set_xlabel("indirect targets T (visited round robin)")
ax.set_ylabel("mispredictions per indirect branch")
ax.set_title("Indirect: number of targets", loc="left", color=INK, fontsize=10)

ax = axes[1][1]
d = sorted([(int(r[0]), float(r[1])) for r in rows["ind_rand"]])
ax.plot([x[0] for x in d], [x[1] for x in d], color=PURPLE, marker="o", markersize=3)
ax.set_xscale("log", base=2)
ax.set_xlabel("period P of a random sequence over 16 targets")
ax.set_ylabel("mispredictions per indirect branch")
ax.set_title("Indirect: sequence length", loc="left", color=INK, fontsize=10)

ax = axes[1][2]
for m, col in ((0, BLUE), (4, ORANGE), (8, PURPLE)):
    d = sorted([(float(r[2]), float(r[3]), int(r[1])) for r in rows["penalty"] if int(r[0]) == m])
    ax.plot([x[0] for x in d], [x[1] for x in d], color=col, marker="o", markersize=4, label=f"{m} dependent multiplies")
ax.set_xlabel("mispredictions per iteration")
ax.set_ylabel("cycles per iteration")
ax.set_title("Misprediction penalty (slope)", loc="left", color=INK, fontsize=10)
ax.legend(frameon=False, fontsize=8)
fig.tight_layout()
fig.savefig(f"{out}/branch_predictors.png", dpi=150)
plt.close(fig)

# 3. return stack ------------------------------------------------------------------------------
fig, axes = plt.subplots(1, 2, figsize=(11, 3.9))
d = sorted([(int(r[1]), float(r[5])) for r in rows["ras"] if r[0] == "random_sites"])
axes[0].plot([x[0] for x in d if x[0] <= 40], [x[1] for x in d if x[0] <= 40], color=BLUE, marker="o", markersize=3)
axes[0].axvline(16.5, color=ORANGE, linestyle=":", linewidth=1.2)
axes[0].text(17, 1, "16 entries", color=ORANGE, fontsize=9)
axes[0].set_xlabel("call depth D")
axes[0].set_ylabel("extra mispredictions per iteration")
axes[0].set_title("Random call sites: returns beyond the stack", loc="left", color=INK, fontsize=10)
d = sorted([(int(r[1]), float(r[3])) for r in rows["ras"] if r[0] == "chain"])
axes[1].plot([x[0] for x in d if x[0] <= 40], [x[1] for x in d if x[0] <= 40], color=GREEN, marker="o", markersize=3)
axes[1].set_xlabel("call depth D")
axes[1].set_ylabel("cycles per iteration (D calls + D returns)")
axes[1].set_title("Fixed call chain: cost per level", loc="left", color=INK, fontsize=10)
fig.tight_layout()
fig.savefig(f"{out}/branch_ras.png", dpi=150)
plt.close(fig)
print("ok")
