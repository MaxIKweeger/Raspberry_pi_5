# Méthodologie

Ce document décrit, par expérience : l'hypothèse, le principe, les biais possibles et les références.
Il est complété à chaque phase. État actuel : phases 0 à 6.

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

## Phase 2 — géométrie et politique de remplacement (`sudo a76probe run --exp cache`, puis `analyze-cache`)

Prérequis : lecture de `/proc/self/pagemap` avec PFN réels (root). La réserve de 2 Gio est verrouillée (`mlock`) ; les PFN
sont relus à la fin (0 page déplacée exigé) et vérifiés contre les plages `System RAM` de `/proc/iomem`.
Les lignes cibles sont à l'offset de page 12288 (set L1 192), loin de la zone de trace (offsets 0–8191).

### E2.1 Taille de ligne
- **Principe** : chaînes dont les éléments sont espacés de 8 à 128 o, lignes visitées dans un ordre aléatoire, éléments
  croissants à l'intérieur d'une ligne ; refills par load = pas / taille de ligne tant que pas < ligne. Normalisé par le
  plateau du pas 128 pour le L2 (une fraction du jeu reste dans le L2).
- **Biais** : les accès ascendants intra-ligne pourraient déclencher un préchargement ; l'ordre aléatoire des lignes l'évite.

### E2.2 Associativité
- **Principe** : K lignes de mêmes bits physiques 6–24 (donc même set à tous les niveaux), chaîne aléatoire à cycle unique ;
  refills du niveau testé par load en fonction de K. Capacité de conflit = (premier K avec ≥ 5 % de refills) − 1.
- **Biais** : une politique non-LRU (PLRU, NRU, aléatoire) produit des rampes, d'où le choix du seuil à 5 % plutôt qu'à 50 %.
  À L3 les lignes d'un même set L3 occupent aussi un même set L2 : la capacité mesurée est L2 + L3 si le L3 est exclusif.

### E2.3 Bits d'index
- **Principe** : avec K = 1,5 × capacité lignes (débordement), inverser un seul bit physique b dans une ligne sur deux ;
  si b sélectionne le set, chaque moitié tient (taux ≈ 0), sinon le débordement persiste. Bits 6–13 : offset dans la page ;
  bits 14–24 : pages de classes de clé différente d'un seul bit.
- **Biais** : un hachage par XOR de bits > 24 échapperait au test ; un bit d'index haché avec d'autres est tout de même détecté.
  Seuil : bit d'index si taux < 50 % du taux de base ; les bits d'index donnent 0,00–0,03, les autres ≈ le taux de base.

### E2.4 Politique de remplacement
- **Principe** : W+1 à W+3 lignes d'un même set, 8 motifs rejoués sur 4096 accès × 16 passes ; accès rendus dépendants
  (le zéro lu est ajouté au pointeur de trace) pour que le cache voie l'ordre du programme. Au L2, 24 accès d'éviction à
  des lignes de même set L1 mais d'autres sets L2 (couleurs de pages différentes) chassent la cible du L1 après chaque accès.
  Taux de miss = refills du niveau / accès cible. Témoin : W lignes cycliques (0,0002–0,0006 attendu).
