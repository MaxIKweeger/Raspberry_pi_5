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

---

# Phase 1 — hiérarchie mémoire

Sources : `results/2026-09-19/{mem_latency,tlb_latency,mem_bandwidth}.csv` (médianes poolées) et
`raw/{mem_lat,tlb_lat,mem_bw}.jsonl` (chaque répétition avec T, fréquence, validité). Cœur 1 (cœurs
1, 2, 3, 0 pour le multi-cœur), **governor `performance`** (`env_phase1.json`), 3 tours d'ordre alterné
(croissant / décroissant / croissant) × 10 répétitions = 30 par point, préchauffage, mémoire préfaultée.
**9900 répétitions au total (2220 + 2280 + 5400), 0 invalide** (fréquence 2,4 GHz avant/après chacune).
Graphiques : `docs/img/{latency,tlb,bandwidth}.png` (script `scripts/plot.py`).
`mlock` échoue pour le tampon de 1 Gio (limite `RLIMIT_MEMLOCK` = 8 Mio) ; les pages sont préfaultées par
`MAP_POPULATE`, l'absence de swap pendant le run n'a pas été surveillée explicitement.

## Latence par pointer chasing (cycle unique de Sattolo, nœuds de 64 o)

| Constat | Valeur | Statut | Confiance | Confirmé par |
|---|---|---|---|---|
| Latence L1D | 1,667 ns = **4,00 cycles** (IC95 [1,667 ; 1,669] ns), de 4 Kio à 64 Kio | mesuré | haute | chrono + PMU (cycles/load = 4,00 ; L1D refill ≈ 0) |
| Capacité L1D | dernier point sans miss : 64 Kio (refill 0,001/load) ; premier avec miss : 72,75 Kio (0,60/load) ⇒ capacité ∈ [64 ; 72,75) Kio, **compatible avec 64 Kio** (sysfs) | déduit | haute (résolution ×1,25) | L1D_CACHE_REFILL |
| Latence L2 | **11,6–11,9 cycles** (4,85–4,96 ns) entre 91 Kio et 347 Kio (L1 refill = 1,000/load, L2 refill ≈ 0) | mesuré | haute | PMU |
| Capacité L2 | transition **progressive** : L2 refill 0,046 à 434 Kio, 0,138 à 512 Kio, 0,61 à 678 Kio, 0,93 à 1 Mio. Compatible avec 512 Kio (sysfs) mais non résolu à mieux que « entre 0,4 et 1 Mio » ; la rampe douce est cohérente avec des conflits dus à l'indexation par adresse physique (hypothèse, phase 2) | déduit / hypothèse | moyenne | L2D_CACHE_REFILL |
| Latence L3 | **36,5–38,5 cycles** (15,2–16,1 ns) pour 1–1,3 Mio, avec 0,25–0,42 refill du TLB L1/load (≈ +5 cycles chacun, voir TLB) ⇒ latence L3 « pure » ≈ 35–37 cycles | mesuré (brut) / déduit (corrigé) | moyenne | PMU (L2 refill ≈ 0,94–0,99, L3 refill ≈ 0) |
| Capacité L3 | L3 refill : 0,032 à 1,62 Mio, 0,075 à 2 Mio, 0,55 à 4 Mio, 0,83 à 8 Mio, 0,96 à 16 Mio. Le modèle « 1 − C/W » donne C ≈ 1,9 Mio à 4 Mio. Compatible avec 2 Mio (sysfs) | déduit | moyenne | L3D_CACHE_REFILL / LL_CACHE_MISS_RD |
| Latence DRAM | **≈ 97–98 ns** (232–236 cycles) à 16–18,8 Mio (L3 refill 0,96–0,98, ≈ 0 page walk) ; **119,7 ns** (287 cycles) à 1 Gio | mesuré | haute (98 ns) / moyenne (attribution du surcoût) | PMU ; surcoût ≈ +22 ns ↔ DTLB_WALK 0,98/load (déduit) |
| Compteurs > 1 par load à 1 Gio | L2D_CACHE_REFILL 1,59 et L3D_CACHE_REFILL 1,06 par load | mesuré | — | **hypothèse** : les accès aux tables de pages lors des walks sont comptés en plus du load |
| Préchargement exploitable | aucun : L1 refill = 1,000/load exactement pour toute taille de 91 Kio à 1 Gio sur un cycle aléatoire | mesuré | haute | L1D_CACHE_REFILL |

Écarts avec la spec : aucun contradictoire. Les transitions sont plus douces que des marches idéales
(rampes L2 et L3), ce qui est un résultat en soi ; l'explication (adresses physiques aléatoires,
politique de remplacement) reste une hypothèse à tester en phase 2.

## TLB (une page de 16 Kio par nœud, ligne aléatoire dans la page ; témoin : même nombre de nœuds, empaquetés)

Pas de hugepages sur ce noyau : seules les pages de 16 Kio sont testées.

