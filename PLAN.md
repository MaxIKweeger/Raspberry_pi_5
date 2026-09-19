# PLAN.md — a76probe

Statut : **phase 0 implémentée et validée sur le Pi (2026-09-19) ; en attente du feu vert pour la phase 1.**
Les chiffres du §1 viennent de commandes en lecture seule exécutées sur le Pi le 2026-09-19 ; les résultats de la phase 0 sont dans `RESULTS.md`.

### Décisions appliquées par défaut (réponse « ok » sans détail sur les questions du §5)
- Q5 : pas de clé SSH installée ; mot de passe passé par variable d'environnement (`deploy.sh`, jamais écrit dans le dépôt).
- Q6 : dépôt git initialisé ; le dépôt est **public** : https://github.com/MaxIKweeger/Raspberry_pi_5 (demande de l'utilisateur).
- Q7 : `perf` non installé. Q8 : cible `aarch64-unknown-linux-musl` statique (validée).
- Q1–Q4 (sudo : pagemap, governor `performance`, `perf_user_access=1`, arrêt de lightdm) : **toujours ouvertes, aucun `sudo` utilisé.** Inutiles pour la phase 0 ; Q1 et Q2 deviennent utiles dès la phase 1/2.
- Publication : `env.json` masque les adresses MAC de la ligne de commande noyau ; l'adresse IP et le mot de passe ne sont pas dans le dépôt.

### Écarts par rapport au plan initial
- Aucune nouvelle dépendance ; `harness.rs` (répétitions gardées + résumés) ajouté ; sous-commande `pmu-list` ajoutée.
- Le chrono par défaut est décidé d'après les mesures (voir `docs/methodology.md`) : CNTVCT pour les régions ≥ ~10 µs, compteur de cycles PMU (via `read()`, ≈ 396 ns) pour les régions ≥ ~100 µs.
- Le groupe PMU maximal sans multiplexage est de 7 événements (dont `cpu_cycles`), à respecter dans les phases suivantes.

## 1. Constats sur la machine (mesurés / lus)

| Élément | Valeur lue | Source |
|---|---|---|
| Carte | Raspberry Pi 5 Model B Rev 1.0, 8 Go | `/proc/device-tree/model`, `free` |
| OS / noyau | Debian 13 (trixie), `6.18.39+rpt-rpi-2712`, aarch64 | `uname -a` |
| Page | **16384 o** | `getconf PAGESIZE` |
| MIDR_EL1 | `0x414fd0b1` (implementer 0x41, part 0xd0b = Cortex-A76) | sysfs + `mrs` exécuté sur le Pi |
| Flags | fp asimd aes pmull sha1 sha2 crc32 **atomics** fphp asimdhp cpuid asimdrdm lrcpc dcpop **asimddp** | `/proc/cpuinfo` |
| L1D / L1I | 64 Ko, 4 voies, 256 sets, ligne 64 o, privé | sysfs `cache/index0,1` |
| L2 | 512 Ko, 8 voies, 1024 sets, ligne 64 o, privé | sysfs `index2` |
| L3 | 2048 Ko, 16 voies, 2048 sets, ligne 64 o, partagé cpus 0-3 | sysfs `index3` |
| Governor | `ondemand`, 1,5–2,4 GHz (pas de 100 MHz) ; `performance` disponible | sysfs cpufreq |
| Température | 52 °C au repos, `get_throttled=0x0` | `vcgencmd`, thermal_zone0 |
| `perf_event_paranoid` | 2 | `/proc/sys` |
| `perf_user_access` | 0 (donc pas de `mrs PMCCNTR_EL0` / rdpmc en user) | `/proc/sys` |
| PMU | device **`armv8_cortex_a76`** (type 9, cpus 0-3), *pas* `armv8_pmuv3_*` | `/sys/bus/event_source/devices` |
| Événements PMU exposés | br_mis_pred, br_mis_pred_retired, br_pred, br_retired, bus_access, bus_cycles, cpu_cycles, dtlb_walk, itlb_walk, inst_retired, inst_spec, l1d_cache, l1d_cache_refill, l1d_cache_wb, l1d_tlb, l1d_tlb_refill, l1i_*, l2d_cache(_allocate/_refill/_wb), l2d_tlb(_refill), l3d_cache(_allocate/_refill), ll_cache_rd, ll_cache_miss_rd, mem_access, stall_backend, stall_frontend, exc_*, ttbr_write_retired, … | `.../events` |
| Hugepages | **Absentes** : ni `/sys/kernel/mm/hugepages`, ni `transparent_hugepage`, ni `vm.nr_hugepages` | sysfs / proc |
| `/proc/self/pagemap` | existe mais `-r--------` ; PFN masqués pour un non-root | `ls -l`, `id` |
| Cmdline noyau | `numa=fake=8`, `numa_policy=interleave`, `cgroup_disable=memory`, `system_heap.max_order=0` | `/proc/cmdline` |
| `perf` (outil) | absent | `command -v` |
| Rust sur le Pi | 1.92.0 stable (rustup, `~/.cargo/env` à sourcer) | non utilisé, voir §2 |
| Charge | ~0, mais session graphique (lightdm/wayvnc) active + 3 sessions SSH | `uptime`, `who` |
| Sudo | `sudo -n true` réussit sans mot de passe (test à blanc, aucune modif) ; je n'utiliserai `sudo` qu'après ton accord explicite | — |

