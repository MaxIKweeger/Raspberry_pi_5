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

---

# Phase 2 — géométrie et politique de remplacement des caches

Sources : `results/2026-09-19/cache_experiments_run{1,2}.csv`, `raw/cache_run{1,2}.jsonl` (chaque répétition),
`analysis_replacement_run{1,2}.txt` (score des politiques par `a76probe analyze-cache`), `phase2_run{1,2}.log`
(sortie de l'acquisition), `env_phase2.json`. **Deux runs complets indépendants** (run 1 puis run 2, 4800 répétitions
chacun, 30 par point en 3 tours d'ordre alterné, cœur 1, governor `performance`), **0 répétition invalide**.
Figures : `docs/img/cache_{assoc,index_bits,replacement}.png` (`scripts/plot_cache.py`).

Méthode d'accès aux adresses physiques (décision déléguée) : `/proc/self/pagemap` lu **sous `sudo`** (lecture seule de
notre propre mapping ; aucun réglage système modifié). Réserve de 2 Gio (131 072 pages de 16 Kio), `mlock` réussi (root),
PFN relus après le run : **0 page déplacée**. Les 131 072 pages tombent dans les plages `System RAM` de `/proc/iomem`
(confirme que PFN × 16 Kio est la bonne unité). Étendue physique 0x1e50000–0x1ff9fc000. Groupes de lignes : bits
physiques 14 à 24 tenus constants (classes de 66–67 pages) ; les bits > 24 ne sont pas contrôlés.

## Taille de ligne

| Constat | Valeur | Statut | Confiance | Source |
|---|---|---|---|---|
| Ligne L1D | refills/load = 0,1250 / 0,2500 / 0,5000 / 1,0000 / 1,0000 pour des pas de 8 / 16 / 32 / 64 / 128 o = pas/64 exactement ⇒ **64 o** | mesuré | haute | L1D_CACHE_REFILL |
| Ligne L2 | rapport au plateau (pas 128) : 0,128 / 0,252 / 0,499 / 0,999 (attendu 0,125 / 0,25 / 0,5 / 1) ⇒ **64 o** | mesuré | haute | L2D_CACHE_REFILL (plateau brut 0,92 : ≈ 8 % des lignes restent dans le L2) |
| Ligne L3 | non mesurée ; 64 o d'après sysfs et la cohérence des tailles ci-dessous | hypothèse | moyenne | — |

## Associativité, sets, bits d'index (identiques dans les deux runs)