| Constat | Valeur | Statut | Confiance | Confirmé par |
|---|---|---|---|---|
| TLB L1 de données | 0 refill/load jusqu'à **48 pages**, 1,000 refill/load dès **52 pages** ⇒ capacité ∈ [48 ; 52), soit **48 entrées** (reach 768 Kio) ; marche très nette, compatible avec un TLB entièrement associatif à remplacement de type LRU (hypothèse) | déduit | haute (capacité) / faible (associativité) | L1D_TLB_REFILL + temps (3,752 ns dès 52 pages) |
| Coût d'un miss TLB L1 servi par le TLB L2 | **+2,085 ns = +5,0 cycles** (52–192 pages, caches identiques : 0 refill L1D) | mesuré | haute | temps ; DTLB_WALK = 0 |
| TLB L2 | walks ≈ 0,005/load à 1280 pages, 0,150 à 1344 pages ⇒ capacité ∈ [1280 ; 1344), soit **1280 entrées** (reach 20 Mio) ; puis progression graduelle 0,30 (1408), 0,51 (1536), 0,71 (2048), 0,93 (4096) : cohérente avec une structure set-associative (hypothèse) | déduit | haute (capacité) / faible (organisation) | DTLB_WALK / L2D_TLB_REFILL + temps |
| Coût d'un page walk | ≈ **7,6 ns (≈ 18 cycles)** à 4096 pages (tables de pages en cache) ; ≈ 9,9 ns à 6144 pages ; jusqu'à ≈ 30–40 ns de surcoût par load à 32–64 Ki pages (tables et lignes hors caches) | déduit (différence paged − packed corrigée du TLB L2) | moyenne | DTLB_WALK par load |

## Bande passante NEON (`ldp q`/`stp q`, trafic lu + écrit, GB/s décimaux, médiane de 30)

Chaque cœur travaille sur son propre tampon ; cœurs 1, 2, 3, 0. Détail : `mem_bandwidth.csv`.

| Niveau / mode | 1 cœur | 4 cœurs (agrégé) | Statut / remarque |
|---|---|---|---|
| Lecture, 16 Kio (L1) | **75,31 GB/s = 31,4 o/cycle** | 300,9 (×4,0) | mesuré, haute ; ≈ 2 × 16 o/cycle |
| Lecture, 32–64 Kio (L1) | 59,3 GB/s = 24,7 o/cycle | 233,6–238,2 | mesuré ; **écart non expliqué** avec 16 Kio (même niveau, 21 % de moins) |
| Lecture, 128–256 Kio (L2) | 44,2 GB/s = 18,4 o/cycle | 173–176 | mesuré |
| Lecture, 1 Mio | 34,9 | 16,1 | mesuré (mélange L2/L3) |
| Lecture, DRAM (128–256 Mio) | **13,83–13,84 GB/s** (5,78 o/cycle) | 12,4 (2 c. : 13,3 ; 3 c. : 12,8) | mesuré, haute ; **l'agrégé baisse avec le nombre de cœurs** |
| Écriture, 16 Kio – 1 Mio | **38,3 GB/s = 16,0 o/cycle, plat** | voir ci-dessous | mesuré ; pas de chute en sortant de L1 ni de L2 |
| Écriture, 2 / 4 Mio | 28,0 / 14,0 | — | mesuré |
| Écriture, DRAM | 9,0 GB/s | 9,3 (4 c.) ; 9,8 (2 c.) | mesuré |
| Copie, 16 Kio (L1) | 74,4–75,5 | 296 | mesuré |
| Copie, 256–512 Kio | 68,5 GB/s = 28,6 o/cycle | 39–57 | mesuré |
| Copie, DRAM | 8,83 | 7,20 | mesuré |

Recoupement PMU (1 cœur, lecture DRAM) : `BUS_ACCESS` = 128,0 par Kio = **8 par ligne de 64 o** ;
L1D_CACHE_REFILL = 14,85/Kio (93 % des 16 lignes/Kio). En écriture DRAM : BUS_ACCESS = 64/Kio, soit la
moitié de la lecture.

Points surprenants / non expliqués (aucun lissage) :
1. **Écriture plate à 16 o/cycle jusqu'à 1 Mio** avec quasi aucun refill L1D/L2D (≈ 0,002/Kio) alors que
   chaque ligne est écrite en entier. *Hypothèse* : les stores séquentiels de lignes complètes sont
   diffusés sans lecture préalable (pas de read-for-ownership) ; à tester en phases 2–3.
2. **Écriture multi-cœurs dans la plage L2** : 2 cœurs à 256 Kio/cœur donnent 30,6 GB/s agrégés (< 38,4 pour un
   seul cœur), 4 cœurs 24,9. Des tampons privés de 256 Kio tiennent pourtant dans un L2 privé de 512 Kio.
   *Hypothèse* : ces écritures transitent par un niveau partagé (L3/interconnexion).
3. **Lecture DRAM plus rapide avec 1 cœur qu'avec 4** (13,84 → 12,4 GB/s) : contention ou conflits de
   pages DRAM ; non isolé. Meilleur débit soutenu = 13,84 GB/s, soit 81 % des « ≈ 17 Go/s » de l'énoncé
   (valeur théorique non vérifiée ici).
4. Lecture 16 Kio vs 32 Kio (voir tableau) : aucune explication testée.
5. Sur flux séquentiel dans la plage L2 (128–256 Kio), L1D_CACHE_REFILL ne compte que ≈ 1,2/Kio au lieu de
   16 lignes/Kio ; en DRAM il compte 14,85/Kio. Renforce l'hypothèse de la phase 0 (remplissages
   déclenchés par le préchargeur non comptés) ; à tester en phase 3.

## Limites de la phase 1

- Tailles échantillonnées par pas ×1,25 : capacités connues à ±25 % près sauf pour les TLB (pas fins).
- Pas de hugepages : le reach TLB n'est établi que pour des pages de 16 Kio.
- L'ordre alterné est appliqué par tours de 10 répétitions ; les 3 tours utilisent des permutations
  différentes (graines distinctes), leurs écarts sont inclus dans l'IC.
- Multi-cœur : la garde de fréquence lit le cœur 1 ; les 4 cœurs partagent une seule politique cpufreq
  (`policy0`), donc la lecture est représentative. Le cœur 0 porte le bruit système (non isolé).
- Les cœurs 2, 3, 0 n'ont pas été comparés individuellement en mono-cœur.
- « GB/s » est décimal (10⁹ o/s) ; le trafic de copie compte lecture + écriture (convention STREAM).