### Points qui changent le plan par rapport à ton brief
1. **Pas de hugepages/THP** : les expériences « hugepages » (TLB phase 1, sets L2/L3 phase 2) doivent passer par autre chose (cf. §4 et questions).
2. **Pas d'accès user au compteur de cycles PMU** (`perf_user_access=0`) : le cycle counter passe par `perf_event_open` + `read()` (syscall) tant que tu ne l'actives pas. Donc chronométrage court = `CNTVCT_EL0` ; PMU = mesures agrégées sur de longues boucles (le surcoût du `read()` est amorti).
3. **Géométrie et indexation** (déduit de sysfs, à confirmer par mesure) : la taille d'une voie L1 = 64 Ko / 4 = 16 Ko = **taille de page**. Les bits d'index L1 (6–13) sont donc entièrement dans l'offset de page → l'index L1 est contrôlable depuis l'adresse virtuelle, sans pagemap. L2 (1024 sets → bits 6–15) et L3 (2048 sets → bits 6–16, peut-être hachés) exigent des bits d'adresse physique au-delà du décalage de page.
4. Les valeurs sysfs sont **des hypothèses** tant que la phase 2 ne les a pas confirmées (elles peuvent être dérivées de CCSIDR/DT).
5. Bruit potentiel : session graphique + `numa=fake=8` (allocation de pages interleavée sur 8 faux nœuds) ; governor `ondemand`.

## 2. Décision : cross-compilation depuis le PC Windows

- Cible : **`aarch64-unknown-linux-musl`**, liée par `rust-lld` (fourni avec rustc), `+crt-static` → binaire statique, aucun sysroot ni linker externe à installer.
- **Validé** : projet de test compilé sur Windows (rustc 1.95.0), `ELF aarch64 statically linked`, copié et exécuté sur le Pi, `mrs midr_el1` → `0x414fd0b1`.
- Cible ajoutée localement : `rustup target add aarch64-unknown-linux-musl` (seule modification de ton PC).
- `.cargo/config.toml` : `build.target`, `linker = "rust-lld"`, `rustflags = ["-C","target-cpu=cortex-a76","-C","target-feature=+crt-static"]`.
- Déploiement/exécution : script `xtask`-like (`deploy.ps1`/`deploy.sh`) : `cargo build --release` → `pscp` → `plink`. Résultats rapatriés vers `results/<date>/`. Aucun mot de passe écrit dans le dépôt (variable d'environnement ; clé SSH proposée en question ouverte).
- `objdump -d` : exécuté sur le Pi (binutils présent) ou `rust-objdump` local ; l'extrait est archivé dans `docs/`.
- Tests unitaires : `cargo test` sur l'hôte (stats, permutation Sattolo, simulateurs de politiques, encodeur d'instructions — code portable). Tests spécifiques cible (`selftest`) : exécutés sur le Pi.
- Compromis musl vs glibc : `libc` crate + syscalls directs (`perf_event_open`, `sched_setaffinity`, `mmap`) fonctionnent à l'identique ; musl a un `malloc` différent, sans importance car toute mémoire de mesure est allouée par `mmap`. Si un souci apparaît, repli : Zig + `cargo-zigbuild` vers `aarch64-unknown-linux-gnu`.
- `unsafe`/`asm!` : stables (1.95). Intrinsics NEON de `core::arch::aarch64` : je préfère `asm!` pour `sdot`/`fmla`/`ldp q` (contrôle du code généré), donc pas de dépendance nightly.