- **Modèles et notation** : voir `src/sim.rs` et `src/analysis.rs`. Départ aléatoire des modèles (la moitié des voies invalides,
  bits d'état aléatoires) car les politiques pseudo-LRU ont plusieurs cycles limites selon l'état initial ; un modèle « atteint »
  un état s'il l'atteint depuis ≥ 1 % des départs ; score = distance moyenne au plus proche état atteignable.
- **Biais** : les tests passent par la hiérarchie : au L2, le flux vu est filtré par le L1 (évité par les évictions) ; les
  accès d'éviction ajoutent du trafic dans d'autres sets. La confiance est plafonnée à « moyenne » (8 modèles seulement).

### E2.5 Inclusion L1/L2
- **Principe** : X gardée chaude en L1 (touchée un accès sur deux) pendant que W lignes du même set L2 tournent ; X n'atteint
  jamais le L2. Sans back-invalidation le L2 finit par évincer la copie périmée de X puis les W lignes tiennent (0 miss) ;
  avec back-invalidation X sort du L1 à chaque éviction (1 miss par accès).
- **Biais** : voir les résultats : la valeur intermédiaire (≈ 0,2) empêche de conclure.

## Phase 3 — préchargeurs (`a76probe run --exp prefetch`; `--exp boundary` pour les frontières seules)

Pas de compteur PMU dédié aux préchargeurs : deux mesures indirectes. (1) **Latence de chargements dépendants** le long d'un stride
constant, rapportée à un ordre aléatoire sur le même jeu (le préchargeur s'entraîne sur les adresses, pas sur les données, donc la
dépendance de données n'empêche pas le gain). (2) **Lignes réellement lues sur le bus** (`BUS_ACCESS` / 8 par ligne, mesuré en phase 1)
pour des flux à froid de longueur n, ce qui compte aussi les lignes préchargées et jamais demandées.

### E3.1 Balayage de stride
- Chaîne visitant la ligne `(j·k) mod N` (N premier : cycle unique), k de ±1 à ±4096 lignes, sur trois jeux : 4093 lignes (256 Kio, servi par
  le L2), 16381 lignes (1 Mio, L3/L2), 1 048 573 lignes (64 Mio, DRAM). Référence : cycle aléatoire de Sattolo sur le même jeu.
  Noyau `chase` (16 `ldr` dépendants). PMU : cycles, refills L1/L2/L3, `LL_CACHE_MISS_RD`, `BUS_ACCESS`, `DTLB_WALK` (7 compteurs).
- **Biais** : (a) strides ≥ N/16 sur les petits jeux se replient en quelques pas (exclus) ; (b) aux grands strides les page walks
  ajoutent de la latence (DTLB_WALK par accès enregistré) ; (c) le préchargement peut aussi gagner sur un cycle aléatoire par hasard
  d'adjacence : le rapport à l'aléatoire mesuré (≈ 1,0 pour les grands strides) le contrôle.

### E3.2 Flux entrelacés
- S flux séquentiels (+1 ligne) de 8 Mio chacun, décalés de 257 lignes, visités en tourniquet dans une seule chaîne dépendante ;
  S de 1 à 32. **Biais** : au-delà de 16 flux le jeu total dépasse 128 Mio ; les accès TLB augmentent aussi avec S.

### E3.3 Distance / degré
- n accès dépendants à froid (adresses neuves dans un pool de 512 Mio, emplacements de 512 Kio tirés au hasard), 64 flux par fenêtre PMU,
  attente de 20 µs avant de relire les compteurs (laisser sortir les préchargements en vol). Noyau `stream_load` : le zéro lu est ajouté à
  l'adresse (dépendance). « Extra » = (bus − base(n = 1)) / 8 − (n − 1). Stores : `stream_store` (indépendants).
- **Biais** : compter des lignes sur le bus suppose 8 accès bus par ligne (vérifié en phase 1) ; les écritures différées de
  précédents flux peuvent s'ajouter (stores) ; l'interprétation en « distance d'avance » suppose que les lignes préchargées ne sont
  jamais réutilisées par la demande suivante (vrai : flux court à adresses neuves).

### E3.4 Frontières
- Flux de 16 lignes : « fin exacte sur la frontière », « traverse la frontière » (8 lignes avant, 8 après), « fin au milieu du bloc » (témoin) ;
  ascendant et descendant (par symétrie autour de la frontière). Avant chaque répétition, le L2/L3 est vidé par la lecture de 8 Mio.
  Trois sources de candidats : pool ordinaire (frontières de 4 Kio à l'intérieur d'une page, frontières de 16 Kio, 64 Kio, 2 Mio), et 32 Mio de mémoire
  **CMA** (`/dev/dma_heap/linux,cma`, groupe `video`), physiquement contiguë par construction, pour des frontières de 4 Kio, 16 Kio et
  64 Kio (4 phases d'alignement, l'adresse physique n'étant pas lisible).
- **Biais** : voir les résidus non expliqués dans `RESULTS.md`.

### E3.5 Stores
- `store_table` : stores indépendants aux adresses d'une table (lue séquentiellement) parcourant le jeu DRAM selon un stride ou un ordre
  aléatoire ; ns/store et lignes bus par store.
- **Biais** : la table est elle-même lue en flux (constante additive) ; loads (dépendants) et stores (indépendants) ne mesurent pas la même
  grandeur (latence contre débit).

## Phase 4 — prédicteurs de branchement (`a76probe run --exp branch`)

**Génération de code à l'exécution plutôt que `build.rs`** : les paramètres (nombre de branches, espacement, distance de corrélation, profondeur d'appel,
nombre de cibles) varient sur de larges plages ; `build.rs` imposerait des milliers de variantes précompilées. Choix : `mmap` en RW, écriture des mots de 32 bits
produits par un assembleur à étiquettes (`a64.rs`), maintenance de cache (`dc cvau` sur les lignes de données, `ic ivau` sur les lignes d'instructions, `dsb ish`,
`isb`, tailles de ligne lues dans `CTR_EL0`), puis `mprotect` R+X : jamais W+X simultanément. Vérification : (1) tests unitaires des encodeurs contre des encodages connus,
(2) exécution du code généré avec résultat vérifié (`selftest_jit`) avant toute mesure.

Chaque fonction générée suit l'ABI C (`x0` = itérations, `x1` = données). PMU (7 compteurs) : cycles, `BR_MIS_PRED`, `BR_RETIRED`, `INST_RETIRED`, `STALL_FRONTEND`,
`L1I_CACHE_REFILL`, `ITLB_WALK`. Chaque configuration est mesurée 3 × 10 répétitions (ordre croissant / décroissant / croissant), préchauffage inclus.

### E4.1 BTB
- **Principe** : anneau de N `b` (un par emplacement, ordre aléatoire à cycle unique) espacés de S octets ; cycles par branche prise.
- **Biais** : au-delà de 64 Kio de code, le cache d'instructions domine (L1I refill enregistré par branche) ; l'espacement de 4 o place plusieurs branches dans un même groupe de récupération.

### E4.2 Motif aléatoire de période P
- **Principe** : une branche `tbz` sur un octet de données aléatoire répété avec la période P (262 144 itérations par appel) ; mispredictions par branche.

### E4.3 Portée de l'historique
- **Principe** : la branche 0 teste un bit aléatoire ; K − 1 branches d'une même direction (prises `cbz xzr` / non prises `cbnz xzr`, 16 o d'écart) ; la branche finale teste
  le même bit. Mispredictions par itération = 0,5 (branche 0) + taux de la finale. **Témoin** : la finale teste un bit indépendant (doit valoir 0,5). Les données sont
  régénérées avant chaque répétition (aucune séquence n'est rejouée).

### E4.4 Nombre de branches statiques
- **Principe** : N `tbz` par itération, la branche i testant le bit (i mod 3) d'un compteur d'itérations (périodes 2, 4, 8) ; les deux issues continuent à l'instruction suivante (ou après des `nop` sautés).

### E4.5 Prédicteur indirect
- **Principe** : `ldrh idx ; ldr cible, [table, idx*8] ; br cible` ; T cibles (blocs de 16 o) visitées en tourniquet, ou séquence aléatoire de période P sur 16 cibles.

### E4.6 Pile de retour
- **Principe** : chaîne de D fonctions imbriquées ; variante **à sites d'appel aléatoires** (chaque niveau appelle le suivant depuis l'un de deux sites choisis par un bit
  aléatoire) pour que l'adresse de retour de chaque niveau ne soit prédictible que par une pile de retour. L'excès de mispredictions au-dessus de 0,5·(D − 1) (les branches de choix de site)
  mesure les retours mal prédits.

### E4.7 Pénalité
- **Principe** : une branche dépendant d'un octet de données dont la probabilité de prise varie (0, 1/32, 1/16, 1/8, 1/4, 1/2) ; régression des cycles par itération sur les mispredictions
  par itération (180 points), IC95 % par bootstrap des paires. Variantes avec 4 et 8 multiplications dépendantes (`mul x,x,x` préserve le bit 0) avant la branche.

## Phase 5 — cœur out-of-order (`a76probe run --exp ooo`)

### E5.1 Fenêtres
- **Principe** : boucle générée à l'exécution : générateur xorshift (3 `eor` avec décalage), `and` de masque, `ldr` aléatoire dans 16 Mio, un consommateur `add x7, x7, x5` (sans lui le
  load, dont le résultat n'est pas lu, ne bloque pas la retraite), puis N remplisseurs indépendants d'une classe donnée, puis `subs` / `b.ne`. Le temps par itération T(N) est linéaire
  par morceaux ; il saute quand une itération de plus ne tient plus dans la ressource limitante. Détection : différence de T entre points consécutifs moins la pente de la fin de
  courbe (débit d'exécution), seuil 2,5 cycles ; grille de N au pas de 2 entre 24 et 144. Référence : même boucle avec le load dans le L1 (temps d'exécution seul).
- **Deux pièges détectés pendant le développement** (conservés ici car instructifs) : (1) l'état du générateur était réinitialisé à chaque appel, donc chaque répétition rejouait les
  mêmes adresses (1 Mio, qui tient en L3) et T saturait à 47 cycles au lieu de la latence DRAM ; corrigé en persistant l'état dans la page de travail ; (2) sans consommateur du load, le
  cœur ne bloquait pas la retraite : T(0) était trop bas ; corrigé par le consommateur.
- **Biais** : la taille estimée d'une ressource dépend du nombre d'instructions de base de la boucle qui l'utilisent (6 destinations entières, 1 load) ; les registres physiques sont
  déduits en ajoutant les registres architecturaux ; la zone de 16 Mio reste dans la portée du TLB L2 (pas de page walks).

### E5.2 MLP
- **Principe** : K chaînes dépendantes indépendantes (K ≤ 22, registres `x2`–`x17` et `x19`–`x24`, ces derniers sauvegardés), cycles aléatoires disjoints ; cycles par load. Deux jeux :
  320 Kio au total (servi par le L2) et 16 Mio (DRAM sans page walks).
- **Biais** : le plateau peut refléter un débit plutôt qu'un nombre de requêtes en vol ; recoupé avec E5.1 à N = 0.

### E5.3 Instructions
- **Principe** : boucles `asm!` de 16 instructions déroulées ; latence = chaîne dépendante sur un registre, débit = 8 registres de destination indépendants (chacun utilisé deux fois).
  Les macros produisent le texte de l'assembleur ; INST_RETIRED / instruction contrôle que la boucle exécute bien 16 + 2 instructions par itération.
- **Biais** : `sdiv` dépend des opérandes ; les latences `fmla` / `sdot` sont celles de la chaîne d'accumulation ; `ldadd` mesure des atomiques sur des lignes présentes en L1.

## Phase 6 — inter-cœurs et cas particuliers (`a76probe run --exp multicore`)

### E6.1 Ping-pong
- **Principe** : deux threads épinglés (`sched_setaffinity`, vérifié), barrière de départ, boucles `asm!` : l'initiateur écrit `2i+1` (`stlr` ou `ldaddal`), attend `2i+2` (`ldar`) ; le
  répondeur attend `2i+1`, écrit `2i+2`. Temps de l'initiateur par `CNTVCT` et compteur de cycles PMU ouvert dans ce thread ; latence aller simple = mesure / (2 × 200 000).
  12 paires ordonnées, ordre alterné entre tours, matrice répétée sur 4 lignes de pages différentes.
- **Biais** : un thread déprogrammé par le système allonge une répétition (traité par la médiane) ; la ligne est seule dans sa page ; le protocole mesure le transfert d'une ligne modifiée.

### E6.2 Forwarding
- **Principe** : boucle `asm!` de 8 paires « store puis load » dont la valeur lue alimente le store suivant (chaîne mémoire dépendante) ; cycles par paire. La référence sans chevauchement (autre ligne) n'a pas
  de dépendance et sert de contrôle.
- **Biais** : un load plus large que le store fournit des octets qui viennent peut-être du L1 ; le contenu n'est pas vérifié (seul le temps l'est).

### E6.3 Accès non alignés
- **Principe** : 8 pointeurs indépendants, chaque adresse utilisée deux fois par itération, jeu de 4 Kio dans le L1 ; décalages de 0 à 63 dans la ligne, plus des franchissements de frontière de 4 Kio et
  de page de 16 Kio avec **deux frontières distinctes** alternées (huit frontières espacées de 16 Kio tomberaient dans les mêmes sets du L1 de 4 voies et donneraient des misses de conflit).
  PMU : cycles, `L1D_CACHE`, `L1D_TLB`, `MEM_ACCESS`.
- **Biais** : mêmes décalages dans la ligne pour les 8 pointeurs ⇒ même banque du L1 : plafond de 1 accès/cycle pour tous les cas.
