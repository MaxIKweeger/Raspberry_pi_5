# Méthodologie

Ce document décrit, par expérience : l'hypothèse, le principe, les biais possibles et les références.
Il est complété à chaque phase. État actuel : phases 0 et 1.

## Règles transverses (appliquées par `harness.rs`)

- Thread épinglé sur un cœur (`sched_setaffinity`, vérifié par `sched_getcpu`).
- 3 répétitions de préchauffage non enregistrées, puis ≥ 30 répétitions enregistrées.
- Mémoire préfaultée (`MAP_POPULATE`), verrouillée (`mlock`, meilleur effort ; l'état est enregistré).
- Statistiques : médiane, MAD (brute, non normalisée), percentiles 5/25/75/95, IC95 % par bootstrap
  percentile de la médiane (2000 rééchantillons, graine fixe → rapports reproductibles).
- Garde (`guard.rs`) : avant chaque répétition attente si T > 75 °C, arrêt si T > 80 °C.
  Avant/après chaque répétition, T et `scaling_cur_freq` sont lus ; la répétition est **invalide** si
  T > 80 °C, si la fréquence a changé, ou si elle diffère de `scaling_max_freq`. Les statistiques ne
  portent que sur les répétitions valides ; les invalides restent dans le JSONL avec leurs raisons.
- Les « attentes » affichées (`expected(code)`) sont dérivées du code de la charge de travail
  (nombre de `ldr`, de branches, d'instructions), jamais de la documentation ni de la mémoire.
- Alternance des conditions (ordre) : appliquée dès la phase 1 (tours croissant / décroissant / croissant,
  ordre des opérations et des conditions tourné d'un tour à l'autre).

## Phase 0 — chaîne de mesure (`a76probe selftest`)

### E0.1 Coût et résolution de CNTVCT_EL0
- **Hypothèse** : lecture `isb; mrs cntvct_el0` de quelques dizaines de cycles ; résolution = 1/CNTFRQ.
- **Principe** : 200 000 lectures consécutives, temps total / nombre d'appels ; et 10 000 deltas
  entre lectures consécutives (fraction de deltas nuls, delta non nul minimal, p99, max).
- **Biais** : l'`isb` sérialise et gonfle volontairement le coût par rapport à une lecture non
  sérialisée ; le max reflète des interruptions (le noyau n'est pas exclu du chronomètre).
- **Référence** : Arm ARM, Generic Timer (CNTVCT_EL0, CNTFRQ_EL0).

### E0.2 Coût et gigue du compteur de cycles PMU
- **Hypothèse** : `perf_user_access=0` ⇒ lecture par appel système, de l'ordre de la centaine de ns à la µs.
- **Principe** : `read()` sur le leader d'un groupe (format GROUP + temps activé/actif), chronométré
  par CNTVCT ; 2000 appels par répétition ; en plus, 1000 appels chronométrés individuellement
  (p50/p99/max) ; coût d'une fenêtre `RESET`+`ENABLE`+`DISABLE`.
- **Biais** : la mesure individuelle est quantifiée par le tick de 18,5 ns.
- **Référence** : `man perf_event_open`.

### E0.3 Cohérence INST_RETIRED / cycles / fréquence
- **Hypothèse** : sur une boucle asm de N instructions connues, `INST_RETIRED ≈ N` ;
  `cycles ≈ fréquence × temps`.
- **Principe** : deux noyaux (`kernels.rs`) : 8 `add` indépendants + `subs` + `b.ne` (10 instr/iter)
  et 10 `add` chaînés + `subs` + `b.ne` (12 instr/iter), 5 M itérations. Fenêtre PMU (cycles,
  inst_retired) autour d'une fenêtre CNTVCT. Le désassemblage est archivé dans `docs/asm/kernels.txt`.
- **Biais** : la fenêtre PMU inclut quelques instructions user autour des `ioctl` (≈ 70 sur 5·10⁷) ;
  fréquence de référence = `scaling_max_freq` (vérifiée inchangée avant/après).

### E0.4 Validation par événement
- **Principe** : 25 événements PMU × 3 charges (32 Ko en L1 lu 2000 fois ; flux de 64 Mo lu 2 fois,
  un `ldr` par ligne ; boucle de 10⁶ itérations). Attentes dérivées du code uniquement pour les
  événements dont la sémantique ARMv8 est sans ambiguïté (INST_RETIRED, L1D_CACHE, MEM_ACCESS,
  BR_RETIRED) ou sous forme de borne (BR_MIS_PRED ≈ 0, L1D_CACHE_REFILL : 0 en L1, ≈ lignes en flux).
  Les autres événements sont enregistrés sans verdict : ils sont des observations à interpréter en
  phases ultérieures.
- **Biais** : « dans la tolérance » ne prouve pas que l'événement compte ce qu'on croit, seulement
  qu'il est compatible avec l'attente. Les tolérances sont larges (5 %) et écrites dans `selftest.rs`.

### E0.5 Capacité d'un groupe d'événements
- **Principe** : ouvrir des groupes de taille croissante (cpu_cycles + événements courants) et
  exécuter une boucle ; le plus grand groupe ouvert sans erreur et avec `time_running == time_enabled`
  est la capacité sans multiplexage.
- **Biais** : sonde unique (déterministe), pas 30 répétitions.

## Choix des sources de temps (décision documentée d'après les mesures)

| Source | Coût | Résolution | Usage retenu |
|---|---|---|---|
| CNTVCT_EL0 + `isb` | 21,7 ns (≈ 52 cycles) | 18,5 ns | délimiter des régions ≥ ~10 µs (erreur de quantification ≤ 0,2 %) ; recoupement temps ↔ cycles |
| Compteur de cycles PMU (`read`) | ≈ 396 ns par lecture, ≈ 1,4 µs par fenêtre | 1 cycle | mesures agrégées sur régions longues (≥ ~100 µs) ; coût de fenêtre soustrait ou amorti |

Pour les mesures très courtes, la méthode est d'amortir : boucles de milliers d'itérations, jamais de
chronométrage d'une opération isolée. `perf_user_access=1` (lecture directe en user) est en attente
d'autorisation (voir `PLAN.md`, Q3).

## Phase 1 — hiérarchie mémoire (`a76probe run --exp latency|tlb|bandwidth`)

Ordre des conditions : chaque expérience est jouée en 3 tours (croissant, décroissant, croissant) de
`ceil(repeat/3)` répétitions ; les valeurs valides des tours sont poolées. Un biais monotone (dérive
thermique) apparaîtrait comme une différence entre tours (tags `round` dans le JSONL).

### E1.1 Latence par pointer chasing
- **Hypothèse** : plateaux L1/L2/L3/DRAM aux tailles de la spec.
- **Principe** : cycle unique aléatoire (Sattolo, testé unitairement), un nœud par ligne de 64 o, adresse
  suivante stockée dans le nœud ; noyau asm `ldr x, [x]` déroulé 16× (`kernels::chase`) ; 2^20 à 2^21 loads par
  répétition ; tailles de 4 Kio à 1 Gio (×1,25 + puissances de 2). Groupe PMU de 7 compteurs : cycles,
  L1D/L2D/L3D refill, LL_CACHE_MISS_RD, L1D_TLB_REFILL, DTLB_WALK.
- **Biais** : (a) au-delà de ~128 Mio, une répétition ne parcourt qu'une fraction du cycle (2 M loads),
  suffisant pour un régime stationnaire mais pas pour tester un motif ; (b) l'indexation L2/L3 dépend
  des adresses physiques aléatoires : les capacités effectives paraissent plus floues ; (c) au-delà de
  ~20 Mio, les page walks s'ajoutent à la latence DRAM ; (d) près des frontières, une répétition mélange
  deux niveaux.
- **Attente dérivée du code** : sans préchargement exploitable, L1 refill/load = 1 dès que la taille
  dépasse L1 ; confirmé par PMU.

### E1.2 TLB
- **Principe** : un nœud par page de 16 Kio (taille lue par `sysconf`), ligne aléatoire dans la page (pour
  ne pas aliaser les sets du L1D, dont la voie fait exactement 16 Kio) ; témoin « packed » : même nombre de
  nœuds empaquetés. La différence paged − packed isole le coût de traduction quand le cache se comporte
  de la même façon.
- **Biais** : à N pages élevé, la répartition des lignes n'est pas identique entre les deux conditions
  (L1 refill paged ≠ packed vers 1024–1280 pages) ; on n'interprète le coût de traduction que là où les
  compteurs de refill de cache sont égaux (52–192 pages), ou avec correction explicite (4096, 6144 pages).
- **Limite** : pas de hugepages ⇒ un seul granule (16 Kio).

### E1.3 Bande passante
- **Principe** : noyaux NEON `ldp q`/`stp q` (128 o par itération, 8 registres), tampon privé par cœur,
  cible de 128 Mio de trafic par répétition et par cœur, threads épinglés (cœurs 1, 2, 3, 0), barrière de
  départ, temps = du premier départ à la dernière fin (CNTVCT partagé). Avec 1 cœur : fenêtre PMU
  (cycles, refills, BUS_ACCESS).
- **Biais** : démarrage non simultané au µs près ; création des threads hors zone chronométrée ; le cœur 0
  porte le bruit système ; « copie » compte lecture + écriture.