## 3. Architecture

```
a76probe/                 (workspace unique, lib + bin)
  Cargo.toml  .cargo/config.toml  rust-toolchain.toml (stable)
  src/lib.rs
  src/main.rs             clap : env | selftest | run | report | list
  src/env.rs              snapshot JSON (sysfs, MIDR, cpufreq, thermique, sysctl, CNTFRQ via mrs)
  src/timing.rs           Clock: CNTVCT (isb) | PmuCycles ; surcoût + gigue mesurés
  src/pmu.rs              perf_event_open, groupes, exclude_kernel, énumération sysfs, events bruts
  src/stats.rs            médiane, MAD, percentiles, IC bootstrap (PRNG maison)
  src/affinity.rs         sched_setaffinity, vérif du cœur
  src/guard.rs            température/fréquence avant/après ; invalide les mesures perturbées
  src/output.rs           JSONL (une ligne = une répétition ; tags cond., ordre, temp, freq)
  src/harness.rs          préchauffage, ≥30 répétitions, ordre des conditions alterné/randomisé
  src/mem.rs              mmap + MAP_POPULATE + mlock, tampons alignés
  src/exp/{mem_lat,mem_bw,cache_geom,replacement,prefetch,branch,ooo,multicore}.rs
  src/kernels/*.rs        boucles critiques en asm! (chaque kernel a son extrait objdump archivé)
  src/sim/*.rs            simulateurs LRU, tree-PLRU, random, FIFO, SRRIP/BRRIP
  src/jit.rs              phase 4
  docs/methodology.md  docs/asm/*.txt
  results/<date>/{env.json, raw/*.jsonl, report.md}   RESULTS.md
```

Chaque mesure enregistre : cœur, T° avant/après, `scaling_cur_freq`, drapeau `valid`, source du chrono, compteurs PMU associés.

### Choix techniques à trancher / justifier
- **Chrono** : deux sources implémentées et comparées (surcoût, gigue) dans `selftest` ; le choix par défaut sera documenté d'après les chiffres mesurés, pas a priori. Conversion ticks→cycles via fréquence effective **mesurée** (cycles PMU / temps), pas supposée.
- **PMU** : `perf_event_open` avec `exclude_kernel=1` fonctionne à `paranoid=2` pour son propre processus (à valider). Groupes limités par le nombre de compteurs matériels (à découvrir ; multiplexage détecté via `time_enabled/time_running`, mesure rejetée si < 100 %). Événements bruts (`config=0x..`) pour d'éventuels compteurs propres au A76 : seulement après vérification dans un document que tu fournis, ou par validation expérimentale.
- **Phase 4, génération de code** : JIT à l'exécution (`mmap` RW → écriture → `dc cvau`/`dsb ish`/`ic ivau`/`dsb ish`/`isb` → `mprotect` RX). Justification : les paramètres (N branches, période P, T cibles) varient sur de larges plages ; `build.rs` multiplierait les binaires. À vérifier que le noyau autorise `PROT_EXEC` sur mmap anonyme (SELinux/PaX absents a priori).
- **Phase 2, bits physiques** : voir question Q1.

