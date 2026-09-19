"""Plot the phase 5 out-of-order experiments.
Usage: python scripts/plot_ooo.py <ooo_experiments.csv> <out_dir>
"""
import csv, sys, collections
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

csv_path, out = sys.argv[1], sys.argv[2]
BLUE, ORANGE, INK, MUTED, GRID = "#1b6ca8", "#d4661f", "#222222", "#666666", "#e3e3e0"
GREEN = "#2a9d8f"
plt.rcParams.update({"font.size": 10, "axes.edgecolor": MUTED, "axes.labelcolor": INK, "xtick.color": MUTED,
                     "ytick.color": MUTED, "axes.spines.top": False, "axes.spines.right": False,
                     "axes.grid": True, "grid.color": GRID, "grid.linewidth": 0.6, "figure.facecolor": "white"})
rows = collections.defaultdict(list)
for r in csv.reader(open(csv_path, encoding="utf-8")):
    rows[r[0]].append(r[1:])

# 1. windows + MLP ---------------------------------------------------------------------------------
names = [("nop", "nop (reorder buffer)"), ("int_add", "integer add (integer register file)"), ("vec_eor", "NEON eor (vector register file)"),
         ("load_l1", "load, L1 hit (load queue)"), ("store_l1", "store, L1 hit (store buffer)")]
fig, axes = plt.subplots(2, 3, figsize=(13, 7))
for ax, (k, title) in zip(axes.flat, names):
    d = sorted([(int(r[1]), float(r[2]), float(r[5])) for r in rows["window"] if r[0] == k])
    d = [x for x in d if x[0] <= 150]
    ax.plot([x[0] for x in d], [x[1] for x in d], color=BLUE, marker="o", markersize=2.5, linewidth=1.3, label="one DRAM miss per iteration")
    ax.plot([x[0] for x in d], [x[2] for x in d], color=MUTED, linestyle="--", linewidth=1.1, label="same loop, load hits L1")
    js = [(int(r[1]), int(r[2]), float(r[3])) for r in rows["window_jump"] if r[0] == k]
    if js:
        lo, hi, _ = max(js, key=lambda j: j[2])
        ax.axvspan(lo, hi, color=ORANGE, alpha=0.35)
        ax.text(hi + 2, 8, f"largest jump at N = {lo}-{hi}", color=ORANGE, fontsize=8)
    ax.set_title(title, loc="left", color=INK, fontsize=10)
    ax.set_xlabel("filler instructions per iteration (N)")
    ax.set_ylabel("cycles per iteration")
axes[0][0].legend(frameon=False, fontsize=7, loc="upper left")
ax = axes[1][2]
for level, col in (("L2-resident", GREEN), ("DRAM", BLUE)):
    d = sorted([(int(r[1]), float(r[2])) for r in rows["mlp"] if r[0] == level])
    ax.plot([x[0] for x in d], [x[1] for x in d], color=col, marker="o", markersize=3, label=level)
ax.set_yscale("log")
ax.set_xlabel("independent dependent-load chains (K)")
ax.set_ylabel("cycles per load")
ax.set_title("Memory-level parallelism", loc="left", color=INK, fontsize=10)
ax.legend(frameon=False, fontsize=8)
fig.suptitle("Window sizes: T(N) jumps where an integer number of iterations stops fitting (nop: ROB = 128)", x=0.01, ha="left", color=INK, fontsize=11)
fig.tight_layout()
fig.savefig(f"{out}/ooo_window.png", dpi=150)
plt.close(fig)

# 2. instructions ---------------------------------------------------------------------------------
instr = [(r[0], ",".join(r[1:-2]), float(r[-2])) for r in rows["instr"]]  # the kind column may contain a comma
lat = [(n, c) for n, k, c in instr if k.startswith("latency")]
tp = [(n, c) for n, k, c in instr if k.startswith("throughput")]
fig, axes = plt.subplots(1, 2, figsize=(12, 6))
for ax, data, title, col in ((axes[0], lat, "Latency (dependent chain), cycles", BLUE), (axes[1], tp, "Reciprocal throughput (independent), cycles per instruction", ORANGE)):
    ax.barh(range(len(data)), [x[1] for x in data], color=col, height=0.65)
    ax.set_yticks(range(len(data)))
    ax.set_yticklabels([x[0] for x in data], fontsize=8)
    ax.invert_yaxis()
    ax.set_xscale("log")
    ax.set_title(title, loc="left", color=INK, fontsize=10)
    for i, x in enumerate(data):
        ax.text(x[1] * 1.05, i, f"{x[1]:.3g}", va="center", fontsize=8, color=INK)
fig.tight_layout()
fig.savefig(f"{out}/ooo_instructions.png", dpi=150)
plt.close(fig)
print("ok")
