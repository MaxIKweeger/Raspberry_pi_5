"""Plot the phase 1 CSV curves (matplotlib). Usage: python scripts/plot.py results/<date> docs/img"""
import csv, sys
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.ticker import FuncFormatter, NullFormatter, FixedLocator

src, out = sys.argv[1], sys.argv[2]
BLUE, ORANGE, INK, MUTED, GRID = "#1b6ca8", "#d4661f", "#222222", "#666666", "#e3e3e0"
plt.rcParams.update({"font.size": 10, "axes.edgecolor": MUTED, "axes.labelcolor": INK, "xtick.color": MUTED,
                     "ytick.color": MUTED, "axes.spines.top": False, "axes.spines.right": False,
                     "axes.grid": True, "grid.color": GRID, "grid.linewidth": 0.6, "figure.facecolor": "white"})

def human(v, _=None):
    for unit, d in (("GiB", 2**30), ("MiB", 2**20), ("KiB", 2**10)):
        if v >= d:
            return f"{v / d:g} {unit}"
    return f"{v:g} B"

def rows(name):
    return list(csv.DictReader(open(f"{src}/{name}")))

# 1. latency
r = rows("mem_latency.csv")
x = [int(a["size_bytes"]) for a in r]
y = [float(a["ns_median"]) for a in r]
lo = [float(a["ns_ci95_lo"]) for a in r]
hi = [float(a["ns_ci95_hi"]) for a in r]
fig, ax = plt.subplots(figsize=(9, 4.6))
ax.fill_between(x, lo, hi, color=BLUE, alpha=0.25, linewidth=0)
ax.plot(x, y, color=BLUE, linewidth=1.6)
ax.set_xscale("log", base=2); ax.set_yscale("log")
ax.xaxis.set_major_formatter(FuncFormatter(human))
ax.yaxis.set_major_formatter(FuncFormatter(lambda v, _: f"{v:g}"))
for s, lab in ((64 << 10, "L1 64 KiB"), (512 << 10, "L2 512 KiB"), (2 << 20, "L3 2 MiB")):
    ax.axvline(s, color=MUTED, linestyle=":", linewidth=1)
    ax.text(s * 1.04, 1.25, lab + "\n(sysfs)", color=MUTED, fontsize=8, va="bottom")
ax.set_xlabel("working set (random single-cycle pointer chase, 64-byte nodes)")
ax.set_ylabel("load-to-use latency (ns)")
ax.set_title("Load latency vs working set, core 1 at 2.4 GHz (median, 95 % bootstrap CI)", loc="left", color=INK, fontsize=11)
fig.tight_layout(); fig.savefig(f"{out}/latency.png", dpi=150); plt.close(fig)

# 2. TLB
r = rows("tlb_latency.csv")
p = [int(a["pages"]) for a in r]
fig, ax = plt.subplots(figsize=(9, 4.6))
ax.plot(p, [float(a["ns_paged"]) for a in r], color=ORANGE, linewidth=1.6, marker="o", markersize=3)
ax.plot(p, [float(a["ns_packed"]) for a in r], color=BLUE, linewidth=1.6, marker="o", markersize=3)
ax.set_xscale("log", base=2); ax.set_yscale("log")
ax.xaxis.set_major_formatter(FuncFormatter(lambda v, _: f"{v:g}"))
ax.yaxis.set_major_formatter(FuncFormatter(lambda v, _: f"{v:g}"))
ax.text(p[-1], float(r[-1]["ns_paged"]) * 1.08, "one node per 16 KiB page", color=ORANGE, ha="right", fontsize=9)
ax.text(p[-1], float(r[-1]["ns_packed"]) * 0.7, "same node count, packed", color=BLUE, ha="right", fontsize=9)
for pg, lab, yf in ((48, "L1 dTLB: 48 pages hit, 52 miss", 0.9), (1280, "L2 TLB: 1280 pages hit, 1344 miss", 0.78)):
    ax.axvline(pg, color=MUTED, linestyle=":", linewidth=1)
    ax.text(pg * 0.95, yf, lab, color=MUTED, fontsize=8, ha="right", transform=ax.get_xaxis_transform())
ax.set_xlabel("number of distinct 16 KiB pages touched")
ax.set_ylabel("load-to-use latency (ns)")
ax.set_title("TLB reach: paged vs packed pointer chase (median)", loc="left", color=INK, fontsize=11)
fig.tight_layout(); fig.savefig(f"{out}/tlb.png", dpi=150); plt.close(fig)

# 3. bandwidth (small multiples, one panel per op; thread count = ordered magnitude -> one hue, light to dark)
r = rows("mem_bandwidth.csv")
shades = {1: "#9ecae1", 2: "#6baed6", 3: "#3182bd", 4: "#08519c"}
fig, axes = plt.subplots(1, 3, figsize=(12, 4.2), sharex=True)
for ax, op in zip(axes, ("read", "write", "copy")):
    for t in (1, 2, 3, 4):
        d = [a for a in r if a["op"] == op and int(a["threads"]) == t]
        d.sort(key=lambda a: int(a["size_per_thread_bytes"]))
        ax.plot([int(a["size_per_thread_bytes"]) for a in d], [float(a["gbps_median"]) for a in d],
                color=shades[t], linewidth=1.6, label=f"{t} core" + ("s" if t > 1 else ""))
    ax.set_xscale("log", base=2); ax.set_yscale("log")
    ax.xaxis.set_major_formatter(FuncFormatter(human))
    ax.yaxis.set_major_locator(FixedLocator([8, 10, 20, 50, 100, 200, 300]))
    ax.yaxis.set_major_formatter(FuncFormatter(lambda v, _: f"{v:g}"))
    ax.yaxis.set_minor_formatter(NullFormatter())
    ax.tick_params(axis="x", labelrotation=45, labelsize=8)
    ax.set_title(op, loc="left", color=INK, fontsize=11)
    ax.set_xlabel("working set per core")
axes[0].set_ylabel("aggregate traffic (GB/s, read + write)")
axes[0].legend(frameon=False, fontsize=8, loc="lower left")
fig.suptitle("NEON streaming bandwidth (ldp/stp q), median of 30 reps", x=0.01, ha="left", color=INK, fontsize=11)
fig.tight_layout(); fig.savefig(f"{out}/bandwidth.png", dpi=150); plt.close(fig)
print("ok")