## 4. Découpage par phase et risques

| Phase | Risque principal | Parade |
|---|---|---|
| 0 | `read()` PMU trop coûteux pour boucles courtes | boucles longues, chrono CNTVCT ; documenter |
| 1 | Pas de hugepages → TLB : un nœud par page de 16 Ko seulement | estimer TLB avec pages 16 Ko ; dire explicitement la limite ; option contiguous/mTHP non dispo |
| 1 | Bruit DRAM / autres cœurs / interleave NUMA fictif | pin cœur 1, cœurs 2-3 seuls utilisés en multi-cœur ; répéter |
| 2 | Bits physiques L2/L3 non contrôlables sans root/hugepages | Q1 ; à défaut : approche statistique (grand nombre de pages, détection des collisions par latence) avec confiance « moyenne/faible » |
| 3 | Préchargeurs non exposés en PMU | sondes temporelles + REFILL ; limites documentées |
| 4 | Écart d'interprétation (BTB à niveaux, fusion de branches) | courbes complètes + corrélation BR_MIS_PRED |
| 5 | Sans PDF Arm, pas de comparaison | on ne compare pas (règle 1) |
| 6 | Topologie : 4 cœurs sur 1 cluster | matrice 4×4 attendue quasi-uniforme ; résultat plat = résultat |
| Tous | Throttling / dérive de fréquence sous `ondemand` | guard + alternance des conditions ; Q2 |

Chaque phase se termine par : tableaux, points surprenants, limites, incertitudes, puis arrêt pour ton feu vert.

## 5. Questions ouvertes (j'attends tes réponses)

- **Q1 — Bits d'adresse physique (phase 2)** : sans hugepages, deux voies : (a) `sudo` pour lire `/proc/self/pagemap` (lecture seule, pas de modification système) ; (b) `/dev/dma_heap/*` (ton utilisateur est dans `video`/`render`) pour obtenir de la mémoire physiquement contiguë sans root, si le mapping est cacheable ; sinon (c) méthode statistique sans root. Ta préférence ? Je te redemanderai avant tout `sudo` concret.
- **Q2 — Governor** : passer en `performance` via `sudo cpupower`/`echo performance | sudo tee .../scaling_governor` (retour : `echo ondemand | sudo tee ...`). Recommandé pour la stabilité de fréquence. Autorises-tu ? Sinon je mesure sous `ondemand` avec garde stricte sur `scaling_cur_freq`.
- **Q3 — `perf_user_access=1`** : `sudo sysctl kernel.perf_user_access=1` (retour : `=0`) permettrait de lire le compteur de cycles PMU directement en user. Autorises-tu ? Sinon lecture par `read()`.
- **Q4 — Bruit système** : OK pour arrêter temporairement lightdm/wayvnc pendant les runs (`sudo systemctl stop lightdm`, retour `start`) ? Sinon on les laisse et on mesure le bruit.
- **Q5 — SSH** : OK pour installer une clé publique dans `~/.ssh/authorized_keys` du Pi (évite le mot de passe dans les commandes et les logs) ? Sinon je continue avec `plink -pw`, mot de passe passé par variable d'environnement.
- **Q6 — Git** : `git init` dans `C:\Users\hugues\Documents\bench` pour des commits atomiques ? (le dossier n'est pas encore un dépôt.)
- **Q7** — `perf` non installé : inutile (énumération par sysfs suffit). Confirmes que je ne l'installe pas ?
- **Q8** — Nom de la cible : musl statique te convient, ou tu préfères glibc (Zig) ?

## 6. Livrable de la phase 0 (après accord)
`a76probe env` et `a76probe selftest` (surcoût/gigue des deux chronos, INST_RETIRED ≈ N sur boucle asm connue, cycles ≈ fréquence × temps, sortie JSONL), `deploy` script, tests unitaires stats/affinity, `PLAN.md` mis à jour.