| Niveau | Capacité de conflit (dernière valeur de K sans miss) | Forme de la montée | Bits d'index (inversion d'un bit) | Sets | Taille = voies × sets × 64 o |
|---|---|---|---|---|---|
| L1D | **4** (0 miss à K=4, 1,000 à K=5) | marche nette | 6–13 | 256 | 4 × 256 × 64 = **64 Kio** ✓ sysfs |
| L2 | **8** (0 à K=8, puis 0,25 / 0,50 / 0,75 / 1,00 à K=9…12) | rampe par paliers de 25 % | 6–15 | 1024 | 8 × 1024 × 64 = **512 Kio** ✓ sysfs |
| L3 | **24** (0,0000 jusqu'à K=24, 0,12 à K=25, ≈ 0,5 vers K=35, bruité) | rampe lente | 6–16 | 2048 | voies L3 = 24 − 8 = **16** ⇒ 16 × 2048 × 64 = **2 Mio** ✓ sysfs |

- Les bits d'index sont **contigus** dans les trois niveaux. Inverser n'importe quel bit de 17 à 24 ne soulage pas le
  L3 (taux ≈ taux de base 0,53 ; à K = 36 le taux de base a valu 0,78 dans une mesure et 0,53 dans une autre : le taux
  de miss du L3 en débordement varie d'une mesure à l'autre) ⇒ **aucun hachage impliquant les bits 17–24 n'est observé**
  (déduit ; bits > 24 non testés). Le bit 16 du L3 laisse un résidu de 0,03 (contre 0,000 pour les autres bits d'index).
- **L3 de type victime (exclusif du L2) — déduit, confiance moyenne à haute** : la capacité de conflit vaut 24 = 8 (L2) + 16
  (L3) : un L3 inclusif du L2 donnerait 16. Recoupements : la phase 1 donnait une capacité effective L3 ≈ 1,9 Mio (2 Mio =
  16 voies × 2048 sets), et la phase 0 avait vu L2D_CACHE_WB ≈ 1 par ligne lue (évictions propres du L2 comptées comme
  écritures vers le L3).
- Sensibilité à la définition de la capacité : « premier K avec ≥ 5 % de refills − 1 » (adoptée) donne 4 / 8 / 24 ; « premier
  K avec ≥ 50 % − 1 » donnerait 4 / 9 / 28–34 (la rampe non-LRU la gonfle) ; c'est la raison du choix.

## Politique de remplacement (W+1 à W+3 lignes dans un set, 8 motifs, comparés à 8 modèles)

Modèles : LRU, tree-PLRU, FIFO, random, SRRIP (2 bits, HP), BRRIP, NRU, SRRIP-FP. Chaque modèle est simulé depuis de
nombreux états initiaux aléatoires (voies invalides, bits PLRU, RRPV, ordre) ; une répétition est notée par sa distance à
l'état permanent atteignable le plus proche ; score = distance moyenne (plus bas = mieux). Motifs : cyclique W+1 et W+2,
dents de scie, aléatoire W+1 et W+3, une ligne chaude + cycle, W−1 lignes chaudes + 2 froides, chaque ligne deux fois.

| Niveau | Run 1 | Run 2 | Conclusion | Confiance |
|---|---|---|---|---|
| L1D (W=4) | tree-PLRU 0,0021 ; LRU 0,0171 ; NRU 0,046 | tree-PLRU 0,0041 ; LRU 0,0155 ; NRU 0,045 | **compatible avec tree-PLRU**, LRU strict écarté (« ligne chaude + cycle » : 0,38–0,40 mesuré, 0,375 PLRU, 0,50 LRU) ; écart résiduel ≈ 0,02 sur ce motif (variante de PLRU non modélisée) | moyenne (faible selon la règle de marge au run 2) |
| L2 (W=8) | tree-PLRU 0,0126 ; LRU 0,0257 ; NRU 0,054 | **NRU 0,0156** ; SRRIP-FP 0,0231 ; tree-PLRU 0,0372 ; LRU 0,0387 | **famille pseudo-LRU, non identifiée** : le meilleur modèle change d'un run à l'autre | faible |
| L3 | non caractérisée : le flux vu par le L3 est filtré par le L2 ; la montée lente après K=24 indique une politique non-LRU | | — | — |

Ce que les données du L2 établissent : (a) les motifs cyclique W+1/W+2, dents de scie et aléatoires sont identiques
aux modèles LRU/PLRU/NRU (indiscernables) et **excluent** random, SRRIP, BRRIP ; (b) les motifs avec réutilisation
prennent **plusieurs états permanents discrets qui changent d'une répétition et d'un run à l'autre** :
« ligne chaude + cycle » ∈ {0,31 ; 0,38 ; 0,43–0,44 ; 0,50}, « W−1 chaudes + 2 froides » ∈ {0,23 ; 0,29 ; 0,56}
(0,56 pour 24 répétitions sur 30 au run 1, 0,23 pour 22 sur 30 au run 2). Un tree-PLRU démarré depuis un set partiellement
invalide reproduit exactement les états 0,315 / 0,375 / 0,44 / 0,50 de « ligne chaude + cycle » (simulation), ce qui
soutient l'hypothèse « pseudo-LRU dont le régime dépend de la disposition initiale des voies » ; l'état 0,23 n'est
reproduit par aucun des 8 modèles (NRU donne 0,19).

## Inclusion

| Constat | Valeur | Statut | Confiance |
|---|---|---|---|
| L2 ↔ L3 | capacité de conflit 24 = 8 + 16 ⇒ L3 exclusif du L2 (type victime) | déduit | moyenne à haute |
| L1 ↔ L2 (back-invalidation) | ligne X gardée chaude en L1 pendant que son set L2 est saturé par 8 autres lignes : L2 refills = **0,197 (run 1) / 0,208 (run 2) par accès** contre 0,0001 sans X. Un L2 tree-PLRU **sans** back-invalidation prédit 0,0 ; **avec** back-invalidation, 1,0 (simulation). La mesure n'est ni l'un ni l'autre | **ambigu** | faible |

Hypothèse non testée pour la valeur intermédiaire : remplacement du L2 tenant compte de la présence de la ligne dans
le L1 (le L2 évite d'évincer les lignes présentes en L1). Le test ne permet pas de conclure sur l'inclusion L1 ⊂ L2.

## Tableau de synthèse {niveau, taille, voies, sets, ligne, politique candidate, confiance}

| Niveau | Taille | Voies | Sets | Ligne | Politique candidate | Confiance |
|---|---|---|---|---|---|---|
| L1D | 64 Kio (mesuré : 4 × 256 × 64) | 4 | 256 (bits 6–13) | 64 o | tree-PLRU | moyenne |
| L2 | 512 Kio (mesuré : 8 × 1024 × 64) | 8 | 1024 (bits 6–15) | 64 o | pseudo-LRU, variante non identifiée | faible |
| L3 | 2 Mio (déduit : 16 voies × 2048 × 64) | 16 (capacité de conflit 24 avec le L2) | 2048 (bits 6–16) | 64 o (non mesuré) | non caractérisée ; L3 victime | moyenne (géométrie) |

## Limites de la phase 2

- Deux runs seulement ; le comportement du L2 dépend de l'état, donc un troisième run pourrait donner un autre « meilleur modèle ».
- Huit modèles seulement ; les variantes de PLRU (arbre, bit-PLRU, etc.) et les politiques adaptatives (DRRIP avec set dueling) ne sont pas toutes représentées.
- Bits physiques > 24 non contrôlés (un hachage sur ces bits ne serait pas détecté) ; le L3 est testé avec 44 lignes par set au plus.
- La capacité de conflit L3 de 24 mélange L2 et L3 ; la décomposition 8 + 16 s'appuie sur la géométrie sysfs et la phase 1.
- Le test d'inclusion L1/L2 est ambigu ; il n'existe pas ici de mesure de l'inclusion du L3.
- Les logs d'acquisition (`phase2_run{1,2}.log`) affichent un score de politique historique (RMS des médianes, puis
  couverture) ; le score de référence est celui de `analysis_replacement_run{1,2}.txt`.
- Exécution sous `sudo` (pagemap) : les fichiers de résultats créés par root ont été rendus à l'utilisateur (`chown`) ;
  aucun autre réglage système n'a été modifié en dehors du governor `performance` (restauré à `ondemand`).

---

# Phase 3 — préchargeurs

Sources : `results/2026-09-19/prefetch_experiments.csv` (médianes poolées), `raw/prefetch.jsonl` (chaque répétition),
`phase3_run.log`, `env_phase3.json`. Un run complet (30 répétitions par point en 3 tours d'ordre alterné), cœur 1, governor
`performance` (restauré à `ondemand`), **9660 répétitions, 0 invalide**. Exécuté avec `sudo` (lecture de `pagemap` pour tenter de
classer les frontières par contiguïté physique ; ce n'est pas nécessaire aux autres mesures). Figures : `docs/img/prefetch_*.png`
(`scripts/plot_prefetch.py`).

Aucun événement PMU spécifique au préchargeur n'est exposé : tout est déduit (a) du temps de chargements dépendants comparé
à un ordre aléatoire sur le même jeu, (b) des lignes réellement lues sur le bus (`BUS_ACCESS` / 8 par ligne, `LL_CACHE_MISS_RD`).

## Détection de stride (chaîne de chargements dépendants, lignes de 64 o, ordre du parcours = stride constant)

Rapport = latence / latence d'un cycle aléatoire sur le même jeu (aléatoire : 4,83 ns L2 ; 15,13 ns L3 ; 110,2 ns DRAM).

| Jeu (niveau qui sert les données) | ±1 ligne | strides 2 à 21 | strides ≥ 22 | Statut |
|---|---|---|---|---|
| L2 (256 Kio) | **0,39** = 1,9 ns ≈ 4,5 cycles (latence L1) | ≈ 1,0 ; dips non monotones à +3 (0,75), +5 (0,87), +10 (0,76) | 0,96–1,05 | mesuré |
| L3 (1 Mio) | **0,14** (≈ 2,1 ns) | 0,29–0,52 (les deux sens) | 0,95–1,10 | mesuré |
| DRAM (64 Mio) | **0,04** = 4,6 ns ≈ 11 cycles (latence L2) | 0,14–0,28 ; **+16 : 0,09**, −16 : 0,12 | 0,92–0,96 (1,02 dès 512 lignes : page walks) | mesuré |

- **Stride maximal suivi : 21 lignes (1344 o) vers le haut ; 22 lignes (1408 o) n'est plus suivi** (rupture nette 0,26 → 0,92 en DRAM,
  0,29 → 1,00 en L3), confiance haute ; dans le sens descendant −16 est suivi et −64 ne l'est pas (−17 à −63 non testés).
- **Deux mécanismes séparés par la taille du jeu** : avec des données dans le L2, seul ±1 est accéléré (jusqu'à la latence L1) ⇒
  préchargeur L1 de type ligne suivante/précédente ; les jeux L3 et DRAM voient un préchargeur à stride (jusqu'à 21 lignes) qui
  ramène le coût vers 0,2–0,5 de l'aléatoire (déduit ; confiance moyenne : les deux préchargeurs ne sont pas isolés par compteur).
- Dips non monotones à +3/+5/+10 en L2 et pic favorable à ±16 en DRAM : **observés, non expliqués**.

## Nombre de flux entrelacés suivis (S flux +1 ligne, 8 Mio chacun, accès en tourniquet, DRAM)

| S | 1 | 4 | 8 | 14 | 16 | 17 | 18 | 20 | 24 | 32 |
|---|---|---|---|---|---|---|---|---|---|---|
| ns/accès | 3,7 | 7,3 | 6,4 | 7,8 | 9,2 | **29,7** | 86,6 | 72,1 | 81,9 | 85,7 |
| bus par ligne demandée (8 = pas de sur-lecture) | 7,9 | 10,3 | 12,8 | 14,8 | **23,8** | 8,7 | 8,5 | 9,2 | 8,7 | 8,7 |

⇒ **jusqu'à 16 flux sont suivis**, dès 17 le préchargement s'effondre (latence proche de l'aléatoire, 110 ns) : **capacité ≈ 16 flux**
(confiance haute ; S = 18–20 est bruité). Le préchargeur lit de plus en plus de lignes par ligne demandée quand S augmente (jusqu'à 23,8/8 ≈ 3× à S = 16).

## Distance et degré (flux à froid de n lignes ; lignes lues sur le bus au-delà des n demandées)

Méthode : n accès dépendants à froid (adresses neuves), compteur `BUS_ACCESS` / 8 moins la ligne de base (n = 1) moins n − 1.

| Flux | Déclenchement | Avance à l'état permanent | Statut |
|---|---|---|---|
| +1 / −1 ligne | au **4ᵉ accès** (extra 1,6 à n = 3 → 10,6 à n = 4) | **≈ 40 lignes** (2,5 Kio) pour n ≥ 48 ; montée 10,6 (n=4), 17,6 (8), 24,9 (24), 35,7 (32), 40,5 (48) ; creux reproductible à n = 16 (13,5) | mesuré ; interprétation « avance » : déduit |
| strides 2, 3, 4, 8 | au 4ᵉ–6ᵉ accès | **≈ 15–16 lignes** (indépendant du stride) | mesuré |
| stride 16 | après 17–24 accès (0 jusqu’à n = 16 sauf un point isolé à n = 6 : 6,1 ; 14,1 à n = 24) | ≈ 16–17 lignes | mesuré |
| stores +1 | au 4ᵉ accès (9,4) | 11–13 (n = 6–12) puis ≈ 8 (n = 32–48) ; **≈ 0 à n = 64, 128, 192, 256 mais 8,3 à n = 96** | mesuré ; irrégularité non expliquée |

- Le degré de préchargement (nombre de lignes lues d'avance) est donc **≈ 40 lignes pour un flux séquentiel** et **≈ 15–16 pour les flux à stride** ;
  plus le stride est grand, plus l'apprentissage demande d'accès.
- Hypothèse pour l'irrégularité des stores : bascule du chemin d'écriture (voir phase 1, écritures plates sans lecture préalable) ; non testée.

## Frontières (flux ascendants de 16 lignes à froid ; « extra » = lignes lues au-delà des 15 demandées)

| Frontière | Fin exacte sur la frontière | Traverse la frontière | Fin au milieu du bloc (témoin) | Lecture |
|---|---|---|---|---|
| 4 Kio, à l'intérieur d'une page de 16 Kio | 17,2 | 24,6 | 14,9 | **pas d'arrêt à 4 Kio** (fin ≥ témoin) |
| 16 Kio, mémoire physiquement contiguë (CMA) | **13,7** | 24,0 | 23,4 | **arrêt à la frontière de page de 16 Kio** : ≈ 10 lignes de moins |
| 16 Kio, pages non contiguës (pool) | **14,9** | 24,9 | 23,6 | idem : la contiguïté physique ne change rien |
| 64 Kio (4 phases d'alignement physique, CMA) | 13,6–14,0 | 23,8–24,1 | 24,3–24,7 | comme 16 Kio (chaque frontière de 64 Kio est aussi une frontière de 16 Kio) |
| 2 Mio (pool) | 12,8 | 24,7 | 24,6 | idem ; alignement physique 2 Mio non testable |

⇒ **le préchargeur ne franchit pas une frontière de page de 16 Kio (granule de traduction)**, même quand la mémoire physique est contiguë
(déduit ; confiance moyenne), **mais continue si la demande traverse la frontière** (24–25 lignes extra, comme au milieu d'un bloc). Aucune limite
supplémentaire à 64 Kio ni 2 Mio n'est visible (les flux qui traversent ces frontières se comportent comme ceux qui traversent 16 Kio).
Le sens descendant donne les mêmes valeurs (13,3–14,4 à 16 Kio, 16,4–16,6 à 4 Kio). Résidus **non expliqués** : un flux qui finit exactement sur
la frontière lit encore ≈ 13–15 lignes de trop, et le témoin « milieu de bloc » vaut 14,9 pour les blocs de 4 Kio contre 23,5 pour les blocs ≥ 16 Kio
(hypothèse : dépend de la position dans la page de 16 Kio).

## Stores contre loads (stores indépendants, adresses lues dans une table, jeu DRAM)

| Stride (lignes) | +1 | −1 | 2 | 4 | 8 | 16 | 32 | 64 | aléatoire |
|---|---|---|---|---|---|---|---|---|---|
| ns par store | 15,6 | 15,4 | 16,4 | 17,9 | 19,7 | 21,6 | 22,5 | 23,3 | 24,1 |
| rapport à l'aléatoire | 0,65 | 0,64 | 0,68 | 0,75 | 0,82 | 0,90 | 0,94 | 0,97 | 1,00 |

Les stores en flux ±1 gagnent ≈ 35 %, le gain **décroît progressivement avec le stride** (contrairement aux loads, plats de 2 à 21 puis
nuls) et ne montre pas de coupure à 21 ; pas de sur-lecture (≈ 9,1 accès bus par store, dont 1 pour la table). Les stores déclenchent bien un
préchargement (flux à froid : 9,4 lignes d'avance dès le 4ᵉ store), moins profond que celui des loads (≈ 8 contre ≈ 40).
Limite : les loads de cette mesure sont dépendants (latence) et les stores indépendants (débit) : la comparaison directe des ratios n'est qu'indicative.

## Limites de la phase 3

- Aucune preuve directe du niveau où arrive la ligne préchargée (L1, L2 ou L3) : déduit des latences (4,5 cycles ≈ L1, 11 cycles ≈ L2).
- Les deux préchargeurs (L1 et L2/L3) ne sont pas isolés par des compteurs dédiés ; « deux mécanismes » est une déduction par taille de jeu.
- Strides négatifs testés seulement à −1, −2, −4, −8, −16, −64 ; le seuil descendant exact reste inconnu.
- Jeux L2/L3 : strides ≥ N/16 exclus (repliement) ; le stride 4096 lignes sur ces jeux n'est pas interprétable.
- Frontières : la mémoire CMA (32 Mio) est contiguë par construction mais son adresse physique n'est pas lisible (pagemap ne renvoie rien
  pour ce mapping) ; la phase d'alignement physique 64 Kio est balayée sur 4 valeurs ; 2 Mio physique non testé ; pas d'accès à une
  paire de pages virtuellement adjacentes et physiquement contiguës dans le pool ordinaire (0 candidat).
- Le dépassement mesuré via le bus inclut d'éventuelles lectures de tables de pages (base n = 1 soustraite).
- Un seul run complet ; les creux (n = 16 en flux +1, stores à n = 64/128/192/256) ne sont pas répliqués indépendamment.

---

# Phase 4 — prédicteurs de branchement

Sources : `results/2026-09-19/branch_experiments.csv` (médianes poolées), `raw/branch.jsonl` (chaque répétition avec T, fréquence, validité),
`phase4_run.log`, `env_phase4.json`. Un run complet (30 répétitions par point en 3 tours d'ordre alterné), cœur 1, governor `performance`,
**14 670 répétitions, 0 invalide**, sans `sudo` (hors changement du governor). Figures : `docs/img/branch_{btb,predictors,ras}.png` (`scripts/plot_branch.py`).

Code généré à l'exécution (`a64.rs` + `jit.rs`) : `mmap` RW, écriture, `dc cvau` / `dsb ish` / `ic ivau` / `dsb ish` / `isb` sur les lignes de
cache lues dans `CTR_EL0`, puis `mprotect` R+X. Les encodeurs sont testés contre des encodages connus (tests unitaires) **et** le code généré
est exécuté et vérifié fonctionnellement avant toute mesure (auto-test JIT : compteurs de branches, boucles, appels imbriqués, sauts indirects ;
il a d'ailleurs détecté un accumulateur non initialisé lors du développement).

## 4.1 BTB (anneau de N branches directes prises, à espacement S octets)

| Constat | Valeur | Statut | Confiance |
|---|---|---|---|
| Palier « zéro bulle » | **1,00 cycle par branche prise** pour N ≤ 12 (espacement ≥ 8 o) ; 1,67 à N = 16 ; 1,93 à N = 24 ; **2,00** à N ≥ 32 | mesuré | haute |
| Capacité du premier niveau | entre **12 et 16 branches** (N = 13–15 non testés) | déduit | moyenne |
| Palier à 2 cycles | maintenu jusqu'à N ≥ 1024–4096 selon l'espacement : 2,03 à N = 2048 (S = 32), 2,05 à N = 1024 (S = 64), 2,50–2,52 jusqu'à N = 4096 (S = 16), 2,75 jusqu'à N = 4096 (S = 8) | mesuré | haute |
| Capacité du second niveau | **≥ 4096 branches prises** (S = 16, 64 Kio de code, L1I refill 0,002/branche) : la borne supérieure est masquée par le cache d'instructions L1 de 64 Kio ; au-delà (S = 16, N = 6144) le coût passe à 4,3 puis 8,0 cycles avec 0,43–0,84 refill L1I par branche | mesuré (minorant) | moyenne |
| Effet de densité | 4 o entre branches (une branche par instruction) : 5,3 cycles/branche dès N = 256 ; 2 branches par groupe de 16 o coûtent plus (2,75) que 1 par groupe (2,0–2,5) | mesuré | haute |

## 4.2 Prédicteur conditionnel : apprentissage d'un motif aléatoire de période P

0 misprediction par branche pour **P ≤ 256** ; 0,007 (P = 384), 0,012 (512), 0,036 (1024), 0,053 (2048), 0,072 (3072) ; **effondrement à partir de
4096** (0,23), 0,38 (8192), 0,49 (65 536, ≈ hasard). Un motif aléatoire de 256 positions est appris exactement, jusqu'à ≈ 3000 positions il est
appris à > 90 %. Mesuré, confiance haute.

## 4.3 Portée de l'historique (branche finale corrélée à une branche aléatoire située K branches plus tôt)

| Variante | Résultat | Lecture |
|---|---|---|
| Fillers **pris** (`cbz xzr`, 16 o d'écart) | taux de la branche finale ≤ 0,02 jusqu'à **K = 2048**, puis **0,52 dès K = 2304** (= témoin) | la corrélation est exploitée jusqu'à ≈ **2048 branches prises** en arrière, plus loin non |
| Fillers **non pris** (`cbnz xzr`) | ≤ 0,03 jusqu'à K = 6144 (sauf des valeurs isolées 0,14–0,17 à K = 3 et 12) | les branches non prises **n'entrent pas dans l'historique** (ou n'y pèsent pas) |
| Témoin (branche finale indépendante) | 0,50 ± 0,01 pour tout K ≤ 1536 (0,52–0,57 ensuite : les fillers eux-mêmes commencent à être mal prédits) | valide la mesure : la prédiction de la variante corrélée n'est pas un artefact |

Confiance moyenne : la valeur de 2048 branches prises est la portée observée de la corrélation ; son interprétation en taille de registre
d'historique n'est pas établie (mécanisme possiblement différent d'un simple registre à décalage). La première version du test (branches espacées
de 4 o) donnait des résultats erratiques pour K ≥ 96 : artefact de densité de branches, corrigé par l'espacement de 16 o.

## 4.4 Nombre de branches conditionnelles statiques (issues périodiques de période 2, 4 ou 8)

Avec 16 o entre branches : 0 misprediction jusqu'à **N = 128**, ≤ 0,014 jusqu'à N = 768, 0,041 (1024), 0,156 (1536), 0,28 (2048), 0,18–0,31 (3072–8192,
irrégulier), 0,47 (16 384). **Capacité ≈ 1000 branches** avant une dégradation notable, jusqu'à 4096 (= 64 Kio de code) ; le L1I pèse à partir de là. Avec 4 o
entre branches, des mispredictions apparaissent dès N = 4 (0,16) : artefact de densité, conservé dans les données. Mesuré, confiance moyenne.

## 4.5 Prédicteur indirect (`br xN`)

| Test | Résultat |
|---|---|
| T cibles visitées en tourniquet (période T) | 0 misprediction pour **T ≤ 32** ; 0,021 à T = 48 ; **0,67 à T = 64**, puis 0,7–0,99 : **capacité entre 48 et 63 cibles** (falaise nette) |
| 16 cibles, séquence aléatoire de période P | 0,000 pour P ≤ 512 ; 0,002 (1024), 0,018 (2048) ; **0,39 à P = 4096**, 0,77 (8192), 0,94 (32 768) |

Mesuré, confiance haute pour la falaise à 48–64 cibles.

## 4.6 Pile de retour (appels imbriqués de profondeur D, 2 sites d'appel aléatoires par niveau)

Chaque niveau contient une branche aléatoire (0,5 misprediction) : on retranche 0,5·(D − 1). L'excès vaut −0,05 à D = 16, −0,04 à D = 17, puis
**+0,46 (D = 18), +0,95 (19), +1,44 (20), +1,94 (21)…** : +0,5 par niveau au-delà de 17. La perte de l'entrée la plus externe (retour vers un site unique,
prédit par un mécanisme de repli) est sans effet ; la perte de la suivante coûte 0,5. **Profondeur de la pile de retour : 16 entrées** (déduit ; confiance haute).
Observation séparée (chaîne d'appels fixes, sans branche aléatoire) : 2,0 cycles par niveau jusqu'à D = 7, puis +8,8 à D = 8 et +9,7 à D = 9, environ 5 cycles
par niveau à partir de D = 16 ; sans misprediction : **coût non expliqué** (aucune hypothèse testée).

## 4.7 Coût d'une mauvaise prédiction

| Résolution de la branche | Pénalité (pente cycles/misprediction, IC95 %) | Cycles/itération sans misprediction |
|---|---|---|
| immédiate (condition disponible tôt) | **14,81 cycles** [14,74 ; 14,86] | 1,95 |
| après 4 multiplications dépendantes | 23,50 [23,46 ; 23,53] | 11,95 |
| après 8 multiplications dépendantes | 32,96 [32,83 ; 33,06] | 23,87 |

180 points par ligne (6 probabilités d'issue × 30 répétitions). La pénalité minimale est ≈ **15 cycles** ; une condition qui se résout plus tard l'allonge
(≈ +2,2 cycles par multiplication dépendante, moins que leur latence de 4 cycles : le recouvrement n'est pas complet). Mesuré ; confiance haute pour la valeur
minimale, moyenne pour son interprétation.

## Limites de la phase 4

- Le BTB de second niveau n'est borné que par le bas (≥ 4096 branches) : le L1I de 64 Kio et son contenu masquent la capacité réelle ; aucun test n'a séparé BTB et I-cache au-delà.
- La « portée d'historique » est mesurée en branches prises avec des motifs répétitifs ; des historiques plus riches (chemins variés) pourraient réduire la portée utile.
- Les tests de capacité (4.2, 4.4, 4.5) dépendent de la structure des motifs (périodique, aléatoire) ; d'autres motifs peuvent donner d'autres capacités.
- Les espacements de 4 o entre branches créent des artefacts (BTB/groupes de récupération) : les résultats correspondants ne sont pas des capacités.
- Le coût par niveau de la chaîne d'appels au-delà de D = 7 n'est pas expliqué.
- Un seul run complet ; pas de réplication indépendante.

---

# Phase 5 — cœur out-of-order

Sources : `results/2026-09-19/ooo_experiments.csv` (médianes poolées), `raw/ooo.jsonl`, `phase5_run.log`, `env_phase5.json`. Un run complet (30 répétitions
par point en 3 tours d'ordre alterné), cœur 1, governor `performance`, **15 960 répétitions, 0 invalide**, sans `sudo` (hors changement du governor).
Figures : `docs/img/ooo_{window,instructions}.png` (`scripts/plot_ooo.py`). Aucune comparaison avec le guide d'optimisation Arm (PDF non fourni).

## 5.1 Tailles des fenêtres (un load manquant en DRAM par itération + N instructions indépendantes)

Chaque itération : 8 instructions fixes (3 pour le générateur pseudo-aléatoire, `and`, le load, un consommateur du load, `subs`, `b.ne`) + N remplisseurs.
Le temps par itération T(N) est linéaire par morceaux et **saute** chaque fois qu'un nombre entier d'itérations cesse de tenir dans la fenêtre : la
frontière « k itérations tiennent » se situe à N = C/k − (instructions de l'itération qui occupent la même ressource). Les sauts se placent aux valeurs
prévues pour une capacité unique C, ce qui valide l'estimation (plusieurs frontières cohérentes par ressource) ; à grand N, T sature à ≈ 235–240 cycles = la latence
DRAM mesurée séparément (235,5 cycles pour une chaîne dépendante, 98 ns, cohérent avec la phase 1).

| Ressource (remplisseur) | Frontières observées (N) | Capacité déduite | Statut | Confiance |
|---|---|---|---|---|
| **Reorder buffer** (`nop`) | 118–120, 54–56, 34–36, 20–24, 16–20, 12–16, 8–12 (attendu pour C = 128 : 120, 56, 34,7, 24, 17,6, 13,3, 10,3) | **128 instructions** (127–128) | mesuré / déduit | haute |
| **Registres entiers** (`add xN, xN, #1`) | 80–82 (+71 cycles), 36–38, 20–24, 12–16, … | ≈ **87 destinations entières en vol** (86–88) ; avec ≈ 31 registres architecturaux : ≈ 118–120 registres physiques | déduit | moyenne (nombre de registres physiques : faible) |
| **Registres vectoriels** (`eor v.16b`) | 94–96 (+65), 46–48, 30–32, 20–24, … | ≈ **96 destinations vectorielles en vol** ; avec 32 registres architecturaux : ≈ 128 registres physiques | déduit | moyenne (registres physiques : faible) |
| **File de loads** (`ldr xzr, [x1, #…]`, hits L1) | rampe de sauts à 34–36 (+42), 36–38, 38–40 ; 16–20, 8–12, … | ≈ **36 loads en vol** (36–42 : la rampe résiduelle entre 36 et 42 n'est pas expliquée) | déduit | moyenne à faible |
| **Store buffer** (`str xzr, [x1, #…]`, hits L1) | 40–42 (+89), 42–44 (+10), 20–24, 12–16, … | ≈ **42 stores en vol** (41–43) | déduit | moyenne |

Les débits des remplisseurs, lus sur la pente de T(N) à grand N, sont ≈ 4 `nop`/cycle, ≈ 3 `add`/cycle, 2 `eor` vectoriels/cycle, 2 loads/cycle et 0,74 store/cycle
(1,35 cycle par store dans cette boucle).

## 5.2 Parallélisme mémoire (K chaînes de chargements dépendants entrelacées)

| Niveau | K = 1 | Plateau (K ≥ 12–14) | Parallélisme effectif | Lecture |
|---|---|---|---|---|
| Jeu dans le L2 (320 Kio) | 12,6 cycles | 2,2 cycles par load | 5,6 | le plateau est le débit L2 → L1 (≈ 29 o/cycle), pas un nombre de misses simultanés : ≥ 6 misses en vol suffisent |
| DRAM (16 Mio, sans page walks) | 235,5 cycles | 26,0 cycles par load | **9,1** | **≈ 9 misses L1 simultanés** ; concorde avec 5.1 (30 cycles par itération à N = 0 pour 235 cycles de latence, soit ≈ 8 loads en vol) |

Confiance moyenne : le plateau à 26 cycles par ligne (≈ 5,9 Go/s en accès aléatoires) peut aussi refléter la limite des accès DRAM aléatoires ; il donne néanmoins le nombre de
requêtes en vol ≈ 9 dans les deux méthodes.

## 5.3 Latence et débit d'instructions (cycles ; INST_RETIRED / instruction = 1,125 = 18/16 pour toutes : la boucle exécute bien ce qui est annoncé)

| Instruction | Latence | Inverse du débit | Remarque |
|---|---|---|---|
| `add x,x,x` | **1** | **0,354** | 17 opérations ALU (16 `add` + `subs`) par 5,66 cycles = **3,0 par cycle** ⇒ 3 ALU entières (déduit) |
| `mul x,x,x` (64 bits) | 4,06 | **3,0** | un 64-bit toutes les 3 cycles ; `madd` identique |
| `madd x,x,x,x` | 4,06 (accumulateur) | 3,0 | |
| `sdiv x,x,x` | 5 (÷ 1) | 20 (0x7fff… ÷ 3) | dépendant des données : deux jeux d'opérandes seulement |
| `fadd d` | 2 | 0,5 | 2 par cycle |
| `fmul d` | 3 | 0,5 | 2 par cycle |
| `fmla v.4s` (NEON) | 2 (chaîne d'accumulation) | 0,5 | 2 par cycle ⇒ 2 × 4 lanes × 2 flops = 16 flops/cycle, soit ≈ 38 Gflop/s simple précision par cœur à 2,4 GHz (déduit, pic) |
| `sdot v.4s, v.16b, v.16b` | 1 (chaîne d'accumulation) | 0,5 | 2 par cycle ⇒ 32 MAC int8/cycle |
| `ldr x,[x]` (hit L1) | **4** | 0,5 | 2 loads par cycle |
| `ldp x,x,[x]` | 4 | 1,0 | |
| `ldp q,q,[x]` | — | 1,0 | 32 o/cycle depuis le L1 |
| `str x,[x]` | — | 0,503 | 2 stores par cycle (même adresse) |
| `stp x,x,[x]` | — | **3,0** | anormalement lent : **non expliqué** |
| `dmb ish` | — | **7** | seul, en série |
| `str` + `dmb ish` | — | **12** par paire | |
| `ldar x,[x]` | 4 | 0,5 | identique à `ldr` dans ces conditions |
| `stlr x,[x]` | — | 0,501 | idem `str` |
| `ldadd` (LSE) | — | **13** | même ligne ou 4 lignes : non pipeliné |

## Limites de la phase 5

- Les tailles de fenêtre supposent que les `nop` occupent des entrées du ROB (confirmé par la cohérence des frontières multiples avec C = 128) ; la présence d'un nombre précis d'entrées pour d'autres classes d'instructions n'est pas testée.
- Le nombre de registres physiques est déduit du nombre de destinations en vol **plus** les registres architecturaux : l'hypothèse (31 entiers, 32 vectoriels) n'est pas vérifiée.
- La rampe de sauts entre 36 et 42 pour les loads reste ambiguë : la file de loads est donnée comme une plage.
- Le plateau du test MLP en DRAM peut être limité par le débit des accès aléatoires.
- `sdiv` : seulement deux jeux d'opérandes ; `stp` : le débit de 3 cycles n'est pas expliqué ; `sdot` et `fmla` : latence mesurée sur la chaîne d'accumulation uniquement.
- Un seul run complet ; pas de réplication indépendante.
