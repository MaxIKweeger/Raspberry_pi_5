"""Plot the phase 3 prefetcher experiments.
Usage: python scripts/plot_prefetch.py <prefetch_experiments.csv> <out_dir>
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

# 1. stride sweep -------------------------------------------------------------------------------
regions = ["L2-resident", "L3-resident", "DRAM"]
fig, axes = plt.subplots(1, 3, figsize=(12.5, 3.9), sharey=True)
for ax, rg in zip(axes, regions):
    d = [r for r in rows["stride"] if r[0] == rg and r[1] != "random"]
    pos = sorted([(int(r[1]), float(r[4])) for r in d if int(r[1]) > 0])
    neg = sorted([(-int(r[1]), float(r[4])) for r in d if int(r[1]) < 0])
    ax.plot([p[0] for p in pos], [p[1] for p in pos], color=BLUE, marker="o", markersize=3.5, linewidth=1.4, label="ascending (+k lines)")
    ax.plot([p[0] for p in neg], [p[1] for p in neg], color=ORANGE, marker="s", markersize=3.5, linewidth=1.2, linestyle="--", label="descending (-k lines)")
    ax.axhline(1.0, color=MUTED, linestyle=":", linewidth=1)
    ax.set_xscale("log", base=2)
    ax.set_title(rg, loc="left", color=INK, fontsize=11)
    ax.set_xlabel("stride |k| in 64-byte lines")
    ax.set_ylim(0, 1.15)
axes[0].set_ylabel("latency / random-order latency")
axes[0].legend(frameon=False, fontsize=8, loc="lower right")
fig.suptitle("Stride detection: dependent-load latency relative to a random cycle over the same set (1.0 = no prefetching)", x=0.01, ha="left", color=INK, fontsize=11)
fig.tight_layout()
fig.savefig(f"{out}/prefetch_stride.png", dpi=150)
plt.close(fig)

# 2. interleaved streams -----------------------------------------------------------------------
d = sorted([(int(r[0]), float(r[1]), float(r[3])) for r in rows["streams"]])
fig, axes = plt.subplots(1, 2, figsize=(11, 3.8))
axes[0].plot([x[0] for x in d], [x[1] for x in d], color=BLUE, marker="o", markersize=4, linewidth=1.5)
axes[0].set_yscale("log")
axes[0].set_xlabel("interleaved sequential streams (S)")
axes[0].set_ylabel("ns per access (dependent loads, DRAM)")
axes[0].set_title("Latency", loc="left", color=INK, fontsize=11)
axes[1].plot([x[0] for x in d], [x[2] for x in d], color=ORANGE, marker="o", markersize=4, linewidth=1.5)
axes[1].set_xlabel("interleaved sequential streams (S)")
axes[1].set_ylabel("bus accesses per demanded line (8 = no over-fetch)")
axes[1].set_title("Bus traffic", loc="left", color=INK, fontsize=11)
fig.suptitle("How many streams keep being followed", x=0.01, ha="left", color=INK, fontsize=11)
fig.tight_layout()
fig.savefig(f"{out}/prefetch_streams.png", dpi=150)
plt.close(fig)

# 3. overshoot ------------------------------------------------------------------------------------
fig, ax = plt.subplots(figsize=(8.5, 4.4))
series = collections.defaultdict(list)
for r in rows["cold"]:
    kind, stride, n, extra = r[0], int(r[1]), int(r[2]), float(r[4])
    series[(kind, stride)].append((n, extra))
palette = {("load", 1): BLUE, ("load", -1): "#5b9bd0", ("load", 2): "#2a9d8f", ("load", 4): "#8a6bbe", ("load", 8): "#b08d57",
           ("load", 16): MUTED, ("load", 3): "#a0a0a0", ("load", -2): "#7fb3a9", ("store", 1): ORANGE}
for key, pts in sorted(series.items()):
    pts.sort()
    if key not in palette:
        continue
    ax.plot([p[0] for p in pts], [p[1] for p in pts], color=palette[key], marker="o", markersize=3, linewidth=1.3,
            linestyle="--" if key[0] == "store" else "-", label=f"{key[0]} stride {key[1]:+d}")
ax.set_xscale("log", base=2)
ax.set_xlabel("stream length n (cold lines demanded)")
ax.set_ylabel("extra lines read on the bus beyond the demanded ones")
ax.set_title("Prefetch run-ahead versus stream length", loc="left", color=INK, fontsize=11)
ax.legend(frameon=False, fontsize=8, ncol=2)
fig.tight_layout()
fig.savefig(f"{out}/prefetch_overshoot.png", dpi=150)
plt.close(fig)

# 4. boundaries -----------------------------------------------------------------------------------
want = ["pool/B=4096", "cma/B=4096", "cma/B=16384", "pool/B=16384/noncontig", "cma/B=65536/phase=0K", "pool/B=2097152/noncontig"]
lab = {"pool/B=4096": "4 KiB\n(inside a 16 KiB page)", "cma/B=4096": "4 KiB\n(contiguous)", "cma/B=16384": "16 KiB\n(contiguous)",
       "pool/B=16384/noncontig": "16 KiB\n(not contiguous)", "cma/B=65536/phase=0K": "64 KiB\n(contiguous)", "pool/B=2097152/noncontig": "2 MiB\n(not contiguous)"}
data = collections.defaultdict(dict)
for r in rows["boundary"]:
    label, direction, kind, extra = r[0], r[1], r[2], float(r[4])
    if direction == "asc":
        data[label][kind] = extra
fig, ax = plt.subplots(figsize=(10, 4.2))
w = 0.26
kinds = [("ends_at_boundary", "stream ends exactly at the boundary", ORANGE), ("crosses_boundary", "stream crosses the boundary", BLUE),
         ("mid_block", "stream ends mid-block (control)", "#9aa5b1")]
labels = [k for k in want if k in data]
for i, (k, name, col) in enumerate(kinds):
    ax.bar([j + (i - 1) * w for j in range(len(labels))], [data[l].get(k, 0) for l in labels], width=w, color=col, label=name)
ax.set_xticks(range(len(labels)))
ax.set_xticklabels([lab[l] for l in labels], fontsize=8)
ax.set_ylabel("extra lines read beyond the 15 demanded")
ax.set_title("Ascending 16-line cold streams around block boundaries", loc="left", color=INK, fontsize=11)
ax.set_ylim(0, 33)
ax.legend(frameon=False, fontsize=8, loc="upper left", ncol=1)
fig.tight_layout()
fig.savefig(f"{out}/prefetch_boundary.png", dpi=150)
plt.close(fig)

# 5. stores ---------------------------------------------------------------------------------------
d = [(r[0], float(r[2])) for r in rows["store"]]
fig, ax = plt.subplots(figsize=(7.5, 3.8))
xs = [x[0] for x in d]
ax.bar(range(len(d)), [x[1] for x in d], color=[ORANGE if x[0] == "random" else BLUE for x in d], width=0.7)
ax.axhline(1.0, color=MUTED, linestyle=":", linewidth=1)
ax.set_xticks(range(len(d)))
ax.set_xticklabels(xs)
ax.set_xlabel("store stride in lines (order of the addresses)")
ax.set_ylabel("ns per store / random")
ax.set_title("Independent stores into DRAM: strided versus random order", loc="left", color=INK, fontsize=11)
fig.tight_layout()
fig.savefig(f"{out}/prefetch_stores.png", dpi=150)
plt.close(fig)
print("ok")
