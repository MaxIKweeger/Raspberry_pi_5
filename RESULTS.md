# RESULTS

Chaque constat : valeur, méthode, statut (mesuré / déduit / hypothèse), confiance, fichier source.
Source unique de la phase 0 : `results/2026-09-19/raw/selftest.jsonl` (+ `env.json`), cœur 1,
30 répétitions, 2460 répétitions au total, **0 invalide** (T 52,35–58,95 °C, fréquence 2 400 000 kHz
avant/après chaque répétition). Les médianes et IC95 % sont les lignes `kind":"summary"` du fichier.

## Phase 0 — machine

| Constat | Valeur | Statut | Confiance | Source |
|---|---|---|---|---|
| Carte / noyau | Raspberry Pi 5 Model B Rev 1.0, 6.18.39+rpt-rpi-2712, Debian 13 | mesuré | haute | env.json |
| Taille de page | 16384 o | mesuré | haute | env.json (`page_size`) ; confirmé par L1D_TLB_REFILL ci-dessous |
| MIDR_EL1 | 0x414fd0b1 (implementer 0x41, part 0xd0b) identique via `mrs` et sysfs | mesuré | haute | env.json |
| CNTFRQ_EL0 | 54 MHz | mesuré | haute | env.json |
| Caches (sysfs) | L1D/L1I 64K 4 voies 256 sets ; L2 512K 8 voies 1024 sets (par cœur) ; L3 2048K 16 voies 2048 sets (cœurs 0-3) ; ligne 64 o | lu dans sysfs = **hypothèse** jusqu'à la phase 2 | — | env.json (`caches`) |
| Hugepages / THP | absents (`/sys/kernel/mm/hugepages`, `transparent_hugepage`) | mesuré | haute | env.json |
| `perf_event_paranoid` / `perf_user_access` | 2 / 0 | mesuré | haute | env.json |
| PMU | device `armv8_cortex_a76`, type 9, cpus 0-3, 40 événements | mesuré | haute | env.json (`pmu`) |
| Fréquence pendant les runs | 2 400 000 kHz constant (governor `ondemand`), effective 2,3986 GHz | mesuré | haute | selftest.jsonl |

## Phase 0 — chaîne de mesure

| Constat | Valeur (médiane [IC95 %]) | Statut | Confiance |
|---|---|---|---|
| Coût `isb; mrs cntvct_el0` | 21,679 ns [21,678 ; 21,680] ≈ 52 cycles | mesuré | haute |
| Delta entre lectures CNTVCT consécutives | min 1 tick (18,5 ns), p99 2 ticks, max 49 ticks [47 ; 52] | mesuré | haute (max = interruptions, attribution non vérifiée) |
| Coût d'un `read()` du compteur de cycles PMU | 395,8 ns [395,4 ; 396,1] ; p50 407 ns, p99 426 ns, max ≈ 685 ns | mesuré | haute (quantification 18,5 ns sur p50/p99) |
| Fenêtre `RESET`+`ENABLE`+`DISABLE` | 1396,6 ns [1395,5 ; 1397,4] | mesuré | haute |
| INST_RETIRED / attendu, boucle 10 instr/iter | 1,000001 | mesuré | haute (confirmé par PMU, pas par le chrono) |
| INST_RETIRED / attendu, boucle 12 instr/iter | 1,000001 | mesuré | haute |
| cycles PMU / (fréquence sysfs × temps CNTVCT) | 0,99941 [0,99935 ; 0,99957] et 0,99953 [0,99946 ; 0,99956] | mesuré | haute (deux sources indépendantes concordent à 0,06 %) |
| Latence de `add` dépendant | 10,00006 cycles pour 10 `add` chaînés par itération ⇒ 1 cycle | déduit (débit de la chaîne série) | haute |
| Débit de `add` indépendants | 3,00005 cycles/itération pour 8 `add` + 1 `subs` + 1 `b.ne`, IPC 3,333 ⇒ ≥ 3 opérations ALU simples/cycle soutenues | déduit ; borne inférieure du débit ALU | moyenne (le goulot exact — ALU ou autre — n'est pas isolé) |
| Événements par groupe sans multiplexage | 7 (dont `cpu_cycles`) ; le 8ᵉ échoue ou multiplexe | mesuré | moyenne (sonde unique ; hypothèse : 6 compteurs programmables + compteur de cycles) |

## Phase 0 — observations PMU à investiguer (aucune conclusion à ce stade)

Flux de 64 Mio lu 2 fois, 1 `ldr` par ligne de 64 o = 2 097 152 lignes ; source : lignes
`event_validation/dram_stream_loads/*` de selftest.jsonl.

| Événement | Médiane | Rapport au nombre de lignes | Statut |
|---|---|---|---|
| L1D_CACHE / MEM_ACCESS | 2 097 197 / 2 097 203 | 1,00002 | mesuré, conforme à l'attente dérivée du code |
| L1D_CACHE_REFILL | 2 092 733 | 0,9979 | mesuré ; écart de −0,2 % non expliqué (prefetch ? fusion de requêtes ?) |
| L1D_TLB_REFILL | 8199 | 4096 pages × 2 passes = 8192 | mesuré ⇒ 1 refill par page de 16 Kio |
| DTLB_WALK / L2D_TLB_REFILL | 7832 / 7802 | ≈ 0,96 × 8192 | mesuré ; ≈ 4 % des refills L1 TLB sont servis sans walk |
| L2D_CACHE | 4 202 105 | 2,004 | mesuré ; 2 accès L2 par ligne (interprétation : hypothèse) |
| L2D_CACHE_REFILL | 1 345 406 | 0,64 | mesuré ; **hypothèse** : les remplissages déclenchés par le préchargeur ne sont pas comptés (phase 3) |
| L2D_CACHE_WB | 2 096 000 | 1,0 | mesuré alors que le flux ne fait que lire ; **hypothèse** : évictions propres de L2 comptées comme write-backs, L3 de type victim (phase 2) |
| L3D_CACHE_REFILL / LL_CACHE_MISS_RD | 2 058 296 / 2 058 404 | 0,98 | mesuré |
| BUS_ACCESS | 16 770 408 | 7,997 | mesuré ; 8 par ligne de 64 o (interprétation : hypothèse, largeur 8 o) |
| BR_MIS_PRED, charge L1 (2000 passes de 512 itérations) | 2003 | 1 par sortie de boucle + 3 | mesuré ; explication par le code (sortie de boucle) : déduit |
| INST_SPEC / INST_RETIRED, charge L1 | 3 172 176 / 3 086 057 | 1,028 | mesuré (≈ 2,8 % d'instructions spéculées non retirées) |

## Limites de la phase 0

- Une seule condition de fréquence (governor `ondemand`, en pratique fixé à 2,4 GHz pendant les
  runs) ; pas testé sous `performance` (non autorisé pour l'instant).
- Coût du chronomètre mesuré avec le chronomètre lui-même (CNTVCT) : biais de quantification connu.
- Les tolérances « dans la tolérance » sont des seuils d'ingénierie (5 %), pas des tests statistiques.
- Cœur 1 uniquement ; les autres cœurs n'ont pas été comparés.
